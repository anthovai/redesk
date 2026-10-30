//! Remote terminal sessions (like RustDesk 1.4's terminal): a shell on the controlled machine
//! in a pseudo-terminal (openpty on macOS/Linux, ConPTY on Windows), running as the agent's
//! user. PTY output goes to the viewer as `0x11` messages, keystrokes come back the same way;
//! Ctrl-C is just byte 0x03, which the PTY turns into SIGINT (docs/PROTOCOL.md §11).

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::json;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::proto;
use crate::recording::Recorder;
use crate::remote::{self, Conn};
use crate::signal::Agent;

pub const MAX_COLS: u16 = 500;
pub const MAX_ROWS: u16 = 300;

fn size(cols: u64, rows: u64) -> PtySize {
    PtySize { cols: (cols as u16).clamp(2, MAX_COLS), rows: (rows as u16).clamp(1, MAX_ROWS), pixel_width: 0, pixel_height: 0 }
}

/// The shell for this user: `$SHELL` (else the account's shell, else /bin/sh) as a login shell
/// on Unix; PowerShell (else `%COMSPEC%`) on Windows.
pub fn shell() -> CommandBuilder {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let ps = std::path::Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let prog = if ps.exists() { ps.to_string_lossy().into_owned() } else { std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()) };
        let mut cmd = CommandBuilder::new(prog);
        if ps.exists() {
            cmd.arg("-NoLogo");
        }
        cmd
    }
    #[cfg(unix)]
    {
        let sh = std::env::var("SHELL").ok().filter(|s| s.starts_with('/') && std::path::Path::new(s).exists()).or_else(account_shell).unwrap_or_else(|| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");
        cmd
    }
}

#[cfg(unix)]
fn account_shell() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage (or null); we copy the string out
    // at once and never keep the pointer.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned();
        (s.starts_with('/') && std::path::Path::new(&s).exists()).then_some(s)
    }
}

fn command() -> CommandBuilder {
    let mut cmd = shell();
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("AEGIRON_SESSION", "terminal");
    // Services started by launchd/systemd often have no locale: without one, shells show Thai
    // and other non-ASCII text as escapes.
    #[cfg(unix)]
    if ["LC_ALL", "LC_CTYPE", "LANG"].iter().all(|v| std::env::var_os(v).is_none_or(|s| s.is_empty())) {
        cmd.env("LANG", "en_US.UTF-8");
    }
    cmd
}

/// A running shell in a PTY.
struct Pty {
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
}

impl Pty {
    /// Starts the shell; output arrives on the returned channel (bounded, so a slow viewer
    /// slows the reader), and the exit code on the oneshot.
    fn spawn(sz: PtySize) -> Result<(Pty, mpsc::Receiver<Vec<u8>>, oneshot::Receiver<u32>, String)> {
        let pair = native_pty_system().openpty(sz).context("cannot open a pseudo-terminal")?;
        let cmd = command();
        let shell = cmd.get_argv().first().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut child = pair.slave.spawn_command(cmd).context("cannot start the shell")?;
        // Only the child keeps the slave open, so the reader sees EOF once the shell exits.
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;

        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(32);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(b) = in_rx.recv() {
                if writer.write_all(&b).and_then(|_| writer.flush()).is_err() {
                    return;
                }
            }
        });
        let (exit_tx, exit_rx) = oneshot::channel();
        std::thread::spawn(move || {
            let code = child.wait().map(|s| s.exit_code()).unwrap_or(1);
            let _ = exit_tx.send(code);
        });
        Ok((Pty { master: pair.master, killer, input: in_tx }, out_rx, exit_rx, shell))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

