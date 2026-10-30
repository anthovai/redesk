//! Session controls a viewer can trigger on this machine, like RustDesk's: view-only mode (shown
//! in the connection manager), blocking the local keyboard and mouse, privacy mode (the screen
//! is blacked out for people at the machine while the viewer still sees it), locking now or when
//! the session ends, and restarting with automatic reconnection.
//!
//! Each control is offered in `info.features` only when this device allows it (Settings →
//! ความปลอดภัย, `aegiron-agent set --allow-…`) and the platform can do it, so older viewers and
//! agents simply do not show it. Every change is reported for the server's audit log.
//!
//! `AEGIRON_TEST_CONTROLS=<file>` (tests only; never set by the app, the installers or the
//! login service) replaces every platform effect with a line appended to that file, so tests
//! never block, black out, lock or restart the machine they run on.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::proto::{self, ViewerMsg};

/// Kept while a platform effect (input blocked, privacy windows) is active; dropping ends it.
pub struct Guard(#[allow(dead_code)] Box<dyn Send>);

impl Guard {
    pub fn new<T: Send + 'static>(inner: T) -> Self {
        Self(Box::new(inner))
    }
}

/// What this machine can do. The real one acts on the system; the test one only logs.
pub trait Platform: Send + Sync {
    fn block_input(&self) -> Result<Guard>;
    fn privacy(&self) -> Result<Guard>;
    fn lock(&self) -> Result<()>;
    fn restart(&self) -> Result<()>;
    /// Whether each control works here: (block input, privacy, lock, restart). `ui`: the
    /// desktop app is attached (macOS privacy mode needs its main thread).
    fn supports(&self, ui: bool) -> [bool; 4];
}

/// The platform for this process: the real system, or the logging stub in tests.
pub fn platform() -> Arc<dyn Platform> {
    match std::env::var_os("AEGIRON_TEST_CONTROLS") {
        Some(p) if !p.is_empty() => Arc::new(DryRun { log: PathBuf::from(p) }),
        _ => Arc::new(System),
    }
}

/// Controls this session may use: allowed by the device and supported by the platform.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Perms {
    pub block_input: bool,
    pub privacy: bool,
    pub lock: bool,
    pub restart: bool,
}

impl Perms {
    /// Blocking input and privacy mode take control away from the local user, so they also
    /// need the input permission; locking and restarting have their own.
    pub fn new(cfg: &Config, supports: [bool; 4]) -> Self {
        let [block, privacy, lock, restart] = supports;
        Self {
            block_input: block && cfg.allow_input && cfg.allow_block_input,
            privacy: privacy && cfg.allow_input && cfg.allow_privacy_mode,
            lock: lock && cfg.allow_lock,
            restart: restart && cfg.allow_restart,
        }
    }

    /// Entries for `info.features`. `view_only` is always there: the agent shows it in the
    /// connection manager and stops injecting input while it is on.
    pub fn features(&self) -> Vec<&'static str> {
        let mut f = vec!["view_only"];
        for (on, name) in [(self.block_input, "block_input"), (self.privacy, "privacy"), (self.lock, "lock"), (self.restart, "restart")] {
            if on {
                f.push(name);
            }
        }
        f
    }
}

/// State of one session's controls, shared with the connection manager.
#[derive(Debug, Default)]
pub struct Flags {
    pub view_only: AtomicBool,
    pub block_input: AtomicBool,
    pub privacy: AtomicBool,
    pub lock_after: AtomicBool,
}

type Report = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// The controls of one session. Dropping it (however the session ends) unblocks input, removes
/// the privacy windows and, if asked for, locks the machine.
pub struct SessionCtl {
    pub perms: Perms,
    pub flags: Arc<Flags>,
    platform: Arc<dyn Platform>,
    report: Report,
    config_path: PathBuf,
    email: String,
    /// Messages to the viewer from background work (a restart that failed).
    out: Option<mpsc::Sender<Vec<u8>>>,
    block: Option<Guard>,
    privacy: Option<(Guard, Guard)>,
}

