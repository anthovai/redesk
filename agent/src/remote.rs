//! Session types besides screen control (docs/PROTOCOL.md §11): what a session is for, the
//! tunnel target policy, and the encrypted channel shared by tunnel and terminal sessions on
//! both ends.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::crypto::Channel;
use crate::direct::{WsRx, WsTx};
use crate::proto;

/// Advertised in `register.features`: the server only asks agents that list them for tunnel
/// or terminal sessions, so older agents are never sent one.
pub const FEATURE_TUNNEL: &str = "tunnel1";
pub const FEATURE_TERMINAL: &str = "terminal1";

/// How often an idle tunnel or terminal viewer sends `ping`, and how long either end waits
/// for anything from the other before giving up.
pub const PING_EVERY: Duration = Duration::from_secs(30);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// Remote control: video, input, files (the original session type).
    Screen,
    /// TCP port forwarding to `host:port` as seen from the controlled machine.
    Tunnel { host: String, port: u16 },
    /// A shell on the controlled machine, running as the agent's user.
    Terminal,
}

impl SessionKind {
    /// From the server's session request (`type`, `target`); absent = screen.
    pub fn from_request(kind: Option<&str>, target: Option<&str>) -> Result<Self> {
        match kind.unwrap_or("screen") {
            "" | "screen" => Ok(SessionKind::Screen),
            "terminal" => Ok(SessionKind::Terminal),
            "tunnel" => {
                let (host, port) = parse_target(target.unwrap_or(""))?;
                Ok(SessionKind::Tunnel { host, port })
            }
            other => bail!("unknown session type {other}"),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            SessionKind::Screen => "screen",
            SessionKind::Tunnel { .. } => "tunnel",
            SessionKind::Terminal => "terminal",
        }
    }

    /// `host:port` of a tunnel (IPv6 in brackets), None otherwise.
    pub fn target(&self) -> Option<String> {
        match self {
            SessionKind::Tunnel { host, port } => Some(join_target(host, *port)),
            _ => None,
        }
    }

    /// What the person at the controlled machine is asked to allow (Thai).
    pub fn purpose(&self) -> String {
        match self {
            SessionKind::Screen => String::new(),
            SessionKind::Tunnel { .. } => format!("เปิด TCP tunnel ไปที่ {}", self.target().unwrap_or_default()),
            SessionKind::Terminal => "เปิดเทอร์มินัล (shell) บนเครื่องนี้".into(),
        }
    }
}

pub fn join_target(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

/// Parses `host:port` (`[v6]:port` for IPv6). Host names are limited to what DNS allows.
pub fn parse_target(s: &str) -> Result<(String, u16)> {
    let s = s.trim();
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, p) = rest.split_once("]:").context("target must be host:port")?;
        h.parse::<std::net::Ipv6Addr>().context("invalid IPv6 address")?;
        (h, p)
    } else {
        let (h, p) = s.rsplit_once(':').context("target must be host:port")?;
        if h.contains(':') {
            bail!("write IPv6 addresses in brackets: [::1]:22");
        }
        (h, p)
    };
    let port: u16 = port.parse().ok().filter(|p| *p > 0).context("invalid port")?;
    let ok_name = !host.is_empty()
        && host.len() <= 253
        && (host.parse::<IpAddr>().is_ok() || host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_'));
    if !ok_name {
        bail!("invalid host name");
    }
    Ok((host.to_ascii_lowercase(), port))
}

/// Whether a tunnel may reach this address: this machine only, or with `lan` also private
/// networks (RFC 1918, link-local, IPv6 ULA). Never the public internet, so a tunnel cannot
/// turn the controlled machine into an open proxy.
pub fn address_allowed(ip: IpAddr, lan: bool) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    if ip.is_loopback() {
        return true;
    }
    if !lan {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
    }
}

/// Resolves a tunnel target and keeps the addresses the policy allows. Every address a name
/// resolves to must be allowed (a name that also points elsewhere is refused), and only these
/// addresses are connected to afterwards, so the answer cannot change in between.
pub async fn resolve_allowed(host: &str, port: u16, lan: bool) -> Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| anyhow!("name lookup timed out"))?
        .with_context(|| format!("cannot resolve {host}"))?
        .collect();
    if addrs.is_empty() {
        bail!("cannot resolve {host}");
    }
    if let Some(bad) = addrs.iter().find(|a| !address_allowed(a.ip(), lan)) {
        bail!("{} is not allowed on this device", bad.ip());
    }
    Ok(addrs)
}

/// The encrypted channel of a tunnel or terminal session, on either end.
pub struct Conn {
    pub tx: WsTx,
    pub rx: WsRx,
    pub ch: Channel,
}

impl Conn {
    pub async fn send(&mut self, plain: &[u8]) -> Result<()> {
        let sealed = self.ch.seal(plain)?;
        self.tx.send(Message::Binary(sealed.into())).await?;
        Ok(())
    }

