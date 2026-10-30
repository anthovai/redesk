//! Signing in from the desktop app (like a RustDesk account): HTTPS calls to the Aegiron API.
//!
//! Sign-in returns the server's identity key over the server's own TLS connection, so a new
//! device can enroll without anyone copying keys or tokens by hand.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::json;

use crate::config::check_server_url;

fn agent() -> ureq::Agent {
    crate::net::with_proxy(ureq::Agent::config_builder())
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("Aegiron/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: String,
}

/// Turns server error messages into Thai for the person at the screen.
fn explain(status: u16, msg: &str) -> String {
    match (status, msg) {
        (401, m) if m.contains("authentication code") => "รหัสยืนยันตัวตน (2FA) ไม่ถูกต้อง".into(),
        (401, _) => "อีเมลหรือรหัสผ่านไม่ถูกต้อง".into(),
        (429, _) => "ลองเข้าสู่ระบบหลายครั้งเกินไป กรุณารอสักครู่".into(),
        _ => format!("เซิร์ฟเวอร์ตอบกลับ {status}: {msg}"),
    }
}

fn call<T: serde::de::DeserializeOwned>(req: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<T, (u16, String)> {
    let mut res = req.map_err(|e| (0, format!("เชื่อมต่อเซิร์ฟเวอร์ไม่ได้: {e}")))?;
    let status = res.status().as_u16();
    if status >= 400 {
        let msg = res.body_mut().read_json::<ApiError>().map(|e| e.error).unwrap_or_default();
        return Err((status, explain(status, &msg)));
    }
    res.body_mut().read_json::<T>().map_err(|e| (status, format!("คำตอบจากเซิร์ฟเวอร์ไม่ถูกต้อง: {e}")))
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub email: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub user: User,
    pub token: String,
    pub server_key: String,
    pub enroll_token: String,
    /// New "remember this device" token, when one was asked for with a correct 2FA code.
    pub trust_token: Option<String>,
}

pub enum Login {
    Ok(Session),
    /// The account has 2FA. `trust_rejected`: the remembered-device token that was sent is
    /// expired or revoked and should be forgotten.
    NeedTotp { trust_rejected: bool },
}

/// One password sign-in attempt from the app.
#[derive(Debug, Clone, Default)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    pub totp: String,
    /// Remembered-device token for this account (skips the TOTP step, not the password).
    pub trust_token: Option<String>,
    /// Ask the server to remember this device for 30 days (only honoured with a valid code).
    pub remember: bool,
}

#[derive(Deserialize)]
struct LoginReply {
    #[serde(default)]
    need_totp: bool,
    #[serde(default)]
    trust_rejected: bool,
    user: Option<User>,
    token: Option<String>,
    server_key: Option<String>,
    enroll_token: Option<String>,
    trust_token: Option<String>,
}

/// The JSON body of POST /api/agent/login. Secrets only ever travel in this body (HTTPS),
/// never in a URL.
fn login_body(req: &LoginRequest, device: &str) -> serde_json::Value {
    let mut body = json!({"email": req.email.trim(), "password": req.password, "totp": req.totp.trim(), "device": device});
    if let Some(t) = req.trust_token.as_deref().filter(|t| !t.is_empty()) {
        body["trust_token"] = json!(t);
    }
    if req.remember && !req.totp.trim().is_empty() {
        body["remember_device"] = json!(true);
    }
    body
}

pub fn login(server: &str, insecure: bool, req: &LoginRequest) -> Result<Login> {
    check_server_url(server, insecure)?;
    let device = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    let r: LoginReply =
        call(agent().post(&format!("{server}/api/agent/login")).header("X-Aegiron", "1").send_json(login_body(req, &device)))
            .map_err(|(_, m)| anyhow!(m))?;
    if r.need_totp {
        return Ok(Login::NeedTotp { trust_rejected: r.trust_rejected });
    }
    let trust_token = r.trust_token.filter(|t| !t.is_empty() && t.len() <= 128);
    match (r.user, r.token, r.server_key, r.enroll_token) {
        (Some(user), Some(token), Some(server_key), Some(enroll_token)) => {
            Ok(Login::Ok(Session { user, token, server_key, enroll_token, trust_token }))
        }
        _ => bail!("เซิร์ฟเวอร์นี้ยังไม่รองรับการเข้าสู่ระบบจากโปรแกรม (ต้องเป็น Aegiron 0.3 ขึ้นไป)"),
    }
}

pub fn logout(server: &str, token: &str) {
    let _ = agent().post(&format!("{server}/api/auth/logout")).header("X-Aegiron", "1").header("Authorization", &format!("Bearer {token}")).send_empty();
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookDevice {
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub online: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookEntry {
    pub device_id: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub tags: Vec<String>,
    pub device: Option<BookDevice>,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(d)?.unwrap_or_default())
}

impl BookEntry {
    pub fn title(&self) -> String {
        let d = self.device.as_ref();
        [Some(self.alias.as_str()), d.map(|d| d.alias.as_str()), d.map(|d| d.hostname.as_str())]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or("ไม่มีชื่อ")
            .to_string()
    }
    pub fn online(&self) -> bool {
        self.device.as_ref().is_some_and(|d| d.online)
    }
}

/// Address book of the signed-in user. Err((401, _)) means the sign-in expired.
pub fn address_book(server: &str, token: &str) -> Result<Vec<BookEntry>, (u16, String)> {
    #[derive(Deserialize)]
    struct R {
        entries: Vec<BookEntry>,
    }
    call::<R>(agent().get(&format!("{server}/api/address-book")).header("Authorization", &format!("Bearer {token}")).call())
        .map(|r| r.entries)
}

/// A one-time link that signs the browser in and opens `next` (e.g. "/connect/123456789").
pub fn handoff_url(server: &str, token: &str, next: &str) -> Result<String, (u16, String)> {
    #[derive(Deserialize)]
    struct R {
        code: String,
    }
    let r: R = call(
        agent()
            .post(&format!("{server}/api/auth/handoff"))
            .header("X-Aegiron", "1")
            .header("Authorization", &format!("Bearer {token}"))
            .send_empty(),
    )?;
    Ok(format!("{server}/api/auth/handoff/{}?next={next}", r.code))
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ticket {
    pub sid: String,
    pub token: String,
    pub relay_path: String,
    /// Direct/P2P candidates, when requested and the agent offered them.
    #[serde(default)]
    pub direct: Option<crate::direct::Ticket>,
    /// Session type the server started ("screen", "tunnel", "terminal"); absent on servers
    /// that predate tunnel and terminal sessions.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TicketDevice {
    pub id: String,
    pub public_key: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub os: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionTicket {
    pub ticket: Ticket,
    pub device: TicketDevice,
}

/// Asks the server to wake the remote agent and returns this viewer's relay ticket.
/// `direct` also asks for direct/P2P candidates (a query flag, which older servers ignore).
pub fn start_session(server: &str, token: &str, device_id: &str, direct: bool) -> Result<SessionTicket, String> {
    start_session_of(server, token, device_id, direct, "screen", None)
}

/// Like [`start_session`] for any session type: `kind` "screen", "tunnel" (with `target`
/// `host:port`) or "terminal".
pub fn start_session_of(server: &str, token: &str, device_id: &str, direct: bool, kind: &str, target: Option<&str>) -> Result<SessionTicket, String> {
    let q = if direct { "?direct=1" } else { "" };
    let mut body = json!({"device_id": device_id});
    if kind != "screen" {
        body["type"] = json!(kind);
        body["target"] = json!(target);
    }
    call::<SessionTicket>(
        agent()
            .post(&format!("{server}/api/sessions{q}"))
            .header("X-Aegiron", "1")
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(&body),
    )
    .map_err(|(status, msg)| match status {
        401 => "การเข้าสู่ระบบหมดอายุ กรุณาเข้าสู่ระบบใหม่".into(),
        403 if kind == "screen" => "คุณไม่มีสิทธิ์เชื่อมต่อเครื่องนี้".into(),
        404 => "ไม่พบ ID นี้ในระบบ".into(),
        409 => "เครื่องปลายทางออฟไลน์อยู่".into(),
        422 if msg.contains("does not support") => "Aegiron บนเครื่องปลายทางยังไม่รองรับเซสชันประเภทนี้ (ต้องอัปเดตเครื่องนั้น)".into(),
        422 if msg.contains("recorded") => "องค์กรกำหนดให้บันทึกเซสชัน จึงเปิด TCP tunnel ไปเครื่องนี้ไม่ได้ (tunnel บันทึกไม่ได้)".into(),
        400 if msg.contains("target") => "ปลายทางของ tunnel ต้องเป็น host:port".into(),
        503 => "เครื่องปลายทางไม่ว่าง ลองใหม่อีกครั้ง".into(),
        _ => msg,
    })
}

// --- single sign-on (OIDC), like RustDesk's /api/oidc/auth + /api/oidc/auth-query ---

/// Starts SSO sign-in. Returns `(code, url)`: open `url` in the browser, then call
/// [`oidc_poll`] with `code` every ~2 seconds until it returns a session (valid 5 minutes).
pub fn oidc_start(server: &str, insecure: bool, device: &str) -> Result<(String, String)> {
    check_server_url(server, insecure)?;
    let server = server.trim_end_matches('/');
    #[derive(Deserialize)]
    struct R {
        code: String,
        url: String,
    }
    let r: R = call(agent().post(&format!("{server}/api/oidc/auth")).header("X-Aegiron", "1").send_json(&json!({"device": device})))
        .map_err(|(status, m)| match status {
            400 => anyhow!("เซิร์ฟเวอร์ยังไม่ได้เปิดการเข้าสู่ระบบด้วย SSO"),
            404 | 405 => anyhow!("เซิร์ฟเวอร์นี้ยังไม่รองรับการเข้าสู่ระบบด้วย SSO"),
            _ => anyhow!(m),
        })?;
    // Only a page on the same server: never open a URL from elsewhere.
    let tail = r.url.strip_prefix("/api/oidc/login?app=").unwrap_or("");
    if r.code.is_empty() || tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        bail!("คำตอบจากเซิร์ฟเวอร์ไม่ถูกต้อง");
    }
    Ok((r.code, format!("{server}{}", r.url)))
}

/// Checks whether the browser part of SSO sign-in finished. `Ok(None)` = still waiting.
pub fn oidc_poll(server: &str, insecure: bool, code: &str) -> Result<Option<Session>> {
    check_server_url(server, insecure)?;
    let server = server.trim_end_matches('/');
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        pending: bool,
        user: Option<User>,
        token: Option<String>,
        server_key: Option<String>,
        enroll_token: Option<String>,
    }
    let r: R = call(agent().get(&format!("{server}/api/oidc/auth-query")).query("code", code).call()).map_err(|(status, m)| match status {
        404 => anyhow!("คำขอเข้าสู่ระบบหมดอายุ กรุณาลองใหม่"),
        403 => anyhow!(oidc_reason(&m)),
        _ => anyhow!(m),
    })?;
    if r.pending {
        return Ok(None);
    }
    match (r.user, r.token, r.server_key, r.enroll_token) {
        (Some(user), Some(token), Some(server_key), Some(enroll_token)) => {
            Ok(Some(Session { user, token, server_key, enroll_token, trust_token: None }))
        }
        _ => bail!("คำตอบจากเซิร์ฟเวอร์ไม่ถูกต้อง"),
    }
}

/// Thai text for "sign-in failed: <reason>" from the server.
fn oidc_reason(msg: &str) -> String {
    match msg.rsplit(": ").next().unwrap_or("") {
        "no_account" => "ยังไม่มีบัญชี Aegiron สำหรับอีเมลนี้ กรุณาติดต่อผู้ดูแลระบบ",
        "domain_not_allowed" => "โดเมนอีเมลนี้ไม่ได้รับอนุญาต",
        "email_unverified" => "อีเมลของบัญชี SSO ยังไม่ได้รับการยืนยัน",
        "no_email" => "บัญชี SSO ไม่มีอีเมล",
        "disabled_account" => "บัญชีนี้ถูกปิดใช้งาน",
        "idp_error" => "ผู้ให้บริการ SSO ปฏิเสธการเข้าสู่ระบบ",
        _ => "เข้าสู่ระบบด้วย SSO ไม่สำเร็จ",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_body_sends_trust_only_when_set() {
        let mut req = LoginRequest { email: " a@b.c ".into(), password: "pw".into(), ..Default::default() };
        let b = login_body(&req, "mac");
        assert_eq!(b["email"], "a@b.c");
        assert!(b.get("trust_token").is_none() && b.get("remember_device").is_none());
        // Remember is only asked for together with a code.
        req.remember = true;
        assert!(login_body(&req, "mac").get("remember_device").is_none());
        req.totp = "123456".into();
        assert_eq!(login_body(&req, "mac")["remember_device"], true);
        req.trust_token = Some("tok".into());
        assert_eq!(login_body(&req, "mac")["trust_token"], "tok");
    }
}
