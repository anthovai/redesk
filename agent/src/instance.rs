//! One running desktop app per user (and config file), like RustDesk.
//!
//! The first instance holds an exclusive lock on `instance.lock` next to the config and listens
//! on a channel only this user can open:
//!
//! - Unix (macOS/Linux): a Unix domain socket `instance.sock` in the config folder (folder 0700,
//!   socket 0600), so other local users cannot reach it.
//! - Windows: a named pipe `\\.\pipe\aegiron-<hash of the config path>` that rejects remote
//!   clients; the default pipe DACL gives other users read access only, so they cannot write a
//!   message to it.
//!
//! A second launch fails to take the lock, sends one short message (`aegiron-show`, or
//! `aegiron-open <9-digit id>` for an `aegiron://` link) and exits. Messages are parsed
//! strictly; a link only ever carries a device ID, never a password.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::local;

/// What a second launch asks the running instance to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Bring the window to the front.
    Show,
    /// Open the connection prompt for this device ID (from an `aegiron://` link).
    Open(String),
}

/// Longest message accepted (`aegiron-open ` + 9 digits fits easily).
const MAX_MSG: u64 = 64;

pub fn encode(req: &Request) -> String {
    match req {
        Request::Show => "aegiron-show".into(),
        Request::Open(id) => format!("aegiron-open {id}"),
    }
}

pub fn parse(raw: &[u8]) -> Option<Request> {
    let msg = std::str::from_utf8(raw).ok()?.trim_end();
    if msg == "aegiron-show" {
        return Some(Request::Show);
    }
    let id = msg.strip_prefix("aegiron-open ")?;
    local::normalize_id(id).filter(|n| n == id).map(Request::Open)
}

/// Lock and socket files live next to the config (per user, owner-only folder).
fn dir_of(config_path: &Path) -> PathBuf {
    config_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name(config_path: &Path) -> String {
    format!(r"\\.\pipe\aegiron-{}", hash_hex(config_path))
}

/// Claims the single instance for `config_path`.
///
/// Returns true when this process is the first instance: `on_request` is then called (on a
/// background thread) for every valid request from later launches. Returns false after
/// handing `req` to the instance that is already running; the caller should exit.
pub fn claim(config_path: &Path, req: &Request, on_request: impl Fn(Request) + Send + 'static) -> bool {
    let dir = dir_of(config_path);
    if let Err(e) = create_private_dir(&dir) {
        tracing::warn!("single instance disabled: cannot create {}: {e}", dir.display());
        return true;
    }
    let lock = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("instance.lock")) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("single instance disabled: {e}");
            return true;
        }
    };
    match lock.try_lock() {
        Ok(()) => {
            // Held (and released by the OS) for the life of the process.
            std::mem::forget(lock);
            if let Err(e) = listen(config_path, on_request) {
                tracing::warn!("cannot listen for other launches: {e}");
            }
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            // The first instance may still be starting up: retry for a moment.
            for _ in 0..20 {
                match send(config_path, req) {
                    Ok(()) => return false,
                    Err(_) => std::thread::sleep(Duration::from_millis(150)),
                }
            }
            tracing::warn!("another Aegiron is running but did not answer; exiting");
            false
        }
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!("single instance disabled: lock failed: {e}");
            true
        }
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn handle(mut conn: impl Read, on_request: &impl Fn(Request)) {
    let mut buf = Vec::new();
    let _ = (&mut conn).take(MAX_MSG).read_to_end(&mut buf);
    match parse(&buf) {
        Some(r) => {
            tracing::info!("request from another launch: {r:?}");
            on_request(r);
        }
        None => tracing::warn!("ignoring invalid message from another launch ({} bytes)", buf.len()),
    }
}

/// `instance.sock` next to the config, unless that path is too long for a Unix socket
/// (about 104 bytes on macOS): then a per-user runtime folder (macOS: `$TMPDIR`, which is
/// private to the user; Linux: `$XDG_RUNTIME_DIR`) with a name derived from the config path.
#[cfg(unix)]
fn socket_path(config_path: &Path) -> PathBuf {
    let p = dir_of(config_path).join("instance.sock");
    if p.as_os_str().len() < 100 {
        return p;
    }
    let var = if cfg!(target_os = "macos") { "TMPDIR" } else { "XDG_RUNTIME_DIR" };
    match std::env::var_os(var).map(PathBuf::from).filter(|d| d.is_absolute()) {
        Some(d) => d.join(format!("aegiron-{}.sock", hash_hex(config_path))),
        None => p,
    }
}

fn hash_hex(config_path: &Path) -> String {
    let h = Sha256::digest(config_path.to_string_lossy().as_bytes());
    h[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn listen(config_path: &Path, on_request: impl Fn(Request) + Send + 'static) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    let path = socket_path(config_path);
    // We hold the lock, so any socket file is left over from a crashed instance.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new().name("instance".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
            handle(conn, &on_request);
        }
    })?;
    Ok(())
}

