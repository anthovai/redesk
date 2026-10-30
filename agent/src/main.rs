//! Aegiron agent: makes this machine reachable for remote control through a self-hosted Aegiron server.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use clap::{Parser, Subcommand};
use tokio::io::AsyncBufReadExt;

use aegiron_agent::config::{ApproveMode, Config, EncoderPref, MANAGED_TEXT, Managed, Verification, ViewerCodec};
use aegiron_agent::net::ProxyConfig;
use aegiron_agent::{fingerprint, identity, service, signal};

#[derive(Parser)]
#[command(name = "aegiron-agent", version, about = "Aegiron remote desktop agent")]
struct Cli {
    /// Config file (default: <user config dir>/Aegiron/agent.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new identity bound to a server.
    Init {
        /// Server URL, e.g. https://aegiron.example.com
        #[arg(long)]
        server: String,
        /// Server public key (shown in the console under Settings and in the server log)
        #[arg(long)]
        key: String,
        /// Enrollment token, if the server requires one
        #[arg(long)]
        enroll_token: Option<String>,
        /// Allow plain ws:// to a non-loopback host (not recommended)
        #[arg(long)]
        insecure: bool,
        /// Overwrite an existing config (generates a new id and key)
        #[arg(long)]
        force: bool,
    },
    /// Sign in with an Aegiron account. On a new machine this also creates the device identity
    /// and enrolls it under that account (no server key or token needed).
    Login {
        /// Server URL (only needed the first time), e.g. https://aegiron.example.com
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        email: String,
        /// Allow plain http:// to a non-loopback host (not recommended)
        #[arg(long)]
        insecure: bool,
        /// With 2FA: remember this device for 30 days, so later sign-ins need only the password
        #[arg(long)]
        remember_device: bool,
    },
    /// Connect to the server and wait for sessions (default).
    Run,
    /// Set the permanent password (enables permanent/both verification).
    SetPassword,
    /// Change security and streaming options.
    Set {
        #[arg(long, value_enum)]
        verification: Option<Verification>,
        #[arg(long, value_enum)]
        approve_mode: Option<ApproveMode>,
        #[arg(long)]
        allow_input: Option<bool>,
        #[arg(long)]
        allow_clipboard: Option<bool>,
        #[arg(long)]
        allow_file_transfer: Option<bool>,
        /// Let viewers draw on this screen (shown only by the desktop app)
        #[arg(long)]
        allow_whiteboard: Option<bool>,
        /// Let viewers change the display resolution (macOS, Windows; restored after the session)
        #[arg(long)]
        allow_resolution: Option<bool>,
        /// Send this machine's audio to viewers (macOS 13+, Windows).
        #[arg(long)]
        allow_audio: Option<bool>,
        /// Let viewers block this machine's keyboard and mouse (needs input; macOS, Windows)
        #[arg(long)]
        allow_block_input: Option<bool>,
        /// Let viewers black out this screen while they control it (needs input; Windows 10 2004+,
        /// macOS with the desktop app)
        #[arg(long)]
        allow_privacy_mode: Option<bool>,
        /// Let viewers lock this machine, now or when the session ends
        #[arg(long)]
        allow_lock: Option<bool>,
        /// Let viewers restart this machine (macOS, Windows)
        #[arg(long)]
        allow_restart: Option<bool>,
        /// H.264 encoder: auto (hardware when available) or software (OpenH264)
        #[arg(long, value_enum)]
        video_encoder: Option<EncoderPref>,
        #[arg(long)]
        max_fps: Option<u32>,
        #[arg(long)]
        jpeg_quality: Option<u8>,
        #[arg(long)]
        max_width: Option<u32>,
        /// Allow direct LAN/P2P sessions (false = always through the server relay)
        #[arg(long)]
        direct: Option<bool>,
        /// Answer LAN discovery broadcasts (UDP 21120)
        #[arg(long)]
        lan_discovery: Option<bool>,
        /// Lock this machine's screen when a remote session ends
        #[arg(long)]
        lock_after_session: Option<bool>,
        /// TCP port for direct sessions (default 21118)
        #[arg(long)]
        direct_port: Option<u16>,
        /// Proxy for connections to the server: socks5://host:port, http://host:port, or "none"
        #[arg(long)]
        proxy: Option<String>,
        /// Proxy username (the password is prompted, or read from AEGIRON_PROXY_PASSWORD)
        #[arg(long)]
        proxy_user: Option<String>,
        /// Let viewers open TCP tunnels through this machine (to this machine only, see --allow-tunnel-lan)
        #[arg(long)]
        allow_tunnel: Option<bool>,
        /// Tunnels may also reach private network addresses (10/8, 172.16/12, 192.168/16, link-local, fc00::/7)
        #[arg(long)]
        allow_tunnel_lan: Option<bool>,
        /// Let viewers open a shell on this machine (as this user)
        #[arg(long)]
        allow_terminal: Option<bool>,
    },
    /// Connect to another device from the terminal (diagnostics): sign in with the remote
    /// password, receive the screen for a few seconds and report the path (relay/direct/p2p).
    /// The remote password is prompted, or read from AEGIRON_REMOTE_PASSWORD.
    Connect {
        /// Device ID
        id: String,
        /// Seconds to keep receiving after the first frame
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Use only this path, without fallback: relay, direct or p2p
        #[arg(long)]
        only: Option<String>,
        /// Video codec to ask for (default: viewer_codec in the config, else h264)
        #[arg(long, value_enum)]
        codec: Option<ViewerCodec>,
    },
    /// Forward a local port to a port on (or next to) another device, like RustDesk's TCP
    /// tunneling: `aegiron-agent tunnel 123456789 3389:localhost:3389`, then connect to
    /// 127.0.0.1:3389. The remote password is prompted, or read from AEGIRON_REMOTE_PASSWORD.
    Tunnel {
        /// Device ID
        id: String,
        /// LOCAL_PORT:REMOTE_HOST:REMOTE_PORT (REMOTE_HOST as seen from the device; IPv6 in brackets)
        spec: String,
    },
    /// Open a shell on another device. The remote password is prompted, or read from
    /// AEGIRON_REMOTE_PASSWORD.
    Terminal {
        /// Device ID
        id: String,
        /// Run this command, print its output and exit with its status (no interactive shell)
        #[arg(long)]
        command: Option<String>,
    },
    /// Show this device's id and key fingerprint.
    Info,
    /// Print the current temporary password of the running agent.
    ShowPassword,
    /// Start the agent automatically at login (per-user service, no root).
    Install,
    /// Remove the login service.
    Uninstall,
}



