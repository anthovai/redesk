//! One remote-control session: relay connect, E2E handshake, authentication, streaming.

use std::io::IsTerminal;
use std::path::PathBuf;
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
use tokio_tungstenite::tungstenite::Message;

use crate::audio;
use crate::capture::{self, Params};
use crate::codec::{self, Codec};
use crate::config::{ApproveMode, EncoderPref, Verification};
use crate::controls;
use crate::crypto::{self, Channel};
use crate::files;
use crate::input;
use crate::proto::{self, ViewerMsg};
use crate::recording::{RecordRequest, Recorder};
use crate::signal::{Agent, Viewer};
use crate::whiteboard::Board;
use crate::status::{ApprovalRequest, SessionInfo};

const MAX_ATTEMPTS: u32 = 3;
const LOCKOUT_AFTER: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
/// Viewers send a keepalive every 10 s while they block the machine.
const VIEWER_SILENT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Hello {
    t: String,
    v: u32,
    epk: String,
}

pub async fn run(agent: Arc<Agent>, sid: &str, token: &str, relay: &str, viewer: Viewer, record: Option<RecordRequest>, direct: Option<crate::direct::Offer>) -> Result<()> {
    let base = agent.config.lock().unwrap().ws_base()?;
    let url = format!("{base}{relay}?sid={sid}&token={token}");

    // 1. Wait for the viewer on the relay (or a direct/P2P path when offered), then read its
    //    ephemeral key. The handshake below is identical on every path.
    let (link, hello) = crate::direct::agent_link(&agent, sid, url, direct).await?;
    let crate::direct::Link { mut tx, mut rx, path: _, keep: _keep } = link;
    let hello: Hello = serde_json::from_str(&hello)?;
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
    let mut viewer_codecs;
    let mut viewer_features;
    let mut wants_audio;
    let mut resumed = false;
    loop {
        let Incoming::Msg(ViewerMsg::Auth { password, codecs, features, audio, resume }) = read_viewer(&mut rx, &mut ch, Duration::from_secs(120)).await? else {
            bail!("expected auth message")
        };
        attempts += 1;
        viewer_codecs = codecs;
        viewer_features = features;
        // Viewers that do not list an audio codec never get 0x05 packets.
        wants_audio = audio.iter().any(|c| c == "opus");

        // Back after a restart this viewer asked for: its one-time token replaces the password
        // and the click (an unknown or expired token falls through to the usual checks).
        if let Some(t) = resume.as_deref()
            && controls::take_resume(&agent.config_path, Some(&viewer.email), t)
        {
            resumed = true;
            break;
        }
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
            if !ask_local_user(&agent, &viewer, record.is_some()).await {
                agent.report(sid, "denied", json!({}));
                send_json(&mut tx, &mut ch, json!({"t": "auth_result", "ok": false, "reason": "denied"})).await?;
                bail!("denied by local user");
            }
        }
        break;
    }
    agent.failures.lock().unwrap().0 = 0;
    if need_password && !resumed && verification != Verification::Permanent {
        agent.rotate_temp_password(); // one-time: the password just used is now dead
    }
    tracing::info!("session {sid}: {} ({}) is now controlling this machine", viewer.email, viewer.ip);

    // Recording required by the organisation's policy: the session does not go ahead unrecorded.
    let mut recorder = RecordingGuard { agent: agent.clone(), rec: None };
    if let Some(req) = &record {
        let device_id = agent.config.lock().unwrap().id.clone();
        match Recorder::start(&crate::recording::dir_for(&agent.config_path), sid, &device_id, &req.token) {
            Ok(r) => {
                recorder.rec = Some(r);
                agent.report(sid, "recording_started", json!({}));
                tracing::info!("session {sid} is being recorded (organisation policy)");
                if std::io::stdout().is_terminal() {
                    println!("  >>> This session is being recorded (กำลังบันทึก).");
                }
            }
            Err(e) => {
                tracing::error!("session {sid}: cannot start the required recording: {e:#}");
                agent.report(sid, "recording_failed", json!({"stage": "start", "error": format!("{e:#}")}));
                send_json(&mut tx, &mut ch, json!({"t": "auth_result", "ok": false, "reason": "recording_failed"})).await?;
                bail!("recording required but unavailable");
            }
        }
    }
    let recording = recorder.rec.is_some();

    // Session controls (view-only, block input, privacy, lock, restart). Dropped when the
    // session ends however it ends: input is unblocked and the screen given back.
    let ui_attached = agent.ui.lock().unwrap().is_some();
    let platform = controls::platform();
    let perms = controls::Perms::new(&agent.config.lock().unwrap(), platform.supports(ui_attached));
    let report: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync> = {
        let (agent, sid) = (agent.clone(), sid.to_string());
        Arc::new(move |kind, detail| {
            agent.report(&sid, kind, detail);
            agent.changed();
        })
    };
    let mut ctl = controls::SessionCtl::new(perms, platform, report, &agent.config_path, &viewer.email, None);
    let mut escapes = controls::escape_signal();

    let kill = Arc::new(tokio::sync::Notify::new());
    agent.status.lock().unwrap().sessions.push(SessionInfo {
        sid: sid.to_string(),
        email: viewer.email.clone(),
        name: viewer.name.clone(),
        ip: viewer.ip.clone(),
        started: std::time::SystemTime::now(),
        kill: kill.clone(),
        recording,
        controls: ctl.flags.clone(),
    });
    agent.changed();
    let _registered = Registered { agent: agent.clone(), sid: sid.to_string() };
    let method = if resumed { "resume" } else if need_password { "password" } else { "click" };
    agent.report(sid, "auth_ok", json!({"method": method, "approved_locally": need_click && !resumed}));
    let _awake = crate::power::KeepAwake::new("Aegiron remote session");

    // 4. Stream.
    let cfg = agent.config.lock().unwrap().clone();
    let displays = capture::displays();
    // Annotations need the desktop app's overlay window; the headless agent ignores them.
    let whiteboard = cfg.allow_whiteboard && agent.ui.lock().unwrap().is_some();
    if whiteboard {
        let area = capture::logical_area(0).unwrap_or_default();
        agent.status.lock().unwrap().boards.push(Board::new(sid, area));
    }
    let audio_on = wants_audio && cfg.allow_audio && audio::capture_supported();
    // Which encoders this session can use (the hardware probes run once per process and may
    // block briefly). AV1 is hardware only, so "software only" in the settings turns it off;
    // it is only probed for viewers that can decode it.
    let lists = |c: &str| viewer_codecs.iter().any(|v| v == c);
    let auto = cfg.video_encoder == EncoderPref::Auto;
    let av1 = match lists("av1") && auto {
        true => tokio::task::spawn_blocking(codec::av1_available).await.unwrap_or(None),
        false => None,
    };
    let codec = Codec::negotiate(&viewer_codecs, av1.is_some());
    let h264 = match codec.is_video() && auto {
        true => tokio::task::spawn_blocking(codec::preferred_encoder).await.unwrap_or("openh264"),
        false => "openh264",
    };
    let encoder = match codec {
        Codec::Av1 => av1.unwrap_or("jpeg"),
        Codec::H264 => h264,
        Codec::Jpeg => "jpeg",
    };
    // Codecs the viewer may switch to with `cfg {codec}` (its list, limited to what this
    // machine encodes).
    let switchable: Vec<Codec> = [(Codec::Av1, av1.is_some() && lists("av1")), (Codec::H264, lists("h264")), (Codec::Jpeg, true)]
        .into_iter()
        .filter_map(|(c, ok)| ok.then_some(c))
        .collect();
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
        "codecs": switchable.iter().map(|c| c.name()).collect::<Vec<_>>(),
        "recording": recording,
        "whiteboard": whiteboard,
        "features": files::FEATURES.iter().copied().chain(ctl.perms.features()).collect::<Vec<_>>(),
        "encoder": encoder,
        "audio": audio_on,
        "audio_codec": if audio_on { Some("opus") } else { None },
    }))
    .await?;

    let params = Params { display: 0, fps: cfg.max_fps, quality: cfg.jpeg_quality, max_width: cfg.max_width, codec };
    let (params_tx, params_rx) = watch::channel(params);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(2);
    ctl.set_out(out_tx.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let force_full = Arc::new(AtomicBool::new(true));
    let _stop_guard = StopOnDrop(stop.clone());
    {
        let (out_tx, stop, force_full) = (out_tx.clone(), stop.clone(), force_full.clone());
        std::thread::spawn(move || capture::run(params_rx, out_tx, stop, force_full, encoder, h264));
    }
    // Audio has its own small queue so it is never stuck behind a large video frame, and is
    // dropped rather than delayed when the network cannot keep up.
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(16);
    let audio_enabled = Arc::new(AtomicBool::new(true));
    let mut audio_open = audio_on;
    if audio_on {
        let (enabled, stop) = (audio_enabled.clone(), stop.clone());
        std::thread::spawn(move || audio::stream(audio_tx, enabled, stop));
    } else {
        drop(audio_tx);
    }
    let last_clip = Arc::new(Mutex::new(String::new()));
    // Clipboard file lists need both permissions and a viewer that asked for them.
    let clip_files = cfg.allow_clipboard && cfg.allow_file_transfer && viewer_features.iter().any(|f| f == "clipfiles");
    let last_files: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    if cfg.allow_clipboard {
        let (out_tx, stop, last_clip) = (out_tx.clone(), stop.clone(), last_clip.clone());
        let last_files = clip_files.then(|| last_files.clone());
        std::thread::spawn(move || watch_clipboard(out_tx, stop, last_clip, last_files));
    }
    let mut files = files::Files::new();
    let input_tx = cfg.allow_input.then(|| input::spawn(capture::geometry(0).unwrap_or_default()));
    // While the machine is blocked, a viewer that went silent (no keepalive) ends the session.
    let mut last_seen = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = kill.notified() => {
                tracing::info!("session {sid}: disconnected by the local user");
                agent.report(sid, "denied", json!({"stage": "during_session"}));
                return Ok(());
            }
            Ok(()) = escapes.changed() => {
                if ctl.blocking() {
                    tracing::info!("session {sid}: ended with {} at this machine", controls::ESCAPE_HINT);
                    agent.report(sid, "denied", json!({"stage": "local_escape"}));
                    return Ok(());
                }
            }
            _ = watchdog.tick() => {
                if ctl.blocking() && last_seen.elapsed() > VIEWER_SILENT {
                    tracing::warn!("session {sid}: viewer silent while the machine is blocked; ending the session");
                    return Ok(());
                }
            }
            pkt = audio_rx.recv(), if audio_open => match pkt {
                Some(plain) => {
                    if let Some(r) = recorder.rec.as_mut() {
                        r.record(&plain);
                    }
                    tx.send(Message::Binary(ch.seal(&plain)?.into())).await?;
                }
                None => audio_open = false,
            },
            out = out_rx.recv() => {
                let Some(plain) = out else { bail!("capture stopped") };
                if let Some(r) = recorder.rec.as_mut() {
                    r.record(&plain);
                }
                tx.send(Message::Binary(ch.seal(&plain)?.into())).await?;
            }
            msg = read_viewer(&mut rx, &mut ch, Duration::from_secs(300)) => {
                last_seen = Instant::now();
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
                if matches!(msg, ViewerMsg::BlockInput { on: true } | ViewerMsg::Privacy { on: true }) {
                    escapes.borrow_and_update(); // an earlier escape does not end this block
                }
                if let Some(reply) = ctl.handle(&msg) {
                    send_json(&mut tx, &mut ch, reply).await?;
                    continue;
                }
                let input_tx = input_tx.as_ref().filter(|_| ctl.input_allowed());
                let send_input = |ev| if let Some(t) = input_tx { let _ = t.send(ev); };
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
                    ViewerMsg::Config { fps, quality, max_width, display, codec } => {
                        params_tx.send_modify(|p| {
                            if let Some(c) = codec.as_deref().and_then(Codec::from_name).filter(|c| switchable.contains(c)) { p.codec = c; }
                            if let Some(f) = fps { p.fps = f.clamp(1, cfg.max_fps.max(1)); }
                            if let Some(q) = quality { p.quality = q.clamp(10, 95); }
                            if let Some(w) = max_width { p.max_width = w.clamp(320, 3840); }
                            if let Some(d) = display.filter(|d| *d < displays.len()) {
                                p.display = d;
                                if let Some(g) = capture::geometry(d) { send_input(input::Event::Geometry(g)); }
                                if whiteboard {
                                    // Strokes belong to the display they were drawn on.
                                    let area = capture::logical_area(d).unwrap_or_default();
                                    if let Some(b) = agent.status.lock().unwrap().boards.iter_mut().find(|b| b.sid == sid) {
                                        *b = Board::new(sid, area);
                                    }
                                    agent.changed();
                                }
                            }
                        });
                        force_full.store(true, Ordering::Relaxed);
                    }
                    ViewerMsg::List { path } if cfg.allow_file_transfer => {
                        let reply = files.list(&path);
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::Put { id, dir, name, size, rel } if cfg.allow_file_transfer => {
                        let reply = files.put(id, &dir, &name, size, rel.as_deref());
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
                    ViewerMsg::GetDir { id, path } if cfg.allow_file_transfer => {
                        tracing::info!("session {sid}: sending folder {path}");
                        agent.report(sid, "file_download", json!({"path": path, "dir": true}));
                        files.get_dir(id, path, out_tx.clone());
                    }
                    ViewerMsg::Mkdir { id, dir, name } if cfg.allow_file_transfer => {
                        let reply = files.mkdir(id, &dir, &name);
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::Remove { id, path } if cfg.allow_file_transfer => {
                        let (reply, ok) = files.rm(id, &path);
                        if ok {
                            tracing::info!("session {sid}: deleted {path}");
                            agent.report(sid, "file_delete", json!({"path": path}));
                        }
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::Rename { id, path, name } if cfg.allow_file_transfer => {
                        let (reply, new) = files.mv(id, &path, &name);
                        if let Some(new) = new {
                            tracing::info!("session {sid}: renamed {path} to {new}");
                            agent.report(sid, "file_rename", json!({"path": path, "to": new}));
                        }
                        send_json(&mut tx, &mut ch, reply).await?;
                    }
                    ViewerMsg::ClipFiles { id, paths } if clip_files => {
                        let (out_tx, last_files, last_clip) = (out_tx.clone(), last_files.clone(), last_clip.clone());
                        tokio::task::spawn_blocking(move || {
                            let reply = set_clipboard_files(id, &paths, &last_files, &last_clip);
                            let _ = out_tx.blocking_send(proto::json(&reply));
                        });
                    }
                    ViewerMsg::Cancel { id } => files.cancel(id),
                    ViewerMsg::ClipFiles { id, .. } => {
                        send_json(&mut tx, &mut ch, json!({"t": "fs_error", "op": "clip_files", "id": id, "message": "clipboard file transfer is disabled on this device"})).await?;
                    }
                    ViewerMsg::GetDir { id, .. } => {
                        send_json(&mut tx, &mut ch, json!({"t": "fs_error", "op": "get_dir", "id": id, "message": "file transfer is disabled on this device"})).await?;
                    }
                    ViewerMsg::Mkdir { id, .. } | ViewerMsg::Remove { id, .. } | ViewerMsg::Rename { id, .. } => {
                        send_json(&mut tx, &mut ch, json!({"t": "fs_error", "id": id, "message": "file transfer is disabled on this device"})).await?;
                    }
                    ViewerMsg::List { .. } | ViewerMsg::Put { .. } | ViewerMsg::Get { .. } => {
                        send_json(&mut tx, &mut ch, json!({"t": "fs_error", "message": "file transfer is disabled on this device"})).await?;
                    }
                    ViewerMsg::Whiteboard { op, id, c, w, p } if whiteboard => {
                        tracing::debug!("whiteboard {op} id={id} points={}", p.len() / 2);
                        let changed = agent
                            .status
                            .lock()
                            .unwrap()
                            .boards
                            .iter_mut()
                            .find(|b| b.sid == sid)
                            .is_some_and(|b| b.apply(&op, id, &c, w, &p));
                        if changed {
                            agent.changed();
                        }
                    }
                    ViewerMsg::Refresh => force_full.store(true, Ordering::Relaxed),
                    ViewerMsg::Audio { on } => audio_enabled.store(on, Ordering::Relaxed),
                    ViewerMsg::Bye => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

type WsError = tokio_tungstenite::tungstenite::Error;

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
        {
            let mut st = self.agent.status.lock().unwrap();
            st.sessions.retain(|s| s.sid != self.sid);
            // Annotations disappear with the session.
            st.boards.retain(|b| b.sid != self.sid);
        }
        self.agent.changed();
    }
}

/// Closes the session's recording however the session ends, then uploads it in the background.
struct RecordingGuard {
    agent: Arc<Agent>,
    rec: Option<Recorder>,
}
impl Drop for RecordingGuard {
    fn drop(&mut self) {
        let Some(rec) = self.rec.take() else { return };
        let agent = self.agent.clone();
        std::thread::spawn(move || match rec.finish() {
            Ok(_) => crate::recording::upload_pending(&agent),
            Err(e) => tracing::error!("cannot finish session recording: {e:#}"),
        });
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
async fn ask_local_user(agent: &Agent, viewer: &Viewer, recorded: bool) -> bool {
    let who = if viewer.name.is_empty() { viewer.email.clone() } else { format!("{} <{}>", viewer.name, viewer.email) };

    // Desktop app running: ask in its window instead of a separate dialog.
    let ui_tx = agent.ui.lock().unwrap().as_ref().map(|u| (u.approvals.clone(), u.repaint.clone()));
    if let Some((tx, repaint)) = ui_tx {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let req = ApprovalRequest { sid: String::new(), who: who.clone(), ip: viewer.ip.clone(), recording: recorded, reply };
        if tx.send(req).is_ok() {
            repaint();
            return matches!(tokio::time::timeout(Duration::from_secs(30), answer).await, Ok(Ok(true)));
        }
    }

    let note = if recorded { "\nเซสชันนี้จะถูกบันทึกตามนโยบายขององค์กร" } else { "" };
    let question = format!("{who} ({}) ขอควบคุมเครื่องนี้ผ่าน Aegiron{note}\nอนุญาตหรือไม่?", viewer.ip);

    let terminal = async {
        if !std::io::stdin().is_terminal() {
            return std::future::pending::<bool>().await;
        }
        let mut lines = agent.stdin_lines.lock().await;
        while lines.try_recv().is_ok() {}
        let rec = if recorded { " The session will be recorded." } else { "" };
        println!("\n  >>> Remote control requested by {who} from {}.{rec} Allow? [y/N] (30s)", viewer.ip);
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

/// Files currently on the clipboard, canonicalised so lists we set ourselves compare equal.
fn clipboard_files(cb: &mut arboard::Clipboard) -> Vec<PathBuf> {
    cb.get().file_list().unwrap_or_default().into_iter().take(MAX_CLIP_FILES).map(|p| p.canonicalize().unwrap_or(p)).collect()
}

const MAX_CLIP_FILES: usize = 100;

/// Puts agent files on the clipboard for a viewer that pasted files (`clip_files`).
fn set_clipboard_files(id: Option<u32>, paths: &[String], last_files: &Mutex<Vec<PathBuf>>, last_clip: &Mutex<String>) -> serde_json::Value {
    let fail = |m: String| json!({"t": "fs_error", "op": "clip_files", "id": id, "message": m});
    let mut list = Vec::new();
    for p in paths.iter().take(MAX_CLIP_FILES) {
        let p = PathBuf::from(p);
        match p.canonicalize() {
            Ok(c) if p.is_absolute() => list.push(c),
            _ => return fail(format!("{} not found", p.display())),
        }
    }
    if list.is_empty() {
        return fail("no files".into());
    }
    let mut cb = match arboard::Clipboard::new() {
        Ok(cb) => cb,
        Err(e) => return fail(e.to_string()),
    };
    // Remember what we set so the watcher does not send it straight back.
    *last_files.lock().unwrap() = list.clone();
    if let Err(e) = cb.set().file_list(&list) {
        return fail(e.to_string());
    }
    *last_files.lock().unwrap() = clipboard_files(&mut cb);
    *last_clip.lock().unwrap() = cb.get_text().unwrap_or_default();
    json!({"t": "fs_ok", "op": "clip_files", "id": id})
}

fn watch_clipboard(out: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>, last: Arc<Mutex<String>>, last_files: Option<Arc<Mutex<Vec<PathBuf>>>>) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return };
    // Don't push whatever was already on the clipboard before the session started.
    if let Ok(t) = cb.get_text() {
        *last.lock().unwrap() = t;
    }
    if let Some(lf) = &last_files {
        *lf.lock().unwrap() = clipboard_files(&mut cb);
    }
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(800));
        if let Some(lf) = &last_files {
            let files = clipboard_files(&mut cb);
            let changed = {
                let mut l = lf.lock().unwrap();
                let changed = *l != files;
                *l = files.clone();
                changed
            };
            if changed && !files.is_empty() && !files.iter().all(|p| files::is_viewer_clip_copy(p)) {
                // Copied files also put their names on the clipboard as text: don't send that.
                *last.lock().unwrap() = cb.get_text().unwrap_or_default();
                let entries: Vec<_> = files.iter().map(|p| files::clip_entry(p)).collect();
                if out.blocking_send(proto::json(&json!({"t": "clip_files", "files": entries}))).is_err() {
                    return;
                }
                continue;
            }
        }
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
