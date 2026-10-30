//! Aegiron agent library: shared by the `aegiron-agent` CLI and the `aegiron` desktop app.

pub mod account;
pub mod audio;
pub mod block_input;
pub mod capture;
pub mod client;
pub mod codec;
pub mod config;
pub mod controls;
pub mod crypto;
pub mod direct;
pub mod files;
pub mod input;
pub mod instance;
pub mod lan;
pub mod local;
pub mod net;
pub mod permissions;
pub mod power;
pub mod privacy;
pub mod proto;
pub mod recording;
pub mod remote;
pub mod resolution;
pub mod service;
pub mod session;
pub mod signal;
pub mod status;
pub mod switch;
pub mod terminal;
pub mod tunnel;
pub mod ui;
pub mod updater;
#[cfg(target_os = "macos")]
pub mod url_event;
pub mod viewer;
pub mod voice;
pub mod whiteboard;

use anyhow::{Result, anyhow};
use argon2::password_hash::{PasswordHasher, SaltString};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

/// Short key fingerprint, e.g. "9371:520b:f9c5:c8fc" (same as the web console shows).
pub fn fingerprint(pk: &[u8]) -> String {
    let h = Sha256::digest(pk);
    h[..8].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().chunks(2).map(|c| c.concat()).collect::<Vec<_>>().join(":")
}

pub fn identity(c: &config::Config) -> Result<SigningKey> {
    let seed: [u8; 32] = B64.decode(&c.signing_key)?.try_into().map_err(|_| anyhow!("bad signing key"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// argon2id PHC string for the permanent password.
pub fn hash_password(pw: &str) -> Result<String> {
    let salt = SaltString::encode_b64(&crypto::random_bytes::<16>()).map_err(|e| anyhow!("{e}"))?;
    Ok(argon2::Argon2::default().hash_password(pw.as_bytes(), &salt).map_err(|e| anyhow!("{e}"))?.to_string())
}

/// "123456789" -> "123 456 789"; custom IDs (letters) are shown as they are.
pub fn format_id(id: &str) -> String {
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return id.to_string();
    }
    id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// A device ID as the server accepts it: 9-16 digits (agents generate 9), or a custom ID an
/// administrator set: 6-32 lowercase letters, digits, '_' or '-', starting with a letter.
pub fn valid_device_id(id: &str) -> bool {
    let b = id.as_bytes();
    if b.iter().all(u8::is_ascii_digit) {
        return (9..=16).contains(&b.len());
    }
    (6..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_ids() {
        for ok in ["123456789", "1234567890123456", "front-desk", "pc_01a"] {
            assert!(super::valid_device_id(ok), "{ok}");
        }
        for bad in ["12345678", "12345678901234567", "Front-desk", "1abcdef", "abc", "pc 01", "pc/../x", "", "ab\u{e9}cdef"] {
            assert!(!super::valid_device_id(bad), "{bad}");
        }
        assert_eq!(super::format_id("123456789"), "123 456 789");
        assert_eq!(super::format_id("front-desk"), "front-desk");
    }
}

/// Shared async runtime of the desktop app (agent signaling and viewer windows).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("aegiron-rt").build().expect("tokio runtime")
    })
}

/// Starts the agent's network side in the background (for the desktop app).
pub fn spawn_runtime(agent: std::sync::Arc<signal::Agent>) {
    runtime().spawn(signal::run_forever(agent));
}
