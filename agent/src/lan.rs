//! LAN facts, Wake-on-LAN and LAN discovery (UDP on the local network only).
//!
//! - `local_facts()` lists MACs and IPv4 subnets of up, non-loopback interfaces (sent at registration).
//! - `wake()` sends magic packets for MACs the server asks this agent to wake.
//! - `listen()` answers `redesk-discover v1` broadcasts; `discover()` finds agents on the LAN.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::signal::Agent;

/// UDP port agents listen on for discovery requests.
pub const DISCOVERY_PORT: u16 = 21120;
/// Discovery request payload (exact match, optional trailing newline).
pub const DISCOVER_MAGIC: &[u8] = b"redesk-discover v1";
/// UDP ports magic packets go to (discard + echo, the common WoL ports).
pub const WOL_PORTS: [u16; 2] = [9, 7];
/// Most MACs a single wake request may carry.
pub const MAX_WOL_MACS: usize = 16;
const MAX_PACKET: usize = 512;

pub type Mac = [u8; 6];

/// An IPv4 address of this machine with its prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LanAddr {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

/// Network facts reported to the server at registration.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetFacts {
    pub macs: Vec<String>,
    pub lan: Vec<LanAddr>,
}

/// An agent that answered a discovery broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanPeer {
    pub id: String,
    pub hostname: String,
    pub os: String,
    /// Source address of the reply (not part of the wire format).
    #[serde(skip_deserializing, default = "unspecified")]
    pub addr: IpAddr,
}

fn unspecified() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

struct Iface {
    mac: Option<Mac>,
    v4: Vec<LanAddr>,
}

/// Up, non-loopback interfaces with their MAC and IPv4 addresses.
fn interfaces() -> Vec<Iface> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .map(|i| Iface {
            mac: i.mac_addr.map(|m| m.octets()).filter(valid_mac),
            v4: i
                .ipv4
                .iter()
                .filter(|n| !n.addr().is_loopback() && !n.addr().is_unspecified())
                .map(|n| LanAddr { ip: n.addr(), prefix: n.prefix_len() })
                .collect(),
        })
        .collect()
}

pub fn local_facts() -> NetFacts {
    let mut f = NetFacts::default();
    let mut ifs = interfaces();
    // Interfaces with an address first; skip virtual NICs (locally administered MAC) without one.
    ifs.sort_by_key(|i| i.v4.is_empty());
    for i in ifs {
        if let Some(m) = i.mac.filter(|m| !i.v4.is_empty() || m[0] & 2 == 0) {
            let s = format_mac(&m);
            if !f.macs.contains(&s) && f.macs.len() < MAX_WOL_MACS {
                f.macs.push(s);
            }
        }
        for a in i.v4 {
            if !f.lan.contains(&a) && f.lan.len() < 16 {
                f.lan.push(a);
            }
        }
    }
    f
}

/// A unicast, non-zero hardware address (what a NIC that can be woken has).
pub fn valid_mac(m: &Mac) -> bool {
    m != &[0; 6] && m[0] & 1 == 0
}

/// Strict MAC parser: exactly six hex pairs separated by all ':' or all '-'.
pub fn parse_mac(s: &str) -> Option<Mac> {
    let b = s.as_bytes();
    if b.len() != 17 {
        return None;
    }
    let sep = b[2];
    if sep != b':' && sep != b'-' {
        return None;
    }
    let mut m = [0u8; 6];
    for (k, out) in m.iter_mut().enumerate() {
        let p = k * 3;
        if k > 0 && b[p - 1] != sep {
            return None;
        }
        let hex = std::str::from_utf8(&b[p..p + 2]).ok()?;
        if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        *out = u8::from_str_radix(hex, 16).ok()?;
    }
    valid_mac(&m).then_some(m)
}

pub fn format_mac(m: &Mac) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Magic packet: 6 x 0xFF followed by the MAC repeated 16 times.
pub fn magic_packet(m: &Mac) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for k in 0..16 {
        p[6 + k * 6..12 + k * 6].copy_from_slice(m);
    }
    p
}

/// Directed broadcast address of ip/prefix (None for /31 and /32, which have none).
pub fn broadcast_addr(ip: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix > 30 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

fn same_subnet(a: Ipv4Addr, b: LanAddr) -> bool {
    let mask = if b.prefix == 0 { 0 } else { u32::MAX << (32 - b.prefix.min(32)) };
    u32::from(a) & mask == u32::from(b.ip) & mask
}

/// Sends magic packets for every MAC to `targets` from `bind`. Returns packets sent.
fn send_magic(bind: Ipv4Addr, targets: &[SocketAddrV4], macs: &[Mac]) -> usize {
    let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(bind, 0)) else { return 0 };
    let _ = sock.set_broadcast(true);
    let mut sent = 0;
    for m in macs {
        let p = magic_packet(m);
        for t in targets {
            if sock.send_to(&p, t).is_ok() {
                sent += 1;
            }
        }
    }
    sent
}

