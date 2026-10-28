//! Headless end-to-end check of the desktop viewer's file transfer against a running server and
//! agent: folder upload, folder download, and clipboard files in both directions.
//!
//!   AEGIRON_TOKEN=<app bearer token> cargo run --example e2e_files -- \
//!       <server> <device-id> <device-password> <local-folder> <remote-dir> <remote-folder> <clip-file>
//!
//! Downloads go to $HOME/Downloads/Aegiron, so run it with HOME pointing at a scratch folder.
//! It replaces this machine's clipboard. `<clip-file>` is a file "copied on the remote side".

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aegiron_agent::viewer::{Paste, Phase, State, Target, TransferStatus, Viewer};
use anyhow::{Result, bail};

fn wait(v: &Viewer, what: &str, secs: u64, mut f: impl FnMut(&mut State) -> bool) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        {
            let mut st = v.state.lock().unwrap();
            if let Some(e) = st.error.clone().filter(|_| st.phase == Phase::Closed) {
                bail!("session closed while waiting for {what}: {e}");
            }
            if f(&mut st) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out waiting for {what}")
}

/// Waits for the newest transfer to finish and returns its result path.
fn finished(v: &Viewer, n: usize) -> Result<String> {
    wait(v, "transfer", 120, |st| st.transfers.len() >= n && st.transfers[n - 1].status != TransferStatus::Active)?;
    let st = v.state.lock().unwrap();
    let t = &st.transfers[n - 1];
    println!(
        "transfer {} {} {:?}: {}/{} files, {} failed, {} of {} bytes",
        t.id,
        if t.up { "up" } else { "down" },
        t.name,
        t.files_done,
        t.files,
        t.failed,
        t.sent,
        t.size
    );
    match &t.status {
        TransferStatus::Done(p) if t.failed == 0 && t.sent == t.size => Ok(p.clone()),
        s => bail!("transfer did not complete cleanly: {s:?}"),
    }
}

