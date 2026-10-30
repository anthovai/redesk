//! Viewer side of tunnel and terminal sessions: asks the server for the session, races the
//! direct paths against the relay, runs the end-to-end handshake and authenticates, exactly as
//! the screen viewer does (viewer.rs), then hands over the encrypted channel.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::crypto::ViewerKeys;
use crate::direct::{Keep, Path, WsRx};
use crate::remote::{self, Conn, SessionKind};
use crate::{account, fingerprint};

pub const CLOSED: &str = "การเชื่อมต่อถูกปิด";

/// What a connection needs from the signed-in app or CLI.
#[derive(Clone)]
pub struct Target {
    pub server: String,
    pub token: String,
    pub insecure: bool,
    pub device_id: String,
    /// Try direct/P2P paths before falling back to the relay.
    pub direct: bool,
}

impl Target {
    /// From this device's config and signed-in account.
    pub fn from_config(c: &crate::config::Config, device_id: &str) -> Result<Self> {
        let acc = c.account.clone().context("เข้าสู่ระบบก่อน (aegiron-agent login --email ...)")?;
        Ok(Target {
            server: c.server_url.clone(),
            token: acc.token,
            insecure: c.allow_insecure_transport,
            // "123 456 789" or a custom ID in any case (see valid_device_id); the server checks it.
            device_id: crate::local::normalize_id(device_id).unwrap_or_else(|| device_id.replace(' ', "")),
            direct: c.direct_enabled,
        })
    }
}

/// A session that passed the handshake and waits for `auth`.
pub struct Pending {
    conn: Conn,
    kind: SessionKind,
    pub need_password: bool,
    pub need_click: bool,
    pub path: Path,
    /// Device key fingerprint (compare with `aegiron-agent info` on that machine).
    pub fingerprint: String,
    /// Alias or host name of the device, from the server.
    pub name: String,
    keep: Keep,
}

pub enum Auth {
    /// Accepted; the agent's `info`.
    Ok(Value),
    /// Wrong password; this many attempts left in this session.
    Retry { left: u32 },
}

/// An authenticated tunnel or terminal session.
pub struct Session {
    pub conn: Conn,
    pub path: Path,
    _keep: Keep,
}

async fn next_text(rx: &mut WsRx, what: &str, secs: u64) -> Result<String> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(secs), rx.next())
            .await
            .map_err(|_| anyhow!("หมดเวลารอ{what}"))?
            .ok_or(anyhow!(CLOSED))??;
        match m {
            Message::Text(t) => return Ok(t.as_str().to_owned()),
            Message::Close(f) => {
                let why = crate::viewer::explain_close(f.as_ref().map(|f| f.reason.as_str()).unwrap_or(""));
                bail!("{}", if why.is_empty() { CLOSED.to_string() } else { why })
            }
            _ => {}
        }
    }
}

/// Opens a tunnel or terminal session up to the password/approval step.
pub async fn connect(t: &Target, kind: &SessionKind) -> Result<Pending> {
    let (server, token, id, direct) = (t.server.clone(), t.token.clone(), t.device_id.clone(), t.direct);
    let (k, target) = (kind.name(), kind.target());
    let ticket = tokio::task::spawn_blocking(move || account::start_session_of(&server, &token, &id, direct, k, target.as_deref()))
        .await?
        .map_err(|e| anyhow!(e))?;
    // Servers before tunnels and terminals ignore the type and would start a screen session.
    if ticket.ticket.kind.as_deref().unwrap_or("screen") != kind.name() {
        bail!("เซิร์ฟเวอร์ยังไม่รองรับเซสชันประเภทนี้ (ต้องอัปเดตเซิร์ฟเวอร์ Aegiron)");
    }
    let device_key: [u8; 32] = B64.decode(&ticket.device.public_key)?.try_into().map_err(|_| anyhow!("bad device key"))?;
    let name = [&ticket.device.alias, &ticket.device.hostname].into_iter().find(|s| !s.is_empty()).cloned().unwrap_or(t.device_id.clone());

    let url = crate::config::ws_url(&t.server, t.insecure, &format!("{}?sid={}&token={}", ticket.ticket.relay_path, ticket.ticket.sid, ticket.ticket.token))?;
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", t.token).parse()?);
    let relay = async move {
        let (ws, _) = crate::net::connect_ws(req).await.context("เชื่อมต่อ relay ไม่สำเร็จ")?;
        let mut l = crate::direct::link(ws, Path::Relay, Default::default());
        let paired = next_text(&mut l.rx, "เครื่องปลายทางตอบรับ", 60).await?;
        if !paired.contains("\"paired\"") {
            bail!("unexpected relay message");
        }
        Ok(l)
    };
    let keys = ViewerKeys::new();
    let direct = ticket.ticket.direct.clone().filter(|_| t.direct).map(|d| crate::direct::ViewerDirect {
        server_url: t.server.clone(),
        sid: ticket.ticket.sid.clone(),
        ticket: d,
        epk: B64.encode(keys.epk),
        only: None,
    });
    let crate::direct::Link { mut tx, mut rx, path, keep } = crate::direct::viewer_race(direct, relay).await?;
    tracing::info!("{} session to {}: connected via {}", kind.name(), t.device_id, path.as_str());

    tx.send(Message::Text(json!({"t": "hello", "v": 1, "epk": B64.encode(keys.epk)}).to_string().into())).await?;
    #[derive(Deserialize)]
    struct Hello {
        epk: String,
        sig: String,
    }
    let hello: Hello = serde_json::from_str(&next_text(&mut rx, "การแลกเปลี่ยนกุญแจ", 30).await?)?;
    let agent_epk: [u8; 32] = B64.decode(hello.epk)?.try_into().map_err(|_| anyhow!("bad agent key"))?;
    let sig: [u8; 64] = B64.decode(hello.sig)?.try_into().map_err(|_| anyhow!("bad signature"))?;
    let ch = keys.finish(&ticket.ticket.sid, agent_epk, &sig, &device_key)?;
    let mut conn = Conn { tx, rx, ch };

    loop {
        let m = next_json(&mut conn, kind).await?;
        match m["t"].as_str().unwrap_or("") {
            "auth_required" => {
                return Ok(Pending {
                    conn,
                    kind: kind.clone(),
                    need_password: m["password"].as_bool().unwrap_or(true),
                    need_click: m["click"].as_bool().unwrap_or(false),
                    path,
                    fingerprint: fingerprint(&device_key),
                    name,
                    keep,
                });
            }
            "auth_result" => bail!("{}", refusal(&m, kind)),
            _ => {}
        }
    }
}