#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(Config::default_path);

    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Init { server, key, enroll_token, insecure, force } => {
            if path.exists() && !force {
                bail!("{} already exists; pass --force to replace this device identity", path.display());
            }
            let mut c = Config::new(server, key)?;
            c.enroll_token = enroll_token;
            c.allow_insecure_transport = insecure;
            c.ws_base()?;
            c.save(&path)?;
            println!("Initialised {}\n  ID: {}\n  Device key fingerprint: {}", path.display(), c.id, fingerprint(identity(&c)?.verifying_key().as_bytes()));
        }
        Cmd::Login { server, email, insecure, remember_device } => {
            let mut existing = Config::load(&path).ok();
            let server = match (&existing, server) {
                (Some(c), _) => c.server_url.clone(),
                (None, Some(s)) => s.trim_end_matches('/').to_string(),
                (None, None) => bail!("--server is required on a new machine"),
            };
            let insecure = insecure || existing.as_ref().is_some_and(|c| c.allow_insecure_transport);
            // AEGIRON_LOGIN_PASSWORD allows unattended scripts; interactive use prompts without echo.
            let password = match std::env::var("AEGIRON_LOGIN_PASSWORD") {
                Ok(p) if !p.is_empty() => p,
                _ => rpassword::prompt_password(format!("Password for {email}: "))?,
            };
            use aegiron_agent::account::{Login, LoginRequest};
            let mut req = LoginRequest {
                email: email.clone(),
                password,
                trust_token: existing.as_ref().and_then(|c| c.trust_token(&server, &email)),
                remember: remember_device,
                ..Default::default()
            };
            let session = loop {
                match aegiron_agent::account::login(&server, insecure, &req)? {
                    Login::Ok(s) => break s,
                    Login::NeedTotp { trust_rejected: r } if req.totp.is_empty() => {
                        req.trust_token = None;
                        // Expired or revoked on the server: forget it now, even if this sign-in fails.
                        if let (true, Some(c)) = (r, existing.as_mut()) {
                            c.set_trust_token(&server, &email, None);
                            c.save(&path)?;
                        }
                        // AEGIRON_LOGIN_TOTP: the current code, for scripted sign-in.
                        req.totp = match std::env::var("AEGIRON_LOGIN_TOTP") {
                            Ok(t) if !t.is_empty() => t,
                            _ => rpassword::prompt_password("2FA code: ")?,
                        };
                    }
                    Login::NeedTotp { .. } => bail!("invalid authentication code"),
                }
            };
            let account = aegiron_agent::config::Account { email: session.user.email.clone(), name: session.user.name.clone(), token: session.token };
            let trust = session.trust_token.clone();
            let mut c = match existing {
                Some(mut c) => {
                    if c.server_key != session.server_key {
                        bail!("server key differs from the one this device trusts; not signing in");
                    }
                    c.account = Some(account);
                    c
                }
                None => {
                    let mut c = Config::new(server.clone(), session.server_key)?;
                    c.enroll_token = Some(session.enroll_token);
                    c.allow_insecure_transport = insecure;
                    c.account = Some(account);
                    c
                }
            };
            if let Some(t) = trust {
                c.set_trust_token(&server, &session.user.email, Some(t));
            }
            c.ws_base()?;
            c.save(&path)?;
            let how = if req.totp.is_empty() && req.trust_token.is_some() { " (2FA skipped: remembered device)" } else { "" };
            println!("Signed in as {} — device ID {}{how}", session.user.email, c.id);
        }
        Cmd::SetPassword => {
            let mut c = Config::load(&path)?;
            refuse_managed(&c, &[(Managed::PermanentPassword, true)])?;
            let pw = rpassword::prompt_password("New permanent password: ")?;
            if pw.len() < 8 {
                bail!("use at least 8 characters");
            }
            if rpassword::prompt_password("Repeat: ")? != pw {
                bail!("passwords do not match");
            }
            c.permanent_password = Some(aegiron_agent::hash_password(&pw)?);
            if c.verification == Verification::Temporary {
                c.verification = Verification::Both;
            }
            c.save(&path)?;
            println!("Permanent password saved (verification = {:?}).", c.verification);
        }
        Cmd::Set {
            verification,
            approve_mode,
            allow_input,
            allow_clipboard,
            allow_file_transfer,
            allow_whiteboard,
            allow_resolution,
            allow_audio,
            allow_block_input,
            allow_privacy_mode,
            allow_lock,
            allow_restart,
            video_encoder,
            max_fps,
            jpeg_quality,
            max_width,
            direct,
            lan_discovery,
            lock_after_session,
            direct_port,
            proxy,
            proxy_user,
            allow_tunnel,
            allow_tunnel_lan,
            allow_terminal,
        } => {
            let mut c = Config::load(&path)?;
            // Settings the organisation's strategy manages cannot be changed here.
            refuse_managed(&c, &[
                (Managed::Verification, verification.is_some()),
                (Managed::ApproveMode, approve_mode.is_some()),
                (Managed::AllowInput, allow_input.is_some()),
                (Managed::AllowClipboard, allow_clipboard.is_some()),
                (Managed::AllowFileTransfer, allow_file_transfer.is_some()),
                (Managed::AllowWhiteboard, allow_whiteboard.is_some()),
                (Managed::AllowAudio, allow_audio.is_some()),
                (Managed::AllowResolution, allow_resolution.is_some()),
                (Managed::AllowBlockInput, allow_block_input.is_some()),
                (Managed::AllowPrivacyMode, allow_privacy_mode.is_some()),
                (Managed::AllowLock, allow_lock.is_some()),
                (Managed::AllowRestart, allow_restart.is_some()),
                (Managed::AllowTunnel, allow_tunnel.is_some()),
                (Managed::AllowTunnelLan, allow_tunnel_lan.is_some()),
                (Managed::AllowTerminal, allow_terminal.is_some()),
                (Managed::DirectEnabled, direct.is_some()),
                (Managed::LanDiscovery, lan_discovery.is_some()),
                (Managed::LockAfterSession, lock_after_session.is_some()),
            ])?;
            if let Some(v) = verification {
                if v != Verification::Temporary && c.permanent_password.is_none() {
                    bail!("run `aegiron-agent set-password` first");
                }
                c.verification = v;
            }
            if let Some(v) = approve_mode { c.approve_mode = v; }
            if let Some(v) = allow_input { c.allow_input = v; }
            if let Some(v) = allow_clipboard { c.allow_clipboard = v; }
            if let Some(v) = allow_file_transfer { c.allow_file_transfer = v; }
            if let Some(v) = allow_whiteboard { c.allow_whiteboard = v; }
            if let Some(v) = allow_resolution { c.allow_resolution = v; }
            if let Some(v) = allow_audio { c.allow_audio = v; }
            if let Some(v) = allow_block_input { c.allow_block_input = v; }
            if let Some(v) = allow_privacy_mode { c.allow_privacy_mode = v; }
            if let Some(v) = allow_lock { c.allow_lock = v; }
            if let Some(v) = allow_restart { c.allow_restart = v; }
            if let Some(v) = video_encoder { c.video_encoder = v; }
            if let Some(v) = max_fps { c.max_fps = v.clamp(1, 60); }
            if let Some(v) = jpeg_quality { c.jpeg_quality = v.clamp(10, 95); }
            if let Some(v) = max_width { c.max_width = v.clamp(320, 3840); }
            if let Some(v) = direct { c.direct_enabled = v; }
            if let Some(v) = lan_discovery { c.lan_discovery = v; }
            if let Some(v) = lock_after_session { c.lock_after_session = v; }
            if let Some(v) = direct_port { c.direct_port = v; }
            if let Some(v) = allow_tunnel { c.allow_tunnel = v; }
            if let Some(v) = allow_tunnel_lan { c.allow_tunnel_lan = v; }
            if let Some(v) = allow_terminal { c.allow_terminal = v; }
            match proxy.as_deref() {
                Some("none" | "") => c.proxy = None,
                Some(addr) => {
                    let mut p = ProxyConfig::parse(addr)?;
                    if let Some(user) = proxy_user.filter(|u| !u.is_empty()) {
                        let pw = match std::env::var("AEGIRON_PROXY_PASSWORD") {
                            Ok(p) if !p.is_empty() => p,
                            _ => rpassword::prompt_password(format!("Proxy password for {user}: "))?,
                        };
                        p.username = Some(user);
                        p.password = Some(pw);
                    }
                    p.validate()?;
                    c.proxy = Some(p);
                }
                None if proxy_user.is_some() => bail!("--proxy-user needs --proxy"),
                None => {}
            }
            c.save(&path)?;
            println!("Saved.");
        }
        Cmd::Info => {
            let c = Config::load(&path)?;
            println!("ID:           {}", c.id);
            println!("Server:       {}", c.server_url);
            println!("Registered:   {}", c.registered);
            println!("Verification: {:?}, approve mode: {:?}", c.verification, c.approve_mode);
            println!("Device key:   {}", fingerprint(identity(&c)?.verifying_key().as_bytes()));
            println!("Server key:   {}", fingerprint(&B64.decode(&c.server_key)?));
            println!("Input: {}, clipboard: {}, file transfer: {}, whiteboard: {}, resolution: {}, audio: {}", c.allow_input, c.allow_clipboard, c.allow_file_transfer, c.allow_whiteboard, c.allow_resolution, c.allow_audio);
            println!("Controls:     block input {}, privacy mode {}, lock {}, restart {}", c.allow_block_input, c.allow_privacy_mode, c.allow_lock, c.allow_restart);
            println!("Encoder:      {:?}", c.video_encoder);
            let tunnel = match (c.allow_tunnel, c.allow_tunnel_lan) {
                (false, _) => "off",
                (true, false) => "allowed (this machine only)",
                (true, true) => "allowed (this machine and private networks)",
            };
            println!("Tunnel:       {tunnel}, terminal: {}", if c.allow_terminal { "allowed" } else { "off" });
            println!("Direct/P2P:   {} (TCP port {})", if c.direct_enabled { "allowed" } else { "off (relay only)" }, c.direct_port);
            println!("Proxy:        {}", c.proxy.as_ref().map(|p| p.display()).unwrap_or_else(|| "none".into()));
            println!("Lock after session: {}", c.lock_after_session);
            match &c.managed {
                Some(p) => println!("Strategy:     \"{}\" (settings it manages cannot be changed on this device)", p.name),
                None => println!("Strategy:     none"),
            }
            service::status()?;
        }
        Cmd::ShowPassword => {
            let pw = std::fs::read_to_string(Config::temp_password_path(&path)).context("agent is not running (no temporary password yet)")?;
            println!("{}", pw.trim());
        }
        Cmd::Install => {
            let c = Config::load(&path)?;
            service::install(&path)?;
            if c.verification == Verification::Temporary && c.approve_mode == ApproveMode::Password {
                println!("Note: no terminal is attached to the service. Read the one-time password with `aegiron-agent show-password`,");
                println!("      or use `set-password` / `set --approve-mode click` for people who connect to this machine.");
            }
            if cfg!(target_os = "macos") {
                println!("macOS: grant Screen Recording and Accessibility to {} in System Settings → Privacy & Security.", std::env::current_exe()?.display());
            }
        }
        Cmd::Uninstall => service::uninstall()?,
        Cmd::Connect { id, seconds, only, codec } => connect(&path, &id, seconds, only, codec).await?,
        Cmd::Tunnel { id, spec } => tunnel(&path, &id, &spec).await?,
        Cmd::Terminal { id, command } => {
            let code = terminal(&path, &id, command).await?;
            std::process::exit(code);
        }
        Cmd::Run => {
            let c = Config::load(&path)?;
            c.ws_base()?;
            let id = c.id.clone();
            let (line_tx, line_rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if line_tx.send(l).await.is_err() {
                        break;
                    }
                }
            });
            let agent = signal::Agent::new(c, path, Some(line_rx))?;
            println!("Aegiron agent {}  —  ID: {}", env!("CARGO_PKG_VERSION"), id);
            agent.rotate_temp_password();
            tokio::select! {
                _ = signal::run_forever(agent) => {}
                _ = tokio::signal::ctrl_c() => println!("bye"),
            }
        }
    }
    Ok(())
}

