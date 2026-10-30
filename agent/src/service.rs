//! Run the agent at login as a per-user service (no root): launchd on macOS, systemd --user on Linux.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const LABEL: &str = "com.aegiron.agent";
/// Names used before the rename to Aegiron; removed on install so two agents never run.
const LEGACY_LABEL: &str = "com.redesk.agent";
const LEGACY_UNIT: &str = "redesk-agent.service";
const LEGACY_RUN_VALUE: &str = "ReDesk";

fn home() -> Result<PathBuf> {
    dirs::home_dir().context("no home directory")
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let mut c = Command::new(cmd);
    c.args(args);
    // The desktop app has no console: without this every `reg` call flashes a console window.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
    let out = c.output().with_context(|| format!("run {cmd}"))?;
    if !out.status.success() {
        bail!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn unit_path() -> Result<PathBuf> {
    Ok(dirs::config_dir().context("no config dir")?.join("systemd/user/aegiron-agent.service"))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(if cfg!(target_os = "macos") {
        home()?.join("Library/Logs/Aegiron/agent.log")
    } else {
        dirs::state_dir().or_else(dirs::data_local_dir).context("no state dir")?.join("aegiron/agent.log")
    })
}

fn launchd_plist(exe: &Path, config: &Path, log: &Path) -> String {
    let args = [exe.display().to_string(), "--config".into(), config.display().to_string(), "run".into()]
        .iter()
        .map(|a| format!("    <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");
    let log = xml_escape(&log.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Drops Windows' verbatim prefix (\\?\) that canonicalize adds; shells and the Run key don't accept it.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

const WIN_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Prefers the desktop app next to this binary (starts in the tray), else the CLI agent.
fn windows_autostart_command(exe: &Path, config: &Path) -> String {
    let gui = exe.with_file_name("aegiron.exe");
    if gui.exists() {
        format!("\"{}\" --background", gui.display())
    } else {
        format!("\"{}\" --config \"{}\" run", exe.display(), config.display())
    }
}

/// Whether the agent is set to start automatically for this user.
pub fn is_installed() -> bool {
    if cfg!(target_os = "macos") {
        plist_path().is_ok_and(|p| p.exists())
    } else if cfg!(target_os = "linux") {
        unit_path().is_ok_and(|p| p.exists())
    } else if cfg!(windows) {
        run("reg", &["query", WIN_RUN_KEY, "/v", "Aegiron"]).is_ok()
    } else {
        false
    }
}

/// Stops and deletes the autostart entry of a ReDesk (pre-rename) agent, if any.
fn remove_legacy() {
    if cfg!(target_os = "macos") {
        if let (Ok(uid), Ok(home)) = (run("id", &["-u"]), home()) {
            let _ = run("launchctl", &["bootout", &format!("gui/{}/{LEGACY_LABEL}", uid.trim())]);
            let _ = std::fs::remove_file(home.join("Library/LaunchAgents").join(format!("{LEGACY_LABEL}.plist")));
        }
    } else if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "disable", "--now", LEGACY_UNIT]);
        if let Some(dir) = dirs::config_dir() {
            let _ = std::fs::remove_file(dir.join("systemd/user").join(LEGACY_UNIT));
        }
    } else if cfg!(windows) {
        let _ = run("reg", &["delete", WIN_RUN_KEY, "/v", LEGACY_RUN_VALUE, "/f"]);
    }
}

fn legacy_installed() -> bool {
    if cfg!(target_os = "macos") {
        home().is_ok_and(|h| h.join("Library/LaunchAgents").join(format!("{LEGACY_LABEL}.plist")).exists())
    } else if cfg!(target_os = "linux") {
        dirs::config_dir().is_some_and(|d| d.join("systemd/user").join(LEGACY_UNIT).exists())
    } else if cfg!(windows) {
        run("reg", &["query", WIN_RUN_KEY, "/v", LEGACY_RUN_VALUE]).is_ok()
    } else {
        false
    }
}

/// After upgrading from ReDesk: swaps its start-at-login entry for Aegiron's (same config), so
/// the machine keeps starting the agent and the deleted old app is not relaunched forever.
/// Returns whether anything was migrated.
pub fn migrate_legacy(config: &Path) -> Result<bool> {
    if !legacy_installed() {
        return Ok(false);
    }
    remove_legacy();
    if !is_installed() {
        install(config)?;
    }
    Ok(true)
}

pub fn install(config: &Path) -> Result<()> {
    remove_legacy();
    let exe = plain(std::env::current_exe()?.canonicalize()?);
    let config = plain(config.canonicalize().context("config not found; run `aegiron-agent init` first")?);
    let log = log_path()?;
    std::fs::create_dir_all(log.parent().unwrap())?;

    if cfg!(target_os = "macos") {
        let plist = plist_path()?;
        std::fs::create_dir_all(plist.parent().unwrap())?;
        std::fs::write(&plist, launchd_plist(&exe, &config, &log))?;
        let uid = run("id", &["-u"])?.trim().to_string();
        let domain = format!("gui/{uid}");
        let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
        run("launchctl", &["bootstrap", &domain, &plist.display().to_string()])?;
        println!("Installed LaunchAgent {}", plist.display());
    } else if cfg!(target_os = "linux") {
        let unit = unit_path()?;
        std::fs::create_dir_all(unit.parent().unwrap())?;
        std::fs::write(
            &unit,
            format!(
                "[Unit]\nDescription=Aegiron remote desktop agent\nAfter=graphical-session.target network-online.target\n\n\
                 [Service]\nExecStart=\"{}\" --config \"{}\" run\nRestart=always\nRestartSec=5\nStandardOutput=append:{}\nStandardError=append:{}\n\n\
                 [Install]\nWantedBy=default.target\n",
                exe.display(),
                config.display(),
                log.display(),
                log.display()
            ),
        )?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "aegiron-agent.service"])?;
        println!("Installed systemd user unit {}", unit.display());
    } else if cfg!(windows) {
        // Per-user "Run" entry: the agent needs the user's desktop session to capture the screen.
        let cmd = windows_autostart_command(&exe, &config);
        run("reg", &["add", WIN_RUN_KEY, "/v", "Aegiron", "/t", "REG_SZ", "/d", &cmd, "/f"])?;
        println!("Registered to start at logon: {cmd}");
    } else {
        bail!("service install is not supported on this platform");
    }
    println!("Logs: {}", log.display());
    Ok(())
}

