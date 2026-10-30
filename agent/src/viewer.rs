//! Native viewer: controls another machine from the desktop app (one window per connection,
//! like RustDesk). Speaks the same end-to-end protocol as the browser viewer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::audio::{self, OpusDecoder, Player};
use crate::status::ChatLine;
use crate::voice::Call;
use crate::codec::{Codec, VideoDecoder};
use crate::crypto::{Channel, ViewerKeys};
use crate::files::{self, ensure_dirs, rel_components, unique_path};
use crate::{account, fingerprint, proto};

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Requesting,
    Waiting,
    Handshake,
    Password,
    Approval,
    Connected,
    Closed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Display {
    pub index: usize,
    pub name: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub displays: Vec<Display>,
    #[serde(default)]
    pub display: usize,
    #[serde(default)]
    pub input: bool,
    #[serde(default)]
    pub clipboard: bool,
    #[serde(default)]
    pub files: bool,
    /// The controlled machine records this session (organisation policy).
    #[serde(default)]
    pub recording: bool,
    /// The controlled machine shows annotations drawn by the viewer.
    #[serde(default)]
    pub whiteboard: bool,
    /// Optional capabilities of the agent ("fs2", "clipfiles"); empty on older agents.
    #[serde(default)]
    pub features: Vec<String>,
    /// The agent streams its audio to this viewer.
    #[serde(default)]
    pub audio: bool,
    /// Codecs this session can switch to with `cfg {codec}`; empty on agents before 0.6.
    #[serde(default)]
    pub codecs: Vec<String>,
}

impl Info {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Text chat with the person at the remote machine.
    pub fn chat(&self) -> bool {
        self.has(proto::FEATURE_CHAT)
    }

    /// Voice calls (the remote machine allows them and has a microphone).
    pub fn voice(&self) -> bool {
        self.has(proto::FEATURE_VOICE) && audio::voice_supported()
    }

    /// Folder transfer and file management.
    pub fn fs2(&self) -> bool {
        self.files && self.has("fs2")
    }

    /// Copy/paste of files through the clipboard (needs both permissions).
    pub fn clip_files(&self) -> bool {
        self.files && self.clipboard && self.has("clipfiles")
    }
}

/// Video codecs this viewer decodes, best first: H.264 unless the user picked AV1. AV1 is
/// listed either way, so the agent can say whether a switch to it is possible.
fn codecs(prefer: Codec) -> [&'static str; 3] {
    match prefer {
        Codec::Av1 => ["av1", "h264", "jpeg"],
        _ => ["h264", "av1", "jpeg"],
    }
}
/// Optional protocol features this viewer supports (sent in `auth`): clipboard files and chat
/// always, voice calls with a microphone and speaker, switch sides when this app runs its own
/// agent (`local_id`).
fn features(switch: bool) -> Vec<&'static str> {
    let mut f = vec!["clipfiles", proto::FEATURE_CHAT];
    if audio::voice_supported() {
        f.push(proto::FEATURE_VOICE);
    }
    if switch {
        f.push(proto::FEATURE_SWITCH);
    }
    f
}

/// Voice call state as seen from the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CallState {
    Idle,
    /// Waiting for the person at the remote machine to accept.
    Calling(Instant),
    Active { since: Instant, muted: bool, remote_muted: bool },
}

fn clip_tmp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!("{}{}-{}", files::CLIP_TMP_PREFIX, std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Audio codecs this viewer plays (none where there is no playback backend).
fn audio_codecs() -> &'static [&'static str] {
    if audio::playback_supported() { &["opus"] } else { &[] }
}

/// Remote screen; `dirty` lists rectangles (x, y, w, h) not yet uploaded to the GPU.
#[derive(Default)]
pub struct Framebuffer {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub dirty: Vec<[usize; 4]>,
    pub resized: bool,
}

impl Framebuffer {
    fn blit(&mut self, x: usize, y: usize, fw: usize, fh: usize, img: &image::RgbaImage) {
        if self.w != fw || self.h != fh {
            tracing::info!("viewer: remote screen {fw}x{fh}");
            *self = Framebuffer { w: fw, h: fh, rgba: vec![0; fw * fh * 4], dirty: Vec::new(), resized: true };
        }
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        let (w, h) = (iw.min(fw.saturating_sub(x)), ih.min(fh.saturating_sub(y)));
        for row in 0..h {
            let src = &img.as_raw()[row * iw * 4..(row * iw + w) * 4];
            let dst = ((y + row) * fw + x) * 4;
            self.rgba[dst..dst + w * 4].copy_from_slice(src);
        }
        if !self.resized {
            self.dirty.push([x, y, w, h]);
        }
    }

