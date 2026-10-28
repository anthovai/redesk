//! Agent configuration, stored as TOML with owner-only permissions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Verification {
    /// One-time password, rotated after every successful login.
    Temporary,
    /// Fixed password set with `set-password`.
    Permanent,
    /// Either of the above.
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ApproveMode {
    /// A correct password is enough (unattended access).
    Password,
    /// The local user must accept; no password needed.
    Click,
    /// Correct password and local acceptance.
    Both,
}

/// The person signed in from the desktop app (bearer token for the Aegiron API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub token: String,
}

/// Transport policy for talking to the server: https, or plain http only to this machine
/// (or when explicitly allowed). Never falls back silently.
pub fn check_server_url(url: &str, allow_insecure: bool) -> Result<()> {
    let (scheme, rest) = url.split_once("://").context("server address must start with https://")?;
    match scheme {
        "https" => Ok(()),
        "http" => {
            let host = rest.split(['/', ':']).next().unwrap_or("");
            let loopback = matches!(host, "localhost" | "127.0.0.1") || rest.starts_with("[::1]");
            if loopback || allow_insecure { Ok(()) } else { bail!("refusing unencrypted connection to {host}; use https://") }
        }
        other => bail!("unsupported scheme {other}"),
    }
}

/// WebSocket URL for `path` on the server, under the same transport policy.
pub fn ws_url(server: &str, allow_insecure: bool, path: &str) -> Result<String> {
    check_server_url(server, allow_insecure)?;
    let (scheme, rest) = server.trim_end_matches('/').split_once("://").unwrap();
    Ok(format!("{}://{rest}{path}", if scheme == "https" { "wss" } else { "ws" }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub id: String,
    pub server_url: String,
    /// Pinned Ed25519 public key of the server (base64). Registration aborts on mismatch.
    pub server_key: String,
    /// Ed25519 seed of this device's identity (base64). Never leaves the machine.
    pub signing_key: String,
    /// True once the server has accepted this id, after which the id never changes.
    #[serde(default)]
    pub registered: bool,
    #[serde(default)]
    pub enroll_token: Option<String>,
    /// argon2id PHC hash of the permanent password.
    #[serde(default)]
    pub permanent_password: Option<String>,
    pub verification: Verification,
    pub approve_mode: ApproveMode,
    /// Permit plain ws:// to a non-loopback server. Off by default; never enabled implicitly.
    #[serde(default)]
    pub allow_insecure_transport: bool,
    pub allow_input: bool,
    pub allow_clipboard: bool,
    #[serde(default = "yes")]
    pub allow_file_transfer: bool,
    pub max_fps: u32,
    pub jpeg_quality: u8,
    pub max_width: u32,
    #[serde(default)]
    pub account: Option<Account>,
    /// Answer LAN discovery broadcasts (UDP 21120) with id, hostname and OS.
    #[serde(default = "yes")]
    pub lan_discovery: bool,
    /// argon2id PHC hash of the 4-8 digit PIN that guards Settings and sign-out in the app.
    #[serde(default)]
    pub unlock_pin: Option<String>,
    /// Let viewers draw annotations on this screen (desktop app only).
    #[serde(default = "yes")]
    pub allow_whiteboard: bool,
}

fn yes() -> bool {
    true
}

impl Config {
    pub fn new(server_url: String, server_key: String) -> Result<Self> {
        let key = B64.decode(server_key.trim()).context("server key is not valid base64")?;
        if key.len() != 32 {
            bail!("server key must be a 32-byte Ed25519 public key");
        }
        Ok(Self {
            id: crate::crypto::random_device_id(),
            server_url: server_url.trim_end_matches('/').to_string(),
            server_key: server_key.trim().to_string(),
            signing_key: B64.encode(crate::crypto::random_bytes::<32>()),
            registered: false,
            enroll_token: None,
            permanent_password: None,
            verification: Verification::Temporary,
            approve_mode: ApproveMode::Password,
            allow_insecure_transport: false,
            allow_input: true,
            allow_clipboard: true,
            allow_file_transfer: true,
            max_fps: 15,
            jpeg_quality: 70,
            max_width: 1920,
            account: None,
            lan_discovery: true,
            unlock_pin: None,
            allow_whiteboard: true,
        })
    }

    pub fn default_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        migrate_legacy_dir(&base.join("ReDesk"), &base.join("Aegiron"));
        base.join("Aegiron").join("agent.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {} (run `aegiron-agent init` first)", path.display()))?;
        toml::from_str(&s).context("invalid agent config")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Where the running agent publishes its current temporary password (owner-only).
    pub fn temp_password_path(config_path: &Path) -> PathBuf {
        config_path.with_file_name("temp_password")
    }

    /// WebSocket base URL derived from server_url, enforcing the transport policy.
    pub fn ws_base(&self) -> Result<String> {
        let (scheme, rest) = self.server_url.split_once("://").context("server url needs a scheme (https://...)")?;
        let ws = match scheme {
            "https" | "wss" => "wss",
            "http" | "ws" => {
                let host = rest.split(['/', ':']).next().unwrap_or("");
                let loopback = matches!(host, "localhost" | "127.0.0.1") || rest.starts_with("[::1]");
                if !loopback && !self.allow_insecure_transport {
                    bail!("refusing unencrypted connection to {host}; use https:// or set allow_insecure_transport = true");
                }
                "ws"
            }
            other => bail!("unsupported scheme {other}"),
        };
        Ok(format!("{ws}://{rest}"))
    }
}

/// Moves the settings folder of a pre-rename (ReDesk) install to the Aegiron folder once, so
/// the device keeps its ID, key, sign-in and history. Does nothing when the new folder exists.
pub fn migrate_legacy_dir(old: &Path, new: &Path) {
    if new.exists() || !old.join("agent.toml").is_file() {
        return;
    }
    if let Err(e) = std::fs::rename(old, new) {
        tracing::warn!("could not move {} to {}: {e}", old.display(), new.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_folder_moves_once() {
        let root = std::env::temp_dir().join(format!("aegiron-migrate-test-{}", std::process::id()));
        let (old, new) = (root.join("ReDesk"), root.join("Aegiron"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("agent.toml"), "id = \"1\"").unwrap();
        migrate_legacy_dir(&old, &new);
        assert!(new.join("agent.toml").is_file() && !old.exists());
        // A second old folder never overwrites the migrated one.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("agent.toml"), "id = \"2\"").unwrap();
        migrate_legacy_dir(&old, &new);
        assert_eq!(std::fs::read_to_string(new.join("agent.toml")).unwrap(), "id = \"1\"");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