pub fn uninstall() -> Result<()> {
    remove_legacy();
    if cfg!(target_os = "macos") {
        let uid = run("id", &["-u"])?.trim().to_string();
        let _ = run("launchctl", &["bootout", &format!("gui/{uid}/{LABEL}")]);
        let p = plist_path()?;
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    } else if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "disable", "--now", "aegiron-agent.service"]);
        let p = unit_path()?;
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    } else if cfg!(windows) {
        let _ = run("reg", &["delete", WIN_RUN_KEY, "/v", "Aegiron", "/f"]);
    } else {
        bail!("not supported on this platform");
    }
    println!("Service removed.");
    Ok(())
}

pub fn status() -> Result<()> {
    let state = if cfg!(target_os = "macos") {
        let uid = run("id", &["-u"])?.trim().to_string();
        match run("launchctl", &["print", &format!("gui/{uid}/{LABEL}")]) {
            Ok(out) => out.lines().find(|l| l.trim_start().starts_with("state =")).unwrap_or("installed").trim().to_string(),
            Err(_) => "not installed".into(),
        }
    } else if cfg!(target_os = "linux") {
        Command::new("systemctl")
            .args(["--user", "is-active", "aegiron-agent.service"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into())
    } else if cfg!(windows) {
        if is_installed() { "starts at logon".into() } else { "not installed".into() }
    } else {
        "unsupported".into()
    };
    println!("Service:      {state}");
    println!("Logs:         {}", log_path()?.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_paths() {
        let p = launchd_plist(Path::new("/Apps/R&D <x>/aegiron-agent"), Path::new("/cfg/agent.toml"), Path::new("/log/a.log"));
        assert!(p.contains("<string>/Apps/R&amp;D &lt;x&gt;/aegiron-agent</string>"));
        assert!(p.contains("<string>run</string>"));
        if let Ok(dir) = std::env::var("AEGIRON_PLIST_OUT") {
            std::fs::write(Path::new(&dir).join("test.plist"), p).unwrap();
        }
    }
}