    /// Replaces the whole picture (a decoded video frame).
    fn set(&mut self, w: usize, h: usize, rgba: &[u8]) {
        if self.w != w || self.h != h {
            tracing::info!("viewer: remote screen {w}x{h}");
            *self = Framebuffer { w, h, rgba: Vec::new(), dirty: Vec::new(), resized: true };
        }
        self.rgba.clear();
        self.rgba.extend_from_slice(rgba);
        if !self.resized {
            self.dirty.clear();
            self.dirty.push([0, 0, w, h]);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum TransferStatus {
    #[default]
    Active,
    Done(String),
    Failed(String),
}

/// One upload or download; a folder counts as one transfer with per-file detail.
#[derive(Clone, Debug, Default)]
pub struct Transfer {
    pub id: u32,
    pub name: String,
    /// Viewer → agent.
    pub up: bool,
    pub folder: bool,
    /// Total bytes and bytes done.
    pub size: u64,
    pub sent: u64,
    pub files: u32,
    pub files_done: u32,
    pub failed: u32,
    /// File in progress (relative path inside a folder) and its progress.
    pub current: String,
    pub cur_size: u64,
    pub cur_sent: u64,
    pub status: TransferStatus,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    #[serde(default)]
    pub dir: bool,
    #[serde(default)]
    pub size: u64,
}

/// The remote directory shown in the file panel.
#[derive(Clone, Debug, Default)]
pub struct RemoteDir {
    pub path: String,
    pub parent: Option<String>,
    pub sep: String,
    pub entries: Vec<RemoteEntry>,
    pub error: Option<String>,
    pub loading: bool,
}

/// A file or folder copied on the remote machine (`clip_files`).
#[derive(Clone, Debug, Deserialize)]
pub struct ClipEntry {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub dir: bool,
    #[serde(default)]
    pub size: u64,
}

pub struct State {
    pub phase: Phase,
    pub error: Option<String>,
    pub attempts_left: Option<u64>,
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    pub info: Option<Info>,
    pub fb: Framebuffer,
    pub fps: u32,
    pub kbps: u32,
    pub remote_clipboard: Option<String>,
    /// Remote pointer, normalised to the screen; None while it is on another display.
    pub cursor: Option<(f32, f32)>,
    /// Codec the agent chose ("h264", "av1" or "jpeg").
    pub codec: String,
    /// Codec the user asked for (H.264 or AV1); the agent may not have it.
    pub want_codec: Codec,
    /// Encoder the agent uses ("videotoolbox", "mediafoundation", "openh264", "jpeg").
    pub encoder: String,
    /// Speaker muted by the user (the agent is asked to stop sending audio).
    pub muted: bool,
    /// Loudest decoded 20 ms block of the last second (RMS, 0–1); None before any audio.
    pub audio_level: Option<f32>,
    pub transfers: Vec<Transfer>,
    /// How the session travels: relay, direct (TCP) or P2P (UDP); None until connected.
    pub path: Option<crate::direct::Path>,
    pub browser: RemoteDir,
    /// Files copied on the remote machine that are too big to fetch without asking.
    pub clip_offer: Option<Vec<ClipEntry>>,
    /// Message for the window to show once (from background transfers).
    pub notice: Option<String>,
    /// Chat with the person at the remote machine (this session only, never stored).
    pub chat: Vec<ChatLine>,
    /// Messages from the remote side not seen yet.
    pub chat_unread: usize,
    pub call: CallState,
    /// Waiting for the other side to answer a switch-sides invitation.
    pub switch_pending: bool,
    /// The sides were switched: the other side now controls this machine and this window closes.
    pub switched: bool,
}

enum Cmd {
    Json(Value),
    Chat(String),
    Call,
    Hangup,
    MuteMic(bool),
    SwitchSides,
    /// Local file or folder → remote directory ("" = ~/Downloads/Aegiron).
    Upload(PathBuf, String),
    /// Remote file or folder → local ~/Downloads/Aegiron.
    Download(String, bool),
    Cancel(u32),
    /// Clipboard paste: upload, put on the remote clipboard, then send these key events.
    PasteFiles(Vec<PathBuf>, Vec<Value>),
    /// Remote clipboard files → local clipboard (`true` = into a temporary folder).
    FetchClip(Vec<ClipEntry>, bool),
    Close,
}

/// Messages for a running transfer, routed by transfer id.
enum Ev {
    Json(Value),
    Chunk(Vec<u8>),
}

/// Files that were last pasted or fetched through the clipboard: (path, size, mtime).
type ClipSig = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;

fn clip_sig(paths: &[PathBuf]) -> ClipSig {
    paths
        .iter()
        .map(|p| {
            let m = std::fs::metadata(p).ok();
            // Canonical, so a list we set compares equal to what the clipboard reports back.
            (p.canonicalize().unwrap_or_else(|_| p.clone()), m.as_ref().map(|m| m.len()).unwrap_or(0), m.and_then(|m| m.modified().ok()))
        })
        .collect()
}

/// Remote clipboard selections up to this size (files only) are fetched without asking.
const AUTO_FETCH_FILES: usize = 20;
const AUTO_FETCH_BYTES: u64 = 32 << 20;
/// A paste that needed longer than this to upload is not pressed automatically any more.
const PASTE_DEADLINE: Duration = Duration::from_secs(10);

pub struct Viewer {
    pub state: Mutex<State>,
    tx: mpsc::UnboundedSender<Cmd>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    routes: Mutex<HashMap<u32, mpsc::UnboundedSender<Ev>>>,
    next_id: AtomicU32,
    cancels: Mutex<HashSet<u32>>,
    last_clip_files: Mutex<ClipSig>,
    /// Temporary folder for small clipboard fetches; removed when the session ends.
    clip_tmp: PathBuf,
    /// Optional features sent in `auth`.
    features: Vec<&'static str>,
    /// Switch sides: the secret that admits this viewer to the other machine (see switch.rs).
    switch_secret: Option<String>,
}

/// What a connection needs from the signed-in app.
pub struct Target {
    pub server: String,
    pub token: String,
    pub insecure: bool,
    pub device_id: String,
    pub name: String,
    /// Try direct/P2P paths before falling back to the relay (Settings → Network).
    pub direct: bool,
    /// Diagnostics (`aegiron-agent connect --only`): use just this path, no fallback.
    pub only: Option<crate::direct::Path>,
    /// Video codec to ask for (Settings → General; H.264 unless the user picked AV1).
    pub codec: Codec,
    /// ID of this app's own (running) agent: enables switch sides.
    pub local_id: Option<String>,
    /// This viewer is the switched side of a switch-sides session: the ticket the server issued
    /// for the invitation, used instead of requesting a session.
    pub switch: Option<SwitchTarget>,
}

pub struct SwitchTarget {
    pub ticket: account::SessionTicket,
    pub secret: String,
}

impl Viewer {
    pub fn connect(target: Target, repaint: Arc<dyn Fn() + Send + Sync>) -> Arc<Viewer> {
        let (tx, rx) = mpsc::unbounded_channel();
        let v = Arc::new(Viewer {
            state: Mutex::new(State {
                phase: Phase::Requesting,
                error: None,
                attempts_left: None,
                device_id: target.device_id.clone(),
                device_name: target.name.clone(),
                fingerprint: String::new(),
                info: None,
                fb: Framebuffer::default(),
                fps: 0,
                kbps: 0,
                remote_clipboard: None,
                cursor: None,
                codec: String::new(),
                want_codec: target.codec,
                encoder: String::new(),
                muted: false,
                audio_level: None,
                transfers: Vec::new(),
                path: None,
                browser: RemoteDir::default(),
                clip_offer: None,
                notice: None,
                chat: Vec::new(),
                chat_unread: 0,
                call: CallState::Idle,
                switch_pending: false,
                switched: false,
            }),
            tx,
            repaint,
            routes: Mutex::default(),
            next_id: AtomicU32::new(1),
            cancels: Mutex::default(),
            last_clip_files: Mutex::default(),
            clip_tmp: clip_tmp_dir(),
            features: features(target.local_id.is_some() && target.switch.is_none()),
            switch_secret: target.switch.as_ref().map(|s| s.secret.clone()),
        });
        let task = v.clone();
        crate::runtime().spawn(async move {
            let res = run(&task, target, rx).await;
            match &res {
                Ok(()) => tracing::info!("viewer session ended"),
                Err(e) => tracing::warn!("viewer session failed: {e:#}"),
            }
            let mut st = task.state.lock().unwrap();
            if let Err(e) = res {
                st.error.get_or_insert_with(|| format!("{e:#}"));
            }
            st.phase = Phase::Closed;
            drop(st);
            let _ = std::fs::remove_dir_all(&task.clip_tmp);
            (task.repaint)();
        });
        v
    }

    /// A connected-looking viewer showing `screen`, without any network (screenshots/docs).
    pub fn demo(name: &str, id: &str, fp: &str, screen: &image::RgbaImage) -> Arc<Viewer> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut fb = Framebuffer::default();
        fb.blit(0, 0, screen.width() as usize, screen.height() as usize, screen);
        let info = Info {
            hostname: name.into(),
            os: "windows".into(),
            displays: vec![
                Display { index: 0, name: "Display 1".into(), primary: true },
                Display { index: 1, name: "Display 2".into(), primary: false },
            ],
            display: 0,
            input: true,
            clipboard: true,
            files: true,
            recording: std::env::var_os("AEGIRON_UI_DEMO_RECORDING").is_some(),
            whiteboard: true,
            features: crate::files::FEATURES.iter().chain(&[proto::FEATURE_CHAT, proto::FEATURE_VOICE, proto::FEATURE_SWITCH]).map(|f| f.to_string()).collect(),
            audio: true,
            codecs: vec!["av1".into(), "h264".into(), "jpeg".into()],
        };
        Arc::new(Viewer {
            state: Mutex::new(State {
                phase: Phase::Connected,
                error: None,
                attempts_left: None,
                device_id: id.into(),
                device_name: name.into(),
                fingerprint: fp.into(),
                info: Some(info),
                fb,
                fps: 15,
                kbps: 1840,
                remote_clipboard: None,
                cursor: None,
                codec: String::new(),
                want_codec: Codec::H264,
                encoder: String::new(),
                muted: false,
                audio_level: None,
                transfers: vec![Transfer { id: 1, name: "รายงานประจำเดือน.xlsx".into(), size: 100, sent: 64, files: 1, ..Default::default() }],
                path: Some(crate::direct::Path::Direct),
                browser: RemoteDir::default(),
                clip_offer: None,
                notice: None,
                chat: Vec::new(),
                chat_unread: 0,
                call: CallState::Idle,
                switch_pending: false,
                switched: false,
            }),
            tx,
            repaint: Arc::new(|| {}),
            routes: Mutex::default(),
            next_id: AtomicU32::new(1),
            cancels: Mutex::default(),
            last_clip_files: Mutex::default(),
            clip_tmp: clip_tmp_dir(),
            features: features(true),
            switch_secret: None,
        })
    }

    pub fn send(&self, v: Value) {
        let _ = self.tx.send(Cmd::Json(v));
    }

    pub fn submit_password(&self, pw: &str) {
        let prefer = {
            let mut st = self.state.lock().unwrap();
            st.error = None;
            st.want_codec
        };
        self.send(json!({"t": "auth", "password": pw, "codecs": codecs(prefer), "features": self.features, "audio": audio_codecs()}));
    }

    /// This window can offer switch sides (this app runs its own agent, online).
    pub fn can_switch(&self) -> bool {
        self.features.contains(&proto::FEATURE_SWITCH)
    }

    /// Sends a chat message to the person at the remote machine.
    pub fn send_chat(&self, text: &str) {
        let text: String = text.trim().chars().take(proto::CHAT_MAX).collect();
        if !text.is_empty() {
            let _ = self.tx.send(Cmd::Chat(text));
        }
    }

    /// The chat panel shows everything received so far.
    pub fn mark_chat_read(&self) {
        self.state.lock().unwrap().chat_unread = 0;
    }

    /// Asks the person at the remote machine for a voice call.
    pub fn call(&self) {
        let _ = self.tx.send(Cmd::Call);
    }

    /// Ends the call (or stops calling).
    pub fn hangup(&self) {
        let _ = self.tx.send(Cmd::Hangup);
    }

    pub fn set_mic_muted(&self, muted: bool) {
        let _ = self.tx.send(Cmd::MuteMic(muted));
    }

    /// Invites the person at the remote machine to control this machine (the user confirmed).
    pub fn switch_sides(&self) {
        self.update(|st| st.switch_pending = true);
        let _ = self.tx.send(Cmd::SwitchSides);
    }

    /// Switches the video codec for this session (H.264 or AV1). Agents that cannot, or that
    /// predate codec switching, keep the current one; `codec` shows what is in use.
    pub fn set_codec(&self, codec: Codec) {
        self.update(|st| st.want_codec = codec);
        self.send(json!({"t": "cfg", "codec": codec.name()}));
    }

    /// Mutes or unmutes the remote audio; the agent stops sending while muted.
    pub fn set_muted(&self, muted: bool) {
        self.update(|st| st.muted = muted);
        self.send(json!({"t": "audio", "on": !muted}));
    }

    /// Uploads a local file or folder into `remote_dir` ("" = the agent's ~/Downloads/Aegiron).
    pub fn upload(&self, path: PathBuf, remote_dir: String) {
        let _ = self.tx.send(Cmd::Upload(path, remote_dir));
    }

    /// Downloads a remote file or folder into this machine's ~/Downloads/Aegiron.
    pub fn download(&self, remote: String, dir: bool) {
        let _ = self.tx.send(Cmd::Download(remote, dir));
    }

    pub fn cancel(&self, id: u32) {
        let _ = self.tx.send(Cmd::Cancel(id));
    }

    pub fn list_dir(&self, path: &str) {
        {
            let mut st = self.state.lock().unwrap();
            st.browser.loading = true;
            st.browser.error = None;
        }
        self.send(json!({"t": "ls", "path": path}));
    }

    /// Sends a file-management request (`mkdir`, `rm`, `mv`); the listing refreshes on `fs_ok`.
    pub fn fs_op(&self, mut msg: Value) {
        msg["id"] = json!(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.send(msg);
    }

    pub fn clear_finished(&self) {
        self.state.lock().unwrap().transfers.retain(|t| t.status == TransferStatus::Active);
    }

    /// Paste in the remote window. If this machine's clipboard holds files (and the agent
    /// allows clipboard files), uploads them, puts them on the remote clipboard and then sends
    /// `paste_keys`. The result says what the caller still has to do.
    pub fn paste_files(&self, paste_keys: Vec<Value>) -> Paste {
        let ok = {
            let st = self.state.lock().unwrap();
            st.phase == Phase::Connected && st.info.as_ref().is_some_and(Info::clip_files)
        };
        if !ok {
            return Paste::Text;
        }
        let files = arboard::Clipboard::new().and_then(|mut c| c.get().file_list()).unwrap_or_default();
        if files.is_empty() {
            return Paste::Text;
        }
        let sig = clip_sig(&files);
        {
            let mut last = self.last_clip_files.lock().unwrap();
            if *last == sig {
                // Already on the remote clipboard (pasted before, or it came from there).
                return Paste::KeysOnly;
            }
            *last = sig;
        }
        let _ = self.tx.send(Cmd::PasteFiles(files, paste_keys));
        Paste::Files
    }

    /// Fetches files the remote side copied and puts them on this machine's clipboard.
    pub fn accept_clip_offer(&self) {
        if let Some(entries) = self.state.lock().unwrap().clip_offer.take() {
            let _ = self.tx.send(Cmd::FetchClip(entries, false));
        }
    }

    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut st = self.state.lock().unwrap();
        let before = st.phase.clone();
        f(&mut st);
        if st.phase != before {
            tracing::info!("viewer {}: {:?} -> {:?}", st.device_id, before, st.phase);
        }
        drop(st);
        (self.repaint)();
    }
}

/// Human-readable reason for a relay close code (same wording as the web viewer).
fn explain_close(reason: &str) -> String {
    if let Some(r) = reason.strip_prefix("agent_rejected:") {
        let r = r.trim();
        return if r.contains("denied") {
            "ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ".into()
        } else if r.contains("too many") {
            "ใส่รหัสผ่านผิดหลายครั้งเกินไป".into()
        } else if r.contains("locked") {
            "เครื่องปลายทางล็อกการยืนยันตัวตนชั่วคราว".into()
        } else {
            format!("เครื่องปลายทางปฏิเสธ: {r}")
        };
    }
    match reason {
        "pair_timeout" => "เครื่องปลายทางไม่ตอบสนอง".into(),
        "agent_disconnected" | "agent_closed" => "เครื่องปลายทางปิดการเชื่อมต่อ".into(),
        "viewer_closed" | "" => String::new(),
        r => format!("การเชื่อมต่อสิ้นสุด ({r})"),
    }
}

async fn next_text(rx: &mut crate::direct::WsRx, what: &str, secs: u64) -> Result<String> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(secs), rx.next())
            .await
            .map_err(|_| anyhow!("หมดเวลารอ{what}"))?
            .ok_or(anyhow!("การเชื่อมต่อถูกปิด"))??;
        match m {
            Message::Text(t) => return Ok(t.as_str().to_owned()),
            Message::Close(f) => bail!("{}", explain_close(f.as_ref().map(|f| f.reason.as_str()).unwrap_or(""))),
            _ => {}
        }
    }
}

