//! TCP tunnels (port forwarding, like RustDesk's "TCP tunneling"): the viewer listens on a
//! local port and every connection to it becomes a stream inside the session's encrypted
//! channel; the agent connects each stream to the session's target (docs/PROTOCOL.md §11).
//!
//! Streams are multiplexed with per-stream flow control: a side never has more than
//! [`WINDOW`] bytes of a stream unacknowledged, and the receiver acknowledges only what it has
//! written to its socket, so a slow consumer slows its own stream down and nothing else.
//! Half-close is carried through (`tcp_eof` shuts down the write side of the other socket).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::AbortHandle;

use crate::client;
use crate::proto;
use crate::remote::{self, Conn, SessionKind};
use crate::signal::Agent;

/// Unacknowledged bytes allowed per stream and direction.
pub const WINDOW: u32 = 256 * 1024;
/// Largest data payload of one `0x10` message.
pub const CHUNK: usize = 32 * 1024;
/// Streams open at the same time in one tunnel session.
pub const MAX_STREAMS: usize = 64;

/// Byte counters of one tunnel session (`up` = viewer to target).
#[derive(Default)]
pub struct Counters {
    pub up: AtomicU64,
    pub down: AtomicU64,
    pub opened: AtomicUsize,
    pub active: AtomicUsize,
}

enum ToSock {
    Data(Vec<u8>),
    Eof,
}

enum Done {
    /// The local socket reached end of file; `tcp_eof` was sent.
    ReadEof,
    /// The peer's `tcp_eof` was applied: our socket's write side is shut down.
    WriteEof,
    /// The local socket failed; `tcp_close` was sent.
    Failed,
}

struct Stream {
    to_sock: mpsc::UnboundedSender<ToSock>,
    /// Bytes received from the peer and not yet written to the socket.
    queued: Arc<AtomicUsize>,
    credit: Arc<Semaphore>,
    tasks: [AbortHandle; 2],
    read_eof: bool,
    write_eof: bool,
}

impl Drop for Stream {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// The streams of one tunnel session; the same code runs on the viewer and the agent.
pub struct Mux {
    streams: HashMap<u32, Stream>,
    /// Plaintext messages for the peer (sealed and sent by the session loop).
    out: mpsc::Sender<Vec<u8>>,
    done_tx: mpsc::UnboundedSender<(u32, Done)>,
    done_rx: mpsc::UnboundedReceiver<(u32, Done)>,
    /// Counters; `up` counts bytes this side's sockets read when `local_is_up`.
    pub counters: Arc<Counters>,
    local_is_up: bool,
}

impl Mux {
    /// `local_is_up`: bytes read from this side's sockets flow viewer → target (the viewer).
    pub fn new(out: mpsc::Sender<Vec<u8>>, local_is_up: bool) -> Self {
        Self::with_counters(out, local_is_up, Arc::default())
    }

    pub fn with_counters(out: mpsc::Sender<Vec<u8>>, local_is_up: bool, counters: Arc<Counters>) -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Mux { streams: HashMap::new(), out, done_tx, done_rx, counters, local_is_up }
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    pub fn contains(&self, id: u32) -> bool {
        self.streams.contains_key(&id)
    }

    /// Starts pumping an established connection as stream `id`.
    pub fn attach(&mut self, id: u32, sock: TcpStream) {
        let _ = sock.set_nodelay(true);
        let (mut rd, mut wr) = sock.into_split();
        let credit = Arc::new(Semaphore::new(WINDOW as usize));
        let queued = Arc::new(AtomicUsize::new(0));
        let (to_sock, mut from_peer) = mpsc::unbounded_channel::<ToSock>();
        let counters = self.counters.clone();
        let up = self.local_is_up;

        // Socket → peer, limited by the credit the peer has acknowledged.
        let reader = {
            let (out, done, credit, counters) = (self.out.clone(), self.done_tx.clone(), credit.clone(), counters.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let n = match rd.read(&mut buf).await {
                        Ok(0) => {
                            let _ = out.send(proto::json(&json!({"t": "tcp_eof", "id": id}))).await;
                            let _ = done.send((id, Done::ReadEof));
                            return;
                        }
                        Ok(n) => n,
                        Err(_) => {
                            let _ = out.send(proto::json(&json!({"t": "tcp_close", "id": id}))).await;
                            let _ = done.send((id, Done::Failed));
                            return;
                        }
                    };
                    let Ok(permit) = credit.acquire_many(n as u32).await else { return };
                    permit.forget();
                    if out.send(proto::tunnel_data(id, &buf[..n])).await.is_err() {
                        return;
                    }
                    let c = if up { &counters.up } else { &counters.down };
                    c.fetch_add(n as u64, Ordering::Relaxed);
                }
            })
        };

