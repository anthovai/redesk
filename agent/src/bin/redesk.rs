//! ReDesk desktop app: the agent with a window and a tray icon.
//!
//!   redesk [--config PATH] [--background] [redesk://connect/<id>]
//!
//! `--background` starts hidden in the tray (used by autostart). Launching it again while it
//! runs just brings the existing window to the front, or hands it the `redesk://` link
//! (registered as a URL handler by the Windows installer, which passes it as "%1").
//! macOS delivers URL-scheme launches as an Apple Event (`application:openURLs:`), not as an
//! argument; winit 0.30 / eframe 0.37 do not expose that event, so on macOS a link currently
//! works only when passed explicitly (`open -a ReDesk --args redesk://connect/<id>`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use eframe::egui;
use redesk_agent::config::Config;
use redesk_agent::local::{self, LocalState};
use redesk_agent::signal::Agent;
use redesk_agent::ui::{self, App, OpenRequests, Options, ShowSignal};

/// Loopback port used to find an already running instance (per machine).
const INSTANCE_PORT: u16 = 21119;
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
            url if url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("redesk:")) => match local::parse_link(url) {
                Some(id) => link = Some(id),
                None => bad_link = Some(url.chars().take(80).collect::<String>()),
            },
            _ => {}
        }
    }
    let screenshot = std::env::var_os("REDESK_UI_SCREENSHOT").map(PathBuf::from);
    let demo = std::env::var("REDESK_UI_DEMO").ok();
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
    if screenshot.is_none() && !claim_single_instance(show.clone(), open.clone(), ctx_cell.clone(), link.as_deref()) {
        return Ok(()); // the running instance was asked to show itself (or open the link)
    }
    // Remembered window geometry (not in demo mode: screenshots use the default size).
    let geom = if demo.is_none() { LocalState::load(&LocalState::path_for(&config_path)).main_window } else { None };

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
        // REDESK_UI_SIZE="WxH" overrides the size (screenshots of long views).
        let forced = std::env::var("REDESK_UI_SIZE").ok().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
        });
        let size = forced.unwrap_or(geom.map_or(MAIN_SIZE, |g| [g.size[0].max(MAIN_MIN[0]), g.size[1].max(MAIN_MIN[1])]));
        let mut viewport = egui::ViewportBuilder::default()
            .with_title("ReDesk")
            .with_inner_size(size)
            .with_min_inner_size(MAIN_MIN)
            .with_icon(Arc::new(egui::IconData { rgba: ui::icon_rgba(64), width: 64, height: 64 }))
            .with_visible(!hidden);
        if let Some(pos) = geom.and_then(|g| g.pos) {
            viewport = viewport.with_position(pos);
        }
        let options = eframe::NativeOptions {
            viewport,
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

/// Messages a second launch sends to the running instance (one per connection):
/// `redesk-show`, or `redesk-open <9-digit id>` for a `redesk://` link.
fn parse_instance_msg(raw: &[u8]) -> Option<Option<String>> {
    let msg = std::str::from_utf8(raw).ok()?.trim_end();
    if msg == "redesk-show" {
        return Some(None);
    }
    let id = msg.strip_prefix("redesk-open ")?;
    local::normalize_id(id).filter(|n| n == id).map(Some)
}

/// Returns false if another instance is running (after asking it to show its window or open
/// `link`).
fn claim_single_instance(show: ShowSignal, open: OpenRequests, ctx: Arc<OnceLock<egui::Context>>, link: Option<&str>) -> bool {
    match TcpListener::bind(("127.0.0.1", INSTANCE_PORT)) {
        Ok(listener) => {
            std::thread::spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    let mut buf = Vec::new();
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                    let _ = (&mut conn).take(64).read_to_end(&mut buf);
                    let Some(msg) = parse_instance_msg(&buf) else { continue };
                    if let Some(id) = msg {
                        open.0.lock().unwrap().push_back(id);
                    }
                    show.0.store(true, Ordering::Relaxed);
                    if let Some(c) = ctx.get() {
                        c.request_repaint();
                    }
                }
            });
            true
        }
        Err(_) => {
            if let Ok(mut s) = TcpStream::connect_timeout(&([127, 0, 0, 1], INSTANCE_PORT).into(), Duration::from_secs(1)) {
                let msg = match link {
                    Some(id) => format!("redesk-open {id}"),
                    None => "redesk-show".to_string(),
                };
                let _ = s.write_all(msg.as_bytes());
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

#[cfg(test)]
mod tests {
    use super::parse_instance_msg;

    #[test]
    fn instance_messages() {
        assert_eq!(parse_instance_msg(b"redesk-show"), Some(None));
        assert_eq!(parse_instance_msg(b"redesk-open 123456789"), Some(Some("123456789".into())));
        assert_eq!(parse_instance_msg(b"redesk-open 123456789\n"), Some(Some("123456789".into())));
        for bad in [&b"redesk-open 12345678"[..], b"redesk-open 123 456 789", b"redesk-open 123456789x", b"redesk-open ", b"hello", b"\xff\xfe", b""] {
            assert_eq!(parse_instance_msg(bad), None);
        }
    }
}