async fn run(v: &Arc<Viewer>, mut t: Target, mut cmds: mpsc::UnboundedReceiver<Cmd>) -> Result<()> {
    // 1. Ask the server to wake the agent (blocking HTTPS call off the async threads). A
    //    switched session already has its ticket (the server issued it for the invitation).
    let (server, token, id) = (t.server.clone(), t.token.clone(), t.device_id.clone());
    let direct = t.direct && t.only != Some(crate::direct::Path::Relay);
    let ticket = match t.switch.take() {
        Some(s) => s.ticket,
        None => tokio::task::spawn_blocking(move || account::start_session(&server, &token, &id, direct))
            .await?
            .map_err(|e| anyhow!(e))?,
    };
    let device_key: [u8; 32] = B64.decode(&ticket.device.public_key)?.try_into().map_err(|_| anyhow!("bad device key"))?;
    let name = [&ticket.device.alias, &ticket.device.hostname].into_iter().find(|s| !s.is_empty()).cloned().unwrap_or(t.name.clone());
    v.update(|st| {
        st.fingerprint = fingerprint(&device_key);
        st.device_name = name;
        st.phase = Phase::Waiting;
    });

    // 2. Relay: the viewer token plus this user's bearer token (the server checks both).
    let url = crate::config::ws_url(&t.server, t.insecure, &format!(
        "{}?sid={}&token={}",
        ticket.ticket.relay_path, ticket.ticket.sid, ticket.ticket.token
    ))?;
    let mut req = url.as_str().into_client_request()?;
    if !t.token.is_empty() {
        req.headers_mut().insert("Authorization", format!("Bearer {}", t.token).parse()?);
    }
    let relay = async move {
        let (ws, _) = crate::net::connect_ws(req).await.context("เชื่อมต่อ relay ไม่สำเร็จ")?;
        let mut l = crate::direct::link(ws, crate::direct::Path::Relay, Default::default());
        let paired = next_text(&mut l.rx, "เครื่องปลายทางตอบรับ", 60).await?;
        if !paired.contains("\"paired\"") {
            bail!("unexpected relay message");
        }
        Ok(l)
    };
    // Direct (LAN/TCP) and P2P (QUIC) candidates race the relay; the relay is always the fallback.
    let keys = ViewerKeys::new();
    let direct = ticket.ticket.direct.clone().filter(|_| direct).map(|d| crate::direct::ViewerDirect {
        server_url: t.server.clone(),
        sid: ticket.ticket.sid.clone(),
        ticket: d,
        epk: B64.encode(keys.epk),
        only: t.only,
    });
    if direct.is_none() && t.only.is_some_and(|o| o != crate::direct::Path::Relay) {
        bail!("the server offered no direct path for this session");
    }
    let crate::direct::Link { mut tx, mut rx, path, keep: _keep } = crate::direct::viewer_race(direct, relay).await?;
    tracing::info!("viewer {}: connected via {}", t.device_id, path.as_str());
    v.update(|st| {
        st.phase = Phase::Handshake;
        st.path = Some(path);
    });

    // 3. End-to-end key exchange, authenticated by the device's registered key.
    tx.send(Message::Text(json!({"t": "hello", "v": 1, "epk": B64.encode(keys.epk)}).to_string().into())).await?;
    #[derive(Deserialize)]
    struct Hello {
        epk: String,
        sig: String,
    }
    let hello: Hello = serde_json::from_str(&next_text(&mut rx, "การแลกเปลี่ยนกุญแจ", 30).await?)?;
    let agent_epk: [u8; 32] = B64.decode(hello.epk)?.try_into().map_err(|_| anyhow!("bad agent key"))?;
    let sig: [u8; 64] = B64.decode(hello.sig)?.try_into().map_err(|_| anyhow!("bad signature"))?;
    let mut ch: Channel = keys.finish(&ticket.ticket.sid, agent_epk, &sig, &device_key)?;

    // Transfers run on their own tasks and feed this bounded queue (back-pressure).
    let (up_tx, mut up_rx) = mpsc::channel::<Vec<u8>>(8);
    let (mut frames, mut bytes, mut stats_at) = (0u32, 0usize, Instant::now());
    let mut video: Option<VideoDecoder> = None;
    let mut opus: Option<OpusDecoder> = None;
    let mut player: Option<Player> = None;
    // AEGIRON_AUDIO_OUT=0 decodes but does not play (tests on the same machine as the agent,
    // where playing would loop back into the capture).
    let mut player_failed = std::env::var("AEGIRON_AUDIO_OUT").is_ok_and(|v| v == "0");
    let mut level: Option<f32> = None;
    // Voice call: this machine's microphone as 0x06 packets, the remote side's voice played here.
    let (voice_tx, mut voice_rx) = mpsc::channel::<Vec<u8>>(16);
    let mut call: Option<Call> = None;
    // Switch sides: the secret handed out with the pending invitation.
    let mut switch_secret: Option<String> = None;
    let sid = ticket.ticket.sid.clone();

    loop {
        tokio::select! {
            Some(plain) = voice_rx.recv() => tx.send(Message::Binary(ch.seal(&plain)?.into())).await?,
            m = rx.next() => {
                let m = match m {
                    None => return Ok(()),
                    Some(m) => m?,
                };
                let data = match m {
                    Message::Binary(b) => b,
                    Message::Close(f) => {
                        let why = explain_close(f.as_ref().map(|f| f.reason.as_str()).unwrap_or(""));
                        if !why.is_empty() {
                            v.update(|st| { st.error.get_or_insert(why); });
                        }
                        return Ok(());
                    }
                    _ => continue,
                };
                bytes += data.len();
                let plain = ch.open(&data)?;
                match plain.first() {
                    Some(&proto::MSG_FRAME) if plain.len() > 9 => {
                        let n = |i: usize| u16::from_be_bytes([plain[i], plain[i + 1]]) as usize;
                        let (x, y, fw, fh) = (n(1), n(3), n(5), n(7));
                        let img = image::load_from_memory_with_format(&plain[9..], image::ImageFormat::Jpeg)?.to_rgba8();
                        v.update(|st| st.fb.blit(x, y, fw, fh, &img));
                        frames += 1;
                    }
                    Some(&proto::MSG_VIDEO) if plain.len() > 7 => {
                        let Some(codec) = Codec::from_id(plain[1]) else { continue };
                        if video.as_ref().is_none_or(|d| d.codec() != codec) {
                            // A new decoder (first frame, codec switch, after an error) starts
                            // at a keyframe; the agent sends one with every switch.
                            if plain[2] & proto::VIDEO_KEYFRAME == 0 {
                                continue;
                            }
                            video = Some(VideoDecoder::new(codec)?);
                        }
                        match video.as_mut().unwrap().decode(&plain[7..]) {
                            Ok(Some((w, h, rgba))) => {
                                v.update(|st| st.fb.set(w, h, rgba));
                                frames += 1;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                // Start over from the next keyframe rather than show a smeared picture.
                                tracing::warn!("viewer: {e:#}; requesting a keyframe");
                                video = None;
                                tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "keyframe"})))?.into())).await?;
                            }
                        }
                    }
                    Some(&proto::MSG_FILE) if plain.len() >= 5 => {
                        let id = u32::from_be_bytes(plain[1..5].try_into().unwrap());
                        if let Some(r) = v.routes.lock().unwrap().get(&id) {
                            let _ = r.send(Ev::Chunk(plain[5..].to_vec()));
                        }
                    }
                    Some(&proto::MSG_AUDIO) => {
                        let Some((proto::AUDIO_OPUS, _seq, packet)) = proto::parse_audio(&plain) else { continue };
                        if opus.is_none() {
                            tracing::info!("viewer: receiving audio (Opus)");
                            opus = Some(OpusDecoder::new()?);
                        }
                        match opus.as_mut().unwrap().decode(packet) {
                            Ok(pcm) => {
                                let l = audio::rms(pcm);
                                level = Some(level.unwrap_or(0.0).max(l));
                                if v.state.lock().unwrap().muted {
                                    if let Some(p) = &player { p.clear(); }
                                } else {
                                    if player.is_none() && !player_failed {
                                        // Opening the output device takes a moment; don't stall other tasks.
                                        match tokio::task::block_in_place(Player::new) {
                                            Ok(p) => player = Some(p),
                                            Err(e) => {
                                                tracing::warn!("viewer: no audio output: {e:#}");
                                                player_failed = true;
                                            }
                                        }
                                    }
                                    if let Some(p) = &player { p.push(pcm); }
                                }
                            }
                            Err(e) => tracing::debug!("viewer: {e:#}"),
                        }
                    }
                    Some(&proto::MSG_VOICE) => {
                        if let Some(c) = call.as_mut() {
                            if c.needs_output() {
                                tokio::task::block_in_place(|| c.receive(&plain));
                            } else {
                                c.receive(&plain);
                            }
                        }
                    }
                    Some(&proto::MSG_JSON) => {
                        let m: Value = serde_json::from_slice(&plain[1..])?;
                        if m["t"] == "voice" {
                            if let Some(reply) = voice_event(v, &m, &mut call, &voice_tx) {
                                tx.send(Message::Binary(ch.seal(&proto::json(&reply))?.into())).await?;
                            }
                            continue;
                        }
                        if m["t"] == "switch_result" && m["ok"] != true {
                            if let Some(s) = switch_secret.take() {
                                crate::switch::withdraw(&s);
                            }
                        }
                        let route = m["id"].as_u64().and_then(|id| v.routes.lock().unwrap().get(&(id as u32)).cloned());
                        if let Some(r) = route {
                            let _ = r.send(Ev::Json(m));
                        } else if let Some(reply) = control(v, &m) {
                            tx.send(Message::Binary(ch.seal(&proto::json(&reply))?.into())).await?;
                        }
                    }
                    _ => {}
                }
                if stats_at.elapsed() >= Duration::from_secs(1) {
                    let (f, kb) = (frames, (bytes * 8 / 1000) as u32);
                    if let Some(l) = level {
                        tracing::debug!("viewer: audio level {l:.4}");
                    }
                    let lvl = level.take().or(opus.as_ref().map(|_| 0.0));
                    v.update(|st| { st.fps = f; st.kbps = kb; st.audio_level = lvl; });
                    (frames, bytes, stats_at) = (0, 0, Instant::now());
                }
            }
            c = cmds.recv() => match c {
                Some(Cmd::Json(j)) => tx.send(Message::Binary(ch.seal(&proto::json(&j))?.into())).await?,
                Some(Cmd::Chat(text)) => {
                    v.update(|st| st.chat.push(ChatLine { mine: true, text: text.clone(), at: std::time::SystemTime::now() }));
                    tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "chat", "text": text})))?.into())).await?;
                }
                Some(Cmd::Call) => {
                    let ok = {
                        let st = v.state.lock().unwrap();
                        st.call == CallState::Idle && st.info.as_ref().is_some_and(Info::voice)
                    };
                    if ok {
                        v.update(|st| st.call = CallState::Calling(Instant::now()));
                        tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "voice", "op": "call"})))?.into())).await?;
                    }
                }
                Some(Cmd::Hangup) => {
                    call = None;
                    let was = std::mem::replace(&mut v.state.lock().unwrap().call, CallState::Idle);
                    (v.repaint)();
                    if was != CallState::Idle {
                        tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "voice", "op": "hangup"})))?.into())).await?;
                    }
                }
                Some(Cmd::MuteMic(muted)) => {
                    if let Some(c) = &call {
                        c.set_muted(muted);
                        v.update(|st| if let CallState::Active { muted: m, .. } = &mut st.call { *m = muted });
                        tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "voice", "op": "mute", "muted": muted})))?.into())).await?;
                    }
                }
                Some(Cmd::SwitchSides) => {
                    // The other side gets a server grant for a session to this machine's agent,
                    // and the secret this agent admits it with (it closes this window then).
                    let Some(local) = t.local_id.clone() else { continue };
                    let weak = Arc::downgrade(v);
                    let secret = crate::switch::offer(move || {
                        if let Some(v) = weak.upgrade() {
                            v.update(|st| st.switched = true);
                            v.close();
                        }
                    });
                    if let Some(old) = switch_secret.replace(secret.clone()) {
                        crate::switch::withdraw(&old);
                    }
                    let (server, token, sid, out, v2) = (t.server.clone(), t.token.clone(), sid.clone(), up_tx.clone(), v.clone());
                    tokio::spawn(async move {
                        let grant = tokio::task::spawn_blocking(move || account::switch_grant(&server, &token, &sid, &local)).await;
                        match grant {
                            Ok(Ok(grant)) => {
                                let _ = out.send(proto::json(&json!({"t": "switch_sides", "grant": grant, "secret": secret}))).await;
                            }
                            Ok(Err(e)) => {
                                crate::switch::withdraw(&secret);
                                v2.update(|st| {
                                    st.switch_pending = false;
                                    st.notice = Some(format!("สลับฝั่งไม่สำเร็จ: {e}"));
                                });
                            }
                            Err(_) => crate::switch::withdraw(&secret),
                        }
                    });
                }
                Some(Cmd::Upload(path, dir)) => {
                    let link = Link { v: v.clone(), out: up_tx.clone() };
                    tokio::spawn(async move { link.upload(&path, &dir).await; });
                }
                Some(Cmd::Download(path, dir)) => {
                    let link = Link { v: v.clone(), out: up_tx.clone() };
                    tokio::spawn(async move { link.download(&path, dir, &files::default_upload_dir()).await; });
                }
                Some(Cmd::Cancel(id)) => {
                    v.cancels.lock().unwrap().insert(id);
                }
                Some(Cmd::PasteFiles(paths, keys)) => {
                    let link = Link { v: v.clone(), out: up_tx.clone() };
                    tokio::spawn(async move { link.paste(paths, keys).await; });
                }
                Some(Cmd::FetchClip(entries, temp)) => {
                    let link = Link { v: v.clone(), out: up_tx.clone() };
                    tokio::spawn(async move { link.fetch_clip(entries, temp).await; });
                }
                Some(Cmd::Close) | None => {
                    if let Some(s) = switch_secret.take().filter(|_| !v.state.lock().unwrap().switched) {
                        crate::switch::withdraw(&s);
                    }
                    let _ = tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "bye"})))?.into())).await;
                    let _ = tx.close().await;
                    return Ok(());
                }
            },
            Some(plain) = up_rx.recv() => tx.send(Message::Binary(ch.seal(&plain)?.into())).await?,
        }
    }
}

