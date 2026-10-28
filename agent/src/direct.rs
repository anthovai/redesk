//! Direct session paths, so a session does not have to go through the server relay:
//!
//! - **Direct (TCP)**: the agent listens on a TCP port (default 21118) and offers its LAN
//!   addresses (plus the public address the server sees) as candidates.
//! - **P2P (QUIC over UDP)**: both ends learn each other's public UDP address from the server's
//!   rendezvous, punch holes in their NATs and run QUIC between them.
//!
//! Both carry exactly the same WebSocket messages as the relay, so the E2E handshake and the
//! encrypted channel are unchanged; a direct path gains no trust. Before any of it, the viewer
//! must prove it holds the session's direct token (issued through signaling) with
//! `aegiron-direct: v1 <sid> <epk> <mac>` on the WebSocket upgrade, where
//! `mac = HMAC-SHA256(token, "aegiron-direct-v1\n" + sid + "\n" + epk)` binds the token to the
//! viewer's ephemeral key; the agent then only accepts a `hello` carrying that same key.
//! Anything else is answered with 403 and closed. See docs/PROTOCOL.md.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use hmac::{Hmac, Mac};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::{Message, http};

use crate::net::WsError;
use crate::signal::Agent;

/// Default TCP port for direct access (same number RustDesk uses for direct IP access).
pub const DEFAULT_PORT: u16 = 21118;
/// Capability the agent advertises at registration.
pub const FEATURE: &str = "direct1";
const HEADER: &str = "aegiron-direct";
const ALPN: &[u8] = b"aegiron-direct/1";
const QUIC_NAME: &str = "aegiron-direct";

/// How long the viewer lets direct candidates run before also starting the relay.
const HEAD_START: Duration = Duration::from_millis(2500);
/// Total time the viewer gives direct candidates.
const DIRECT_BUDGET: Duration = Duration::from_secs(9);
/// Direct connections the agent accepts per session (one per candidate the viewer races).
const MAX_ACCEPTS: u32 = 6;
/// Per source address, unauthenticated connection attempts per minute on the direct port.
const ATTEMPTS_PER_MIN: u32 = 30;
const MAX_HANDSHAKES: usize = 16;

/// Which way the session's bytes travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Relay,
    Direct,
    P2p,
}

impl Path {
    pub fn as_str(self) -> &'static str {
        match self {
            Path::Relay => "relay",
            Path::Direct => "direct",
            Path::P2p => "p2p",
        }
    }

    /// Thai label for the viewer's connection info.
    pub fn label(self) -> &'static str {
        match self {
            Path::Relay => "รีเลย์ (ผ่านเซิร์ฟเวอร์)",
            Path::Direct => "ตรง (TCP)",
            Path::P2p => "P2P (UDP)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "relay" => Some(Path::Relay),
            "direct" | "tcp" => Some(Path::Direct),
            "p2p" | "quic" | "udp" => Some(Path::P2p),
            _ => None,
        }
    }
}

pub type WsTx = Pin<Box<dyn Sink<Message, Error = WsError> + Send>>;
pub type WsRx = Pin<Box<dyn Stream<Item = Result<Message, WsError>> + Send>>;

/// Resources that must live as long as the session (QUIC endpoint/connection, audit guard).
#[derive(Default)]
pub struct Keep(Vec<Box<dyn Any + Send>>);

/// One established path: WebSocket messages, whatever carries them.
pub struct Link {
    pub tx: WsTx,
    pub rx: WsRx,
    pub path: Path,
    pub keep: Keep,
}

pub fn link<S>(ws: WebSocketStream<S>, path: Path, keep: Keep) -> Link
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = ws.split();
    Link { tx: Box::pin(tx), rx: Box::pin(rx), path, keep }
}

/// `direct` in the agent's session request (signaling).
#[derive(Debug, Clone, Deserialize)]
pub struct Offer {
    pub token: String,
    #[serde(default)]
    pub punch_key: Option<String>,
    #[serde(default)]
    pub udp_port: Option<u16>,
}

/// `ticket.direct` returned to a native viewer by `POST /api/sessions?direct=1`.
#[derive(Debug, Clone, Deserialize)]
pub struct Ticket {
    pub token: String,
    #[serde(default)]
    pub tcp: Vec<String>,
    #[serde(default)]
    pub quic: Option<QuicTicket>,
    #[serde(default)]
    pub nat: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuicTicket {
    pub cert: String,
    pub punch_key: String,
    pub udp_port: u16,
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &str, msg: &str) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key.as_bytes()).expect("hmac accepts any key length");
    m.update(msg.as_bytes());
    m
}

