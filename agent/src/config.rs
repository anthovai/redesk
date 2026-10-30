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

/// Which H.264 encoder sessions use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum EncoderPref {
    /// The platform's hardware encoder (VideoToolbox, Media Foundation) when it works, else OpenH264.
    #[default]
    Auto,
    /// Always OpenH264 (software).
    Software,
}

/// Video codec this app asks for when it controls another machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ViewerCodec {
    #[default]
    H264,
    /// AV1 when the other machine has a hardware AV1 encoder, else H.264.
    Av1,
}

impl ViewerCodec {
    pub fn codec(self) -> crate::codec::Codec {
        match self {
            ViewerCodec::H264 => crate::codec::Codec::H264,
            ViewerCodec::Av1 => crate::codec::Codec::Av1,
        }
    }
}

/// The person signed in from the desktop app (bearer token for the Aegiron API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub token: String,
}

/// "Remember this device for 30 days" from a 2FA sign-in: lets this account skip the TOTP
/// code (never the password) on this server until the server-side expiry or revocation.
/// Stays after signing out, like a browser's trusted-device cookie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedLogin {
    pub server: String,
    pub email: String,
    pub token: String,
}

/// An organisation strategy received from the server: settings the administrator manages for
/// this device. `None` fields are not managed (the local value applies and may be changed).
/// Applied over the local settings whenever it arrives and after every local change, so the
/// app, the CLI and sessions all see the managed values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub settings: PolicySettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve_mode: Option<ApproveMode>,
    /// false: one-time passwords only (a permanent password is neither accepted nor settable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_permanent_password: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_clipboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_file_transfer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_whiteboard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_discovery: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_after_session: Option<bool>,
    /// Enforced by the server (sessions are recorded); shown for information only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_sessions: Option<bool>,
}

/// Settings a strategy can manage, by their config / CLI names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Managed {
    ApproveMode,
    Verification,
    PermanentPassword,
    AllowInput,
    AllowClipboard,
    AllowFileTransfer,
    AllowAudio,
    AllowWhiteboard,
    DirectEnabled,
    LanDiscovery,
    LockAfterSession,
}

/// Shown next to settings a strategy manages (app) and in CLI refusals.
pub const MANAGED_TEXT: &str = "จัดการโดยองค์กรของคุณ";

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
    /// Send this machine's sound to viewers that ask for it.
    #[serde(default = "yes")]
    pub allow_audio: bool,
    /// Hardware or software H.264 (absent in older configs = auto).
    #[serde(default)]
    pub video_encoder: EncoderPref,
    /// Codec to ask for when controlling other machines (absent = H.264).
    #[serde(default)]
    pub viewer_codec: ViewerCodec,
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
    /// Allow direct sessions (LAN/TCP and P2P over UDP hole punching). False = always through
    /// the server relay (enterprise policy). Applies to both incoming and outgoing sessions.
    #[serde(default = "yes")]
    pub direct_enabled: bool,
    /// TCP port this device accepts direct sessions on (a random port is used when busy).
    #[serde(default = "default_direct_port")]
    pub direct_port: u16,
    /// Outbound proxy for every connection to the server (signaling, relay, HTTPS API).
    #[serde(default)]
    pub proxy: Option<crate::net::ProxyConfig>,
    /// Trusted-device tokens for 2FA sign-in, one per (server, email). Secrets: this file is
    /// owner-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_logins: Vec<TrustedLogin>,
    /// Lock this machine's screen when a remote session ends.
    #[serde(default)]
    pub lock_after_session: bool,
    /// The organisation's strategy for this device, as last received from the server (kept so
    /// it also applies while offline). None = no strategy: every setting is local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<Policy>,
}

fn yes() -> bool {
    true
}

