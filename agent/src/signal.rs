//! Signaling connection to the Aegiron server: registration, keepalive, session requests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::config::Config;
use crate::crypto;
use crate::status::{ServerState, Status, UiHooks};

/// Shared agent state used by the signaling loop and all sessions.
pub struct Agent {
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub identity: SigningKey,
    pub temp_password: Mutex<String>,
    /// (consecutive failed logins, locked until)
    pub failures: Mutex<(u32, Option<Instant>)>,
    /// Lines typed on the agent's terminal, used for accept/deny prompts.
    pub stdin_lines: tokio::sync::Mutex<mpsc::Receiver<String>>,
    /// Messages queued for the current signaling connection (if any).
    pub to_server: Mutex<Option<mpsc::Sender<String>>>,
    /// Connection and session state for the desktop UI.
    pub status: Mutex<Status>,
    pub ui: Mutex<Option<UiHooks>>,
}

impl Agent {
    /// `stdin_lines`: terminal input for y/N prompts (None for the desktop app).
    pub fn new(config: Config, config_path: PathBuf, stdin_lines: Option<mpsc::Receiver<String>>) -> Result<Arc<Self>> {
        let seed: [u8; 32] = B64.decode(&config.signing_key)?.try_into().map_err(|_| anyhow!("bad signing key"))?;
        let lines = stdin_lines.unwrap_or_else(|| mpsc::channel(1).1);
        Ok(Arc::new(Self {
            identity: SigningKey::from_bytes(&seed),
            config: Mutex::new(config),
            config_path,
            temp_password: Mutex::new(String::new()),
            failures: Mutex::new((0, None)),
            stdin_lines: tokio::sync::Mutex::new(lines),
            to_server: Mutex::new(None),
            status: Mutex::new(Status::default()),
            ui: Mutex::new(None),
        }))
    }

    pub fn rotate_temp_password(&self) {
        let pw = crypto::random_password(8);
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            println!("\n  Aegiron temporary password: {pw}\n");
        }
        if let Err(e) = write_private(&Config::temp_password_path(&self.config_path), &pw) {
            tracing::warn!("cannot save temporary password file: {e}");
        }
        *self.temp_password.lock().unwrap() = pw;
        self.changed();
    }

    /// Wakes the desktop UI (if any) so it redraws with fresh state.
    pub fn changed(&self) {
        if let Some(ui) = self.ui.lock().unwrap().as_ref() {
            (ui.repaint)();
        }
    }

    pub fn set_server_state(&self, s: ServerState) {
        self.status.lock().unwrap().server = s;
        self.changed();
    }

    /// Best-effort message to the server; dropped if signaling is down.
    pub fn send_server(&self, v: serde_json::Value) {
        if let Some(tx) = self.to_server.lock().unwrap().as_ref() {
            let _ = tx.try_send(v.to_string());
        }
    }

    /// Reports a session event for the server's audit log.
    pub fn report(&self, sid: &str, kind: &str, detail: serde_json::Value) {
        self.send_server(json!({"t": "event", "sid": sid, "kind": kind, "detail": detail}));
    }
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    std::io::Write::write_all(&mut opts.open(path)?, contents.as_bytes())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ServerMsg {
    #[serde(rename = "challenge")]
    Challenge { nonce: String },
    #[serde(rename = "registered")]
    Registered { sig: String },
    #[serde(rename = "error")]
    Error { code: String, message: String },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "session")]
    Session { sid: String, token: String, relay: String, viewer: Viewer },
    /// Wake-on-LAN request: send magic packets for these MACs on our LAN.
    #[serde(rename = "wol")]
    Wol {
        macs: Vec<String>,
        #[serde(default)]
        target: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Viewer {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub ip: String,
}

fn parse(msg: Message) -> Option<ServerMsg> {
    match msg {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

async fn next_msg<S>(rx: &mut S) -> Result<ServerMsg>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(15), rx.next()).await?.ok_or(anyhow!("connection closed"))??;
        if let Some(m) = parse(m) {
            return Ok(m);
        }
    }
}