fn direct_msg(sid: &str, epk: &str) -> String {
    format!("aegiron-direct-v1\n{sid}\n{epk}")
}

/// Value of the `aegiron-direct` upgrade header.
pub fn header_value(token: &str, sid: &str, epk: &str) -> String {
    let mac = B64.encode(hmac(token, &direct_msg(sid, epk)).finalize().into_bytes());
    format!("v1 {sid} {epk} {mac}")
}

/// Rendezvous proof, same as the server's `PunchMAC`.
pub fn punch_mac(key: &str, sid: &str, role: &str, ts: u64) -> String {
    B64.encode(hmac(key, &format!("aegiron-punch-v1\n{sid}\n{role}\n{ts}")).finalize().into_bytes())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Host part of the server URL (for the UDP rendezvous on the same host).
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let auth = rest.split('/').next()?;
    let auth = auth.rsplit_once('@').map(|(_, h)| h).unwrap_or(auth);
    let host = if let Some(v6) = auth.strip_prefix('[') { v6.split(']').next()? } else { auth.split(':').next()? };
    (!host.is_empty()).then(|| host.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn read_text(rx: &mut WsRx, what: &str, secs: u64) -> Result<String> {
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

// =============================================================================================
// Agent side
// =============================================================================================

struct Accepted {
    link: Link,
    epk: String,
}

struct Pending {
    token: String,
    tx: mpsc::Sender<Accepted>,
    accepted: u32,
}

static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

struct PendingGuard(String);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        PENDING.lock().unwrap().remove(&self.0);
    }
}

fn is_pending(sid: &str) -> bool {
    PENDING.lock().unwrap().contains_key(sid)
}

/// Validates the upgrade header against a session waiting for its viewer.
fn check_header(value: &str) -> Option<(String, mpsc::Sender<Accepted>)> {
    let mut it = value.split(' ');
    let (v, sid, epk, mac) = (it.next()?, it.next()?, it.next()?, it.next()?);
    if v != "v1" || it.next().is_some() || epk.len() > 64 {
        return None;
    }
    let mac = B64.decode(mac).ok()?;
    let mut pending = PENDING.lock().unwrap();
    let p = pending.get_mut(sid)?;
    hmac(&p.token, &direct_msg(sid, epk)).verify_slice(&mac).ok()?;
    if p.accepted >= MAX_ACCEPTS {
        return None;
    }
    p.accepted += 1;
    Some((epk.to_string(), p.tx.clone()))
}

fn forbidden() -> ErrorResponse {
    let mut r = http::Response::new(Some("forbidden".to_string()));
    *r.status_mut() = http::StatusCode::FORBIDDEN;
    r
}

/// Accepts one direct WebSocket on `stream` (TCP or a QUIC stream) if its token proof is valid.
#[allow(clippy::result_large_err)] // the callback's error type is tungstenite's
async fn accept_direct<S>(stream: S, path: Path, keep: Keep) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    let cb = {
        let slot = slot.clone();
        move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
            match req.headers().get(HEADER).and_then(|v| v.to_str().ok()).and_then(check_header) {
                Some(found) => {
                    *slot.lock().unwrap() = Some(found);
                    Ok(resp)
                }
                None => Err(forbidden()),
            }
        }
    };
    let Ok(Ok(ws)) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::accept_hdr_async(stream, cb)).await else {
        return false;
    };
    let Some((epk, tx)) = slot.lock().unwrap().take() else { return false };
    tx.send(Accepted { link: link(ws, path, keep), epk }).await.is_ok()
}

/// Per-address attempt counter for the direct port.
#[derive(Default)]
struct Limiter {
    hits: HashMap<IpAddr, (Instant, u32)>,
}

impl Limiter {
    fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.hits.len() > 4096 {
            self.hits.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
        }
        let e = self.hits.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(60) {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= ATTEMPTS_PER_MIN
    }
}

static LISTENER: tokio::sync::OnceCell<Option<u16>> = tokio::sync::OnceCell::const_new();