fn notice(v: &Viewer, what: &str) -> Result<String> {
    let mut got = String::new();
    wait(v, what, 60, |st| match st.notice.take() {
        Some(n) => {
            got = n;
            true
        }
        None => false,
    })?;
    println!("notice: {got}");
    Ok(got)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let [server, device, password, local, remote_dir, remote_folder, clip_file] = a.as_slice() else {
        bail!("usage: e2e_files <server> <device-id> <device-password> <local-folder> <remote-dir> <remote-folder> <clip-file>")
    };
    let token = std::env::var("AEGIRON_TOKEN")?;
    let v = Viewer::connect(
        Target { server: server.clone(), token, insecure: true, device_id: device.clone(), name: "e2e".into(), direct: false, only: None },
        Arc::new(|| {}),
    );
    wait(&v, "password prompt", 30, |st| st.phase == Phase::Password)?;
    v.submit_password(password);
    wait(&v, "connected", 30, |st| st.phase == Phase::Connected && st.info.is_some())?;
    let info = v.state.lock().unwrap().info.clone().unwrap();
    println!("connected to {} features={:?} fs2={} clip_files={}", info.hostname, info.features, info.fs2(), info.clip_files());

    // 1. Folder upload into <remote-dir>.
    v.upload(PathBuf::from(local), remote_dir.clone());
    println!("uploaded folder -> {}", finished(&v, 1)?);

    // 2. Folder download of <remote-dir>/<remote-folder> into ~/Downloads/Aegiron.
    v.download(format!("{remote_dir}/{remote_folder}"), true);
    println!("downloaded folder -> {}", finished(&v, 2)?);

    // 3. Remote file management: new folder, rename, delete.
    v.list_dir(remote_dir);
    wait(&v, "listing", 10, |st| st.browser.path == *remote_dir && !st.browser.loading)?;
    v.fs_op(serde_json::json!({"t": "mkdir", "dir": remote_dir, "name": "new folder"}));
    wait(&v, "mkdir", 10, |st| st.browser.entries.iter().any(|e| e.name == "new folder" && e.dir))?;
    v.fs_op(serde_json::json!({"t": "mv", "path": format!("{remote_dir}/new folder"), "name": "renamed"}));
    wait(&v, "rename", 10, |st| st.browser.entries.iter().any(|e| e.name == "renamed"))?;
    v.fs_op(serde_json::json!({"t": "mv", "path": format!("{remote_dir}/renamed"), "name": "../escape"}));
    wait(&v, "rename refused", 10, |st| st.browser.error.as_deref().is_some_and(|e| e.contains("unsafe")))?;
    v.fs_op(serde_json::json!({"t": "rm", "path": format!("{remote_dir}/renamed")}));
    wait(&v, "delete", 10, |st| !st.browser.loading && !st.browser.entries.iter().any(|e| e.name == "renamed"))?;
    println!("mkdir / rename / delete OK, unsafe rename refused");

    if info.clip_files() {
        // 4. Paste local files into the remote window: uploaded, then on the remote clipboard.
        let first = std::fs::read_dir(local)?.flatten().find(|e| e.path().is_file()).map(|e| e.path()).unwrap();
        arboard::Clipboard::new()?.set().file_list(&[first.clone(), PathBuf::from(local)])?;
        let r = v.paste_files(Vec::new());
        println!("paste -> {r:?}");
        if r != Paste::Files {
            bail!("expected a file paste");
        }
        notice(&v, "paste")?;
        let n = v.state.lock().unwrap().transfers.len();
        finished(&v, n - 1)?;
        finished(&v, n)?;
        // Same machine: the agent's clipboard is this clipboard.
        std::thread::sleep(Duration::from_millis(300));
        let remote = arboard::Clipboard::new()?.get().file_list()?;
        println!("clipboard now holds the agent's copies: {remote:?}");

        // 5. "Copy" a file on the remote side: the viewer fetches it and puts it on this clipboard.
        arboard::Clipboard::new()?.set().file_list(&[PathBuf::from(clip_file)])?;
        notice(&v, "remote copy")?;
        let local_copy = arboard::Clipboard::new()?.get().file_list()?;
        println!("clipboard now holds the local copy: {local_copy:?}");
        if local_copy.len() != 1 || std::fs::read(&local_copy[0])? != std::fs::read(clip_file)? {
            bail!("clipboard copy differs");
        }
        println!("clipboard round trip OK");
    }
    // 6. Cancel half-way, both directions (E2E_BIG = a local folder holding a large file).
    if let Ok(big) = std::env::var("E2E_BIG") {
        let cancel_newest = |what: &str, before: usize| -> Result<()> {
            wait(&v, what, 60, |st| st.transfers.len() > before && st.transfers.last().is_some_and(|t| t.sent > 0))?;
            let id = v.state.lock().unwrap().transfers.last().unwrap().id;
            v.cancel(id);
            wait(&v, "cancelled", 30, |st| st.transfers.last().is_some_and(|t| matches!(t.status, TransferStatus::Failed(_))))?;
            let st = v.state.lock().unwrap();
            let t = st.transfers.last().unwrap();
            println!("cancelled {what} at {} of {} bytes: {:?}", t.sent, t.size, t.status);
            Ok(())
        };
        let count = || v.state.lock().unwrap().transfers.len();
        let n = count();
        v.upload(PathBuf::from(&big), remote_dir.clone());
        cancel_newest("upload", n)?;
        let name = PathBuf::from(&big).file_name().unwrap().to_string_lossy().into_owned();
        let n = count();
        v.download(format!("{remote_dir}/{name}"), true);
        cancel_newest("download", n)?;
        // Give the agent a moment, then the session must still work.
        std::thread::sleep(Duration::from_millis(500));
        v.list_dir(remote_dir);
        wait(&v, "listing after cancel", 10, |st| !st.browser.loading && st.browser.path == *remote_dir)?;
        println!("session still responsive after cancels");
    }
    v.close();
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}