impl SessionCtl {
    pub fn new(perms: Perms, platform: Arc<dyn Platform>, report: Report, config_path: &Path, email: &str, out: Option<mpsc::Sender<Vec<u8>>>) -> Self {
        Self {
            perms,
            flags: Arc::default(),
            platform,
            report,
            config_path: config_path.to_path_buf(),
            email: email.to_string(),
            out,
            block: None,
            privacy: None,
        }
    }

    /// Messages for the viewer from background work (once the session's queue exists).
    pub fn set_out(&mut self, out: mpsc::Sender<Vec<u8>>) {
        self.out = Some(out);
    }

    /// Input from the viewer is injected only while it is not in view-only mode.
    pub fn input_allowed(&self) -> bool {
        !self.flags.view_only.load(Ordering::Relaxed)
    }

    /// The person at the machine cannot use it right now (input blocked or screen blacked out).
    pub fn blocking(&self) -> bool {
        self.block.is_some() || self.privacy.is_some()
    }

    /// Handles a control message; returns the `ctl` reply, or None for other messages.
    pub fn handle(&mut self, msg: &ViewerMsg) -> Option<Value> {
        let reply = |op: &str, r: Result<Option<bool>>| match r {
            Ok(on) => json!({"t": "ctl", "op": op, "ok": true, "on": on}),
            Err(e) => json!({"t": "ctl", "op": op, "ok": false, "message": format!("{e:#}")}),
        };
        Some(match *msg {
            ViewerMsg::ViewOnly { on } => {
                if self.flags.view_only.swap(on, Ordering::Relaxed) != on {
                    (self.report)("view_only", json!({"on": on}));
                }
                reply("view_only", Ok(Some(on)))
            }
            ViewerMsg::BlockInput { on } => reply("block_input", self.set_block(on).map(Some)),
            ViewerMsg::Privacy { on } => reply("privacy", self.set_privacy(on).map(Some)),
            ViewerMsg::Lock => reply("lock", self.lock().map(|_| None)),
            ViewerMsg::LockAfter { on } => reply(
                "lock_after",
                self.allowed(self.perms.lock, "locking").map(|_| {
                    if self.flags.lock_after.swap(on, Ordering::Relaxed) != on {
                        (self.report)("lock_after_session", json!({"on": on}));
                    }
                    Some(on)
                }),
            ),
            ViewerMsg::Restart => match self.restart() {
                Ok(token) => json!({"t": "ctl", "op": "restart", "ok": true, "resume": token}),
                Err(e) => reply("restart", Err(e)),
            },
            _ => return None,
        })
    }

    fn allowed(&self, ok: bool, what: &str) -> Result<()> {
        if ok { Ok(()) } else { bail!("{what} is not allowed on this device") }
    }

    fn set_block(&mut self, on: bool) -> Result<bool> {
        self.allowed(self.perms.block_input, "blocking input")?;
        match (on, self.block.is_some()) {
            (true, false) => self.block = Some(self.platform.block_input()?),
            (false, true) => self.block = None,
            _ => return Ok(on),
        }
        self.flags.block_input.store(on, Ordering::Relaxed);
        (self.report)("block_input", json!({"on": on}));
        Ok(on)
    }

    /// Privacy mode also blocks input: nobody should work blind on a black screen.
    fn set_privacy(&mut self, on: bool) -> Result<bool> {
        self.allowed(self.perms.privacy, "privacy mode")?;
        match (on, self.privacy.is_some()) {
            (true, false) => {
                let block = self.platform.block_input()?;
                self.privacy = Some((self.platform.privacy()?, block));
            }
            (false, true) => self.privacy = None,
            _ => return Ok(on),
        }
        self.flags.privacy.store(on, Ordering::Relaxed);
        (self.report)("privacy_mode", json!({"on": on}));
        Ok(on)
    }