/// Starts the direct TCP listener once (on `port`, or a random port if it is taken) and
/// returns the port. It only ever accepts connections for sessions waiting for their viewer.
pub async fn ensure_listener(port: u16) -> Option<u16> {
    *LISTENER
        .get_or_init(|| async move {
            let l = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("direct port {port} unavailable ({e}); using a random port");
                    match tokio::net::TcpListener::bind(("0.0.0.0", 0)).await {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::warn!("direct connections unavailable: {e}");
                            return None;
                        }
                    }
                }
            };
            let bound = l.local_addr().ok()?.port();
            tracing::info!("direct connections on TCP {bound}");
            tokio::spawn(serve(l));
            Some(bound)
        })
        .await
}

async fn serve(l: tokio::net::TcpListener) {
    let limiter = Mutex::new(Limiter::default());
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES));
    loop {
        let Ok((tcp, from)) = l.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        if !limiter.lock().unwrap().allow(from.ip(), Instant::now()) {
            continue; // dropped: closes immediately
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            if !accept_direct(tcp, Path::Direct, Keep::default()).await {
                tracing::debug!("direct: rejected connection from {from}");
            }
            drop(permit);
        });
    }
}

/// Sends `direct_ended` (bytes for the audit log) when the session is over.
struct EndGuard {
    agent: Arc<Agent>,
    sid: String,
    path: Path,
    to_viewer: Arc<AtomicU64>,
    to_agent: Arc<AtomicU64>,
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        self.agent.send_server(json!({
            "t": "direct_ended", "sid": self.sid, "path": self.path.as_str(), "reason": "closed",
            "bytes_to_viewer": self.to_viewer.load(Ordering::Relaxed),
            "bytes_to_agent": self.to_agent.load(Ordering::Relaxed),
        }));
    }
}

/// Counts bytes both ways and reports the end of a direct session to the server.
fn audited(mut l: Link, agent: Arc<Agent>, sid: &str) -> Link {
    let (to_viewer, to_agent) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let (tv, ta) = (to_viewer.clone(), to_agent.clone());
    let tx = l.tx.with(move |m: Message| {
        tv.fetch_add(m.len() as u64, Ordering::Relaxed);
        futures_util::future::ready(Ok::<_, WsError>(m))
    });
    let rx = l.rx.inspect(move |m| {
        if let Ok(m) = m {
            ta.fetch_add(m.len() as u64, Ordering::Relaxed);
        }
    });
    l.keep.0.push(Box::new(EndGuard { agent, sid: sid.to_string(), path: l.path, to_viewer, to_agent }));
    Link { tx: Box::pin(tx), rx: Box::pin(rx), path: l.path, keep: l.keep }
}

async fn relay_link(url: String) -> Result<Link> {
    let (ws, _) = crate::net::connect_ws(url.as_str()).await.context("relay connect")?;
    let mut l = link(ws, Path::Relay, Keep::default());
    let paired = read_text(&mut l.rx, "pairing", 60).await?;
    if !paired.contains(r#""paired""#) {
        bail!("unexpected relay message: {paired}");
    }
    Ok(l)
}

type Waiting = Result<(Link, String), (Path, anyhow::Error)>;

/// Waits for the viewer's `hello`; on a direct path it must carry the key the token was bound to.
async fn wait_hello(mut l: Link, bound: Option<String>) -> Waiting {
    let path = l.path;
    let text = read_text(&mut l.rx, "viewer hello", 30).await.map_err(|e| (path, e))?;
    if let Some(epk) = bound {
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| (path, e.into()))?;
        if v["epk"].as_str() != Some(epk.as_str()) {
            return Err((path, anyhow!("hello does not match the direct token")));
        }
    }
    Ok((l, text))
}

