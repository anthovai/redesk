//! ReDesk agent library: shared by the `redesk-agent` CLI and the `redesk` desktop app.

pub mod account;
pub mod capture;
pub mod codec;
pub mod config;
pub mod crypto;
pub mod files;
pub mod input;
pub mod lan;
pub mod permissions;
pub mod power;
pub mod proto;
pub mod service;
pub mod session;
pub mod signal;
pub mod status;
pub mod ui;
pub mod updater;
pub mod viewer;

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

/// "123456789" -> "123 456 789"
pub fn format_id(id: &str) -> String {
    id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// Shared async runtime of the desktop app (agent signaling and viewer windows).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("redesk-rt").build().expect("tokio runtime")
    })
}

/// Starts the agent's network side in the background (for the desktop app).
pub fn spawn_runtime(agent: std::sync::Arc<signal::Agent>) {
    runtime().spawn(signal::run_forever(agent));
}