/// Fails when one of the requested changes touches a setting the organisation manages.
fn refuse_managed(c: &Config, wanted: &[(Managed, bool)]) -> Result<()> {
    let names = |m: Managed| match m {
        Managed::ApproveMode => "--approve-mode",
        Managed::Verification => "--verification",
        Managed::PermanentPassword => "set-password",
        Managed::AllowInput => "--allow-input",
        Managed::AllowClipboard => "--allow-clipboard",
        Managed::AllowFileTransfer => "--allow-file-transfer",
        Managed::AllowAudio => "--allow-audio",
        Managed::AllowWhiteboard => "--allow-whiteboard",
        Managed::AllowResolution => "--allow-resolution",
        Managed::AllowBlockInput => "--allow-block-input",
        Managed::AllowPrivacyMode => "--allow-privacy-mode",
        Managed::AllowLock => "--allow-lock",
        Managed::AllowRestart => "--allow-restart",
        Managed::AllowTunnel => "--allow-tunnel",
        Managed::AllowTunnelLan => "--allow-tunnel-lan",
        Managed::AllowTerminal => "--allow-terminal",
        Managed::DirectEnabled => "--direct",
        Managed::LanDiscovery => "--lan-discovery",
        Managed::LockAfterSession => "--lock-after-session",
    };
    let locked: Vec<&str> = wanted.iter().filter(|(m, asked)| *asked && c.is_managed(*m)).map(|(m, _)| names(*m)).collect();
    if locked.is_empty() {
        return Ok(());
    }
    let strategy = c.managed.as_ref().map(|p| p.name.as_str()).unwrap_or_default();
    bail!("{} is managed by your organisation (strategy \"{strategy}\", {MANAGED_TEXT}); ask your administrator", locked.join(", "))
}