    fn lock(&self) -> Result<()> {
        self.allowed(self.perms.lock, "locking")?;
        self.platform.lock()?;
        (self.report)("lock_screen", json!({}));
        Ok(())
    }

    /// Issues a resume token, then restarts a moment later so the reply reaches the viewer
    /// first. If the restart fails the token is revoked and the viewer told.
    fn restart(&self) -> Result<String> {
        self.allowed(self.perms.restart, "restarting")?;
        let token = issue_resume(&self.config_path, &self.email)?;
        (self.report)("restart", json!({}));
        let (platform, out, path, t, report) = (self.platform.clone(), self.out.clone(), self.config_path.clone(), token.clone(), self.report.clone());
        std::thread::spawn(move || {
            std::thread::sleep(RESTART_DELAY);
            if let Err(e) = platform.restart() {
                tracing::warn!("restart failed: {e:#}");
                take_resume(&path, None, &t);
                report("restart_failed", json!({"error": format!("{e:#}")}));
                if let Some(out) = out {
                    let _ = out.blocking_send(proto::json(&json!({"t": "ctl", "op": "restart", "ok": false, "message": format!("{e:#}")})));
                }
            }
        });
        Ok(token)
    }
}

const RESTART_DELAY: Duration = Duration::from_millis(1200);