async fn recv_opt<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Agent side of a session: joins the relay and, when the server offered it, also waits for the
/// viewer on direct paths. The first path that delivers the viewer's `hello` wins; returns that
/// link and the hello text. Without an offer this is exactly the old relay-only behaviour.
pub async fn agent_link(agent: &Arc<Agent>, sid: &str, relay_url: String, offer: Option<Offer>) -> Result<(Link, String)> {
    let (enabled, port, server_url) = {
        let c = agent.config.lock().unwrap();
        (c.direct_enabled, c.direct_port, c.server_url.clone())
    };
    let mut direct_rx = None;
    let mut _pending = None;
    match offer {
        Some(o) if enabled => {
            let (tx, rx) = mpsc::channel(MAX_ACCEPTS as usize);
            PENDING.lock().unwrap().insert(sid.to_string(), Pending { token: o.token.clone(), tx: tx.clone(), accepted: 0 });
            _pending = Some(PendingGuard(sid.to_string()));
            direct_rx = Some(rx);
            send_offer(agent, sid, &o, port, &server_url).await;
        }
        Some(_) => agent.send_server(json!({"t": "direct_offer", "sid": sid, "disabled": true})),
        None => {}
    }

    let relay = relay_link(relay_url);
    tokio::pin!(relay);
    let mut relay_done = false;
    let mut waiting: FuturesUnordered<BoxFuture<'static, Waiting>> = FuturesUnordered::new();
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    let (l, hello) = loop {
        tokio::select! {
            r = &mut relay, if !relay_done => {
                relay_done = true;
                match r {
                    Ok(l) => waiting.push(wait_hello(l, None).boxed()),
                    Err(e) if direct_rx.is_none() => return Err(e),
                    Err(e) => tracing::warn!("session {sid}: relay unavailable, waiting for a direct path: {e:#}"),
                }
            }
            Some(acc) = recv_opt(&mut direct_rx) => {
                let mut l = acc.link;
                let paired = json!({"t": "paired", "path": l.path.as_str()}).to_string();
                if l.tx.send(Message::Text(paired.into())).await.is_ok() {
                    waiting.push(wait_hello(l, Some(acc.epk)).boxed());
                }
            }
            Some(r) = waiting.next() => match r {
                Ok(w) => break w,
                Err((Path::Relay, e)) if direct_rx.is_none() => return Err(e),
                Err((path, e)) => tracing::debug!("session {sid}: {} path dropped: {e:#}", path.as_str()),
            },
            _ = &mut deadline => bail!("timed out waiting for the viewer"),
        }
    };
    tracing::info!("session {sid}: viewer connected via {}", l.path.as_str());
    if l.path == Path::Relay {
        return Ok((l, hello));
    }
    agent.send_server(json!({"t": "direct_started", "sid": sid, "path": l.path.as_str()}));
    Ok((audited(l, agent.clone(), sid), hello))
}

/// Collects candidates (TCP listener, QUIC certificate) and sends them to the server.
async fn send_offer(agent: &Arc<Agent>, sid: &str, o: &Offer, port: u16, server_url: &str) {
    let mut tcp = Vec::new();
    if let Some(port) = ensure_listener(port).await {
        let facts = tokio::task::spawn_blocking(crate::lan::local_facts).await.unwrap_or_default();
        tcp = facts.lan.iter().map(|a| format!("{}:{port}", a.ip)).collect();
    }
    let mut cert_hash = String::new();
    if let (Some(key), Some(udp_port), Some(host)) = (o.punch_key.clone(), o.udp_port.filter(|p| *p > 0), url_host(server_url)) {
        match rcgen::generate_simple_self_signed(vec![QUIC_NAME.to_string()]) {
            Ok(ck) => {
                let cert = ck.cert.der().clone();
                cert_hash = hex(&Sha256::digest(cert.as_ref()));
                let pkey = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
                let sid = sid.to_string();
                tokio::spawn(async move {
                    if let Err(e) = agent_quic(sid.clone(), host, udp_port, key, cert, pkey).await {
                        tracing::debug!("session {sid}: P2P unavailable: {e:#}");
                    }
                });
            }
            Err(e) => tracing::warn!("cannot create a QUIC certificate: {e}"),
        }
    }
    let nat = nat_type().map(|n| n.as_str()).unwrap_or("");
    agent.send_server(json!({"t": "direct_offer", "sid": sid, "tcp": tcp, "cert": cert_hash, "nat": nat}));
}

/// Agent half of P2P: rendezvous, punch, then accept QUIC connections for this session.
async fn agent_quic(sid: String, host: String, udp_port: u16, key: String, cert: CertificateDer<'static>, pkey: PrivateKeyDer<'static>) -> Result<()> {
    let cfg = quic_server(cert, pkey)?;
    let sid2 = sid.clone();
    let sock = tokio::task::spawn_blocking(move || -> Result<UdpSocket> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        let server = resolve(&host, udp_port)?;
        let peer = rendezvous(&sock, server, &sid2, "agent", &key, Duration::from_secs(12), || is_pending(&sid2))?;
        punch(&sock, peer);
        prepare_for_quinn(&sock)?;
        Ok(sock)
    })
    .await??;
    let ep = quinn::Endpoint::new(quinn::EndpointConfig::default(), Some(cfg), sock, Arc::new(quinn::TokioRuntime))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while let Ok(Some(incoming)) = tokio::time::timeout_at(deadline, ep.accept()).await {
        if !is_pending(&sid) {
            break;
        }
        let ep = ep.clone();
        tokio::spawn(async move {
            let res: Result<bool> = async {
                let conn = tokio::time::timeout(Duration::from_secs(5), incoming).await??;
                let (send, recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi()).await??;
                let keep = Keep(vec![Box::new(ep), Box::new(conn)]);
                Ok(accept_direct(tokio::io::join(recv, send), Path::P2p, keep).await)
            }
            .await;
            if !matches!(res, Ok(true)) {
                tracing::debug!("P2P: rejected connection ({res:?})");
            }
        });
    }
    Ok(())
}