/// Runs an approved terminal session on the agent until the shell exits, the viewer leaves or
/// the local user disconnects it.
pub async fn serve(agent: &Arc<Agent>, sid: &str, conn: &mut Conn, kill: Arc<Notify>, recorder: &mut Option<Recorder>) -> Result<()> {
    conn.send_json(json!({
        "t": "info",
        "session": "terminal",
        "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        "os": std::env::consts::OS,
        "recording": recorder.is_some(),
    }))
    .await?;

    // The viewer says how big its terminal is before the shell starts.
    let sz = loop {
        let Some(plain) = conn.recv().await? else { return Ok(()) };
        let Some(m) = remote::json_of(&plain) else { continue };
        match m["t"].as_str().unwrap_or("") {
            "term_open" => break size(m["cols"].as_u64().unwrap_or(80), m["rows"].as_u64().unwrap_or(24)),
            "bye" => return Ok(()),
            _ => {}
        }
    };
    let (pty, mut out, mut exited, shell) = match Pty::spawn(sz) {
        Ok(p) => p,
        Err(e) => {
            conn.send_json(json!({"t": "term_exit", "code": null, "message": format!("{e:#}")})).await?;
            bail!("{e:#}");
        }
    };
    tracing::info!("session {sid}: terminal opened ({shell}, {}x{})", sz.cols, sz.rows);
    agent.report(sid, "terminal_open", json!({"shell": shell, "cols": sz.cols, "rows": sz.rows}));
    let record = |rec: &mut Option<Recorder>, plain: &[u8]| {
        if let Some(r) = rec.as_mut() {
            r.record(plain);
        }
    };
    record(recorder, &proto::json(&json!({"t": "term_size", "cols": sz.cols, "rows": sz.rows})));
    conn.send_json(json!({"t": "term_started", "shell": shell})).await?;

    let code = loop {
        tokio::select! {
            _ = kill.notified() => {
                tracing::info!("session {sid}: disconnected by the local user");
                agent.report(sid, "denied", json!({"stage": "during_session"}));
                agent.report(sid, "terminal_closed", json!({"reason": "local_user"}));
                return Ok(());
            }
            o = out.recv() => match o {
                Some(bytes) => {
                    let plain = proto::term_data(&bytes);
                    record(recorder, &plain);
                    conn.send(&plain).await?;
                }
                // Output ended: wait briefly for the exit code.
                None => break tokio::time::timeout(Duration::from_secs(2), &mut exited).await.ok().and_then(Result::ok),
            },
            c = &mut exited => {
                // The shell exited. Its last output may still be on its way (and ConPTY only
                // ends the output when the console is closed), so drain for a moment.
                let deadline = tokio::time::sleep(Duration::from_millis(300));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        Some(bytes) = out.recv() => {
                            let plain = proto::term_data(&bytes);
                            record(recorder, &plain);
                            conn.send(&plain).await?;
                        }
                        _ = &mut deadline => break,
                    }
                }
                break c.ok();
            }
            m = conn.recv() => {
                let Some(plain) = m? else {
                    agent.report(sid, "terminal_closed", json!({"reason": "viewer_left"}));
                    return Ok(());
                };
                match plain.first() {
                    Some(&proto::MSG_TERM) => {
                        let _ = pty.input.send(plain[1..].to_vec());
                    }
                    Some(&proto::MSG_JSON) => {
                        let Some(m) = remote::json_of(&plain) else { continue };
                        match m["t"].as_str().unwrap_or("") {
                            "term_resize" => {
                                let sz = size(m["cols"].as_u64().unwrap_or(80), m["rows"].as_u64().unwrap_or(24));
                                if pty.master.resize(sz).is_ok() {
                                    record(recorder, &proto::json(&json!({"t": "term_size", "cols": sz.cols, "rows": sz.rows})));
                                }
                            }
                            "bye" => {
                                agent.report(sid, "terminal_closed", json!({"reason": "viewer_left"}));
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    tracing::info!("session {sid}: shell exited ({code:?})");
    agent.report(sid, "terminal_closed", json!({"reason": "exited", "code": code}));
    conn.send_json(json!({"t": "term_exit", "code": code})).await?;
    // ConPTY can block while closing; never hold up the session for it.
    std::thread::spawn(move || drop(pty));
    // Give the viewer a moment to read the last messages before the channel closes.
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.recv()).await;
    Ok(())
}

// --- viewer side ---

pub enum TermCmd {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// How a terminal session ended.
pub enum TermEnd {
    /// The shell exited with this code (None if unknown).
    Exited(Option<u32>),
    /// The user closed it.
    Closed,
}

/// Runs the viewer side of an authenticated terminal session: starts the shell at `cols` x
/// `rows`, feeds its output to `output` and sends what arrives on `cmds`.
pub async fn run_client(conn: &mut Conn, cols: u16, rows: u16, mut cmds: mpsc::UnboundedReceiver<TermCmd>, mut output: impl FnMut(&[u8])) -> Result<TermEnd> {
    conn.send_json(json!({"t": "term_open", "cols": cols, "rows": rows})).await?;
    let mut ping = tokio::time::interval(remote::PING_EVERY);
    loop {
        tokio::select! {
            _ = ping.tick() => conn.send_json(json!({"t": "ping"})).await?,
            c = cmds.recv() => match c {
                Some(TermCmd::Input(b)) => {
                    for chunk in b.chunks(16 * 1024) {
                        conn.send(&proto::term_data(chunk)).await?;
                    }
                }
                Some(TermCmd::Resize(c, r)) => conn.send_json(json!({"t": "term_resize", "cols": c, "rows": r})).await?,
                Some(TermCmd::Close) | None => {
                    conn.close().await;
                    return Ok(TermEnd::Closed);
                }
            },
            m = conn.recv() => {
                let Some(plain) = m? else { bail!("{}", crate::client::CLOSED) };
                match plain.first() {
                    Some(&proto::MSG_TERM) => output(&plain[1..]),
                    Some(&proto::MSG_JSON) => {
                        let Some(m) = remote::json_of(&plain) else { continue };
                        match m["t"].as_str().unwrap_or("") {
                            "term_exit" => {
                                if let Some(msg) = m["message"].as_str() {
                                    bail!("{msg}");
                                }
                                conn.close().await;
                                return Ok(TermEnd::Exited(m["code"].as_u64().map(|c| c as u32)));
                            }
                            "bye" => bail!("{}", crate::client::CLOSED),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A real shell in a PTY: runs a command, sees UTF-8 output, interrupts a sleep with
    /// Ctrl-C and resizes.
    #[tokio::test(flavor = "multi_thread")]
    async fn shell_in_a_pty() {
        // SAFETY: test-only; no other thread of this test reads the environment meanwhile.
        unsafe { std::env::set_var("SHELL", "/bin/sh") };
        let (pty, mut out, exited, _shell) = Pty::spawn(size(80, 24)).unwrap();
        let mut seen = Vec::new();
        let mut wait_for = async |needle: &str, seen: &mut Vec<u8>| {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !String::from_utf8_lossy(seen).contains(needle) {
                match tokio::time::timeout_at(deadline, out.recv()).await {
                    Ok(Some(b)) => seen.extend_from_slice(&b),
                    _ => panic!("did not see {needle:?} in {:?}", String::from_utf8_lossy(seen)),
                }
            }
        };
        pty.input.send(b"echo \"$((6*7)) \xe0\xb8\xaa\xe0\xb8\xa7\xe0\xb8\xb1\xe0\xb8\xaa\xe0\xb8\x94\xe0\xb8\xb5\"\r".to_vec()).unwrap();
        wait_for("42 สวัสดี", &mut seen).await;
        pty.input.send(b"sleep 30; echo slept\r".to_vec()).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        pty.input.send(vec![0x03]).unwrap();
        pty.input.send(b"echo after-int\r".to_vec()).unwrap();
        wait_for("after-int\r\n", &mut seen).await;
        // The command line is echoed ("...; echo slept"), its output would be a line of its own.
        assert!(!String::from_utf8_lossy(&seen).contains("\nslept\r"));
        pty.master.resize(size(100, 40)).unwrap();
        pty.input.send(b"stty size; exit 3\r".to_vec()).unwrap();
        wait_for("40 100", &mut seen).await;
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), exited).await.unwrap().unwrap(), 3);
    }
}
