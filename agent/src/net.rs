//! Outbound connections to the Aegiron server, optionally through a SOCKS5 or HTTP CONNECT proxy.
//!
//! The proxy only carries bytes: TLS to the server is still negotiated end to end and verified
//! exactly as without a proxy (rustls with the system roots), so a proxy can never weaken it.
//! Proxy credentials live in `agent.toml` (owner-only, like the account token) and are sent
//! only to the proxy (SOCKS5 username/password, RFC 1929, or `Proxy-Authorization: Basic`);
//! they are never put into a URL.

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type WsError = tokio_tungstenite::tungstenite::Error;
pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PROXY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    /// SOCKS5 (the proxy resolves the server name).
    Socks5,
    /// HTTP proxy using CONNECT tunnels.
    Http,
}

/// `[proxy]` in agent.toml.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Stored like the other secrets in agent.toml (owner-only file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

impl ProxyConfig {
    /// Parses `socks5://host:port` or `http://host:port`. Credentials in the address are refused
    /// on purpose: give them separately so they never end up in a URL, a log or shell history.
    pub fn parse(addr: &str) -> Result<Self> {
        let (scheme, rest) = addr.trim().split_once("://").unwrap_or(("socks5", addr.trim()));
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => ProxyKind::Socks5,
            "http" => ProxyKind::Http,
            other => bail!("unsupported proxy type {other} (use socks5:// or http://)"),
        };
        let rest = rest.trim_end_matches('/');
        if rest.contains('@') {
            bail!("do not put credentials in the proxy address; use --proxy-user (password is prompted)");
        }
        let (host, port) = split_host_port(rest).ok_or_else(|| anyhow::anyhow!("proxy address must be host:port"))?;
        let p = Self { kind, host, port, username: None, password: None };
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        if self.host.is_empty() || self.host.chars().any(|c| c.is_whitespace() || c == '/' || c == '@') {
            bail!("invalid proxy host");
        }
        if self.port == 0 {
            bail!("invalid proxy port");
        }
        let long = |s: &Option<String>| s.as_ref().is_some_and(|s| s.len() > 255);
        if long(&self.username) || long(&self.password) {
            bail!("proxy username and password must be at most 255 bytes");
        }
        Ok(())
    }

    fn credentials(&self) -> Option<(&str, &str)> {
        self.username.as_deref().filter(|u| !u.is_empty()).map(|u| (u, self.password.as_deref().unwrap_or("")))
    }

    /// The proxy without credentials, for display.
    pub fn display(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
        };
        format!("{scheme}://{}", join_host_port(&self.host, self.port))
    }
}

fn split_host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (h, p)
    } else {
        s.rsplit_once(':')?
    };
    Some((host.to_string(), port.parse().ok()?))
}

fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

static PROXY: RwLock<Option<ProxyConfig>> = RwLock::new(None);

/// Sets the proxy used by every later server connection (called when the config is loaded/saved).
pub fn set_proxy(p: Option<ProxyConfig>) {
    *PROXY.write().unwrap() = p;
}

pub fn proxy() -> Option<ProxyConfig> {
    PROXY.read().unwrap().clone()
}

/// The configured proxy for ureq (HTTPS API calls). None keeps ureq's default behaviour.
pub fn ureq_proxy() -> Option<ureq::Proxy> {
    let p = proxy()?;
    let proto = match p.kind {
        ProxyKind::Socks5 => ureq::ProxyProtocol::Socks5h,
        ProxyKind::Http => ureq::ProxyProtocol::Http,
    };
    let mut b = ureq::Proxy::builder(proto).host(&p.host).port(p.port);
    if let Some((u, pw)) = p.credentials() {
        b = b.username(u).password(pw);
    }
    match b.build() {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("ignoring invalid proxy settings: {e}");
            None
        }
    }
}

/// Applies the configured proxy to a ureq agent configuration.
pub fn with_proxy(b: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    match ureq_proxy() {
        Some(p) => b.proxy(Some(p)),
        None => b,
    }
}

