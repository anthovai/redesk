//! Switch sides (like RustDesk): the viewer lets the person at the controlled machine control
//! the viewer's own machine in return. Only between two desktop apps, each running its agent.
//!
//! 1. Viewer machine V (its user clicked "switch sides" and confirmed): `offer` creates a
//!    one-time secret, kept in this process for 60 s; the app gets a server grant
//!    (`POST /api/sessions/{sid}/switch`) and sends `switch_sides {grant, secret}` to the
//!    controlled agent inside the end-to-end channel.
//! 2. Controlled machine H (its user accepted in the connection manager): `connect` redeems the
//!    grant over H's signaling connection and gets a relay ticket for a session to V, whose
//!    viewer is H's own app.
//! 3. V's agent gets that session marked `switch` and accepts it without password or prompt
//!    only if the viewer presents a secret from step 1 (`redeem`). The server never sees the
//!    secret and cannot use a grant itself; H cannot use the secret without a server grant.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::signal::Agent;

const OFFER_TTL: Duration = Duration::from_secs(60);

struct Offer {
    secret: String,
    /// Called once when the switched session starts (the app closes the original window).
    on_start: Box<dyn FnOnce() + Send>,
    expires: Instant,
}

static OFFERS: Mutex<Vec<Offer>> = Mutex::new(Vec::new());

/// Registers a new one-time secret for an incoming switched session; `on_start` runs when it is
/// used. Expired offers are dropped.
pub fn offer(on_start: impl FnOnce() + Send + 'static) -> String {
    let secret = crate::crypto::random_password(32);
    let mut offers = OFFERS.lock().unwrap();
    offers.retain(|o| o.expires > Instant::now());
    offers.push(Offer { secret: secret.clone(), on_start: Box::new(on_start), expires: Instant::now() + OFFER_TTL });
    secret
}

/// Takes a valid, unexpired offer matching `secret` (constant-time comparison) and returns its
/// callback. Each secret works once.
pub fn redeem(secret: &str) -> Option<Box<dyn FnOnce() + Send>> {
    let mut offers = OFFERS.lock().unwrap();
    offers.retain(|o| o.expires > Instant::now());
    let eq = |a: &str| a.len() == secret.len() && a.bytes().zip(secret.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    let i = offers.iter().position(|o| eq(&o.secret))?;
    Some(offers.swap_remove(i).on_start)
}

/// Forgets an offer that was not used (the other side declined, or the request failed).
pub fn withdraw(secret: &str) {
    OFFERS.lock().unwrap().retain(|o| o.secret != secret);
}

// --- controlled side: redeem the grant over signaling ---------------------------------------

static WAITERS: Mutex<Vec<(i64, oneshot::Sender<Value>)>> = Mutex::new(Vec::new());

/// Signaling delivered `{"t":"switch_ticket","req",…}`.
pub fn ticket_arrived(m: Value) {
    let req = m["req"].as_i64().unwrap_or(0);
    let mut w = WAITERS.lock().unwrap();
    if let Some(i) = w.iter().position(|(r, _)| *r == req) {
        let (_, tx) = w.swap_remove(i);
        let _ = tx.send(m);
    }
}

/// Redeems `grant` for a session to the viewer's machine; returns the ticket for this app's viewer.
pub async fn connect(agent: &Agent, grant: &str, direct: bool) -> Result<crate::account::SessionTicket> {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    let req = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    WAITERS.lock().unwrap().push((req, tx));
    let sent = agent.to_server.lock().unwrap().as_ref().is_some_and(|s| s.try_send(json!({"t": "switch_connect", "grant": grant, "req": req, "direct": direct}).to_string()).is_ok());
    let reply = match sent {
        true => tokio::time::timeout(Duration::from_secs(20), rx).await.ok().and_then(Result::ok),
        false => None,
    };
    WAITERS.lock().unwrap().retain(|(r, _)| *r != req);
    let Some(m) = reply else { bail!("เซิร์ฟเวอร์ไม่ตอบ (เครื่องนี้ออฟไลน์หรือเซิร์ฟเวอร์รุ่นเก่า)") };
    match m["error"].as_str() {
        Some("offline") => bail!("เครื่องของอีกฝ่ายออฟไลน์อยู่"),
        Some("invalid_grant") => bail!("คำเชิญหมดอายุแล้ว"),
        Some(e) => bail!("สลับฝั่งไม่สำเร็จ ({e})"),
        None => serde_json::from_value(json!({"ticket": m["ticket"], "device": m["device"]})).map_err(|e| anyhow!("bad switch ticket: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn offers_are_one_time() {
        let used = Arc::new(AtomicBool::new(false));
        let u = used.clone();
        let secret = offer(move || u.store(true, Ordering::Relaxed));
        assert_eq!(secret.len(), 32);
        assert!(redeem("wrong").is_none());
        assert!(redeem(&secret[..31]).is_none());
        let cb = redeem(&secret).expect("valid secret");
        cb();
        assert!(used.load(Ordering::Relaxed));
        assert!(redeem(&secret).is_none(), "second use");

        let other = offer(|| {});
        withdraw(&other);
        assert!(redeem(&other).is_none(), "withdrawn");
    }
}
