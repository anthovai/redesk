//! ReDesk desktop app: the agent with a window and a tray icon.
//!
//!   redesk [--config PATH] [--background]
//!
//! `--background` starts hidden in the tray (used by autostart). Launching it again while it
//! runs just brings the existing window to the front.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use eframe::egui;
use redesk_agent::config::Config;
use redesk_agent::signal::Agent;
use redesk_agent::ui::{self, App, Options, ShowSignal};

/// Loopback port used to find an already running instance (per machine).
const INSTANCE_PORT: u16 = 21119;

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let (mut config_path, mut background) = (Config::default_path(), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--background" => background = true,
            "--config" => config_path = args.next().map(PathBuf::from).unwrap_or(config_path),
            _ => {}
        }
    }
    let screenshot = std::env::var_os("REDESK_UI_SCREENSHOT").map(PathBuf::from);
    let demo = std::env::var("REDESK_UI_DEMO").ok();
    init_logging();

    let show = ShowSignal::default();
    let ctx_cell: Arc<OnceLock<egui::Context>> = Arc::default();
    if screenshot.is_none() && !claim_single_instance(show.clone(), ctx_cell.clone()) {
        return Ok(()); // the running instance was asked to show itself
    }

    let agent = match &demo {
        Some(v) if v == "setup" => None,
        Some(_) => {
            let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7u8; 32]);
            let mut c = Config::new("https://redesk.kgarden.co.th".into(), key).expect("demo config");
            c.id = "895698986".into();
            Agent::new(c, std::env::temp_dir().join("redesk-demo.toml"), None).ok()
        }
        None => match Config::load(&config_path) {
            Ok(c) => match Agent::new(c, config_path.clone(), None) {
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
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("ReDesk")
                .with_inner_size([880.0, 560.0])
                .with_min_inner_size([780.0, 500.0])
                .with_icon(Arc::new(egui::IconData { rgba: ui::icon_rgba(64), width: 64, height: 64 }))
                .with_visible(!hidden),
            renderer,
            ..Default::default()
        };
        eframe::run_native(
            "ReDesk",
            options,
            Box::new(move |cc| {
                let _ = ctx_cell.set(cc.egui_ctx.clone());
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

/// Returns false if another instance is running (after asking it to show its window).
fn claim_single_instance(show: ShowSignal, ctx: Arc<OnceLock<egui::Context>>) -> bool {
    match TcpListener::bind(("127.0.0.1", INSTANCE_PORT)) {
        Ok(listener) => {
            std::thread::spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    let mut buf = [0u8; 16];
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                    if conn.read(&mut buf).is_ok_and(|n| &buf[..n] == b"redesk-show") {
                        show.0.store(true, Ordering::Relaxed);
                        if let Some(c) = ctx.get() {
                            c.request_repaint();
                        }
                    }
                }
            });
            true
        }
        Err(_) => {
            if let Ok(mut s) = TcpStream::connect_timeout(&([127, 0, 0, 1], INSTANCE_PORT).into(), Duration::from_secs(1)) {
                let _ = s.write_all(b"redesk-show");
                return false;
            }
            true // port taken by something else: run anyway
        }
    }
}

fn init_logging() {
    let dir = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("ReDesk");
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("redesk.log"));
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
        let open = MenuItem::new("เปิด ReDesk", true, None);
        let exit = MenuItem::new("ออกจาก ReDesk", true, None);
        let menu = Menu::new();
        let _ = menu.append(&open);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&exit);
        let (open_id, exit_id) = (open.id().clone(), exit.id().clone());

        let Ok(icon) = Icon::from_rgba(ui::icon_rgba(32), 32, 32) else { return };
        match TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("ReDesk").with_icon(icon).build() {
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