/// WebSocket to the server (signaling or relay), through the proxy when one is configured.
/// TLS (wss) runs inside the tunnel with the same certificate verification as a direct connection.
pub async fn connect_ws<R: IntoClientRequest + Unpin>(req: R) -> Result<(Ws, Response), WsError> {
    let req = req.into_client_request()?;
    let Some(p) = proxy() else {
        return tokio_tungstenite::connect_async(req).await;
    };
    let uri = req.uri();
    let host = uri.host().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });
    let tcp = tunnel(&p, &host, port).await.map_err(WsError::Io)?;
    tokio_tungstenite::client_async_tls(req, tcp).await
}

/// Opens a TCP tunnel to `host:port` through the proxy.
pub async fn tunnel(p: &ProxyConfig, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let fut = async {
        let mut s = TcpStream::connect((p.host.as_str(), p.port)).await?;
        s.set_nodelay(true)?;
        match p.kind {
            ProxyKind::Socks5 => socks5_connect(&mut s, p.credentials(), host, port).await?,
            ProxyKind::Http => http_connect(&mut s, p.credentials(), host, port).await?,
        }
        Ok(s)
    };
    tokio::time::timeout(PROXY_TIMEOUT, fut).await.map_err(|_| io_err("proxy timed out"))?
}

fn io_err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// RFC 1928 CONNECT with optional RFC 1929 username/password authentication.
async fn socks5_connect<S>(s: &mut S, creds: Option<(&str, &str)>, host: &str, port: u16) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if creds.is_some() {
        s.write_all(&[5, 2, 0x00, 0x02]).await?;
    } else {
        s.write_all(&[5, 1, 0x00]).await?;
    }
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await?;
    if r[0] != 5 {
        return Err(io_err("not a SOCKS5 proxy"));
    }
    match (r[1], creds) {
        (0x00, _) => {}
        (0x02, Some((u, pw))) => {
            let mut m = vec![1, u.len() as u8];
            m.extend_from_slice(u.as_bytes());
            m.push(pw.len() as u8);
            m.extend_from_slice(pw.as_bytes());
            s.write_all(&m).await?;
            s.read_exact(&mut r).await?;
            if r[1] != 0 {
                return Err(io_err("SOCKS5 proxy rejected the username or password"));
            }
        }
        (0x02, None) => return Err(io_err("SOCKS5 proxy requires a username and password")),
        _ => return Err(io_err("SOCKS5 proxy offered no acceptable authentication method")),
    }
    let mut req = vec![5, 1, 0];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            req.push(1);
            req.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            req.push(4);
            req.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.len() > 255 {
                return Err(io_err("host name too long"));
            }
            req.push(3);
            req.push(host.len() as u8);
            req.extend_from_slice(host.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0 {
        return Err(io_err(format!("SOCKS5 proxy could not connect (code {})", head[1])));
    }
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            l[0] as usize
        }
        _ => return Err(io_err("bad SOCKS5 reply")),
    };
    let mut rest = vec![0u8; skip + 2];
    s.read_exact(&mut rest).await?;
    Ok(())
}

