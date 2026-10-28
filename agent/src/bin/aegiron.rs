//! Aegiron desktop app: the agent with a window and a tray icon.
//!
//!   aegiron [--config PATH] [--background] [aegiron://connect/<id>]
//!
//! `--background` starts hidden in the tray (used by autostart). Launching it again while it
//! runs just brings the existing window to the front, or hands it the `aegiron://` link
//! (see `aegiron_agent::instance`: a per-user Unix socket or named pipe, not a TCP port).
//! Windows passes links as "%1" (registered by the installer). macOS delivers them as a
//! `kAEGetURL` Apple Event, both at launch and while the app runs (`aegiron_agent::url_event`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use eframe::egui;
use aegiron_agent::config::Config;
use aegiron_agent::instance::{self, Request};
use aegiron_agent::local::{self, LocalState};
use aegiron_agent::service;
use aegiron_agent::signal::Agent;
use aegiron_agent::ui::{self, App, OpenRequests, Options, ShowSignal};

/// Size of the main window unless a remembered one is available.
const MAIN_SIZE: [f32; 2] = [1000.0, 640.0];
const MAIN_MIN: [f32; 2] = [820.0, 540.0];

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let (mut config_path, mut background, mut link) = (Config::default_path(), false, None);
    let mut bad_link = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--background" => background = true,
            "--config" => config_path = args.next().map(PathBuf::from).unwrap_or(config_path),
            url if ["aegiron:", "redesk:"].iter().any(|s| url.get(..s.len()).is_some_and(|p| p.eq_ignore_ascii_case(s))) => match local::parse_link(url) {
                Some(id) => link = Some(id),
                None => bad_link = Some(url.chars().take(80).collect::<String>()),
            },
            _ => {}
        }
    }
    let screenshot = std::env::var_os("AEGIRON_UI_SCREENSHOT").map(PathBuf::from);
    let demo = std::env::var("AEGIRON_UI_DEMO").ok();
    init_logging();
    if let Some(u) = bad_link {
        tracing::warn!("ignoring invalid link {u:?}");
    }

    let show = ShowSignal::default();
    let open = OpenRequests::default();
    if let Some(id) = &link {
        open.0.lock().unwrap().push_back(id.clone());
    }
    let ctx_cell: Arc<OnceLock<egui::Context>> = Arc::default();
    if screenshot.is_none() {
        let req = link.clone().map_or(Request::Show, Request::Open);
        let (s, o, c) = (show.clone(), open.clone(), ctx_cell.clone());
        let first = instance::claim(&config_path, &req, move |r| deliver(&s, &o, &c, r));
        if !first {
            tracing::info!("another Aegiron is running; sent it {req:?}");
            return Ok(()); // the running instance was asked to show itself (or open the link)
        }
        #[cfg(target_os = "macos")]
        install_url_handler(&show, &open, &ctx_cell);
    }
    // Remembered window geometry (not in demo mode: screenshots use the default size).
    let geom = if demo.is_none() { LocalState::load(&LocalState::path_for(&config_path)).main_window } else { None };

    let agent = match &demo {
        Some(v) if v == "setup" => None,
        Some(_) => {
            let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7u8; 32]);
            let mut c = Config::new("https://aegiron.kgarden.co.th".into(), key).expect("demo config");
            c.id = "895698986".into();
            Agent::new(c, std::env::temp_dir().join("aegiron-demo.toml"), None).ok()
        }
        None => match Config::load(&config_path) {
            Ok(c) => match Agent::new(c, config_path.clone(), None) {
                Ok(a) if screenshot.is_none() => {
                    match service::migrate_legacy(&config_path) {
                        Ok(true) => tracing::info!("moved ReDesk's start-at-login entry to Aegiron"),
                        Ok(false) => {}
                        Err(e) => tracing::warn!("could not migrate ReDesk's start-at-login entry: {e:#}"),
                    }
                    Some(a)
                }
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::error!("invalid config {}: {e:#}", config_path.display());
                    None
                }
            },
            Err(_) => None, // first run: the window shows the setup form
        },
    };

    let quit = Arc::new(AtomicBool::new(false));
    let has_tray = cfg!(any(windows, target_os = "macos")) && screenshot.is_none();
    let opts = Options {
        config_path,
        show: show.clone(),
        open,
        quit: quit.clone(),
        has_tray,
        start_runtime: demo.is_none(),
        screenshot,
    };
    // Windows: Direct3D 12 via wgpu works on VMs and machines without OpenGL 2 drivers;
    // fall back to the other renderer if the preferred one cannot start.
    let (first, second) = if cfg!(windows) {
        (eframe::Renderer::Wgpu, eframe::Renderer::Glow)
    } else {
        (eframe::Renderer::Glow, eframe::Renderer::Wgpu)
    };
    let hidden = background && has_tray && agent.is_some();
    let launch = |renderer| {
        let (opts, agent, demo, ctx_cell, show, quit) =
            (opts.clone(), agent.clone(), demo.clone(), ctx_cell.clone(), show.clone(), quit.clone());
        // AEGIRON_UI_SIZE="WxH" overrides the size (screenshots of long views).
        let forced = std::env::var("AEGIRON_UI_SIZE").ok().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
        });
        let size = forced.unwrap_or(geom.map_or(MAIN_SIZE, |g| [g.size[0].max(MAIN_MIN[0]), g.size[1].max(MAIN_MIN[1])]));
        let mut viewport = egui::ViewportBuilder::default()
            .with_title("Aegiron")
            .with_inner_size(size)
            .with_min_inner_size(MAIN_MIN)
            .with_icon(Arc::new(egui::IconData { rgba: ui::icon_rgba(64), width: 64, height: 64 }))
            .with_visible(!hidden)
            // The GL surface format is shared by all windows: it must allow transparency so the
            // whiteboard overlay can be see-through. Every other window paints an opaque
            // background and opts out explicitly.
            .with_transparent(true);
        if let Some(pos) = geom.and_then(|g| g.pos) {
            viewport = viewport.with_position(pos);
        }
        let options = eframe::NativeOptions {
            viewport,
            renderer,
            ..Default::default()
        };
        eframe::run_native(
            "Aegiron",
            options,
            Box::new(move |cc| {
                let _ = ctx_cell.set(cc.egui_ctx.clone());
                // Again now that AppKit has finished launching (it may install its own handler).
                #[cfg(target_os = "macos")]
                if opts.screenshot.is_none() {
                    install_url_handler(&show, &opts.open, &ctx_cell);
                }
                if has_tray {
                    tray::create(&cc.egui_ctx, show, quit);
                }
                let mut app = App::new(cc, opts, agent);
                if let Some(view) = &demo {
                    app.demo(view);
                }
                Ok(Box::new(app))
            }),
        )
    };
    launch(first).or_else(|e| {
        tracing::warn!("{first:?} renderer failed ({e}); retrying with {second:?}");
        launch(second)
    })
}

