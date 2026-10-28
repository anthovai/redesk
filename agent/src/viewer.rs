//! Native viewer: controls another machine from the desktop app (one window per connection,
//! like RustDesk). Speaks the same end-to-end protocol as the browser viewer.

use std::path::PathBuf;
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
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::crypto::{Channel, ViewerKeys};
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferStatus {
    Active,
    Done(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub id: u32,
    pub name: String,
    pub size: u64,
    pub sent: u64,
    pub status: TransferStatus,
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
    pub transfers: Vec<Transfer>,
}

enum Cmd {
    Json(Value),
    Upload(PathBuf),
    Close,
}

pub struct Viewer {
    pub state: Mutex<State>,
    tx: mpsc::UnboundedSender<Cmd>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

/// What a connection needs from the signed-in app.
pub struct Target {
    pub server: String,
    pub token: String,
    pub insecure: bool,
    pub device_id: String,
    pub name: String,
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
                transfers: Vec::new(),
            }),
            tx,
            repaint,
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
                transfers: vec![Transfer { id: 1, name: "รายงานประจำเดือน.xlsx".into(), size: 100, sent: 64, status: TransferStatus::Active }],
            }),
            tx,
            repaint: Arc::new(|| {}),
        })
    }

    pub fn send(&self, v: Value) {
        let _ = self.tx.send(Cmd::Json(v));
    }

    pub fn submit_password(&self, pw: &str) {
        self.state.lock().unwrap().error = None;
        self.send(json!({"t": "auth", "password": pw}));
    }

    pub fn upload(&self, path: PathBuf) {
        let _ = self.tx.send(Cmd::Upload(path));
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

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_text(rx: &mut futures_util::stream::SplitStream<Ws>, what: &str, secs: u64) -> Result<String> {
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

async fn run(v: &Arc<Viewer>, t: Target, mut cmds: mpsc::UnboundedReceiver<Cmd>) -> Result<()> {
    // 1. Ask the server to wake the agent (blocking HTTPS call off the async threads).
    let (server, token, id) = (t.server.clone(), t.token.clone(), t.device_id.clone());
    let ticket = tokio::task::spawn_blocking(move || account::start_session(&server, &token, &id))
        .await?
        .map_err(|e| anyhow!(e))?;
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
    req.headers_mut().insert("Authorization", format!("Bearer {}", t.token).parse()?);
    let (ws, _) = connect_async(req).await.context("เชื่อมต่อ relay ไม่สำเร็จ")?;
    let (mut tx, mut rx) = ws.split();

    let paired = next_text(&mut rx, "เครื่องปลายทางตอบรับ", 60).await?;
    if !paired.contains("\"paired\"") {
        bail!("unexpected relay message");
    }
    v.update(|st| st.phase = Phase::Handshake);

    // 3. End-to-end key exchange, authenticated by the device's registered key.
    let keys = ViewerKeys::new();
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

    // File uploads read on their own task and feed this bounded queue (back-pressure).
    let (up_tx, mut up_rx) = mpsc::channel::<Vec<u8>>(8);
    let mut next_transfer = 1u32;
    let (mut frames, mut bytes, mut stats_at) = (0u32, 0usize, Instant::now());

    loop {
        tokio::select! {
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
                    Some(&proto::MSG_JSON) => {
                        let m: Value = serde_json::from_slice(&plain[1..])?;
                        if let Some(reply) = control(v, &m) {
                            tx.send(Message::Binary(ch.seal(&proto::json(&reply))?.into())).await?;
                        }
                    }
                    _ => {}
                }
                if stats_at.elapsed() >= Duration::from_secs(1) {
                    let (f, kb) = (frames, (bytes * 8 / 1000) as u32);
                    v.update(|st| { st.fps = f; st.kbps = kb; });
                    (frames, bytes, stats_at) = (0, 0, Instant::now());
                }
            }
            c = cmds.recv() => match c {
                Some(Cmd::Json(j)) => tx.send(Message::Binary(ch.seal(&proto::json(&j))?.into())).await?,
                Some(Cmd::Upload(path)) => {
                    let id = next_transfer;
                    next_transfer += 1;
                    start_upload(v.clone(), id, path, up_tx.clone());
                }
                Some(Cmd::Close) | None => {
                    let _ = tx.send(Message::Binary(ch.seal(&proto::json(&json!({"t": "bye"})))?.into())).await;
                    let _ = tx.close().await;
                    return Ok(());
                }
            },
            Some(plain) = up_rx.recv() => tx.send(Message::Binary(ch.seal(&plain)?.into())).await?,
        }
    }
}

/// Handles a control message from the agent; may return an immediate reply.
fn control(v: &Viewer, m: &Value) -> Option<Value> {
    let t = m["t"].as_str().unwrap_or("");
    let mut reply = None;
    v.update(|st| match t {
        "auth_required" => {
            if m["password"].as_bool().unwrap_or(true) {
                st.phase = Phase::Password;
            } else {
                st.phase = Phase::Approval;
                reply = Some(json!({"t": "auth", "password": null}));
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
            _ => {}
        },
        "info" => st.info = serde_json::from_value(m.clone()).ok(),
        "clip" => st.remote_clipboard = m["s"].as_str().map(str::to_owned),
        "put_done" => set_transfer(st, m, TransferStatus::Done(m["path"].as_str().unwrap_or("").to_owned())),
        "fs_error" => set_transfer(st, m, TransferStatus::Failed(m["message"].as_str().unwrap_or("").to_owned())),
        _ => {}
    });
    reply
}

fn set_transfer(st: &mut State, m: &Value, status: TransferStatus) {
    let id = m["id"].as_u64().unwrap_or(0) as u32;
    if let Some(t) = st.transfers.iter_mut().find(|t| t.id == id) {
        t.status = status;
    }
}

fn start_upload(v: Arc<Viewer>, id: u32, path: PathBuf, out: mpsc::Sender<Vec<u8>>) {
    tokio::spawn(async move {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let res: Result<()> = async {
            let mut f = tokio::fs::File::open(&path).await?;
            let size = f.metadata().await?.len();
            v.update(|st| st.transfers.push(Transfer { id, name: name.clone(), size, sent: 0, status: TransferStatus::Active }));
            // Empty dir = the agent's ~/Downloads/ReDesk.
            out.send(proto::json(&json!({"t": "put", "id": id, "dir": "", "name": name, "size": size}))).await?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut sent = 0u64;
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                out.send(proto::file_chunk(id, &buf[..n])).await?;
                sent += n as u64;
                v.update(|st| {
                    if let Some(t) = st.transfers.iter_mut().find(|t| t.id == id) {
                        t.sent = sent;
                    }
                });
            }
            out.send(proto::json(&json!({"t": "put_end", "id": id}))).await?;
            Ok(())
        }
        .await;
        if let Err(e) = res {
            v.update(|st| match st.transfers.iter_mut().find(|t| t.id == id) {
                Some(t) => t.status = TransferStatus::Failed(e.to_string()),
                None => st.transfers.push(Transfer { id, name, size: 0, sent: 0, status: TransferStatus::Failed(e.to_string()) }),
            });
        }
    });
}