    pub async fn send_json(&mut self, v: serde_json::Value) -> Result<()> {
        self.send(&proto::json(&v)).await
    }

    /// The next decrypted message; None when the other side closed. Cancel-safe.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let m = tokio::time::timeout(IDLE_TIMEOUT, self.rx.next()).await.map_err(|_| anyhow!("no reply for {}s", IDLE_TIMEOUT.as_secs()))?;
            match m {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(Message::Binary(b))) => return Ok(Some(self.ch.open(&b)?)),
                Some(Ok(_)) => continue,
            }
        }
    }

    pub async fn close(&mut self) {
        let _ = self.send_json(serde_json::json!({"t": "bye"})).await;
        let _ = self.tx.close().await;
    }
}

/// Parses a `0x01` JSON message.
pub fn json_of(plain: &[u8]) -> Option<serde_json::Value> {
    (plain.first() == Some(&proto::MSG_JSON)).then(|| serde_json::from_slice(&plain[1..]).ok()).flatten()
}

/// Why the agent refused a tunnel or terminal session (`auth_result.reason`), in Thai.
pub fn explain_refusal(reason: &str, kind: &SessionKind) -> String {
    match reason {
        "tunnel_disabled" => "เครื่องปลายทางไม่อนุญาต TCP tunnel (เปิดได้ที่ ตั้งค่า → ความปลอดภัย ของเครื่องนั้น)".into(),
        "terminal_disabled" => "เครื่องปลายทางไม่อนุญาตเทอร์มินัล (เปิดได้ที่ ตั้งค่า → ความปลอดภัย ของเครื่องนั้น)".into(),
        "target_not_allowed" => format!(
            "เครื่องปลายทางไม่อนุญาตให้ต่อไปที่ {} (ได้เฉพาะ localhost เว้นแต่เครื่องนั้นอนุญาตเครือข่ายภายใน)",
            kind.target().unwrap_or_default()
        ),
        "recording_required" => "องค์กรกำหนดให้บันทึกเซสชัน แต่ TCP tunnel บันทึกไม่ได้".into(),
        "session_mismatch" => "เซิร์ฟเวอร์หรือเครื่องปลายทางยังไม่รองรับเซสชันประเภทนี้".into(),
        "denied" => "ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ".into(),
        "locked" => "เครื่องปลายทางล็อกการยืนยันตัวตนชั่วคราว".into(),
        "recording_failed" => "องค์กรกำหนดให้บันทึกเซสชันนี้ แต่เครื่องปลายทางเริ่มบันทึกไม่ได้".into(),
        "wrong_password" => "รหัสผ่านไม่ถูกต้อง".into(),
        r => format!("เครื่องปลายทางปฏิเสธ: {r}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("localhost:22").unwrap(), ("localhost".into(), 22));
        assert_eq!(parse_target("[::1]:3389").unwrap(), ("::1".into(), 3389));
        assert_eq!(parse_target("10.0.0.5:80").unwrap(), ("10.0.0.5".into(), 80));
        assert_eq!(parse_target("NAS.local:445").unwrap(), ("nas.local".into(), 445));
        for bad in ["", "host", "host:0", "host:99999", ":80", "a b:80", "[zz]:80", "x/y:80", "::1:22"] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
        assert_eq!(join_target("::1", 22), "[::1]:22");
        let k = SessionKind::from_request(Some("tunnel"), Some("127.0.0.1:8080")).unwrap();
        assert_eq!(k.target().as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(SessionKind::from_request(None, None).unwrap(), SessionKind::Screen);
        assert!(SessionKind::from_request(Some("tunnel"), None).is_err());
        assert!(SessionKind::from_request(Some("vpn"), None).is_err());
    }

    #[test]
    fn target_policy() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for a in ["127.0.0.1", "127.8.9.10", "::1", "::ffff:127.0.0.1"] {
            assert!(address_allowed(ip(a), false), "{a}");
        }
        for a in ["10.1.2.3", "192.168.1.10", "172.16.0.1", "169.254.1.1", "fd00::1", "fe80::1"] {
            assert!(!address_allowed(ip(a), false), "{a} without lan");
            assert!(address_allowed(ip(a), true), "{a} with lan");
        }
        for a in ["8.8.8.8", "1.1.1.1", "2001:4860::8888", "0.0.0.0", "::", "224.0.0.1", "255.255.255.255"] {
            assert!(!address_allowed(ip(a), true), "{a}");
        }
    }

    #[tokio::test]
    async fn resolve_checks_every_address() {
        assert!(resolve_allowed("127.0.0.1", 80, false).await.is_ok());
        assert!(resolve_allowed("localhost", 80, false).await.unwrap().iter().all(|a| a.ip().is_loopback()));
        assert!(resolve_allowed("10.0.0.1", 80, false).await.is_err());
        assert!(resolve_allowed("10.0.0.1", 80, true).await.is_ok());
        assert!(resolve_allowed("8.8.8.8", 53, true).await.is_err());
    }
}
