//! Aegiron agent: makes this machine reachable for remote control through a self-hosted Aegiron server.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use clap::{Parser, Subcommand};
use tokio::io::AsyncBufReadExt;

use aegiron_agent::config::{ApproveMode, Config, Verification};
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
        #[arg(long)]
        max_fps: Option<u32>,
        #[arg(long)]
        jpeg_quality: Option<u8>,
        #[arg(long)]
        max_width: Option<u32>,
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
        Cmd::Login { server, email, insecure } => {
            let existing = Config::load(&path).ok();
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
            let mut totp = String::new();
            let session = loop {
                match aegiron_agent::account::login(&server, insecure, &email, &password, &totp)? {
                    aegiron_agent::account::Login::Ok(s) => break s,
                    aegiron_agent::account::Login::NeedTotp if totp.is_empty() => totp = rpassword::prompt_password("2FA code: ")?,
                    aegiron_agent::account::Login::NeedTotp => bail!("invalid authentication code"),
                }
            };
            let account = aegiron_agent::config::Account { email: session.user.email.clone(), name: session.user.name.clone(), token: session.token };
            let c = match existing {
                Some(mut c) => {
                    if c.server_key != session.server_key {
                        bail!("server key differs from the one this device trusts; not signing in");
                    }
                    c.account = Some(account);
                    c
                }
                None => {
                    let mut c = Config::new(server, session.server_key)?;
                    c.enroll_token = Some(session.enroll_token);
                    c.allow_insecure_transport = insecure;
                    c.account = Some(account);
                    c
                }
            };
            c.ws_base()?;
            c.save(&path)?;
            println!("Signed in as {} — device ID {}", session.user.email, c.id);
        }
        Cmd::SetPassword => {
            let mut c = Config::load(&path)?;
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
        Cmd::Set { verification, approve_mode, allow_input, allow_clipboard, allow_file_transfer, max_fps, jpeg_quality, max_width } => {
            let mut c = Config::load(&path)?;
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
            if let Some(v) = max_fps { c.max_fps = v.clamp(1, 60); }
            if let Some(v) = jpeg_quality { c.jpeg_quality = v.clamp(10, 95); }
            if let Some(v) = max_width { c.max_width = v.clamp(320, 3840); }
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
            println!("Input: {}, clipboard: {}, file transfer: {}", c.allow_input, c.allow_clipboard, c.allow_file_transfer);
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