impl Drop for SessionCtl {
    fn drop(&mut self) {
        // Unblock first, so the lock screen is usable.
        self.block = None;
        self.privacy = None;
        if self.flags.lock_after.load(Ordering::Relaxed) && self.perms.lock {
            match self.platform.lock() {
                Ok(()) => (self.report)("lock_screen", json!({"after_session": true})),
                Err(e) => tracing::warn!("could not lock after the session: {e:#}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Local escape: Ctrl+Alt+Shift+Esc at the machine ends every session that blocks it.

/// The key combination that gives the machine back to the person in front of it.
pub const ESCAPE_HINT: &str = "Ctrl + Alt + Shift + Esc";

/// Counts escapes; sessions watch it (a watch channel cannot miss one between polls).
fn escapes() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Called by the input blocker when the escape combination is pressed locally.
pub fn escaped() {
    tracing::warn!("{ESCAPE_HINT} pressed at this machine: ending sessions that block it");
    escapes().send_modify(|n| *n += 1);
}

/// Changes whenever the person at the machine presses the escape combination.
pub fn escape_signal() -> tokio::sync::watch::Receiver<u64> {
    escapes().subscribe()
}

// ---------------------------------------------------------------------------------------------
// Resume tokens: reconnecting after a restart the viewer asked for.

/// How long a viewer may take to come back after a restart.
const RESUME_TTL: u64 = 15 * 60;
const MAX_RESUME: usize = 4;

#[derive(Serialize, Deserialize)]
struct Resume {
    /// SHA-256 of the token (hex); the token itself only travels inside the encrypted channel.
    hash: String,
    email: String,
    expires: u64,
}

fn resume_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("resume.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn load_resume(path: &Path) -> Vec<Resume> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Vec<Resume>>(&b).ok()).unwrap_or_default().into_iter().filter(|r| r.expires > now()).collect()
}

fn save_resume(path: &Path, list: &[Resume]) -> Result<()> {
    crate::signal::write_private(path, &serde_json::to_string(list)?)?;
    Ok(())
}

/// A single-use token that lets `email` reconnect within 15 minutes without the password or a
/// click. Stored hashed next to the config (owner-only), so it survives the restart.
pub fn issue_resume(config_path: &Path, email: &str) -> Result<String> {
    let token = B64URL.encode(crate::crypto::random_bytes::<32>());
    let path = resume_path(config_path);
    let mut list = load_resume(&path);
    list.push(Resume { hash: hash(&token), email: email.to_lowercase(), expires: now() + RESUME_TTL });
    let skip = list.len().saturating_sub(MAX_RESUME);
    save_resume(&path, &list[skip..]).map_err(|e| anyhow!("cannot store the reconnect token: {e:#}"))?;
    Ok(token)
}

/// Consumes a resume token. `email`: the viewer must be the one it was issued to (None only
/// revokes).
pub fn take_resume(config_path: &Path, email: Option<&str>, token: &str) -> bool {
    let path = resume_path(config_path);
    let mut list = load_resume(&path);
    let h = hash(token);
    let Some(i) = list.iter().position(|r| r.hash == h) else { return false };
    let r = list.remove(i);
    if let Err(e) = save_resume(&path, &list) {
        tracing::warn!("cannot update reconnect tokens: {e:#}");
        return false; // never accept a token that could be used again
    }
    email.is_some_and(|e| e.to_lowercase() == r.email)
}

// ---------------------------------------------------------------------------------------------
// Platforms

/// The real system.
struct System;

impl Platform for System {
    fn block_input(&self) -> Result<Guard> {
        crate::block_input::acquire().map(Guard::new)
    }

    fn privacy(&self) -> Result<Guard> {
        crate::privacy::show().map(Guard::new)
    }

    fn lock(&self) -> Result<()> {
        lock_screen()
    }

    fn restart(&self) -> Result<()> {
        run_first(&restart_commands(), &mut |c| {
            let out = std::process::Command::new(c[0]).args(&c[1..]).stdin(std::process::Stdio::null()).output()?;
            match out.status.success() {
                true => Ok(()),
                false => bail!("{}", String::from_utf8_lossy(&out.stderr).trim()),
            }
        })
    }

    fn supports(&self, ui: bool) -> [bool; 4] {
        [crate::block_input::supported(), crate::privacy::supported(ui), cfg!(any(target_os = "macos", windows, target_os = "linux")), !restart_commands().is_empty()]
    }
}

/// Logs what would have happened (AEGIRON_TEST_CONTROLS).
struct DryRun {
    log: PathBuf,
}

impl DryRun {
    fn log(path: &Path, line: &str) {
        use std::io::Write;
        tracing::info!("test controls: {line}");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

struct DryRunGuard(PathBuf, &'static str);

impl Drop for DryRunGuard {
    fn drop(&mut self) {
        DryRun::log(&self.0, &format!("{} off", self.1));
    }
}

impl Platform for DryRun {
    fn block_input(&self) -> Result<Guard> {
        Self::log(&self.log, "block_input on");
        Ok(Guard::new(DryRunGuard(self.log.clone(), "block_input")))
    }

    fn privacy(&self) -> Result<Guard> {
        Self::log(&self.log, "privacy on");
        Ok(Guard::new(DryRunGuard(self.log.clone(), "privacy")))
    }

    fn lock(&self) -> Result<()> {
        Self::log(&self.log, "lock");
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        Self::log(&self.log, &format!("restart: {}", restart_commands().first().map(|c| c.join(" ")).unwrap_or_default()));
        Ok(())
    }

    fn supports(&self, _ui: bool) -> [bool; 4] {
        [true; 4]
    }
}

/// Restart commands as the logged-in user, tried in order. macOS: System Events asks apps to
/// quit like the Apple menu does (the first use asks the person at the machine to let Aegiron
/// control System Events); loginwindow is the fallback.
fn restart_commands() -> Vec<Vec<&'static str>> {
    if cfg!(target_os = "macos") {
        vec![
            vec!["osascript", "-e", "tell application \"System Events\" to restart"],
            vec!["osascript", "-e", "tell application \"loginwindow\" to «event aevtrrst»"],
        ]
    } else if cfg!(windows) {
        vec![vec!["shutdown", "/r", "/t", "0"]]
    } else {
        Vec::new()
    }
}

/// Runs the first command that succeeds; the error of the last one otherwise.
fn run_first(cmds: &[Vec<&str>], run: &mut dyn FnMut(&[&str]) -> Result<()>) -> Result<()> {
    let mut last = anyhow!("restarting is not supported on this system");
    for c in cmds {
        match run(c) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("{}: {e:#}", c.join(" "));
                last = e;
            }
        }
    }
    Err(last)
}

#[cfg(target_os = "macos")]
fn lock_screen() -> Result<()> {
    use std::ffi::c_void;
    // SACLockScreenImmediate (login.framework) locks like ⌃⌘Q without injecting keys; the
    // shortcut itself is the fallback.
    let lib = c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login";
    // SAFETY: dlopen/dlsym with valid C strings; the symbol has the signature `int (void)`.
    unsafe {
        let h = libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY);
        if !h.is_null() {
            let f = libc::dlsym(h, c"SACLockScreenImmediate".as_ptr());
            if !f.is_null() {
                let f: extern "C" fn() -> i32 = std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(f);
                if f() == 0 {
                    return Ok(());
                }
            }
        }
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
        fn CFRelease(cf: *const c_void);
    }
    const KEY_Q: u16 = 12;
    const FLAGS: u64 = 0x0004_0000 | 0x0010_0000; // control | command
    // SAFETY: events are created, posted and released here.
    unsafe {
        for down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), KEY_Q, down);
            if ev.is_null() {
                bail!("cannot create the lock shortcut");
            }
            CGEventSetFlags(ev, FLAGS);
            CGEventPost(0, ev);
            CFRelease(ev);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn lock_screen() -> Result<()> {
    // SAFETY: no arguments; fails only without an interactive desktop.
    unsafe { windows::Win32::System::Shutdown::LockWorkStation() }.map_err(|e| anyhow!("LockWorkStation: {e}"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn lock_screen() -> Result<()> {
    let ok = std::process::Command::new("loginctl").arg("lock-session").status().map(|s| s.success()).unwrap_or(false);
    if ok { Ok(()) } else { bail!("loginctl lock-session failed") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aegiron-controls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg() -> Config {
        Config::new("https://a.example".into(), base64::engine::general_purpose::STANDARD.encode([7u8; 32])).unwrap()
    }

    #[test]
    fn features_follow_permissions_and_platform() {
        let mut c = cfg();
        assert_eq!(Perms::new(&c, [true; 4]).features(), ["view_only", "block_input", "privacy", "lock", "restart"]);
        // Old configs without the new keys allow everything, like RustDesk's defaults.
        let old: Config = toml::from_str(&toml::to_string(&c).unwrap().replace("allow_block_input = true\n", "")).unwrap();
        assert!(old.allow_block_input);
        // No input permission: nothing that takes the machine away from its user.
        c.allow_input = false;
        assert_eq!(Perms::new(&c, [true; 4]).features(), ["view_only", "lock", "restart"]);
        c.allow_input = true;
        c.allow_privacy_mode = false;
        c.allow_restart = false;
        assert_eq!(Perms::new(&c, [true; 4]).features(), ["view_only", "block_input", "lock"]);
        // Unsupported here (e.g. macOS privacy without the desktop app, restart on Linux).
        assert_eq!(Perms::new(&cfg(), [true, false, true, false]).features(), ["view_only", "block_input", "lock"]);
    }

    #[test]
    fn resume_tokens_are_single_use_and_bound_to_the_viewer() {
        let d = dir("resume");
        let cp = d.join("agent.toml");
        let t = issue_resume(&cp, "Me@Example.com").unwrap();
        assert!(!take_resume(&cp, Some("me@example.com"), "wrong"));
        assert!(take_resume(&cp, Some("me@example.com"), &t));
        assert!(!take_resume(&cp, Some("me@example.com"), &t), "used twice");
        // Another viewer: refused, and the token is gone.
        let t = issue_resume(&cp, "me@example.com").unwrap();
        assert!(!take_resume(&cp, Some("other@example.com"), &t));
        assert!(!take_resume(&cp, Some("me@example.com"), &t));
        // Only hashes on disk, owner-only.
        let t = issue_resume(&cp, "me@example.com").unwrap();
        let disk = std::fs::read_to_string(d.join("resume.json")).unwrap();
        assert!(!disk.contains(&t) && disk.contains(&hash(&t)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("resume.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Expired tokens are dropped.
        let expired = vec![Resume { hash: hash("old"), email: "me@example.com".into(), expires: now() - 1 }];
        save_resume(&resume_path(&cp), &expired).unwrap();
        assert!(!take_resume(&cp, Some("me@example.com"), "old"));
        // At most a few at a time.
        let tokens: Vec<_> = (0..6).map(|_| issue_resume(&cp, "me@example.com").unwrap()).collect();
        assert_eq!(load_resume(&resume_path(&cp)).len(), MAX_RESUME);
        assert!(!take_resume(&cp, Some("me@example.com"), &tokens[0]));
        assert!(take_resume(&cp, Some("me@example.com"), &tokens[5]));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn restart_tries_commands_in_order() {
        let cmds = vec![vec!["a", "1"], vec!["b"], vec!["c"]];
        let mut seen = Vec::new();
        let r = run_first(&cmds, &mut |c| {
            seen.push(c[0].to_string());
            if c[0] == "b" { Ok(()) } else { bail!("no {}", c[0]) }
        });
        assert!(r.is_ok());
        assert_eq!(seen, ["a", "b"]);
        let r = run_first(&cmds, &mut |c| bail!("no {}", c[0]));
        assert_eq!(r.unwrap_err().to_string(), "no c");
        assert!(run_first(&[], &mut |_| Ok(())).is_err());
        // Each supported OS has a user-level command.
        if cfg!(any(target_os = "macos", windows)) {
            assert!(!restart_commands().is_empty());
        }
    }

    /// Records calls and the guards that are alive.
    #[derive(Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        fail_block: AtomicBool,
    }

    struct FakeGuard(Arc<Mutex<Vec<String>>>, &'static str);
    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(format!("{} off", self.1));
        }
    }

    impl Platform for Fake {
        fn block_input(&self) -> Result<Guard> {
            if self.fail_block.load(Ordering::Relaxed) {
                bail!("Accessibility permission missing");
            }
            self.calls.lock().unwrap().push("block on".into());
            Ok(Guard::new(FakeGuard(self.calls.clone(), "block")))
        }
        fn privacy(&self) -> Result<Guard> {
            self.calls.lock().unwrap().push("privacy on".into());
            Ok(Guard::new(FakeGuard(self.calls.clone(), "privacy")))
        }
        fn lock(&self) -> Result<()> {
            self.calls.lock().unwrap().push("lock".into());
            Ok(())
        }
        fn restart(&self) -> Result<()> {
            self.calls.lock().unwrap().push("restart".into());
            bail!("cancelled by an app")
        }
        fn supports(&self, _: bool) -> [bool; 4] {
            [true; 4]
        }
    }

    fn session(perms: Perms, name: &str) -> (SessionCtl, Arc<Fake>, Arc<Mutex<Vec<String>>>, PathBuf, mpsc::Receiver<Vec<u8>>) {
        let fake = Arc::new(Fake::default());
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let ev = events.clone();
        let report: Report = Arc::new(move |k, d| ev.lock().unwrap().push(format!("{k} {d}")));
        let d = dir(name);
        let (tx, rx) = mpsc::channel(4);
        let s = SessionCtl::new(perms, fake.clone(), report, &d.join("agent.toml"), "me@example.com", Some(tx));
        (s, fake, events, d, rx)
    }

    const ALL: Perms = Perms { block_input: true, privacy: true, lock: true, restart: true };

    #[test]
    fn session_controls_act_report_and_clean_up() {
        let (mut s, fake, events, d, _rx) = session(ALL, "session");
        let ok = |v: Option<Value>| v.unwrap()["ok"].as_bool().unwrap();
        assert!(s.input_allowed());
        assert!(ok(s.handle(&ViewerMsg::ViewOnly { on: true })));
        assert!(!s.input_allowed() && s.flags.view_only.load(Ordering::Relaxed));
        assert!(ok(s.handle(&ViewerMsg::BlockInput { on: true })));
        assert!(ok(s.handle(&ViewerMsg::BlockInput { on: true })), "idempotent");
        assert!(s.blocking());
        assert!(ok(s.handle(&ViewerMsg::Privacy { on: true })));
        assert!(ok(s.handle(&ViewerMsg::BlockInput { on: false })));
        assert!(s.blocking(), "privacy still blocks");
        assert!(ok(s.handle(&ViewerMsg::Privacy { on: false })));
        assert!(!s.blocking());
        assert!(ok(s.handle(&ViewerMsg::Lock)));
        assert!(ok(s.handle(&ViewerMsg::LockAfter { on: true })));
        assert!(s.handle(&ViewerMsg::Refresh).is_none());
        assert_eq!(*fake.calls.lock().unwrap(), ["block on", "block on", "privacy on", "block off", "privacy off", "block off", "lock"]);
        assert!(ok(s.handle(&ViewerMsg::BlockInput { on: true })));
        drop(s);
        // Ending the session unblocks before locking.
        assert_eq!(fake.calls.lock().unwrap()[7..], ["block on", "block off", "lock"]);
        let ev = events.lock().unwrap().join("\n");
        for k in [r#"view_only {"on":true}"#, r#"block_input {"on":true}"#, r#"privacy_mode {"on":true}"#, r#"privacy_mode {"on":false}"#, "lock_screen {}", r#"lock_after_session {"on":true}"#, r#"lock_screen {"after_session":true}"#] {
            assert!(ev.contains(k), "missing {k} in\n{ev}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn refused_without_permission_or_when_the_platform_fails() {
        let (mut s, fake, events, d, _rx) = session(Perms { lock: true, ..Default::default() }, "refused");
        for m in [ViewerMsg::BlockInput { on: true }, ViewerMsg::Privacy { on: true }, ViewerMsg::Restart] {
            let r = s.handle(&m).unwrap();
            assert_eq!(r["ok"], false);
            assert!(r["message"].as_str().unwrap().contains("not allowed"));
        }
        assert!(!s.blocking());
        drop(s);
        let (mut s, fake2, _, d2, _rx2) = session(ALL, "fail");
        fake2.fail_block.store(true, Ordering::Relaxed);
        let r = s.handle(&ViewerMsg::Privacy { on: true }).unwrap();
        assert_eq!(r["ok"], false);
        assert!(r["message"].as_str().unwrap().contains("Accessibility"));
        assert!(!s.blocking() && !s.flags.privacy.load(Ordering::Relaxed));
        assert!(fake.calls.lock().unwrap().is_empty() && fake2.calls.lock().unwrap().is_empty());
        assert!(events.lock().unwrap().is_empty());
        drop(s);
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::remove_dir_all(&d2).unwrap();
    }

    #[test]
    fn failed_restart_revokes_the_token_and_tells_the_viewer() {
        let (mut s, fake, events, d, mut rx) = session(ALL, "restart");
        let r = s.handle(&ViewerMsg::Restart).unwrap();
        assert_eq!(r["ok"], true);
        let token = r["resume"].as_str().unwrap().to_string();
        let msg = rx.blocking_recv().unwrap();
        let v: Value = serde_json::from_slice(&msg[1..]).unwrap();
        assert_eq!((v["op"].as_str(), v["ok"].as_bool()), (Some("restart"), Some(false)));
        assert_eq!(*fake.calls.lock().unwrap(), ["restart"]);
        assert!(!take_resume(&d.join("agent.toml"), Some("me@example.com"), &token), "revoked");
        assert!(events.lock().unwrap().iter().any(|e| e.starts_with("restart_failed")));
        drop(s);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