/// `aegiron-agent connect`: runs the desktop app's viewer without a window.
async fn connect(path: &std::path::Path, id: &str, seconds: u64, only: Option<String>, codec: Option<ViewerCodec>) -> Result<()> {
    use aegiron_agent::direct::Path as Route;
    use aegiron_agent::viewer::{Phase, Target, Viewer};
    use std::time::{Duration, Instant};

    let c = Config::load(path)?;
    let acc = c.account.clone().context("sign in first: aegiron-agent login --email ...")?;
    let only = only.map(|o| Route::parse(&o).context("--only must be relay, direct or p2p")).transpose()?;
    if !c.direct_enabled && only.is_some_and(|o| o != Route::Relay) {
        bail!("direct connections are turned off on this device (direct_enabled = false)");
    }
    let password = match std::env::var("AEGIRON_REMOTE_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => rpassword::prompt_password(format!("Password of {id}: "))?,
    };
    let id = aegiron_agent::local::normalize_id(id).unwrap_or_else(|| id.replace(' ', ""));
    let target = Target {
        server: c.server_url.clone(),
        token: acc.token,
        insecure: c.allow_insecure_transport,
        device_id: id.clone(),
        name: id.clone(),
        direct: c.direct_enabled,
        only,
        codec: codec.unwrap_or(c.viewer_codec).codec(),
    };
    let viewer = Viewer::connect(target, std::sync::Arc::new(|| {}));
    let started = Instant::now();
    let (mut sent, mut shown) = (false, None::<Instant>);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (phase, error, path, w, h) = {
            let st = viewer.state.lock().unwrap();
            (st.phase.clone(), st.error.clone(), st.path, st.fb.w, st.fb.h)
        };
        match phase {
            Phase::Closed => bail!("connection closed: {}", error.unwrap_or_default()),
            Phase::Password if sent && error.is_some() => bail!("{}", error.unwrap_or_default()),
            Phase::Password if !sent => {
                viewer.submit_password(&password);
                sent = true;
            }
            Phase::Connected if w > 0 && shown.is_none() => {
                println!("connected to {id} via {} — screen {w}x{h}", path.map(|p| p.as_str()).unwrap_or("?"));
                shown = Some(Instant::now());
            }
            _ => {}
        }
        if shown.is_some_and(|t| t.elapsed() >= Duration::from_secs(seconds)) {
            break;
        }
        if started.elapsed() > Duration::from_secs(90) {
            bail!("timed out (phase {phase:?})");
        }
    }
    {
        let st = viewer.state.lock().unwrap();
        println!("path={} fps={} kbps={} codec={}", st.path.map(|p| p.as_str()).unwrap_or("?"), st.fps, st.kbps, st.codec);
    }
    viewer.close();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