// =============================================================================================
// Viewer side
// =============================================================================================

/// What the viewer needs to try direct paths for one session.
pub struct ViewerDirect {
    pub server_url: String,
    pub sid: String,
    pub ticket: Ticket,
    /// The viewer's ephemeral X25519 key (base64), bound into the token proof.
    pub epk: String,
    /// Test/diagnostic: use only this path (no fallback).
    pub only: Option<Path>,
}

/// Races direct candidates against the relay. Direct paths get a short head start; the relay
/// starts after it (or at once when all direct attempts failed) and is always the fallback.
pub async fn viewer_race<F>(d: Option<ViewerDirect>, relay: F) -> Result<Link>
where
    F: Future<Output = Result<Link>>,
{
    let Some(d) = d.filter(|d| d.only != Some(Path::Relay)) else { return relay.await };
    let header = header_value(&d.ticket.token, &d.sid, &d.epk);
    let mut attempts: FuturesUnordered<BoxFuture<'static, Result<Link>>> = FuturesUnordered::new();
    if d.only != Some(Path::P2p) {
        for c in d.ticket.tcp.iter().filter_map(|c| c.parse::<SocketAddr>().ok()) {
            attempts.push(viewer_tcp(c, header.clone()).boxed());
        }
    }
    if d.only != Some(Path::Direct)
        && let Some(q) = d.ticket.quic.clone()
    {
        attempts.push(viewer_quic(d.server_url.clone(), d.sid.clone(), q, header.clone()).boxed());
    }
    let direct = async move {
        let mut last = anyhow!("no direct candidates");
        while let Some(r) = attempts.next().await {
            match r {
                Ok(l) => return Ok(l),
                Err(e) => {
                    tracing::debug!("direct candidate failed: {e:#}");
                    last = e;
                }
            }
        }
        Err(last)
    };
    let direct = async move { tokio::time::timeout(DIRECT_BUDGET, direct).await.unwrap_or_else(|_| Err(anyhow!("direct paths timed out"))) };
    if d.only.is_some() {
        return direct.await;
    }
    tokio::pin!(direct);
    tokio::select! {
        r = &mut direct => match r {
            Ok(l) => return Ok(l),
            Err(e) => {
                tracing::info!("viewer: no direct path ({e:#}); using the relay");
                return relay.await;
            }
        },
        _ = tokio::time::sleep(HEAD_START) => {}
    }
    tokio::pin!(relay);
    let (mut direct_live, mut relay_err) = (true, None);
    loop {
        tokio::select! {
            r = &mut direct, if direct_live => match r {
                Ok(l) => return Ok(l),
                Err(e) => {
                    tracing::info!("viewer: no direct path ({e:#})");
                    if let Some(e) = relay_err.take() {
                        return Err(e);
                    }
                    direct_live = false;
                }
            },
            r = &mut relay, if relay_err.is_none() => match r {
                Ok(l) => return Ok(l),
                Err(e) if !direct_live => return Err(e),
                Err(e) => relay_err = Some(e),
            },
        }
    }
}

async fn ws_client<S>(stream: S, header: String, path: Path, keep: Keep) -> Result<Link>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut req = "ws://aegiron-direct/".into_client_request()?;
    req.headers_mut().insert(HEADER, header.parse()?);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(4), tokio_tungstenite::client_async(req, stream)).await.context("direct handshake timed out")??;
    let mut l = link(ws, path, keep);
    let t = read_text(&mut l.rx, "direct pairing", 4).await?;
    if !t.contains(r#""paired""#) {
        bail!("unexpected direct reply");
    }
    Ok(l)
}