/// A `voice` message from the agent; may return a reply.
fn voice_event(v: &Viewer, m: &Value, call: &mut Option<Call>, voice_tx: &mpsc::Sender<Vec<u8>>) -> Option<Value> {
    let mut reply = None;
    match m["op"].as_str().unwrap_or("") {
        "accept" if matches!(v.state.lock().unwrap().call, CallState::Calling(_)) => match tokio::task::block_in_place(|| Call::start(voice_tx.clone())) {
            Ok(c) => {
                *call = Some(c);
                v.update(|st| st.call = CallState::Active { since: Instant::now(), muted: false, remote_muted: false });
            }
            Err(e) => {
                tracing::warn!("viewer: cannot start the voice call: {e:#}");
                v.update(|st| {
                    st.call = CallState::Idle;
                    st.notice = Some(format!("เปิดไมโครโฟนไม่สำเร็จ: {e}"));
                });
                reply = Some(json!({"t": "voice", "op": "hangup"}));
            }
        },
        "decline" => v.update(|st| {
            st.call = CallState::Idle;
            st.notice = Some(
                match m["reason"].as_str() {
                    Some("timeout") => "ปลายทางไม่รับสาย",
                    Some("unavailable") => "ปลายทางรับสายไม่ได้ (ไม่อนุญาตการโทร หรือไม่มีไมโครโฟน)",
                    _ => "ปลายทางปฏิเสธการโทร",
                }
                .into(),
            );
        }),
        "hangup" => {
            *call = None;
            v.update(|st| {
                if st.call != CallState::Idle {
                    st.notice = Some("ปลายทางวางสายแล้ว".into());
                }
                st.call = CallState::Idle;
            });
        }
        "mute" => v.update(|st| {
            if let CallState::Active { remote_muted, .. } = &mut st.call {
                *remote_muted = m["muted"].as_bool().unwrap_or(false);
            }
        }),
        _ => {}
    }
    reply
}