        // Peer → socket; acknowledges what was written so the peer may send more.
        let writer = {
            let (out, done, queued, counters) = (self.out.clone(), self.done_tx.clone(), queued.clone(), counters.clone());
            tokio::spawn(async move {
                while let Some(cmd) = from_peer.recv().await {
                    match cmd {
                        ToSock::Data(d) => {
                            if wr.write_all(&d).await.is_err() {
                                let _ = out.send(proto::json(&json!({"t": "tcp_close", "id": id}))).await;
                                let _ = done.send((id, Done::Failed));
                                return;
                            }
                            queued.fetch_sub(d.len(), Ordering::Relaxed);
                            let c = if up { &counters.down } else { &counters.up };
                            c.fetch_add(d.len() as u64, Ordering::Relaxed);
                            if out.send(proto::json(&json!({"t": "tcp_ack", "id": id, "n": d.len()}))).await.is_err() {
                                return;
                            }
                        }
                        ToSock::Eof => {
                            // Half-close: the peer finished sending; we may still read.
                            let _ = wr.shutdown().await;
                            let _ = done.send((id, Done::WriteEof));
                            // Keep the write half alive until the stream is dropped.
                            std::future::pending::<()>().await;
                        }
                    }
                }
            })
        };
        self.counters.opened.fetch_add(1, Ordering::Relaxed);
        self.counters.active.fetch_add(1, Ordering::Relaxed);
        self.streams.insert(
            id,
            Stream { to_sock, queued, credit, tasks: [reader.abort_handle(), writer.abort_handle()], read_eof: false, write_eof: false },
        );
    }

    fn remove(&mut self, id: u32) {
        if self.streams.remove(&id).is_some() {
            self.counters.active.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// A `0x10` data message from the peer.
    pub async fn on_data(&mut self, plain: &[u8]) {
        if plain.len() < 5 {
            return;
        }
        let id = u32::from_be_bytes(plain[1..5].try_into().unwrap());
        let data = &plain[5..];
        let Some(s) = self.streams.get(&id) else { return };
        // A peer that ignores the window would make us buffer without limit.
        if s.write_eof || s.queued.load(Ordering::Relaxed) + data.len() > WINDOW as usize + CHUNK {
            tracing::warn!("tunnel stream {id}: peer exceeded the flow-control window; closing it");
            let _ = self.out.send(proto::json(&json!({"t": "tcp_close", "id": id}))).await;
            self.remove(id);
            return;
        }
        s.queued.fetch_add(data.len(), Ordering::Relaxed);
        let _ = s.to_sock.send(ToSock::Data(data.to_vec()));
    }

    /// Stream control from the peer (`tcp_ack`, `tcp_eof`, `tcp_close`); false if `m` is none of these.
    pub fn on_control(&mut self, m: &Value) -> bool {
        let id = m["id"].as_u64().unwrap_or(0) as u32;
        match m["t"].as_str().unwrap_or("") {
            "tcp_ack" => {
                if let Some(s) = self.streams.get(&id) {
                    let n = m["n"].as_u64().unwrap_or(0).min(WINDOW as u64) as usize;
                    // Never more credit than the window, whatever the peer claims.
                    let room = (WINDOW as usize).saturating_sub(s.credit.available_permits());
                    s.credit.add_permits(n.min(room));
                }
            }
            "tcp_eof" => {
                if let Some(s) = self.streams.get(&id) {
                    let _ = s.to_sock.send(ToSock::Eof);
                }
            }
            "tcp_close" => self.remove(id),
            _ => return false,
        }
        true
    }

    /// The next event from this side's sockets; call from the session loop's select.
    pub async fn next_done(&mut self) {
        let Some((id, d)) = self.done_rx.recv().await else { return std::future::pending().await };
        let Some(s) = self.streams.get_mut(&id) else { return };
        match d {
            Done::ReadEof => s.read_eof = true,
            Done::WriteEof => s.write_eof = true,
            Done::Failed => {
                self.remove(id);
                return;
            }
        }
        if s.read_eof && s.write_eof {
            self.remove(id);
        }
    }
}

// --- agent side ---

/// Runs an approved tunnel session on the agent until the viewer leaves or the local user
/// disconnects it.
pub async fn serve(agent: &Arc<Agent>, sid: &str, conn: &mut Conn, kill: Arc<Notify>, host: &str, port: u16) -> Result<()> {
    let lan = agent.config.lock().unwrap().allow_tunnel_lan;
    let target = remote::join_target(host, port);
    conn.send_json(json!({
        "t": "info",
        "session": "tunnel",
        "target": target,
        "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        "os": std::env::consts::OS,
        "window": WINDOW,
        "max_streams": MAX_STREAMS,
    }))
    .await?;
    agent.report(sid, "tunnel_open", json!({"target": target}));
    tracing::info!("session {sid}: TCP tunnel to {target}");

    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut mux = Mux::new(out_tx, false);
    let (conn_tx, mut conn_rx) = mpsc::unbounded_channel::<(u32, Result<TcpStream>)>();
    let mut connecting = 0usize;
    let counters = mux.counters.clone();
    let _report = ReportOnDrop { agent: agent.clone(), sid: sid.to_string(), target: target.clone(), counters: counters.clone() };

    loop {
        tokio::select! {
            _ = kill.notified() => {
                tracing::info!("session {sid}: disconnected by the local user");
                agent.report(sid, "denied", json!({"stage": "during_session"}));
                return Ok(());
            }
            Some(plain) = out_rx.recv() => conn.send(&plain).await?,
            _ = mux.next_done() => {}
            Some((id, res)) = conn_rx.recv() => {
                connecting -= 1;
                match res {
                    Ok(sock) => {
                        mux.attach(id, sock);
                        conn.send_json(json!({"t": "tcp_opened", "id": id})).await?;
                    }
                    Err(e) => {
                        tracing::info!("session {sid}: tunnel connection to {target} failed: {e:#}");
                        conn.send_json(json!({"t": "tcp_error", "id": id, "message": format!("{e:#}")})).await?;
                    }
                }
            }
            m = conn.recv() => {
                let Some(plain) = m? else { return Ok(()) };
                match plain.first() {
                    Some(&proto::MSG_TUNNEL) => mux.on_data(&plain).await,
                    Some(&proto::MSG_JSON) => {
                        let Some(m) = remote::json_of(&plain) else { continue };
                        if mux.on_control(&m) {
                            continue;
                        }
                        match m["t"].as_str().unwrap_or("") {
                            "tcp_open" => {
                                let id = m["id"].as_u64().unwrap_or(0) as u32;
                                if id == 0 || mux.contains(id) {
                                    continue;
                                }
                                if mux.len() + connecting >= MAX_STREAMS {
                                    conn.send_json(json!({"t": "tcp_error", "id": id, "message": "too many connections"})).await?;
                                    continue;
                                }
                                connecting += 1;
                                let (tx, host) = (conn_tx.clone(), host.to_string());
                                tokio::spawn(async move {
                                    let _ = tx.send((id, connect_target(&host, port, lan).await));
                                });
                            }
                            "bye" => return Ok(()),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Connects to the tunnel target under the device's policy (loopback, or private networks).
async fn connect_target(host: &str, port: u16, lan: bool) -> Result<TcpStream> {
    let addrs = remote::resolve_allowed(host, port, lan).await?;
    let mut last = anyhow!("no address");
    for a in addrs {
        match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(a)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = anyhow!("{a}: {e}"),
            Err(_) => last = anyhow!("{a}: timed out"),
        }
    }
    Err(last)
}

struct ReportOnDrop {
    agent: Arc<Agent>,
    sid: String,
    target: String,
    counters: Arc<Counters>,
}

impl Drop for ReportOnDrop {
    fn drop(&mut self) {
        let c = &self.counters;
        self.agent.report(
            &self.sid,
            "tunnel_closed",
            json!({
                "target": self.target,
                "connections": c.opened.load(Ordering::Relaxed),
                "bytes_up": c.up.load(Ordering::Relaxed),
                "bytes_down": c.down.load(Ordering::Relaxed),
            }),
        );
    }
}

// --- viewer side ---

#[derive(Clone, Debug, PartialEq)]
pub enum TunnelPhase {
    Connecting,
    /// Waiting for the person at the other machine to accept.
    Approval,
    Open,
    Closed,
}

/// One tunnel opened by this app or the CLI.
pub struct Tunnel {
    pub device_id: String,
    pub name: String,
    pub local: SocketAddr,
    pub target: String,
    pub started: Instant,
    pub phase: Mutex<TunnelPhase>,
    pub error: Mutex<Option<String>>,
    pub counters: Arc<Counters>,
    pub path: Mutex<Option<crate::direct::Path>>,
    stop: Notify,
    stopped: AtomicBool,
}

impl Tunnel {
    pub fn phase(&self) -> TunnelPhase {
        self.phase.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.stop.notify_one();
    }
}

/// Listens on `127.0.0.1:local_port` and forwards every connection to `rhost:rport` on the
/// device. The port is bound before the session starts, so a busy port fails at once. Runs
/// until [`Tunnel::stop`] or the session ends; `repaint` is called on every state change.
pub async fn open(
    t: client::Target,
    name: String,
    local_port: u16,
    rhost: &str,
    rport: u16,
    password: Option<String>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> Result<(Arc<Tunnel>, tokio::task::JoinHandle<()>)> {
    let kind = SessionKind::Tunnel { host: remote::parse_target(&remote::join_target(rhost, rport))?.0, port: rport };
    let listener = TcpListener::bind(("127.0.0.1", local_port)).await.map_err(|e| anyhow!("เปิดพอร์ต {local_port} ไม่ได้: {e}"))?;
    let tunnel = Arc::new(Tunnel {
        device_id: t.device_id.clone(),
        name,
        local: listener.local_addr()?,
        target: kind.target().unwrap_or_default(),
        started: Instant::now(),
        phase: Mutex::new(TunnelPhase::Connecting),
        error: Mutex::new(None),
        counters: Arc::default(),
        path: Mutex::new(None),
        stop: Notify::new(),
        stopped: AtomicBool::new(false),
    });
    let task = {
        let tunnel = tunnel.clone();
        tokio::spawn(async move {
            let res = run_viewer(&tunnel, t, kind, listener, password, repaint.clone()).await;
            if let Err(e) = res {
                tracing::warn!("tunnel {} → {}: {e:#}", tunnel.local, tunnel.target);
                tunnel.error.lock().unwrap().get_or_insert(format!("{e:#}"));
            } else {
                tracing::info!("tunnel {} → {} closed", tunnel.local, tunnel.target);
            }
            *tunnel.phase.lock().unwrap() = TunnelPhase::Closed;
            repaint();
        })
    };
    Ok((tunnel, task))
}

async fn run_viewer(
    tunnel: &Arc<Tunnel>,
    t: client::Target,
    kind: SessionKind,
    listener: TcpListener,
    password: Option<String>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    let set = |p: TunnelPhase| {
        *tunnel.phase.lock().unwrap() = p;
        repaint();
    };
    let stopped = async {
        if !tunnel.stopped.load(Ordering::Relaxed) {
            tunnel.stop.notified().await;
        }
    };
    let connect = async {
        let mut pending = client::connect(&t, &kind).await?;
        *tunnel.path.lock().unwrap() = Some(pending.path);
        if pending.need_click && !pending.need_password {
            set(TunnelPhase::Approval);
        }
        match pending.auth(password.as_deref(), || set(TunnelPhase::Approval)).await? {
            client::Auth::Ok(info) => Ok::<_, anyhow::Error>((pending.into_conn(), info)),
            client::Auth::Retry { .. } => bail!("รหัสผ่านไม่ถูกต้อง"),
        }
    };
    let (mut session, _info) = tokio::select! {
        r = connect => r?,
        _ = stopped => return Ok(()),
    };
    set(TunnelPhase::Open);

    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let mut mux = Mux::with_counters(out_tx, true, tunnel.counters.clone());
    let mut waiting: HashMap<u32, TcpStream> = HashMap::new();
    let mut next_id: u32 = 1;
    let mut ping = tokio::time::interval(remote::PING_EVERY);
    let mut stats = tokio::time::interval(Duration::from_secs(1));
    let conn = &mut session.conn;
    loop {
        tokio::select! {
            _ = tunnel.stop.notified() => {
                conn.close().await;
                return Ok(());
            }
            _ = ping.tick() => conn.send_json(json!({"t": "ping"})).await?,
            _ = stats.tick() => repaint(),
            a = listener.accept() => {
                let Ok((sock, from)) = a else { continue };
                if mux.len() + waiting.len() >= MAX_STREAMS {
                    tracing::warn!("tunnel {}: too many connections, refusing {from}", tunnel.local);
                    continue;
                }
                let id = next_id;
                next_id = next_id.wrapping_add(1).max(1);
                waiting.insert(id, sock);
                conn.send_json(json!({"t": "tcp_open", "id": id})).await?;
            }
            Some(plain) = out_rx.recv() => conn.send(&plain).await?,
            _ = mux.next_done() => {}
            m = conn.recv() => {
                let Some(plain) = m? else { bail!("{}", client::CLOSED) };
                match plain.first() {
                    Some(&proto::MSG_TUNNEL) => mux.on_data(&plain).await,
                    Some(&proto::MSG_JSON) => {
                        let Some(m) = remote::json_of(&plain) else { continue };
                        if mux.on_control(&m) {
                            continue;
                        }
                        let id = m["id"].as_u64().unwrap_or(0) as u32;
                        match m["t"].as_str().unwrap_or("") {
                            "tcp_opened" => {
                                if let Some(sock) = waiting.remove(&id) {
                                    mux.attach(id, sock);
                                }
                            }
                            "tcp_error" => {
                                // Dropping the socket closes the local connection.
                                waiting.remove(&id);
                                tracing::info!("tunnel {}: {}", tunnel.target, m["message"].as_str().unwrap_or(""));
                            }
                            "bye" => bail!("{}", client::CLOSED),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two muxes joined back to back (no encryption): a client connection on side A reaches an
    /// echo-with-half-close server through side B.
    #[tokio::test]
    async fn streams_flow_control_and_half_close() {
        // Target: reads everything until EOF, then answers with the byte count and closes.
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let taddr = target.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = target.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut n = 0usize;
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match s.read(&mut buf).await.unwrap() {
                            0 => break,
                            k => n += k,
                        }
                    }
                    s.write_all(format!("got {n}").as_bytes()).await.unwrap();
                    s.shutdown().await.unwrap();
                });
            }
        });

        let (a_out, mut a_rx) = mpsc::channel(64);
        let (b_out, mut b_rx) = mpsc::channel(64);
        let mut a = Mux::new(a_out, true);
        let mut b = Mux::new(b_out, false);
        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let faddr = front.local_addr().unwrap();

        let clients = tokio::spawn(async move {
            let mut results = Vec::new();
            for size in [0usize, 1, 3 * WINDOW as usize + 12345] {
                let mut c = TcpStream::connect(faddr).await.unwrap();
                let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
                c.write_all(&data).await.unwrap();
                c.shutdown().await.unwrap(); // half-close: we still read the answer
                let mut reply = String::new();
                c.read_to_string(&mut reply).await.unwrap();
                results.push(reply);
            }
            results
        });

        let mut next = 1u32;
        let mut finished = false;
        let mut results = None;
        let clients = clients;
        tokio::pin!(clients);
        while !finished {
            tokio::select! {
                Ok((s, _)) = front.accept() => {
                    a.attach(next, s);
                    let t = TcpStream::connect(taddr).await.unwrap();
                    b.attach(next, t);
                    next += 1;
                }
                Some(m) = a_rx.recv() => match m[0] {
                    proto::MSG_TUNNEL => b.on_data(&m).await,
                    _ => { b.on_control(&remote::json_of(&m).unwrap()); }
                },
                Some(m) = b_rx.recv() => match m[0] {
                    proto::MSG_TUNNEL => a.on_data(&m).await,
                    _ => { a.on_control(&remote::json_of(&m).unwrap()); }
                },
                _ = a.next_done() => {}
                _ = b.next_done() => {}
                r = &mut clients => { results = Some(r.unwrap()); finished = true; }
            }
        }
        let size = 3 * WINDOW as usize + 12345;
        assert_eq!(results.unwrap(), vec!["got 0".to_string(), "got 1".into(), format!("got {size}")]);
        assert_eq!(a.counters.up.load(Ordering::Relaxed), (size + 1) as u64);
        assert_eq!(b.counters.up.load(Ordering::Relaxed), (size + 1) as u64);
    }

    #[tokio::test]
    async fn ack_never_exceeds_window() {
        let (out, _rx) = mpsc::channel(4);
        let mut m = Mux::new(out, true);
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _c = TcpStream::connect(l.local_addr().unwrap()).await.unwrap();
        let (s, _) = l.accept().await.unwrap();
        m.attach(7, s);
        assert!(m.on_control(&json!({"t": "tcp_ack", "id": 7, "n": u32::MAX})));
        assert_eq!(m.streams[&7].credit.available_permits(), WINDOW as usize);
        assert!(m.on_control(&json!({"t": "tcp_close", "id": 7})));
        assert!(m.is_empty());
        assert!(!m.on_control(&json!({"t": "tcp_open", "id": 8})));
    }
}