async fn viewer_tcp(addr: SocketAddr, header: String) -> Result<Link> {
    let tcp = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await.context("connect timed out")??;
    tcp.set_nodelay(true)?;
    ws_client(tcp, header, Path::Direct, Keep::default()).await.with_context(|| format!("direct {addr}"))
}

async fn viewer_quic(server_url: String, sid: String, q: QuicTicket, header: String) -> Result<Link> {
    let pin = unhex32(&q.cert).context("bad certificate hash")?;
    let host = url_host(&server_url).context("bad server address")?;
    let (sock, peer) = tokio::task::spawn_blocking(move || -> Result<(UdpSocket, SocketAddr)> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        let server = resolve(&host, q.udp_port)?;
        let peer = rendezvous(&sock, server, &sid, "viewer", &q.punch_key, Duration::from_secs(6), || true)?;
        punch(&sock, peer);
        prepare_for_quinn(&sock)?;
        Ok((sock, peer))
    })
    .await??;
    let mut ep = quinn::Endpoint::new(quinn::EndpointConfig::default(), None, sock, Arc::new(quinn::TokioRuntime))?;
    ep.set_default_client_config(quic_client(pin)?);
    let conn = tokio::time::timeout(Duration::from_secs(5), ep.connect(peer, QUIC_NAME)?).await.context("QUIC connect timed out")??;
    let (send, recv) = conn.open_bi().await?;
    let keep = Keep(vec![Box::new(ep), Box::new(conn)]);
    ws_client(tokio::io::join(recv, send), header, Path::P2p, keep).await.with_context(|| format!("P2P {peer}"))
}

// =============================================================================================
// UDP: rendezvous, hole punching, NAT type
// =============================================================================================

fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).copied().context("cannot resolve server")
}

/// Pads a UDP request: the server only answers requests at least as long as its reply.
fn padded(mut v: serde_json::Value) -> Vec<u8> {
    v["pad"] = json!("x".repeat(160));
    v.to_string().into_bytes()
}

/// Registers this socket's public address for `role` and returns the peer's once known.
fn rendezvous(sock: &UdpSocket, server: SocketAddr, sid: &str, role: &str, key: &str, budget: Duration, keep_going: impl Fn() -> bool) -> Result<SocketAddr> {
    sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    let deadline = Instant::now() + budget;
    let mut buf = [0u8; 1500];
    while Instant::now() < deadline && keep_going() {
        let ts = unix_now();
        let req = padded(json!({"t": "punch", "sid": sid, "role": role, "ts": ts, "mac": punch_mac(key, sid, role, ts)}));
        sock.send_to(&req, server)?;
        let until = Instant::now() + Duration::from_millis(250);
        while Instant::now() < until {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            if from != server {
                continue;
            }
            #[derive(Deserialize)]
            struct R {
                #[serde(default)]
                peer: String,
            }
            if let Ok(r) = serde_json::from_slice::<R>(&buf[..n])
                && let Ok(p) = r.peer.parse()
            {
                return Ok(p);
            }
        }
    }
    bail!("no peer address from the rendezvous")
}

/// Opens our NAT mapping towards the peer so its packets get through.
fn punch(sock: &UdpSocket, peer: SocketAddr) {
    for _ in 0..3 {
        let _ = sock.send_to(b"aegiron-punch", peer);
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn prepare_for_quinn(sock: &UdpSocket) -> Result<()> {
    sock.set_read_timeout(None)?;
    sock.set_nonblocking(true)?;
    Ok(())
}

/// What the NAT in front of this machine does to UDP (decides whether P2P can work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NatType {
    /// Public address, no NAT.
    Open,
    /// Same mapping for every destination: hole punching works.
    Cone,
    /// A new mapping per destination: hole punching usually fails (relay or LAN direct instead).
    Symmetric,
    /// No answer: UDP is blocked or the server has no UDP port open.
    Blocked,
}

impl NatType {
    pub fn as_str(self) -> &'static str {
        match self {
            NatType::Open => "open",
            NatType::Cone => "cone",
            NatType::Symmetric => "symmetric",
            NatType::Blocked => "blocked",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NatType::Open => "ไม่มี NAT (P2P ได้)",
            NatType::Cone => "Cone NAT (P2P ได้)",
            NatType::Symmetric => "Symmetric NAT (P2P มักไม่สำเร็จ ใช้ relay หรือ LAN แทน)",
            NatType::Blocked => "ตรวจไม่ได้ (UDP ถูกบล็อก)",
        }
    }
}