fn remote_password(id: &str) -> Result<String> {
    match std::env::var("AEGIRON_REMOTE_PASSWORD") {
        Ok(p) if !p.is_empty() => Ok(p),
        _ => Ok(rpassword::prompt_password(format!("Password of {id}: "))?),
    }
}

/// `aegiron-agent tunnel <id> <local>:<host>:<port>`
async fn tunnel(path: &std::path::Path, id: &str, spec: &str) -> Result<()> {
    use aegiron_agent::tunnel::TunnelPhase;
    use std::sync::Arc;
    use std::sync::atomic::Ordering::Relaxed;

    let (local, target) = spec.split_once(':').context("use LOCAL_PORT:REMOTE_HOST:REMOTE_PORT, e.g. 8080:localhost:80")?;
    let local: u16 = local.parse().context("invalid local port")?;
    let (host, port) = aegiron_agent::remote::parse_target(target)?;
    let c = Config::load(path)?;
    let t = aegiron_agent::client::Target::from_config(&c, id)?;
    // Asked before connecting so the prompt does not race the session; empty = the device
    // only asks its user to accept.
    let password = match std::env::var("AEGIRON_REMOTE_PASSWORD") {
        Ok(p) if !p.is_empty() => Some(p),
        _ if std::io::IsTerminal::is_terminal(&std::io::stdin()) => Some(remote_password(id)?).filter(|p| !p.is_empty()),
        _ => None,
    };
    let (tun, mut task) = aegiron_agent::tunnel::open(t, id.to_string(), local, &host, port, password, Arc::new(|| {})).await?;
    let mut shown = TunnelPhase::Connecting;
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = &mut task => break,
            _ = tokio::signal::ctrl_c() => {
                tun.stop();
                let _ = (&mut task).await;
                break;
            }
            _ = ticks.tick() => {
                let phase = tun.phase();
                if phase != shown {
                    match phase {
                        TunnelPhase::Approval => eprintln!("waiting for the person at {id} to accept..."),
                        TunnelPhase::Open => {
                            let via = tun.path.lock().unwrap().map(|p| p.as_str()).unwrap_or("?");
                            println!("tunnel open: {} -> {} on {id} (via {via}); Ctrl-C to stop", tun.local, tun.target);
                        }
                        _ => {}
                    }
                    shown = phase;
                }
            }
        }
    }
    let c = &tun.counters;
    eprintln!("tunnel closed: {} connections, {} bytes up, {} bytes down", c.opened.load(Relaxed), c.up.load(Relaxed), c.down.load(Relaxed));
    match tun.error.lock().unwrap().clone() {
        Some(e) => bail!("{e}"),
        None => Ok(()),
    }
}