/// Where magic packets go from one interface address.
fn wol_targets(a: LanAddr, ports: &[u16]) -> Vec<SocketAddrV4> {
    let mut dst = vec![Ipv4Addr::BROADCAST];
    if let Some(b) = broadcast_addr(a.ip, a.prefix) {
        dst.push(b);
    }
    dst.iter().flat_map(|ip| ports.iter().map(move |p| SocketAddrV4::new(*ip, *p))).collect()
}

/// Wakes `macs` from every interface (subnet broadcast + 255.255.255.255, ports 9 and 7).
/// Blocking; returns the number of packets sent.
pub fn wake(macs: &[Mac]) -> usize {
    let macs = &macs[..macs.len().min(MAX_WOL_MACS)];
    let mut sent = 0;
    for a in interfaces().into_iter().flat_map(|i| i.v4) {
        sent += send_magic(a.ip, &wol_targets(a, &WOL_PORTS), macs);
    }
    if sent == 0 {
        // No usable interface address: let the OS pick the route.
        let t: Vec<_> = WOL_PORTS.iter().map(|p| SocketAddrV4::new(Ipv4Addr::BROADCAST, *p)).collect();
        sent = send_magic(Ipv4Addr::UNSPECIFIED, &t, macs);
    }
    // Debug builds only: copy to a loopback port so tests can observe packets without LAN access.
    #[cfg(debug_assertions)]
    if let Some(a) = std::env::var("REDESK_WOL_DEBUG_TARGET").ok().and_then(|t| t.parse::<SocketAddrV4>().ok())
        && a.ip().is_loopback()
    {
        sent += send_magic(Ipv4Addr::LOCALHOST, &[a], macs);
    }
    sent
}

/// Handles a server `{"t":"wol","macs":[...]}` request: validates, sends, reports `wol_sent`.
pub async fn handle_wol(agent: Arc<Agent>, macs: Vec<String>, target: Option<String>) {
    let parsed: Vec<Mac> = macs.iter().take(MAX_WOL_MACS).filter_map(|s| parse_mac(s)).collect();
    if parsed.is_empty() {
        tracing::warn!("wol request without a valid MAC ignored");
        return;
    }
    let list = parsed.iter().map(format_mac).collect::<Vec<_>>().join(",");
    let p = parsed.clone();
    let sent = tokio::task::spawn_blocking(move || wake(&p)).await.unwrap_or(0);
    tracing::info!("wake-on-lan: {sent} packets for {list}");
    let target = target.filter(|t| t.len() == 9 && t.bytes().all(|c| c.is_ascii_digit())).unwrap_or_default();
    agent.report("", "wol_sent", json!({"macs": list, "packets": sent, "target": target}));
}

// --- discovery ---

fn is_discover(p: &[u8]) -> bool {
    let p = p.strip_suffix(b"\n").unwrap_or(p);
    p == DISCOVER_MAGIC
}

/// Parses a discovery reply; rejects malformed ids and oversized fields.
pub fn parse_reply(p: &[u8], from: IpAddr) -> Option<LanPeer> {
    if p.len() > MAX_PACKET {
        return None;
    }
    let mut peer: LanPeer = serde_json::from_slice(p).ok()?;
    if peer.id.len() != 9 || !peer.id.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if peer.hostname.len() > 128 || peer.os.len() > 64 {
        return None;
    }
    peer.addr = from;
    Some(peer)
}

/// Reply rate limits: per source and overall (limits use as a reflector).
struct Limiter {
    last: HashMap<IpAddr, Instant>,
    window: Instant,
    count: u32,
}

impl Limiter {
    const PER_SOURCE: Duration = Duration::from_millis(500);
    const PER_SECOND: u32 = 20;

    fn new() -> Self {
        Self { last: HashMap::new(), window: Instant::now(), count: 0 }
    }

    fn allow(&mut self, from: IpAddr, now: Instant) -> bool {
        if now.duration_since(self.window) >= Duration::from_secs(1) {
            self.window = now;
            self.count = 0;
        }
        if self.count >= Self::PER_SECOND {
            return false;
        }
        if self.last.get(&from).is_some_and(|t| now.duration_since(*t) < Self::PER_SOURCE) {
            return false;
        }
        if self.last.len() > 1024 {
            self.last.retain(|_, t| now.duration_since(*t) < Self::PER_SOURCE);
        }
        self.last.insert(from, now);
        self.count += 1;
        true
    }
}

