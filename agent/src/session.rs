//! One remote-control session: relay connect, E2E handshake, authentication, streaming.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::capture::{self, Params};
use crate::codec::Codec;
use crate::config::{ApproveMode, Verification};
use crate::crypto::{self, Channel};
use crate::files;
use crate::input;
use crate::proto::{self, ViewerMsg};
use crate::signal::{Agent, Viewer};
use crate::status::{ApprovalRequest, SessionInfo};

const MAX_ATTEMPTS: u32 = 3;
const LOCKOUT_AFTER: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Hello {
    t: String,
    v: u32,
    epk: String,
}

pub async fn run(agent: Arc<Agent>, sid: &str, token: &str, relay: &str, viewer: Viewer) -> Result<()> {
    let base = agent.config.lock().unwrap().ws_base()?;
    let url = format!("{base}{relay}?sid={sid}&token={token}");
    let (ws, _) = connect_async(url.as_str()).await.context("relay connect")?;
    let (mut tx, mut rx) = ws.split();

    // 1. Wait for the server to pair us with the viewer, then read the viewer's ephemeral key.
    let paired = read_text(&mut rx, "pairing", 60).await?;
    if !paired.contains(r#""paired""#) {
        bail!("unexpected relay message: {paired}");
    }
    let hello: Hello = serde_json::from_str(&read_text(&mut rx, "viewer hello", 30).await?)?;
    if hello.t != "hello" || hello.v != 1 {
        bail!("unsupported viewer protocol");
    }
    let viewer_epk: [u8; 32] = B64.decode(hello.epk)?.try_into().map_err(|_| anyhow!("bad viewer key"))?;

    // 2. Key exchange, signed with the device identity the viewer fetched from the server.
    let hs = crypto::agent_handshake(&agent.identity, sid, viewer_epk)?;
    let reply = json!({"t": "hello", "v": 1, "epk": B64.encode(hs.agent_epk), "sig": B64.encode(hs.signature)});
    tx.send(Message::Text(reply.to_string().into())).await?;
    let mut ch = hs.channel;

    // 3. Authentication inside the encrypted channel.
    let (verification, approve) = {
        let c = agent.config.lock().unwrap();
        (c.verification, c.approve_mode)
    };
    let need_password = approve != ApproveMode::Click;
    let need_click = approve != ApproveMode::Password;
    send_json(&mut tx, &mut ch, json!({"t": "auth_required", "password": need_password, "click": need_click})).await?;

    let mut attempts = 0;
    let mut codec;
    loop {
        let Incoming::Msg(ViewerMsg::Auth { password, codecs }) = read_viewer(&mut rx, &mut ch, Duration::from_secs(120)).await? else {
            bail!("expected auth message")
        };
        attempts += 1;
        codec = Codec::negotiate(&codecs);

        if need_password {
            if let Some(wait) = locked_for(&agent) {
                agent.report(sid, "locked", json!({"retry_after": wait.as_secs()}));
                send_json(&mut tx, &mut ch, json!({"t": "auth_result", "ok": false, "reason": "locked", "retry_after": wait.as_secs()})).await?;
                bail!("authentication locked");
            }
            if !check_password(&agent, verification, password.as_deref().unwrap_or("")) {
                record_failure(&agent);
                tracing::warn!("wrong password for session {sid} from {} ({})", viewer.email, viewer.ip);
                agent.report(sid, "auth_failed", json!({"attempt": attempts}));
                let left = MAX_ATTEMPTS - attempts;
                send_json(&mut tx, &mut ch, json!({"t": "auth_result", "ok": false, "reason": "wrong_password", "attempts_left": left})).await?;
                if left == 0 {
                    bail!("too many wrong passwords");
                }
                continue;
            }
        }
        if need_click {
            send_json(&mut tx, &mut ch, json!({"t": "auth_pending"})).await?;
            if !ask_local_user(&agent, &viewer).await {
                agent.report(sid, "denied", json!({}));
                send_json(&mut tx, &mut ch, json!({"t": "auth_result", "ok": false, "reason": "denied"})).await?;
                bail!("denied by local user");
            }
        }
        break;
    }
    agent.failures.lock().unwrap().0 = 0;
    if need_password && verification != Verification::Permanent {
        agent.rotate_temp_password(); // one-time: the password just used is now dead
    }
    tracing::info!("session {sid}: {} ({}) is now controlling this machine", viewer.email, viewer.ip);
    let kill = Arc::new(tokio::sync::Notify::new());
    agent.status.lock().unwrap().sessions.push(SessionInfo {
        sid: sid.to_string(),
        email: viewer.email.clone(),
        name: viewer.name.clone(),
        ip: viewer.ip.clone(),
        started: std::time::SystemTime::now(),
        kill: kill.clone(),
    });
    agent.changed();
    let _registered = Registered { agent: agent.clone(), sid: sid.to_string() };
    agent.report(sid, "auth_ok", json!({"method": if need_password { "password" } else { "click" }, "approved_locally": need_click}));
    let _awake = crate::power::KeepAwake::new("Aegiron remote session");

    // 4. Stream.
    let cfg = agent.config.lock().unwrap().clone();
    let displays = capture::displays();
    send_json(&mut tx, &mut ch, json!({
        "t": "auth_result", "ok": true,
    }))
    .await?;
    send_json(&mut tx, &mut ch, json!({
        "t": "info",
        "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        "os": std::env::consts::OS,
        "displays": displays,
        "display": 0,
        "input": cfg.allow_input,
        "clipboard": cfg.allow_clipboard,
        "files": cfg.allow_file_transfer,
        "max_fps": cfg.max_fps,
        "codec": codec.name(),
    }))
    .await?;

    let params = Params { display: 0, fps: cfg.max_fps, quality: cfg.jpeg_quality, max_width: cfg.max_width, codec };
    let (params_tx, params_rx) = watch::channel(params);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let force_full = Arc::new(AtomicBool::new(true));
    let _stop_guard = StopOnDrop(stop.clone());
    {
        let (out_tx, stop, force_full) = (out_tx.clone(), stop.clone(), force_full.clone());
        std::thread::spawn(move || capture::run(params_rx, out_tx, stop, force_full));
    }
    let last_clip = Arc::new(Mutex::new(String::new()));
    if cfg.allow_clipboard {
        let (out_tx, stop, last_clip) = (out_tx.clone(), stop.clone(), last_clip.clone());
        std::thread::spawn(move || watch_clipboard(out_tx, stop, last_clip));
    }
    let mut files = files::Files::new();
    let input_tx = cfg.allow_input.then(|| input::spawn(capture::geometry(0).unwrap_or_default()));

    loop {
        tokio::select! {
            _ = kill.notified() => {
                tracing::info!("session {sid}: disconnected by the local user");
                agent.report(sid, "denied", json!({"stage": "during_session"}));
                return Ok(());
            }
            out = out_rx.recv() => {
                let Some(plain) = out else { bail!("capture stopped") };
                tx.send(Message::Binary(ch.seal(&plain)?.into())).await?;
            }
            msg = read_viewer(&mut rx, &mut ch, Duration::from_secs(300)) => {
                let msg = match msg {
                    Ok(Incoming::Msg(m)) => m,
                    Ok(Incoming::FileChunk(id, data)) => {
                        if cfg.allow_file_transfer {
                            if let Some(e) = files.chunk(id, &data) {
                                send_json(&mut tx, &mut ch, e).await?;
                            }
                        }
                        continue;
                    }
                    Err(e) if e.to_string() == "relay closed" => return Ok(()),
                    Err(e) => return Err(e),
                };
                let send_input = |ev| if let Some(t) = &input_tx { let _ = t.send(ev); };
                match msg {
                    ViewerMsg::MouseMove { x, y } => send_input(input::Event::Move { x, y }),
                    ViewerMsg::MouseButton { b, down, x, y } => send_input(input::Event::Button { b, down, x, y }),
                    ViewerMsg::Wheel { dx, dy } => send_input(input::Event::Wheel { dx: dx.clamp(-20, 20), dy: dy.clamp(-20, 20) }),
                    ViewerMsg::Key { code, key, down } => send_input(input::Event::Key { code, key, down }),
                    ViewerMsg::Text { s } => send_input(input::Event::Text(s.chars().take(4096).collect())),
                    ViewerMsg::Clipboard { s } if cfg.allow_clipboard => {
                        *last_clip.lock().unwrap() = s.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(s)) {
                                tracing::debug!("clipboard set failed: {e}");
                            }
                        });
                    }
                    ViewerMsg::Config { fps, quality, max_width, display } => {
                        params_tx.send_modify(|p| {
                            if let Some(f) = fps { p.fps = f.clamp(1, cfg.max_fps.max(1)); }
                            if let Some(q) = quality { p.quality = q.clamp(10, 95); }
                            if let Some(w) = max_width { p.max_width = w.clamp(320, 3840); }
                            if let Some(d) = display.filter(|d| *d < displays.len()) {
                                p.display = d;
                                if let Some(g) = capture::geometry(d) { send_input(input::Event::Geometry(g)); }
                            }
                        });
                        force_full.store(true, Ordering::Relaxed);
                    }
                    ViewerMsg::List { path } if cfg.allow_file_transfer => {
                        let reply = files.list(&path);
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::Put { id, dir, name, size } if cfg.allow_file_transfer => {
                        let reply = files.put(id, &dir, &name, size);
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::PutEnd { id } if cfg.allow_file_transfer => {
                        let (reply, done) = files.put_end(id);
                        if let Some((path, size)) = done {
                            tracing::info!("session {sid}: received file {path} ({size} bytes)");
                            agent.report(sid, "file_upload", json!({"path": path, "size": size}));
                        }
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::Get { id, path } if cfg.allow_file_transfer => {
                        tracing::info!("session {sid}: sending file {path}");
                        agent.report(sid, "file_download", json!({"path": path}));
                        files.get(id, path, out_tx.clone());
                    }
                    ViewerMsg::Cancel { id } => files.cancel(id),
                    ViewerMsg::List { .. } | ViewerMsg::Put { .. } | ViewerMsg::Get { .. } => {
                        send_json(&mut tx, &mut ch, json!({"t": "fs_error", "message": "file transfer is disabled on this device"})).await?;
                    }
                    ViewerMsg::Refresh => force_full.store(true, Ordering::Relaxed),
                    ViewerMsg::Bye => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

type WsError = tokio_tungstenite::tungstenite::Error;

async fn read_text<S>(rx: &mut S, what: &str, secs: u64) -> Result<String>
where
    S: futures_util::Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(secs), rx.next())
            .await
            .with_context(|| format!("timed out waiting for {what}"))?
            .ok_or(anyhow!("relay closed"))??;
        match m {
            Message::Text(t) => return Ok(t.as_str().to_owned()),
            Message::Close(_) => bail!("relay closed"),
            _ => {}
        }
    }
}

async fn send_json<K>(tx: &mut K, ch: &mut Channel, v: serde_json::Value) -> Result<()>
where
    K: futures_util::Sink<Message, Error = WsError> + Unpin,
{
    tx.send(Message::Binary(ch.seal(&proto::json(&v))?.into())).await?;
    Ok(())
}

/// Removes the session from the UI list when it ends, however it ends.
struct Registered {
    agent: Arc<Agent>,
    sid: String,
}
impl Drop for Registered {
    fn drop(&mut self) {
        self.agent.status.lock().unwrap().sessions.retain(|s| s.sid != self.sid);
        self.agent.changed();
    }
}

struct StopOnDrop(Arc<AtomicBool>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

enum Incoming {
    Msg(ViewerMsg),
    FileChunk(u32, Vec<u8>),
}

async fn read_viewer<S>(rx: &mut S, ch: &mut Channel, idle: Duration) -> Result<Incoming>
where
    S: futures_util::Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(idle, rx.next()).await.context("viewer idle timeout")?;
        let data = match m {
            None | Some(Ok(Message::Close(_))) => bail!("relay closed"),
            Some(Err(e)) => return Err(e.into()),
            Some(Ok(Message::Binary(b))) => b,
            Some(Ok(_)) => continue,
        };
        let mut plain = ch.open(&data)?;
        match plain.first() {
            Some(&proto::MSG_FILE) if plain.len() >= 5 => {
                let id = u32::from_be_bytes(plain[1..5].try_into().unwrap());
                plain.drain(..5);
                return Ok(Incoming::FileChunk(id, plain));
            }
            Some(&proto::MSG_JSON) => {}
            _ => continue,
        }
        match serde_json::from_slice::<ViewerMsg>(&plain[1..]) {
            Ok(m) => return Ok(Incoming::Msg(m)),
            Err(e) => tracing::debug!("ignoring viewer message: {e}"),
        }
    }
}

fn check_password(agent: &Agent, mode: Verification, pw: &str) -> bool {
    if pw.is_empty() {
        return false;
    }
    let temp_ok = || {
        let t = agent.temp_password.lock().unwrap();
        t.len() == pw.len() && t.bytes().zip(pw.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    };
    let perm_ok = || {
        let hash = agent.config.lock().unwrap().permanent_password.clone();
        hash.is_some_and(|h| PasswordHash::new(&h).is_ok_and(|p| Argon2::default().verify_password(pw.as_bytes(), &p).is_ok()))
    };
    match mode {
        Verification::Temporary => temp_ok(),
        Verification::Permanent => perm_ok(),
        Verification::Both => temp_ok() || perm_ok(),
    }
}

fn locked_for(agent: &Agent) -> Option<Duration> {
    let f = agent.failures.lock().unwrap();
    f.1.and_then(|until| until.checked_duration_since(Instant::now()))
}

fn record_failure(agent: &Agent) {
    let mut f = agent.failures.lock().unwrap();
    f.0 += 1;
    if f.0 >= LOCKOUT_AFTER {
        f.0 = 0;
        f.1 = Some(Instant::now() + LOCKOUT);
        tracing::warn!("too many failed logins; locking for {}s", LOCKOUT.as_secs());
    }
}

/// Asks the person at the machine to accept the connection. Shows a desktop dialog
/// (macOS osascript / Linux zenity) and, when attached to a terminal, a y/N prompt;
/// whichever is answered first wins. No answer within 30s means deny.
async fn ask_local_user(agent: &Agent, viewer: &Viewer) -> bool {
    let who = if viewer.name.is_empty() { viewer.email.clone() } else { format!("{} <{}>", viewer.name, viewer.email) };

    // Desktop app running: ask in its window instead of a separate dialog.
    let ui_tx = agent.ui.lock().unwrap().as_ref().map(|u| (u.approvals.clone(), u.repaint.clone()));
    if let Some((tx, repaint)) = ui_tx {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let req = ApprovalRequest { sid: String::new(), who: who.clone(), ip: viewer.ip.clone(), reply };
        if tx.send(req).is_ok() {
            repaint();
            return matches!(tokio::time::timeout(Duration::from_secs(30), answer).await, Ok(Ok(true)));
        }
    }

    let question = format!("{who} ({}) ขอควบคุมเครื่องนี้ผ่าน Aegiron\nอนุญาตหรือไม่?", viewer.ip);

    let terminal = async {
        if !std::io::stdin().is_terminal() {
            return std::future::pending::<bool>().await;
        }
        let mut lines = agent.stdin_lines.lock().await;
        while lines.try_recv().is_ok() {}
        println!("\n  >>> Remote control requested by {who} from {}. Allow? [y/N] (30s)", viewer.ip);
        match lines.recv().await {
            Some(l) => matches!(l.trim().to_lowercase().as_str(), "y" | "yes"),
            None => std::future::pending::<bool>().await,
        }
    };
    let dialog = async {
        match gui_confirm(&question).await {
            Some(answer) => answer,
            None => std::future::pending::<bool>().await, // no GUI available
        }
    };
    let answer = tokio::time::timeout(Duration::from_secs(32), async {
        tokio::select! {
            a = terminal => a,
            a = dialog => a,
        }
    })
    .await
    .unwrap_or(false);
    if !answer {
        println!("  >>> Denied.");
    }
    answer
}

/// Desktop confirmation dialog. None = no dialog mechanism on this system.
/// The child is killed if the prompt is answered elsewhere first.
async fn gui_confirm(question: &str) -> Option<bool> {
    let mut cmd;
    if cfg!(target_os = "macos") {
        let q = question.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display dialog \"{q}\" with title \"Aegiron\" buttons {{\"ปฏิเสธ\", \"อนุญาต\"}} default button \"ปฏิเสธ\" with icon caution giving up after 30"
        );
        cmd = tokio::process::Command::new("osascript");
        cmd.arg("-e").arg(script);
    } else if cfg!(target_os = "linux") && std::env::var_os("DISPLAY").or(std::env::var_os("WAYLAND_DISPLAY")).is_some() {
        cmd = tokio::process::Command::new("zenity");
        cmd.args(["--question", "--title=Aegiron", "--timeout=30", "--ok-label=อนุญาต", "--cancel-label=ปฏิเสธ"]).arg(format!("--text={question}"));
    } else {
        return None;
    }
    let out = cmd.kill_on_drop(true).stdin(std::process::Stdio::null()).output().await.ok()?;
    if cfg!(target_os = "macos") {
        let s = String::from_utf8_lossy(&out.stdout);
        Some(s.contains("button returned:อนุญาต") && !s.contains("gave up:true"))
    } else {
        Some(out.status.success())
    }
}

fn watch_clipboard(out: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>, last: Arc<Mutex<String>>) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return };
    // Don't push whatever was already on the clipboard before the session started.
    if let Ok(t) = cb.get_text() {
        *last.lock().unwrap() = t;
    }
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(800));
        let Ok(t) = cb.get_text() else { continue };
        let changed = {
            let mut l = last.lock().unwrap();
            if *l != t && t.len() <= 1 << 20 {
                *l = t.clone();
                true
            } else {
                false
            }
        };
        if changed && out.blocking_send(proto::json(&json!({"t": "clip", "s": t}))).is_err() {
            return;
        }
    }
}