/// `aegiron-agent terminal <id> [--command CMD]`; returns the process exit code.
async fn terminal(path: &std::path::Path, id: &str, command: Option<String>) -> Result<i32> {
    use aegiron_agent::remote::SessionKind;
    use aegiron_agent::terminal::{TermCmd, TermEnd};
    use std::io::{IsTerminal, Write};

    let c = Config::load(path)?;
    let t = aegiron_agent::client::Target::from_config(&c, id)?;
    let (mut session, _info) =
        aegiron_agent::client::open(&t, &SessionKind::Terminal, || remote_password(id), || eprintln!("waiting for the person at {id} to accept...")).await?;
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let interactive = command.is_none() && std::io::stdin().is_terminal();
    let (cols, rows) = term::size().unwrap_or((80, 24));
    let raw = if interactive { term::RawMode::enable() } else { None };
    match command {
        // `exit` without a status ends the shell with the command's status.
        Some(cmd) => {
            let _ = cmd_tx.send(TermCmd::Input(format!("{cmd}\rexit\r").into_bytes()));
        }
        None => {
            let tx = cmd_tx.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut stdin = tokio::io::stdin();
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = stdin.read(&mut buf).await {
                    if n == 0 || tx.send(TermCmd::Input(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            });
            #[cfg(unix)]
            {
                let tx = cmd_tx.clone();
                tokio::spawn(async move {
                    let Ok(mut winch) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()) else { return };
                    while winch.recv().await.is_some() {
                        if let Some((c, r)) = term::size()
                            && tx.send(TermCmd::Resize(c, r)).is_err()
                        {
                            break;
                        }
                    }
                });
            }
        }
    }
    let mut out = std::io::stdout();
    let end = aegiron_agent::terminal::run_client(&mut session.conn, cols, rows, cmd_rx, |b| {
        let _ = out.write_all(b);
        let _ = out.flush();
    })
    .await;
    drop(raw);
    match end? {
        TermEnd::Exited(code) => Ok(code.map(|c| c as i32).unwrap_or(1)),
        TermEnd::Closed => Ok(0),
    }
}

/// The local terminal of `aegiron-agent terminal`: size and raw mode (Unix).
mod term {
    #[cfg(unix)]
    pub fn size() -> Option<(u16, u16)> {
        // SAFETY: TIOCGWINSZ fills a winsize struct we own.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            (libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0).then_some((ws.ws_col, ws.ws_row))
        }
    }

    #[cfg(not(unix))]
    pub fn size() -> Option<(u16, u16)> {
        None
    }

    /// Keystrokes go to the remote shell as typed (Ctrl-C included) until dropped.
    pub struct RawMode {
        #[cfg(unix)]
        saved: libc::termios,
    }

    impl RawMode {
        #[cfg(unix)]
        pub fn enable() -> Option<Self> {
            // SAFETY: plain termios calls on stdin with structs we own.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                    return None;
                }
                let saved = t;
                libc::cfmakeraw(&mut t);
                (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) == 0).then_some(RawMode { saved })
            }
        }

        #[cfg(not(unix))]
        pub fn enable() -> Option<Self> {
            None
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: restores the attributes saved by `enable`.
            #[cfg(unix)]
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
            }
        }
    }
}