static NAT: Mutex<Option<NatType>> = Mutex::new(None);

pub fn nat_type() -> Option<NatType> {
    *NAT.lock().unwrap()
}

/// NAT-type probe against the server's UDP ports `port` and `port + 1` (blocking, ~2 s max).
pub fn probe_nat(host: &str, port: u16) -> Result<NatType> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    let a = resolve(host, port)?;
    let b = SocketAddr::new(a.ip(), port + 1);
    sock.set_read_timeout(Some(Duration::from_millis(150)))?;
    let (mut seen_a, mut seen_b) = (None::<SocketAddr>, None::<SocketAddr>);
    let req = padded(json!({"t": "nat"}));
    let mut buf = [0u8; 1500];
    for _ in 0..4 {
        if seen_a.is_none() {
            sock.send_to(&req, a)?;
        }
        if seen_b.is_none() {
            sock.send_to(&req, b)?;
        }
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until && (seen_a.is_none() || seen_b.is_none()) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            let addr = serde_json::from_slice::<serde_json::Value>(&buf[..n]).ok().and_then(|v| v["addr"].as_str()?.parse::<SocketAddr>().ok());
            match addr {
                Some(m) if from == a => seen_a = Some(m),
                Some(m) if from == b => seen_b = Some(m),
                _ => {}
            }
        }
        if seen_a.is_some() && seen_b.is_some() {
            break;
        }
    }
    let local_port = sock.local_addr()?.port();
    let local_ips: Vec<IpAddr> = crate::lan::local_facts().lan.iter().map(|l| IpAddr::V4(l.ip)).collect();
    Ok(match (seen_a, seen_b) {
        (Some(x), Some(y)) if x != y => NatType::Symmetric,
        (Some(x), Some(_)) if x.port() == local_port && (local_ips.contains(&x.ip()) || x.ip().is_loopback()) => NatType::Open,
        (Some(_), Some(_)) => NatType::Cone,
        _ => NatType::Blocked,
    })
}

/// Runs the NAT probe in the background and remembers the result (shown in Settings → Network).
pub async fn probe_and_store(server_url: String, port: u16) {
    let Some(host) = url_host(&server_url) else { return };
    match tokio::task::spawn_blocking(move || probe_nat(&host, port)).await {
        Ok(Ok(t)) => {
            tracing::info!("NAT type: {}", t.as_str());
            *NAT.lock().unwrap() = Some(t);
        }
        Ok(Err(e)) => tracing::debug!("NAT probe failed: {e:#}"),
        Err(_) => {}
    }
}

// =============================================================================================
// QUIC configuration
// =============================================================================================

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(30_000))));
    t.stream_receive_window(quinn::VarInt::from_u32(8 << 20));
    t.receive_window(quinn::VarInt::from_u32(16 << 20));
    t.send_window(16 << 20);
    Arc::new(t)
}