fn default_direct_port() -> u16 {
    crate::direct::DEFAULT_PORT
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
            allow_audio: true,
            video_encoder: EncoderPref::Auto,
            viewer_codec: ViewerCodec::H264,
            max_fps: 15,
            jpeg_quality: 70,
            max_width: 1920,
            account: None,
            lan_discovery: true,
            unlock_pin: None,
            allow_whiteboard: true,
            direct_enabled: true,
            direct_port: crate::direct::DEFAULT_PORT,
            proxy: None,
            trusted_logins: Vec::new(),
            lock_after_session: false,
            managed: None,
        })
    }

    /// Whether the organisation's strategy manages this setting.
    pub fn is_managed(&self, what: Managed) -> bool {
        let Some(p) = &self.managed else { return false };
        let s = &p.settings;
        match what {
            Managed::ApproveMode => s.approve_mode.is_some(),
            // One-time passwords only: the accepted password kind is fixed too.
            Managed::Verification | Managed::PermanentPassword => s.allow_permanent_password == Some(false),
            Managed::AllowInput => s.allow_input.is_some(),
            Managed::AllowClipboard => s.allow_clipboard.is_some(),
            Managed::AllowFileTransfer => s.allow_file_transfer.is_some(),
            Managed::AllowAudio => s.allow_audio.is_some(),
            Managed::AllowWhiteboard => s.allow_whiteboard.is_some(),
            Managed::DirectEnabled => s.direct_enabled.is_some(),
            Managed::LanDiscovery => s.lan_discovery.is_some(),
            Managed::LockAfterSession => s.lock_after_session.is_some(),
        }
    }

    /// Writes the managed values over the local ones. Returns true when something changed.
    pub fn apply_policy(&mut self) -> bool {
        let Some(p) = self.managed.clone() else { return false };
        let before = (
            self.approve_mode,
            self.verification,
            self.permanent_password.clone(),
            (self.allow_input, self.allow_clipboard, self.allow_file_transfer, self.allow_audio, self.allow_whiteboard),
            (self.direct_enabled, self.lan_discovery, self.lock_after_session),
        );
        let s = &p.settings;
        fn set<T: Copy>(dst: &mut T, v: Option<T>) {
            if let Some(v) = v {
                *dst = v;
            }
        }
        set(&mut self.approve_mode, s.approve_mode);
        if s.allow_permanent_password == Some(false) {
            self.verification = Verification::Temporary;
            self.permanent_password = None;
        }
        set(&mut self.allow_input, s.allow_input);
        set(&mut self.allow_clipboard, s.allow_clipboard);
        set(&mut self.allow_file_transfer, s.allow_file_transfer);
        set(&mut self.allow_audio, s.allow_audio);
        set(&mut self.allow_whiteboard, s.allow_whiteboard);
        set(&mut self.direct_enabled, s.direct_enabled);
        set(&mut self.lan_discovery, s.lan_discovery);
        set(&mut self.lock_after_session, s.lock_after_session);
        let after = (
            self.approve_mode,
            self.verification,
            self.permanent_password.clone(),
            (self.allow_input, self.allow_clipboard, self.allow_file_transfer, self.allow_audio, self.allow_whiteboard),
            (self.direct_enabled, self.lan_discovery, self.lock_after_session),
        );
        before != after
    }

    /// Takes a strategy from the server (None = the device has none) and applies it. Returns
    /// true when the config changed and should be saved. Settings stay as they were when a
    /// strategy is removed, but become editable again.
    pub fn set_policy(&mut self, policy: Option<Policy>) -> bool {
        let changed = self.managed != policy;
        self.managed = policy;
        self.apply_policy() || changed
    }

    pub fn default_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        migrate_legacy_dir(&base.join("ReDesk"), &base.join("Aegiron"));
        base.join("Aegiron").join("agent.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {} (run `aegiron-agent init` first)", path.display()))?;
        let mut c: Self = toml::from_str(&s).context("invalid agent config")?;
        // A file edited by hand cannot loosen what the organisation manages.
        c.apply_policy();
        crate::net::set_proxy(c.proxy.clone());
        Ok(c)
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
        crate::net::set_proxy(self.proxy.clone());
        Ok(())
    }

    /// The trusted-device token for this account on this server, if any.
    pub fn trust_token(&self, server: &str, email: &str) -> Option<String> {
        let (server, email) = trust_key(server, email);
        self.trusted_logins.iter().find(|t| t.server == server && t.email == email).map(|t| t.token.clone())
    }

    /// Stores (Some) or forgets (None) the trusted-device token for this account.
    pub fn set_trust_token(&mut self, server: &str, email: &str, token: Option<String>) {
        let (server, email) = trust_key(server, email);
        self.trusted_logins.retain(|t| !(t.server == server && t.email == email));
        if let Some(token) = token {
            self.trusted_logins.push(TrustedLogin { server, email, token });
        }
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

fn trust_key(server: &str, email: &str) -> (String, String) {
    (server.trim().trim_end_matches('/').to_ascii_lowercase(), email.trim().to_lowercase())
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
    fn trust_tokens_per_account() {
        let mut c = Config::new("https://a.example".into(), B64.encode([7u8; 32])).unwrap();
        c.set_trust_token("https://A.example/", " Me@Example.com", Some("t1".into()));
        c.set_trust_token("https://a.example", "other@example.com", Some("t2".into()));
        assert_eq!(c.trust_token("https://a.example", "me@example.com").as_deref(), Some("t1"));
        assert_eq!(c.trust_token("https://b.example", "me@example.com"), None);
        c.set_trust_token("https://a.example", "me@example.com", Some("t3".into()));
        assert_eq!(c.trusted_logins.len(), 2);
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.trust_token("https://a.example", "ME@example.com").as_deref(), Some("t3"));
        c.set_trust_token("https://a.example", "me@example.com", None);
        assert_eq!(c.trust_token("https://a.example", "me@example.com"), None);
        assert_eq!(c.trust_token("https://a.example", "other@example.com").as_deref(), Some("t2"));
    }

    #[test]
    fn policy_locks_and_releases() {
        let mut c = Config::new("https://a.example".into(), B64.encode([7u8; 32])).unwrap();
        c.permanent_password = Some("hash".into());
        c.verification = Verification::Both;
        let p: Policy = serde_json::from_str(
            r#"{"id":3,"name":"Kiosk","settings":{"approve_mode":"click","allow_file_transfer":false,"allow_permanent_password":false,"direct_enabled":false,"lock_after_session":true,"record_sessions":true}}"#,
        )
        .unwrap();
        assert!(c.set_policy(Some(p.clone())));
        assert_eq!(c.approve_mode, ApproveMode::Click);
        assert!(!c.allow_file_transfer && !c.direct_enabled && c.lock_after_session);
        assert_eq!((c.verification, c.permanent_password.as_deref()), (Verification::Temporary, None));
        assert!(c.is_managed(Managed::AllowFileTransfer) && c.is_managed(Managed::PermanentPassword));
        assert!(!c.is_managed(Managed::AllowInput) && c.allow_input);
        // Same policy again: nothing to save.
        assert!(!c.set_policy(Some(p)));
        // A local change to a managed setting is undone; unmanaged ones stay.
        c.allow_file_transfer = true;
        c.allow_input = false;
        assert!(c.apply_policy());
        assert!(!c.allow_file_transfer && !c.allow_input);
        // Survives a save/load round trip, including a hand edit of the file.
        let text = toml::to_string_pretty(&c).unwrap().replacen("allow_file_transfer = false", "allow_file_transfer = true", 1);
        let mut back: Config = toml::from_str(&text).unwrap();
        back.apply_policy();
        assert!(!back.allow_file_transfer && back.managed.as_ref().unwrap().name == "Kiosk");
        // Removed: values stay, nothing is managed any more.
        assert!(c.set_policy(None));
        assert!(!c.allow_file_transfer && !c.is_managed(Managed::AllowFileTransfer));
        // Configs without the new fields still load.
        let old = toml::to_string_pretty(&Config::new("https://a.example".into(), B64.encode([7u8; 32])).unwrap()).unwrap();
        let old = old.lines().filter(|l| !l.starts_with("lock_after_session")).collect::<Vec<_>>().join("\n");
        let c: Config = toml::from_str(&old).unwrap();
        assert!(c.managed.is_none() && !c.lock_after_session);
    }

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