/// Hands a request from another launch (or an Apple Event) to the window.
fn deliver(show: &ShowSignal, open: &OpenRequests, ctx: &OnceLock<egui::Context>, req: Request) {
    if let Request::Open(id) = req {
        open.0.lock().unwrap().push_back(id);
    }
    show.0.store(true, Ordering::Relaxed);
    if let Some(c) = ctx.get() {
        c.request_repaint();
    }
}

#[cfg(target_os = "macos")]
fn install_url_handler(show: &ShowSignal, open: &OpenRequests, ctx: &Arc<OnceLock<egui::Context>>) {
    let (show, open, ctx) = (show.clone(), open.clone(), ctx.clone());
    aegiron_agent::url_event::install(move |r| match r {
        Ok(id) => {
            tracing::info!("aegiron:// link received (Apple Event): {id}");
            deliver(&show, &open, &ctx, Request::Open(id));
        }
        Err(u) => tracing::warn!("ignoring invalid aegiron:// link (Apple Event): {u:?}"),
    });
}

fn init_logging() {
    let dir = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("Aegiron");
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("aegiron.log"));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match file {
        Ok(f) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod tray {
    use super::*;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    pub fn create(ctx: &egui::Context, show: ShowSignal, quit: Arc<AtomicBool>) {
        let open = MenuItem::new("เปิด Aegiron", true, None);
        let exit = MenuItem::new("ออกจาก Aegiron", true, None);
        let menu = Menu::new();
        let _ = menu.append(&open);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&exit);
        let (open_id, exit_id) = (open.id().clone(), exit.id().clone());

        let Ok(icon) = Icon::from_rgba(ui::icon_rgba(32), 32, 32) else { return };
        match TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Aegiron").with_icon(icon).build() {
            Ok(tray) => {
                // Lives for the whole process.
                Box::leak(Box::new(tray));
            }
            Err(e) => {
                tracing::warn!("tray icon unavailable: {e}");
                return;
            }
        }

        let (c1, s1, q1) = (ctx.clone(), show.clone(), quit);
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == open_id {
                s1.0.store(true, Ordering::Relaxed);
            } else if e.id == exit_id {
                q1.store(true, Ordering::Relaxed);
            }
            c1.request_repaint();
        }));
        let c2 = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show.0.store(true, Ordering::Relaxed);
                c2.request_repaint();
            }
        }));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod tray {
    use super::*;
    pub fn create(_: &egui::Context, _: ShowSignal, _: Arc<AtomicBool>) {}
}