fn refusal(m: &Value, kind: &SessionKind) -> String {
    remote::explain_refusal(m["reason"].as_str().unwrap_or(""), kind)
}

async fn next_json(conn: &mut Conn, kind: &SessionKind) -> Result<Value> {
    loop {
        let Some(plain) = conn.recv().await? else { bail!("{CLOSED}") };
        if let Some(m) = remote::json_of(&plain) {
            if m["t"] == "auth_result" && m["ok"] == false && m["reason"] != "wrong_password" {
                bail!("{}", refusal(&m, kind));
            }
            return Ok(m);
        }
    }
}

impl Pending {
    /// Sends the password (None when the device only asks for approval) and waits for the
    /// answer. `waiting` is called when the person at the other machine has to accept.
    pub async fn auth(&mut self, password: Option<&str>, waiting: impl Fn()) -> Result<Auth> {
        if self.need_password && password.is_none_or(str::is_empty) {
            bail!("เครื่องปลายทางต้องใช้รหัสผ่าน");
        }
        let pw = if self.need_password { password } else { None };
        let msg = json!({
            "t": "auth", "password": pw, "codecs": [], "features": [], "audio": [],
            "session": self.kind.name(), "target": self.kind.target(),
        });
        self.conn.send_json(msg).await?;
        loop {
            let m = next_json(&mut self.conn, &self.kind).await?;
            match m["t"].as_str().unwrap_or("") {
                "auth_pending" => waiting(),
                "auth_result" if m["ok"] == true => break,
                "auth_result" => {
                    let left = m["attempts_left"].as_u64().unwrap_or(0) as u32;
                    if left == 0 {
                        bail!("ใส่รหัสผ่านผิดหลายครั้งเกินไป");
                    }
                    return Ok(Auth::Retry { left });
                }
                _ => {}
            }
        }
        loop {
            let m = next_json(&mut self.conn, &self.kind).await?;
            if m["t"] == "info" {
                if m["session"].as_str() != Some(self.kind.name()) {
                    self.conn.close().await;
                    bail!("เครื่องปลายทางยังไม่รองรับเซสชันประเภทนี้ (ต้องอัปเดต Aegiron บนเครื่องนั้น)");
                }
                return Ok(Auth::Ok(m));
            }
        }
    }

    pub fn into_conn(self) -> Session {
        Session { conn: self.conn, path: self.path, _keep: self.keep }
    }
}

/// Connects and authenticates in one go (CLI): the password is asked for by `password` only
/// when the device needs one.
pub async fn open(t: &Target, kind: &SessionKind, password: impl Fn() -> Result<String>, waiting: impl Fn()) -> Result<(Session, Value)> {
    let mut p = connect(t, kind).await?;
    let pw = if p.need_password { Some(password()?) } else { None };
    if !p.need_password {
        waiting();
    }
    match p.auth(pw.as_deref(), waiting).await? {
        Auth::Ok(info) => Ok((p.into_conn(), info)),
        Auth::Retry { left } => bail!("รหัสผ่านไม่ถูกต้อง (เหลือ {left} ครั้ง)"),
    }
}