/// HTTP/1.1 CONNECT. Reads the reply byte by byte so nothing after the header is consumed.
async fn http_connect<S>(s: &mut S, creds: Option<(&str, &str)>, host: &str, port: u16) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let target = join_host_port(host, port);
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: Aegiron/{}\r\n", env!("CARGO_PKG_VERSION"));
    if let Some((u, pw)) = creds {
        req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", B64.encode(format!("{u}:{pw}"))));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;
    let mut head = Vec::with_capacity(256);
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return Err(io_err("HTTP proxy reply too long"));
        }
        s.read_exact(&mut b).await?;
        head.push(b[0]);
    }
    let status = String::from_utf8_lossy(&head);
    let code = status.split_whitespace().nth(1).unwrap_or("");
    match code {
        c if c.starts_with('2') => Ok(()),
        "407" => Err(io_err("HTTP proxy requires authentication (407)")),
        c => Err(io_err(format!("HTTP proxy refused CONNECT ({c})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parse_refuses_credentials_in_address() {
        let p = ProxyConfig::parse("socks5://proxy.corp:1080").unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port), (ProxyKind::Socks5, "proxy.corp", 1080));
        let p = ProxyConfig::parse("http://[::1]:3128").unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port), (ProxyKind::Http, "::1", 3128));
        assert!(ProxyConfig::parse("http://user:pw@proxy:3128").is_err());
        assert!(ProxyConfig::parse("ftp://proxy:21").is_err());
        assert!(ProxyConfig::parse("http://proxy").is_err());
        let mut p = ProxyConfig::parse("http://proxy:1").unwrap();
        p.password = Some("secret".into());
        assert!(!format!("{p:?}").contains("secret"));
        assert!(!p.display().contains("secret"));
    }

    /// Minimal proxy for tests: SOCKS5 (user "u"/"p") or HTTP CONNECT (Basic u:p), then pipes bytes.
    async fn fake_proxy(kind: ProxyKind) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            let target = match kind {
                ProxyKind::Socks5 => {
                    let mut h = [0u8; 2];
                    c.read_exact(&mut h).await.unwrap();
                    let mut m = vec![0u8; h[1] as usize];
                    c.read_exact(&mut m).await.unwrap();
                    assert!(m.contains(&2));
                    c.write_all(&[5, 2]).await.unwrap();
                    let mut v = [0u8; 2];
                    c.read_exact(&mut v).await.unwrap();
                    let mut u = vec![0u8; v[1] as usize];
                    c.read_exact(&mut u).await.unwrap();
                    let mut pl = [0u8; 1];
                    c.read_exact(&mut pl).await.unwrap();
                    let mut pw = vec![0u8; pl[0] as usize];
                    c.read_exact(&mut pw).await.unwrap();
                    let ok = u == b"u" && pw == b"p";
                    c.write_all(&[1, if ok { 0 } else { 1 }]).await.unwrap();
                    if !ok {
                        return;
                    }
                    let mut r = [0u8; 4];
                    c.read_exact(&mut r).await.unwrap();
                    assert_eq!(r[3], 3, "host name must be resolved by the proxy");
                    let mut l = [0u8; 1];
                    c.read_exact(&mut l).await.unwrap();
                    let mut host = vec![0u8; l[0] as usize];
                    c.read_exact(&mut host).await.unwrap();
                    let mut port = [0u8; 2];
                    c.read_exact(&mut port).await.unwrap();
                    c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                    format!("{}:{}", String::from_utf8(host).unwrap().replace("localhost", "127.0.0.1"), u16::from_be_bytes(port))
                }
                ProxyKind::Http => {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        c.read_exact(&mut b).await.unwrap();
                        head.push(b[0]);
                    }
                    let head = String::from_utf8(head).unwrap();
                    if !head.contains(&format!("Proxy-Authorization: Basic {}", B64.encode("u:p"))) {
                        c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await.unwrap();
                        return;
                    }
                    c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                    head.split_whitespace().nth(1).unwrap().replace("localhost", "127.0.0.1")
                }
            };
            let mut up = TcpStream::connect(target).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut c, &mut up).await;
        });
        port
    }

    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            let (mut r, mut w) = c.split();
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });
        port
    }

    #[tokio::test]
    async fn tunnels_through_socks5_and_http_with_auth() {
        for kind in [ProxyKind::Socks5, ProxyKind::Http] {
            let echo = echo_server().await;
            let port = fake_proxy(kind).await;
            let p = ProxyConfig { kind, host: "127.0.0.1".into(), port, username: Some("u".into()), password: Some("p".into()) };
            let mut s = tunnel(&p, "localhost", echo).await.unwrap();
            s.write_all(b"ping").await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping", "{kind:?}");

            // Wrong password is refused.
            let port = fake_proxy(kind).await;
            let bad = ProxyConfig { password: Some("x".into()), port, ..p };
            assert!(tunnel(&bad, "localhost", echo).await.is_err(), "{kind:?}");
        }
    }

    #[tokio::test]
    async fn websocket_through_proxy() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (c, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(c).await.unwrap();
            use futures_util::{SinkExt, StreamExt};
            let m = ws.next().await.unwrap().unwrap();
            ws.send(m).await.unwrap();
        });
        let port = fake_proxy(ProxyKind::Http).await;
        let p = ProxyConfig { kind: ProxyKind::Http, host: "127.0.0.1".into(), port, username: Some("u".into()), password: Some("p".into()) };
        let s = tunnel(&p, "localhost", ws_port).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async_tls(format!("ws://localhost:{ws_port}/x"), s).await.unwrap();
        use futures_util::{SinkExt, StreamExt};
        ws.send(tokio_tungstenite::tungstenite::Message::Text("hi".into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap().as_str(), "hi");
    }
}