/// Handles a control message from the agent; may return an immediate reply.
fn control(v: &Viewer, m: &Value) -> Option<Value> {
    let t = m["t"].as_str().unwrap_or("");
    let mut reply = None;
    v.update(|st| match t {
        "auth_required" => {
            if v.switch_secret.is_some() && m["switch"] != true {
                // An agent that does not know switch sides asks for its password instead.
                st.error = Some("เครื่องของอีกฝ่ายใช้ Aegiron รุ่นที่ไม่รองรับการสลับฝั่ง".into());
                reply = Some(json!({"t": "bye"}));
            } else if m["password"].as_bool().unwrap_or(true) {
                st.phase = Phase::Password;
            } else {
                st.phase = Phase::Approval;
                reply = Some(json!({"t": "auth", "password": null, "codecs": codecs(st.want_codec), "features": v.features, "audio": audio_codecs(), "switch": v.switch_secret}));
            }
        }
        "auth_pending" => st.phase = Phase::Approval,
        "auth_result" => match (m["ok"].as_bool().unwrap_or(false), m["reason"].as_str().unwrap_or("")) {
            (true, _) => {
                st.phase = Phase::Connected;
                st.error = None;
            }
            (false, "wrong_password") => {
                let left = m["attempts_left"].as_u64().unwrap_or(0);
                st.attempts_left = Some(left);
                st.error = Some(if left > 0 { format!("รหัสผ่านไม่ถูกต้อง (เหลือ {left} ครั้ง)") } else { "รหัสผ่านไม่ถูกต้อง".into() });
                if left > 0 {
                    st.phase = Phase::Password;
                }
            }
            (false, "locked") => st.error = Some(format!("ล็อกชั่วคราว ลองใหม่ใน {} วินาที", m["retry_after"].as_u64().unwrap_or(60))),
            (false, "denied") => st.error = Some("ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ".into()),
            (false, "recording_failed") => st.error = Some("องค์กรกำหนดให้บันทึกเซสชันนี้ แต่เครื่องปลายทางเริ่มบันทึกไม่ได้".into()),
            _ => {}
        },
        "info" => {
            st.info = serde_json::from_value(m.clone()).ok();
            st.codec = m["codec"].as_str().unwrap_or("jpeg").to_owned();
            // Older agents do not say; they always used OpenH264 for H.264.
            st.encoder = m["encoder"].as_str().unwrap_or(if st.codec == "h264" { "openh264" } else { "jpeg" }).to_owned();
            tracing::info!("viewer: codec {} via {}, audio {}", st.codec, st.encoder, m["audio"].as_bool().unwrap_or(false));
        }
        // The agent switched encoders mid-session (e.g. hardware -> OpenH264 -> JPEG).
        "encoder" => {
            st.encoder = m["encoder"].as_str().unwrap_or("").to_owned();
            if let Some(c) = m["codec"].as_str() {
                st.codec = c.to_owned();
            }
            tracing::info!("viewer: encoder now {}", st.encoder);
        }
        "cursor" => {
            let (x, y) = (m["x"].as_f64().unwrap_or(-1.0) as f32, m["y"].as_f64().unwrap_or(-1.0) as f32);
            st.cursor = m["visible"].as_bool().unwrap_or(false).then_some((x, y));
        }
        "clip" => st.remote_clipboard = m["s"].as_str().map(str::to_owned),
        "chat" => {
            let text: String = m["text"].as_str().unwrap_or("").chars().take(proto::CHAT_MAX).collect();
            if !text.trim().is_empty() {
                st.chat.push(ChatLine { mine: false, text, at: std::time::SystemTime::now() });
                st.chat_unread += 1;
            }
        }
        "switch_result" if m["ok"] != true => {
            st.switch_pending = false;
            st.notice = Some(
                match m["reason"].as_str() {
                    Some("declined") => "ปลายทางปฏิเสธการสลับฝั่ง".to_string(),
                    Some("unavailable") => "ปลายทางสลับฝั่งไม่ได้".to_string(),
                    Some(r) => format!("สลับฝั่งไม่สำเร็จ: {r}"),
                    None => "สลับฝั่งไม่สำเร็จ".to_string(),
                },
            );
        }
        "clip_files" => {
            let entries: Vec<ClipEntry> = serde_json::from_value(m["files"].clone()).unwrap_or_default();
            if entries.is_empty() || !st.info.as_ref().is_some_and(Info::clip_files) {
                return;
            }
            let small = entries.len() <= AUTO_FETCH_FILES
                && entries.iter().all(|e| !e.dir)
                && entries.iter().map(|e| e.size).sum::<u64>() <= AUTO_FETCH_BYTES;
            if small {
                st.clip_offer = None;
                let _ = v.tx.send(Cmd::FetchClip(entries, true));
            } else {
                st.clip_offer = Some(entries);
            }
        }
        "ls" => {
            st.browser = RemoteDir {
                path: m["path"].as_str().unwrap_or("").to_owned(),
                parent: m["parent"].as_str().map(str::to_owned),
                sep: m["sep"].as_str().unwrap_or("/").to_owned(),
                entries: serde_json::from_value(m["entries"].clone()).unwrap_or_default(),
                error: None,
                loading: false,
            };
        }
        "fs_ok" => {
            if !st.browser.path.is_empty() {
                st.browser.loading = true;
                reply = Some(json!({"t": "ls", "path": st.browser.path}));
            }
        }
        "fs_error" => {
            let msg = m["message"].as_str().unwrap_or("").to_owned();
            let id = m["id"].as_u64().map(|i| i as u32);
            match st.transfers.iter_mut().find(|t| Some(t.id) == id) {
                Some(t) => t.status = TransferStatus::Failed(msg),
                // A late answer for a file of a finished/cancelled transfer: not a panel error.
                None if matches!(m["op"].as_str(), Some("put" | "get" | "get_dir")) => {}
                None => {
                    st.browser.error = Some(msg);
                    st.browser.loading = false;
                }
            }
        }
        _ => {}
    });
    reply
}