/// Keeps the signaling connection alive forever, reconnecting with backoff.
pub async fn run_forever(agent: Arc<Agent>) {
    if agent.config.lock().unwrap().lan_discovery {
        tokio::spawn(crate::lan::listen(agent.clone(), crate::lan::DISCOVERY_PORT));
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        agent.set_server_state(ServerState::Connecting);
        match run_once(&agent).await {
            Ok(()) => {
                tracing::warn!("server closed the signaling connection");
                agent.set_server_state(ServerState::Error("server closed the connection".into()));
            }
            Err(e) => {
                tracing::warn!("signaling: {e:#}");
                agent.set_server_state(ServerState::Error(format!("{e}")));
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn run_once(agent: &Arc<Agent>) -> Result<()> {
    let (url, id, server_key, enroll) = {
        let c = agent.config.lock().unwrap();
        (format!("{}/ws/agent", c.ws_base()?), c.id.clone(), c.server_key.clone(), c.enroll_token.clone())
    };
    let server_key: [u8; 32] = B64.decode(&server_key)?.try_into().map_err(|_| anyhow!("bad server key"))?;
    let server_key = VerifyingKey::from_bytes(&server_key)?;

    let (ws, _) = connect_async(url.as_str()).await.with_context(|| format!("connect {url}"))?;
    let (mut tx, mut rx) = ws.split();

    let ServerMsg::Challenge { nonce: server_nonce } = next_msg(&mut rx).await? else { bail!("expected challenge") };
    let my_nonce = B64.encode(crypto::random_bytes::<32>());
    let sig = agent.identity.sign(format!("redesk-register-v1\n{id}\n{server_nonce}\n{my_nonce}").as_bytes());
    let net = tokio::task::spawn_blocking(crate::lan::local_facts).await.unwrap_or_default();
    let reg = json!({
        "t": "register",
        "id": id,
        "pk": B64.encode(agent.identity.verifying_key().as_bytes()),
        "nonce": my_nonce,
        "sig": B64.encode(sig.to_bytes()),
        "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "version": env!("CARGO_PKG_VERSION"),
        "enroll_token": enroll,
        "macs": net.macs,
        "lan": net.lan,
    });
    tx.send(Message::Text(reg.to_string().into())).await?;

    match next_msg(&mut rx).await? {
        ServerMsg::Registered { sig } => {
            let sig: [u8; 64] = B64.decode(sig)?.try_into().map_err(|_| anyhow!("bad server signature"))?;
            let msg = format!("redesk-server-v1\n{id}\n{my_nonce}\n{server_nonce}");
            server_key
                .verify(msg.as_bytes(), &Signature::from_bytes(&sig))
                .map_err(|_| anyhow!("Handshake failed: invalid public key from server (pinned key mismatch)"))?;
        }
        ServerMsg::Error { code, message } => {
            let mut c = agent.config.lock().unwrap();
            if code == "id_taken" && !c.registered {
                // Collision on a never-registered id: pick a new one and retry.
                c.id = crypto::random_device_id();
                c.save(&agent.config_path)?;
                bail!("id collision, switched to new id {}", c.id);
            }
            bail!("server rejected registration: {code}: {message}");
        }
        other => bail!("unexpected reply {other:?}"),
    }
    {
        let mut c = agent.config.lock().unwrap();
        if !c.registered {
            c.registered = true;
            c.enroll_token = None; // single use; don't keep the secret around
            c.save(&agent.config_path)?;
        }
    }
    tracing::info!("registered with server as {id}");
    agent.set_server_state(ServerState::Online);

    let mut ping = tokio::time::interval(Duration::from_secs(20));
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    *agent.to_server.lock().unwrap() = Some(out_tx);
    let _clear = ClearOnDrop(agent.clone());
    loop {
        tokio::select! {
            _ = ping.tick() => tx.send(Message::Text(r#"{"t":"ping"}"#.into())).await?,
            Some(m) = out_rx.recv() => tx.send(Message::Text(m.into())).await?,
            m = tokio::time::timeout(Duration::from_secs(70), rx.next()) => {
                let Some(m) = m.context("server silent for 70s")? else { return Ok(()) };
                match parse(m?) {
                    Some(ServerMsg::Session { sid, token, relay, viewer }) => {
                        let agent = agent.clone();
                        tokio::spawn(async move {
                            tracing::info!("session {sid} requested by {} from {}", viewer.email, viewer.ip);
                            if let Err(e) = crate::session::run(agent.clone(), &sid, &token, &relay, viewer).await {
                                tracing::warn!("session {sid} ended: {e:#}");
                                agent.send_server(json!({"t": "session_reject", "sid": sid, "reason": format!("{e}")}));
                            }
                        });
                    }
                    Some(ServerMsg::Wol { macs, target }) => {
                        tokio::spawn(crate::lan::handle_wol(agent.clone(), macs, target));
                    }
                    Some(ServerMsg::Pong) | None => {}
                    Some(other) => tracing::debug!("ignoring {other:?}"),
                }
            }
        }
    }
}

struct ClearOnDrop(Arc<Agent>);
impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        *self.0.to_server.lock().unwrap() = None;
    }
}