/// Only answer this machine or hosts on one of its own subnets.
fn local_source(from: IpAddr, nets: &[LanAddr]) -> bool {
    match from {
        IpAddr::V4(v4) => v4.is_loopback() || nets.iter().any(|n| same_subnet(v4, *n)),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Answers discovery broadcasts until the socket fails. Runs only when `lan_discovery` is on.
pub async fn listen(agent: Arc<Agent>, port: u16) {
    let sock = match bind_listener(port) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("LAN discovery disabled: cannot bind UDP {port}: {e}");
            return;
        }
    };
    tracing::info!("LAN discovery listening on UDP {port}");
    let mut limiter = Limiter::new();
    let mut nets: Vec<LanAddr> = Vec::new();
    let mut nets_at: Option<Instant> = None;
    let mut buf = [0u8; MAX_PACKET + 1];
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("discovery recv: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        if n > MAX_PACKET || !is_discover(&buf[..n]) {
            continue;
        }
        if !agent.config.lock().unwrap().lan_discovery {
            continue;
        }
        let now = Instant::now();
        if nets_at.is_none_or(|t| now.duration_since(t) > Duration::from_secs(30)) {
            nets = tokio::task::spawn_blocking(|| local_facts().lan).await.unwrap_or_default();
            nets_at = Some(now);
        }
        if !local_source(from.ip(), &nets) || !limiter.allow(from.ip(), now) {
            continue;
        }
        let reply = json!({
            "id": agent.config.lock().unwrap().id,
            "hostname": hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
            "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        });
        let _ = sock.send_to(reply.to_string().as_bytes(), from).await;
    }
}

fn bind_listener(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Several agents (users) on one unix host may share the port; broadcasts reach all of them.
    // Not on Windows, where SO_REUSEADDR would let another process hijack the port.
    #[cfg(unix)]
    {
        s.set_reuse_address(true)?;
        s.set_reuse_port(true)?;
    }
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    s.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(s.into())
}

/// Broadcasts a discovery request on every interface (and to this machine) and collects
/// replies for `timeout`. Blocking: call it off the UI thread. Deduplicated by id.
pub fn discover(timeout: Duration) -> Vec<LanPeer> {
    discover_on(DISCOVERY_PORT, timeout)
}

pub fn discover_on(port: u16, timeout: Duration) -> Vec<LanPeer> {
    let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) else { return Vec::new() };
    let _ = sock.set_broadcast(true);
    let mut dst = vec![Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST];
    for a in interfaces().into_iter().flat_map(|i| i.v4) {
        if let Some(b) = broadcast_addr(a.ip, a.prefix)
            && !dst.contains(&b)
        {
            dst.push(b);
        }
    }
    for d in &dst {
        let _ = sock.send_to(DISCOVER_MAGIC, SocketAddrV4::new(*d, port));
    }
    let deadline = Instant::now() + timeout;
    let mut out: Vec<LanPeer> = Vec::new();
    let mut buf = [0u8; MAX_PACKET + 1];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let _ = sock.set_read_timeout(Some(left.max(Duration::from_millis(1))));
        let Ok((n, from)) = sock.recv_from(&mut buf) else { break };
        if let Some(p) = parse_reply(&buf[..n], from.ip())
            && !out.iter().any(|o| o.id == p.id)
            && out.len() < 256
        {
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.hostname.cmp(&b.hostname).then(a.id.cmp(&b.id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_packet_layout() {
        let m = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let p = magic_packet(&m);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for k in 0..16 {
            assert_eq!(&p[6 + k * 6..12 + k * 6], &m);
        }
    }

    #[test]
    fn mac_parsing_is_strict() {
        assert_eq!(parse_mac("00:11:22:aa:BB:cc"), Some([0x00, 0x11, 0x22, 0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_mac("00-11-22-aa-bb-cc"), Some([0x00, 0x11, 0x22, 0xaa, 0xbb, 0xcc]));
        for bad in [
            "", "00:11:22:33:44", "00:11:22:33:44:55:66", "00:11-22:33:44:55", "0011.2233.4455", "001122334455",
            "00:11:22:33:44:5g", " 0:11:22:33:44:55", "+0:11:22:33:44:55", "00:00:00:00:00:00", "ff:ff:ff:ff:ff:ff",
            "01:00:5e:00:00:01", "00:11:22:33:44:55\n",
        ] {
            assert_eq!(parse_mac(bad), None, "{bad:?}");
        }
        assert_eq!(format_mac(&parse_mac("0A-0B-0C-0D-0E-0F").unwrap()), "0a:0b:0c:0d:0e:0f");
    }

    #[test]
    fn broadcast_addresses() {
        let ip = Ipv4Addr::new(192, 168, 1, 37);
        assert_eq!(broadcast_addr(ip, 24), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(broadcast_addr(ip, 16), Some(Ipv4Addr::new(192, 168, 255, 255)));
        assert_eq!(broadcast_addr(Ipv4Addr::new(10, 0, 0, 5), 30), Some(Ipv4Addr::new(10, 0, 0, 7)));
        assert_eq!(broadcast_addr(Ipv4Addr::new(10, 1, 2, 3), 8), Some(Ipv4Addr::new(10, 255, 255, 255)));
        assert_eq!(broadcast_addr(ip, 0), Some(Ipv4Addr::BROADCAST));
        assert_eq!(broadcast_addr(ip, 31), None);
        assert_eq!(broadcast_addr(ip, 32), None);
        let t = wol_targets(LanAddr { ip, prefix: 24 }, &WOL_PORTS);
        assert!(t.contains(&SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 255), 9)));
        assert!(t.contains(&SocketAddrV4::new(Ipv4Addr::BROADCAST, 7)));
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn discovery_packets() {
        assert!(is_discover(b"redesk-discover v1"));
        assert!(is_discover(b"redesk-discover v1\n"));
        assert!(!is_discover(b"redesk-discover v2"));
        assert!(!is_discover(b"redesk-discover v1 extra"));
        let from: IpAddr = "192.168.1.9".parse().unwrap();
        let p = parse_reply(br#"{"id":"123456789","hostname":"pc","os":"windows x86_64"}"#, from).unwrap();
        assert_eq!((p.id.as_str(), p.hostname.as_str(), p.addr), ("123456789", "pc", from));
        assert!(parse_reply(br#"{"id":"12345678x","hostname":"pc","os":"x"}"#, from).is_none());
        assert!(parse_reply(br#"{"id":"123456789"}"#, from).is_none());
        assert!(parse_reply(b"not json", from).is_none());
        let big = format!(r#"{{"id":"123456789","hostname":"{}","os":"x"}}"#, "a".repeat(600));
        assert!(parse_reply(big.as_bytes(), from).is_none());
    }

    #[test]
    fn source_and_rate_limits() {
        let nets = [LanAddr { ip: Ipv4Addr::new(192, 168, 1, 37), prefix: 24 }];
        assert!(local_source("192.168.1.200".parse().unwrap(), &nets));
        assert!(local_source("127.0.0.1".parse().unwrap(), &nets));
        assert!(!local_source("192.168.2.1".parse().unwrap(), &nets));
        assert!(!local_source("8.8.8.8".parse().unwrap(), &nets));

        let mut l = Limiter::new();
        let t0 = Instant::now();
        let a: IpAddr = "192.168.1.2".parse().unwrap();
        assert!(l.allow(a, t0));
        assert!(!l.allow(a, t0 + Duration::from_millis(100)));
        assert!(l.allow(a, t0 + Duration::from_millis(600)));
        let mut l = Limiter::new();
        let ok = (0..50u8).filter(|k| l.allow(IpAddr::V4(Ipv4Addr::new(10, 0, 0, *k)), t0)).count();
        assert_eq!(ok, Limiter::PER_SECOND as usize);
    }

    #[tokio::test]
    async fn discover_finds_listening_agent() {
        use base64::Engine;
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let cfg = crate::config::Config::new("http://localhost:1".into(), key).unwrap();
        let id = cfg.id.clone();
        let agent = Agent::new(cfg, std::env::temp_dir().join("redesk-lan-test.toml"), None).unwrap();
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        tokio::spawn(listen(agent.clone(), port));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let peers = tokio::task::spawn_blocking(move || discover_on(port, Duration::from_millis(800))).await.unwrap();
        let me = peers.iter().find(|p| p.id == id).expect("agent answers discovery");
        assert!(me.addr.is_loopback());
        assert_eq!(peers.iter().filter(|p| p.id == id).count(), 1);

        // Turned off at runtime: no answer.
        agent.config.lock().unwrap().lan_discovery = false;
        tokio::time::sleep(Duration::from_millis(600)).await; // past the per-source limit
        let peers = tokio::task::spawn_blocking(move || discover_on(port, Duration::from_millis(500))).await.unwrap();
        assert!(peers.iter().all(|p| p.id != id));
    }

    #[test]
    fn magic_packet_reaches_local_listener() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let port = rx.local_addr().unwrap().port();
        let m = parse_mac("02:00:00:aa:bb:cc").unwrap();
        let n = send_magic(Ipv4Addr::LOCALHOST, &[SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)], &[m]);
        assert_eq!(n, 1);
        let mut buf = [0u8; 200];
        let (len, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], &magic_packet(&m));
    }
}