/// What a paste in the remote window should do.
#[derive(Debug, PartialEq)]
pub enum Paste {
    /// No files on this clipboard: hand over the text and press paste remotely.
    Text,
    /// Files are being uploaded; paste is pressed remotely when they are there.
    Files,
    /// The same files are already on the remote clipboard: only press paste remotely.
    KeysOnly,
}

const CLOSED: &str = "การเชื่อมต่อถูกปิด";
const CANCELLED: &str = "ยกเลิกแล้ว";
const NO_REPLY: &str = "เครื่องปลายทางไม่ตอบ";
/// Progress is pushed to the window at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A transfer task's handle on the session: the outgoing queue plus reply routing by id.
struct Link {
    v: Arc<Viewer>,
    out: mpsc::Sender<Vec<u8>>,
}

impl Link {
    fn next_id(&self) -> u32 {
        self.v.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn route(&self, id: u32, tx: mpsc::UnboundedSender<Ev>) {
        self.v.routes.lock().unwrap().insert(id, tx);
    }

    fn unroute(&self, id: u32) {
        self.v.routes.lock().unwrap().remove(&id);
    }

    fn cancelled(&self, id: u32) -> bool {
        self.v.cancels.lock().unwrap().contains(&id)
    }

    async fn send(&self, m: Value) -> Result<()> {
        self.out.send(proto::json(&m)).await.map_err(|_| anyhow!(CLOSED))
    }

    /// Sends a request carrying `id` and returns its first reply (`fs_error` becomes an error).
    async fn request(&self, id: u32, m: Value) -> Result<Value> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.route(id, tx);
        let res = async {
            self.send(m).await?;
            loop {
                match tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
                    Err(_) => bail!(NO_REPLY),
                    Ok(None) => bail!(CLOSED),
                    Ok(Some(Ev::Json(r))) if r["t"] == "fs_error" => bail!("{}", r["message"].as_str().unwrap_or("error")),
                    Ok(Some(Ev::Json(r))) => return Ok(r),
                    Ok(Some(Ev::Chunk(_))) => {}
                }
            }
        }
        .await;
        self.unroute(id);
        res
    }

    fn add(&self, t: Transfer) {
        self.v.update(|st| st.transfers.push(t));
    }

    fn update(&self, id: u32, f: impl FnOnce(&mut Transfer)) {
        self.v.update(|st| {
            if let Some(t) = st.transfers.iter_mut().find(|t| t.id == id) {
                f(t)
            }
        });
    }

    fn notice(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.v.update(|st| st.notice = Some(msg));
    }

    fn finish<T: std::fmt::Display>(&self, id: u32, res: Result<T>) -> Option<T> {
        self.v.cancels.lock().unwrap().remove(&id);
        match res {
            Ok(v) => {
                let path = v.to_string();
                self.update(id, |t| {
                    t.status = TransferStatus::Done(path);
                    t.current.clear();
                });
                Some(v)
            }
            Err(e) => {
                tracing::warn!("transfer {id}: {e:#}");
                self.update(id, |t| t.status = TransferStatus::Failed(format!("{e:#}")));
                None
            }
        }
    }

    // ---- upload (viewer → agent) ----

    /// Uploads a local file or folder into `remote_dir`; returns what was created remotely.
    async fn upload(&self, path: &Path, remote_dir: &str) -> Option<String> {
        let id = self.next_id();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        self.add(Transfer { id, name: name.clone(), up: true, ..Default::default() });
        let res = self.upload_inner(id, path, &name, remote_dir).await;
        self.finish(id, res)
    }

    async fn upload_inner(&self, id: u32, path: &Path, name: &str, remote_dir: &str) -> Result<String> {
        let meta = tokio::fs::metadata(path).await?;
        if !meta.is_dir() {
            self.update(id, |t| {
                t.files = 1;
                t.size = meta.len();
                t.current = name.to_owned();
            });
            let (tx, mut rx) = mpsc::unbounded_channel();
            self.route(id, tx);
            let res = async {
                self.send_file(id, id, path, json!({"t": "put", "id": id, "dir": remote_dir, "name": name})).await?;
                loop {
                    match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                        Err(_) => bail!(NO_REPLY),
                        Ok(None) => bail!(CLOSED),
                        Ok(Some(Ev::Json(m))) if m["t"] == "put_done" => return Ok(m["path"].as_str().unwrap_or("").to_owned()),
                        Ok(Some(Ev::Json(m))) if m["t"] == "fs_error" => bail!("{}", m["message"].as_str().unwrap_or("error")),
                        Ok(Some(_)) => {}
                    }
                }
            }
            .await;
            self.unroute(id);
            if res.is_ok() {
                self.update(id, |t| t.files_done = 1);
            }
            return res;
        }

        // A folder: create it (uniquely named) on the agent, then send its files one by one.
        let local_root = path.to_path_buf();
        let tree = tokio::task::spawn_blocking(move || files::walk(&local_root)).await??;
        let (count, total) = (tree.files.len() as u32, tree.size());
        self.update(id, |t| {
            t.folder = true;
            t.files = count;
            t.size = total;
        });
        let rid = self.next_id();
        let reply = self.request(rid, json!({"t": "mkdir", "id": rid, "dir": remote_dir, "name": name})).await?;
        let root = reply["path"].as_str().ok_or_else(|| anyhow!("bad mkdir reply"))?.to_owned();
        for d in tree.empty_dirs() {
            let rid = self.next_id();
            if let Err(e) = self.request(rid, json!({"t": "mkdir", "id": rid, "dir": root, "name": d})).await {
                tracing::warn!("upload: mkdir {d}: {e:#}");
            }
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pending = HashSet::new();
        let mut result = Ok(());
        for (rel, file, _) in &tree.files {
            if self.cancelled(id) {
                result = Err(anyhow!(CANCELLED));
                break;
            }
            let sub = self.next_id();
            self.route(sub, tx.clone());
            pending.insert(sub);
            self.update(id, |t| t.current = rel.clone());
            let file_name = rel.rsplit('/').next().unwrap_or(rel);
            let put = json!({"t": "put", "id": sub, "dir": root, "name": file_name, "rel": rel});
            if let Err(e) = self.send_file(id, sub, file, put).await {
                let fatal = self.cancelled(id) || e.to_string() == CLOSED;
                pending.remove(&sub);
                self.unroute(sub);
                if fatal {
                    result = Err(e);
                    break;
                }
                tracing::warn!("upload {rel}: {e:#}");
                self.update(id, |t| t.failed += 1);
            }
            while let Ok(ev) = rx.try_recv() {
                self.upload_reply(id, ev, &mut pending);
            }
        }
        while result.is_ok() && !pending.is_empty() {
            match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                Ok(Some(ev)) => self.upload_reply(id, ev, &mut pending),
                _ => result = Err(anyhow!(NO_REPLY)),
            }
        }
        for sub in pending {
            self.unroute(sub);
        }
        result.map(|()| root)
    }

    /// Counts the agent's answer for one file of a folder upload.
    fn upload_reply(&self, id: u32, ev: Ev, pending: &mut HashSet<u32>) {
        let Ev::Json(m) = ev else { return };
        let sub = m["id"].as_u64().unwrap_or(0) as u32;
        let ok = match m["t"].as_str() {
            Some("put_done") => true,
            Some("fs_error") => false,
            _ => return,
        };
        if pending.remove(&sub) {
            self.unroute(sub);
            if !ok {
                tracing::warn!("upload: {}", m["message"]);
            }
            self.update(id, |t| if ok { t.files_done += 1 } else { t.failed += 1 });
        }
    }

    /// Streams one local file as `put` (+ chunks) + `put_end` under transfer id `sub`;
    /// progress goes to transfer `id`.
    async fn send_file(&self, id: u32, sub: u32, path: &Path, mut put: Value) -> Result<()> {
        let f = tokio::fs::File::open(path).await?;
        let size = f.metadata().await?.len();
        put["size"] = json!(size);
        self.update(id, |t| {
            t.cur_size = size;
            t.cur_sent = 0;
        });
        self.send(put).await?;
        let mut f = f.take(size);
        let mut buf = vec![0u8; 64 * 1024];
        let (mut sent, mut shown, mut at) = (0u64, 0u64, Instant::now());
        loop {
            if self.cancelled(id) {
                let _ = self.send(json!({"t": "cancel", "id": sub})).await;
                bail!(CANCELLED);
            }
            let n = match f.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    let _ = self.send(json!({"t": "cancel", "id": sub})).await;
                    return Err(e.into());
                }
            };
            if n == 0 {
                break;
            }
            self.out.send(proto::file_chunk(sub, &buf[..n])).await.map_err(|_| anyhow!(CLOSED))?;
            sent += n as u64;
            if at.elapsed() >= PROGRESS_EVERY {
                let delta = sent - shown;
                self.update(id, |t| {
                    t.cur_sent = sent;
                    t.sent += delta;
                });
                (shown, at) = (sent, Instant::now());
            }
        }
        let delta = sent - shown;
        self.update(id, |t| {
            t.cur_sent = sent;
            t.sent += delta;
        });
        if sent != size {
            let _ = self.send(json!({"t": "cancel", "id": sub})).await;
            bail!("ไฟล์ถูกแก้ไขระหว่างส่ง");
        }
        self.send(json!({"t": "put_end", "id": sub})).await
    }

    // ---- download (agent → viewer) ----

    /// Downloads a remote file or folder into `dest`; returns the local path created.
    async fn download(&self, remote: &str, dir: bool, dest: &Path) -> Option<PathBuf> {
        let id = self.next_id();
        let name = remote.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(remote).to_owned();
        self.add(Transfer { id, name, folder: dir, ..Default::default() });
        let (tx, rx) = mpsc::unbounded_channel();
        self.route(id, tx);
        let mut partial = None;
        let res = self.download_inner(id, remote, dir, dest, rx, &mut partial).await;
        self.unroute(id);
        if res.is_err() {
            if self.cancelled(id) {
                let _ = self.send(json!({"t": "cancel", "id": id})).await;
            }
            if let Some(p) = partial {
                let _ = tokio::fs::remove_file(p).await;
            }
        }
        self.finish(id, res.map(|p| p.display().to_string())).map(PathBuf::from)
    }

    async fn download_inner(
        &self,
        id: u32,
        remote: &str,
        dir: bool,
        dest: &Path,
        mut rx: mpsc::UnboundedReceiver<Ev>,
        partial: &mut Option<PathBuf>,
    ) -> Result<PathBuf> {
        use tokio::io::AsyncWriteExt;
        let op = if dir { "get_dir" } else { "get" };
        self.send(json!({"t": op, "id": id, "path": remote})).await?;
        let create = |p: PathBuf| async move { tokio::fs::OpenOptions::new().write(true).create_new(true).open(p).await };
        let mut root: Option<PathBuf> = None;
        let mut cur: Option<tokio::fs::File> = None;
        let (mut cur_sent, mut shown, mut at, mut idle) = (0u64, 0u64, Instant::now(), Duration::ZERO);
        loop {
            let ev = match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Err(_) => {
                    if self.cancelled(id) {
                        bail!(CANCELLED);
                    }
                    idle += Duration::from_millis(500);
                    if idle > Duration::from_secs(120) {
                        bail!(NO_REPLY);
                    }
                    continue;
                }
                Ok(None) => bail!(CLOSED),
                Ok(Some(ev)) => ev,
            };
            idle = Duration::ZERO;
            if self.cancelled(id) {
                bail!(CANCELLED);
            }
            let m = match ev {
                Ev::Chunk(data) => {
                    if let Some(f) = cur.as_mut() {
                        f.write_all(&data).await?;
                    }
                    cur_sent += data.len() as u64;
                    if at.elapsed() >= PROGRESS_EVERY {
                        let delta = cur_sent - shown;
                        self.update(id, |t| {
                            t.cur_sent = cur_sent;
                            t.sent += delta;
                        });
                        (shown, at) = (cur_sent, Instant::now());
                    }
                    continue;
                }
                Ev::Json(m) => m,
            };
            let t = m["t"].as_str().unwrap_or("");
            // These end the file in progress (if any).
            if matches!(t, "dir_file" | "dir_skip" | "get_end") {
                let delta = cur_sent - shown;
                let done = match cur.take() {
                    Some(mut f) => {
                        f.flush().await?;
                        *partial = None;
                        true
                    }
                    None => false,
                };
                self.update(id, |t| {
                    t.cur_sent = cur_sent;
                    t.sent += delta;
                    if done {
                        t.files_done += 1;
                    }
                });
                (cur_sent, shown) = (0, 0);
            }
            match t {
                "get_start" => {
                    let name = files::sanitize(m["name"].as_str().unwrap_or(""));
                    let size = m["size"].as_u64().unwrap_or(0);
                    let p = unique_path(dest, &name);
                    cur = Some(create(p.clone()).await?);
                    *partial = Some(p.clone());
                    root = Some(p);
                    self.update(id, |t| {
                        t.files = 1;
                        t.size = size;
                        t.cur_size = size;
                        t.current = name;
                    });
                }
                "dir_start" => {
                    // The folder name and every relative path come from the remote side: untrusted.
                    let name = rel_components(m["name"].as_str().unwrap_or(""))
                        .ok()
                        .filter(|c| c.len() == 1)
                        .map(|mut c| c.remove(0))
                        .unwrap_or_else(|| "folder".into());
                    let p = unique_path(dest, &name);
                    std::fs::create_dir(&p)?;
                    let mut bad = 0;
                    for d in m["dirs"].as_array().into_iter().flatten() {
                        if rel_components(d.as_str().unwrap_or("")).and_then(|c| ensure_dirs(&p, &c)).is_err() {
                            bad += 1;
                        }
                    }
                    root = Some(p);
                    let (files, size) = (m["files"].as_u64().unwrap_or(0) as u32, m["size"].as_u64().unwrap_or(0));
                    self.update(id, |t| {
                        t.files = files;
                        t.size = size;
                        t.failed += bad;
                    });
                }
                "dir_file" => {
                    let Some(root) = root.as_ref() else { bail!("unexpected dir_file") };
                    let rel = m["rel"].as_str().unwrap_or("");
                    let size = m["size"].as_u64().unwrap_or(0);
                    let target = rel_components(rel).and_then(|c| {
                        let parent = ensure_dirs(root, &c[..c.len() - 1])?;
                        Ok(unique_path(&parent, &c[c.len() - 1]))
                    });
                    match target {
                        Ok(p) => {
                            cur = Some(create(p.clone()).await?);
                            *partial = Some(p);
                        }
                        Err(e) => {
                            // Skip this file's chunks; the rest of the folder still comes.
                            tracing::warn!("download: {e}");
                            self.update(id, |t| t.failed += 1);
                        }
                    }
                    self.update(id, |t| {
                        t.current = rel.to_owned();
                        t.cur_size = size;
                        t.cur_sent = 0;
                    });
                }
                "dir_skip" => {
                    tracing::warn!("download: remote skipped {}: {}", m["rel"], m["message"]);
                    self.update(id, |t| t.failed += 1);
                }
                "get_end" => return root.ok_or_else(|| anyhow!("empty download")),
                "fs_error" => bail!("{}", m["message"].as_str().unwrap_or("error")),
                _ => {}
            }
        }
    }

    // ---- clipboard files ----

    /// Paste with files on this machine's clipboard: upload them to the agent's
    /// ~/Downloads/Aegiron, put them on its clipboard, then press paste remotely.
    async fn paste(&self, paths: Vec<PathBuf>, keys: Vec<Value>) {
        let started = Instant::now();
        let mut remote = Vec::new();
        for p in &paths {
            if let Some(r) = self.upload(p, "").await {
                remote.push(r);
            }
        }
        if remote.is_empty() {
            self.v.last_clip_files.lock().unwrap().clear();
            self.notice("ส่งไฟล์จากคลิปบอร์ดไม่สำเร็จ");
            return;
        }
        let rid = self.next_id();
        match self.request(rid, json!({"t": "clip_files", "id": rid, "paths": remote})).await {
            Ok(_) if started.elapsed() <= PASTE_DEADLINE => {
                for k in keys {
                    let _ = self.send(k).await;
                }
                self.notice(format!("วางไฟล์ {} รายการแล้ว (สำเนาอยู่ที่ Downloads/Aegiron ของเครื่องปลายทาง)", remote.len()));
            }
            // Took long enough that the user may be doing something else: don't press paste.
            Ok(_) => self.notice("ส่งไฟล์เสร็จแล้ว กดวางอีกครั้งเพื่อวางบนเครื่องปลายทาง"),
            Err(e) => {
                self.v.last_clip_files.lock().unwrap().clear();
                self.notice(format!("ไฟล์อยู่ที่ Downloads/Aegiron ของเครื่องปลายทาง แต่ใส่คลิปบอร์ดไม่สำเร็จ: {e}"));
            }
        }
    }

    /// Files copied on the remote machine: download them, then put them on this clipboard.
    async fn fetch_clip(&self, entries: Vec<ClipEntry>, temp: bool) {
        let dest = if temp {
            let d = self.v.clip_tmp.join(self.next_id().to_string());
            if let Err(e) = std::fs::create_dir_all(&d) {
                self.notice(format!("รับไฟล์จากคลิปบอร์ดไม่สำเร็จ: {e}"));
                return;
            }
            d
        } else {
            files::default_upload_dir()
        };
        let mut local = Vec::new();
        for e in &entries {
            if let Some(p) = self.download(&e.path, e.dir, &dest).await {
                local.push(p);
            }
        }
        if local.is_empty() {
            self.notice("รับไฟล์จากคลิปบอร์ดของเครื่องปลายทางไม่สำเร็จ");
            return;
        }
        let list = local.clone();
        let set = tokio::task::spawn_blocking(move || arboard::Clipboard::new().and_then(|mut c| c.set().file_list(&list))).await;
        match set {
            Ok(Ok(())) => {
                // Pasting these back into the remote window must not upload them again.
                *self.v.last_clip_files.lock().unwrap() = clip_sig(&local);
                self.notice(if temp {
                    format!("คัดลอก {} รายการจากเครื่องปลายทางแล้ว วางในเครื่องนี้ได้เลย", local.len())
                } else {
                    format!("บันทึก {} รายการไว้ที่ Downloads/Aegiron และคัดลอกไว้ในคลิปบอร์ดแล้ว", local.len())
                });
            }
            _ => self.notice("บันทึกไฟล์ไว้ที่ Downloads/Aegiron แล้ว แต่ใส่คลิปบอร์ดไม่สำเร็จ"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_come_from_info() {
        // An agent before folder transfer sends no `features`: only single files are offered.
        let old: Info = serde_json::from_value(json!({"hostname": "pc", "os": "windows", "files": true, "clipboard": true})).unwrap();
        assert!(!old.fs2() && !old.clip_files());
        let new: Info = serde_json::from_value(json!({"files": true, "clipboard": true, "features": ["fs2", "clipfiles"]})).unwrap();
        assert!(new.fs2() && new.clip_files());
        // Clipboard files need both permissions.
        let no_clip: Info = serde_json::from_value(json!({"files": true, "clipboard": false, "features": ["fs2", "clipfiles"]})).unwrap();
        assert!(no_clip.fs2() && !no_clip.clip_files());
        let no_files: Info = serde_json::from_value(json!({"files": false, "clipboard": true, "features": ["fs2", "clipfiles"]})).unwrap();
        assert!(!no_files.fs2() && !no_files.clip_files());
    }
}