fn quic_server(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Result<quinn::ServerConfig> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

fn quic_client(pin: [u8; 32]) -> Result<quinn::ClientConfig> {
    let p = provider();
    // The agent's certificate is self-signed per session; its SHA-256 came with the ticket over
    // the server's TLS. Accept exactly that certificate and nothing else (stricter than a CA).
    let mut tls = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert { sha256: pin, provider: p }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut cfg = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

/// Certificate pinning by SHA-256 of the end-entity certificate; signatures are still verified.
#[derive(Debug)]
struct PinnedCert {
    sha256: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if intermediates.is_empty() && Sha256::digest(end_entity.as_ref()).as_slice() == self.sha256 {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(url_host("https://aegiron.example.com:8443/x").as_deref(), Some("aegiron.example.com"));
        assert_eq!(url_host("http://[::1]:21114").as_deref(), Some("::1"));
        assert_eq!(url_host("http://localhost").as_deref(), Some("localhost"));
        let h = [7u8; 32];
        assert_eq!(unhex32(&hex(&h)), Some(h));
        assert_eq!(unhex32("zz"), None);
        // Same construction as the server's PunchMAC (Go test vector in hub/direct_test.go).
        assert_eq!(punch_mac("key", "sid", "agent", 1700000000), "G+czfC5NMvtWfd2kon2rnIRV/1GMh4HL7VhEBL4GcEA=");
    }

    #[test]
    fn header_proof_is_bound_to_token_sid_and_key() {
        let (tx, _rx) = mpsc::channel(1);
        PENDING.lock().unwrap().insert("s-test".into(), Pending { token: "tok".into(), tx, accepted: 0 });
        let good = header_value("tok", "s-test", "EPK");
        assert!(check_header(&good).is_some());
        assert!(check_header(&header_value("other", "s-test", "EPK")).is_none(), "wrong token");
        assert!(check_header(&header_value("tok", "s-none", "EPK")).is_none(), "unknown session");
        let swapped = good.replace(" EPK ", " EVIL ");
        assert!(check_header(&swapped).is_none(), "key not bound");
        assert!(check_header("v2 s-test EPK x").is_none());
        for _ in 1..MAX_ACCEPTS {
            assert!(check_header(&good).is_some());
        }
        assert!(check_header(&good).is_none(), "accept cap");
        PENDING.lock().unwrap().remove("s-test");
    }

    #[test]
    fn limiter_caps_attempts_per_address() {
        let mut l = Limiter::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let t = Instant::now();
        for _ in 0..ATTEMPTS_PER_MIN {
            assert!(l.allow(ip, t));
        }
        assert!(!l.allow(ip, t));
        assert!(l.allow("10.0.0.2".parse().unwrap(), t));
        assert!(l.allow(ip, t + Duration::from_secs(61)));
    }

    async fn pair_over_tcp(token_for_viewer: &str) -> Result<Link> {
        let (tx, mut rx) = mpsc::channel(4);
        let sid = format!("s-{}", unix_now() as u32 ^ rand_u32());
        PENDING.lock().unwrap().insert(sid.clone(), Pending { token: "secret".into(), tx, accepted: 0 });
        let _g = PendingGuard(sid.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = l.accept().await.unwrap();
            accept_direct(tcp, Path::Direct, Keep::default()).await;
        });
        // Agent: acknowledge like agent_link does.
        tokio::spawn(async move {
            if let Some(mut acc) = rx.recv().await {
                let _ = acc.link.tx.send(Message::Text(r#"{"t":"paired","path":"direct"}"#.into())).await;
                let _ = acc.link.rx.next().await;
            }
        });
        viewer_tcp(addr, header_value(token_for_viewer, &sid, "EPK")).await
    }

    fn rand_u32() -> u32 {
        u32::from_le_bytes(crate::crypto::random_bytes::<4>())
    }

    #[tokio::test]
    async fn direct_tcp_requires_the_session_token() {
        assert_eq!(pair_over_tcp("secret").await.unwrap().path, Path::Direct);
        assert!(pair_over_tcp("guess").await.is_err());
    }

    #[tokio::test]
    async fn viewer_falls_back_to_relay_when_direct_fails() {
        let d = ViewerDirect {
            server_url: "http://127.0.0.1:1".into(),
            sid: "s".into(),
            ticket: Ticket { token: "t".into(), tcp: vec!["127.0.0.1:1".into()], quic: None, nat: String::new() },
            epk: "E".into(),
            only: None,
        };
        let started = Instant::now();
        let err = viewer_race(Some(d), async { Err::<Link, _>(anyhow!("relay used")) }).await.err().unwrap();
        assert_eq!(err.to_string(), "relay used");
        assert!(started.elapsed() < HEAD_START, "a failed direct path must not delay the relay");
    }

    #[tokio::test]
    async fn quic_with_pinned_certificate() {
        let ck = rcgen::generate_simple_self_signed(vec![QUIC_NAME.to_string()]).unwrap();
        let cert = ck.cert.der().clone();
        let pin: [u8; 32] = Sha256::digest(cert.as_ref()).into();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
        let server = quinn::Endpoint::server(quic_server(cert, key).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            while let Some(inc) = server.accept().await {
                if let Ok(conn) = inc.await
                    && let Ok((mut s, _r)) = conn.accept_bi().await
                {
                    let _ = s.write_all(b"ok").await;
                    let _ = s.finish();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        });
        let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        ep.set_default_client_config(quic_client(pin).unwrap());
        let conn = ep.connect(addr, QUIC_NAME).unwrap().await.unwrap();
        let (mut s, mut r) = conn.open_bi().await.unwrap();
        s.write_all(b"hi").await.unwrap();
        assert_eq!(r.read_to_end(10).await.unwrap(), b"ok");
        // A different pinned hash must fail the TLS handshake.
        ep.set_default_client_config(quic_client([0u8; 32]).unwrap());
        assert!(ep.connect(addr, QUIC_NAME).unwrap().await.is_err());
    }
}