#[cfg(unix)]
fn send(config_path: &Path, req: &Request) -> std::io::Result<()> {
    let mut s = std::os::unix::net::UnixStream::connect(socket_path(config_path))?;
    s.set_write_timeout(Some(Duration::from_secs(1)))?;
    s.write_all(encode(req).as_bytes())
}

#[cfg(windows)]
fn listen(config_path: &Path, on_request: impl Fn(Request) + Send + 'static) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = pipe_name(config_path);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    // Create the first pipe instance now, so that a name squatted by someone else fails here.
    let first = {
        let _g = rt.enter();
        ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true).create(&name)?
    };
    std::thread::Builder::new().name("instance".into()).spawn(move || {
        rt.block_on(async move {
            let mut server = first;
            loop {
                if server.connect().await.is_ok() {
                    let mut buf = Vec::new();
                    let mut limited = (&mut server).take(MAX_MSG);
                    let _ = tokio::time::timeout(Duration::from_secs(1), limited.read_to_end(&mut buf)).await;
                    handle(&buf[..], &on_request);
                }
                server = match ServerOptions::new().reject_remote_clients(true).create(&name) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("instance pipe: {e}");
                        return;
                    }
                };
            }
        });
    })?;
    Ok(())
}

#[cfg(windows)]
fn send(config_path: &Path, req: &Request) -> std::io::Result<()> {
    let mut pipe = std::fs::OpenOptions::new().write(true).open(pipe_name(config_path))?;
    pipe.write_all(encode(req).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(parse(b"aegiron-show"), Some(Request::Show));
        assert_eq!(parse(b"aegiron-open 123456789"), Some(Request::Open("123456789".into())));
        assert_eq!(parse(b"aegiron-open 123456789\n"), Some(Request::Open("123456789".into())));
        for bad in [
            &b"aegiron-open 12345678"[..],
            b"aegiron-open 123 456 789",
            b"aegiron-open 123456789x",
            b"aegiron-open 123456789 password",
            b"aegiron-open ",
            b"hello",
            b"\xff\xfe",
            b"",
        ] {
            assert_eq!(parse(bad), None);
        }
        for r in [Request::Show, Request::Open("987654321".into())] {
            assert_eq!(parse(encode(&r).as_bytes()), Some(r));
        }
    }

    #[test]
    fn pipe_name_is_per_config() {
        let a = pipe_name(Path::new(r"C:\Users\a\AppData\Roaming\Aegiron\agent.toml"));
        let b = pipe_name(Path::new(r"C:\Users\b\AppData\Roaming\Aegiron\agent.toml"));
        assert!(a.starts_with(r"\\.\pipe\aegiron-") && a.len() == r"\\.\pipe\aegiron-".len() + 16);
        assert_ne!(a, b);
    }

    #[test]
    fn second_launch_hands_over() {
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("aegiron-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = dir.join("agent.toml");
        let (tx, rx) = mpsc::channel();
        assert!(claim(&cfg, &Request::Show, move |r| tx.send(r).unwrap()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&socket_path(&cfg)), 0o600);
        }
        // The lock is taken, so these are "second launches".
        assert!(!claim(&cfg, &Request::Open("123456789".into()), |_| unreachable!()));
        assert!(!claim(&cfg, &Request::Show, |_| unreachable!()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Open("123456789".into()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Show);
        // Junk is dropped without reaching the app.
        #[cfg(unix)]
        let mut s = std::os::unix::net::UnixStream::connect(socket_path(&cfg)).unwrap();
        #[cfg(windows)]
        let mut s = (0..20)
            .find_map(|_| {
                let r = std::fs::OpenOptions::new().write(true).open(pipe_name(&cfg));
                if r.is_err() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                r.ok()
            })
            .expect("instance pipe");
        s.write_all(b"aegiron-open 1; rm -rf /").unwrap();
        drop(s);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // Still listening after the junk.
        assert!(!claim(&cfg, &Request::Open("987654321".into()), |_| unreachable!()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Open("987654321".into()));
        let _ = std::fs::remove_dir_all(&dir); // Windows: the held lock file stays until exit
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_config_path_still_works() {
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("aegiron-instance-long-{}-{}", std::process::id(), "x".repeat(80)));
        let cfg = dir.join("agent.toml");
        let sock = socket_path(&cfg);
        assert!(sock.as_os_str().len() < 104 && sock.file_name().unwrap().to_string_lossy().starts_with("aegiron-"));
        let (tx, rx) = mpsc::channel();
        assert!(claim(&cfg, &Request::Show, move |r| tx.send(r).unwrap()));
        assert!(!claim(&cfg, &Request::Open("111222333".into()), |_| unreachable!()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Request::Open("111222333".into()));
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
