//! Desktop window for the agent, laid out like the remote-support apps people already know:
//! this machine's ID and one-time password on the left, controlling another machine and the
//! live sessions on the right, a status bar along the bottom.

use std::collections::VecDeque;
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, TextEdit, TextStyle, Ui, ViewportCommand, pos2, vec2,
};
use tokio::sync::mpsc;

use crate::account::{self, BookEntry};
use crate::config::{Account, ApproveMode, Config, Verification};
use crate::signal::Agent;
use crate::status::{ApprovalRequest, ServerState, SessionInfo, UiHooks};
use crate::{fingerprint, format_id, service};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);
const SIDEBAR_WIDTH: f32 = 340.0;

#[path = "ui_permissions.rs"]
mod permissions_ui;

// ---------------------------------------------------------------------------------------------
// Look and feel

#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    side: Color32,
    field: Color32,
    line: Color32,
    fg: Color32,
    muted: Color32,
    accent: Color32,
    on_accent: Color32,
    accent_soft: Color32,
    ok: Color32,
    warn: Color32,
    danger: Color32,
}

impl Palette {
    fn of(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self {
                bg: Color32::from_rgb(0x1a, 0x1e, 0x22),
                side: Color32::from_rgb(0x14, 0x17, 0x1a),
                field: Color32::from_rgb(0x24, 0x29, 0x2e),
                line: Color32::from_rgb(0x2e, 0x34, 0x3a),
                fg: Color32::from_rgb(0xe8, 0xec, 0xef),
                muted: Color32::from_rgb(0x95, 0x9f, 0xa8),
                accent: Color32::from_rgb(0x2d, 0xd4, 0xbf),
                on_accent: Color32::from_rgb(0x05, 0x26, 0x22),
                accent_soft: Color32::from_rgb(0x15, 0x33, 0x30),
                ok: Color32::from_rgb(0x4a, 0xde, 0x80),
                warn: Color32::from_rgb(0xfb, 0xbf, 0x24),
                danger: Color32::from_rgb(0xf8, 0x71, 0x71),
            }
        } else {
            Self {
                bg: Color32::WHITE,
                side: Color32::from_rgb(0xf3, 0xf5, 0xf6),
                field: Color32::from_rgb(0xf3, 0xf5, 0xf6),
                line: Color32::from_rgb(0xe2, 0xe6, 0xe9),
                fg: Color32::from_rgb(0x16, 0x1b, 0x20),
                muted: Color32::from_rgb(0x6b, 0x75, 0x80),
                accent: Color32::from_rgb(0x0f, 0x76, 0x6e),
                on_accent: Color32::WHITE,
                accent_soft: Color32::from_rgb(0xe3, 0xf2, 0xf0),
                ok: Color32::from_rgb(0x16, 0xa3, 0x4a),
                warn: Color32::from_rgb(0xd9, 0x77, 0x06),
                danger: Color32::from_rgb(0xdc, 0x26, 0x26),
            }
        }
    }
}

fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semi".into()))
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Thai + Latin system fonts (regular and semibold). A single primary font keeps Thai words
/// shaped in one run; falling back between fonts mid-word leaves visible gaps.
fn install_fonts(ctx: &egui::Context) {
    // "semi" must exist even if no system font is found.
    let mut defs = egui::FontDefinitions::default();
    let prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    defs.families.insert(FontFamily::Name("semi".into()), prop);
    ctx.set_fonts(defs);

    // (path, face index in a .ttc)
    const REGULAR: &[(&str, u32)] = &[
        (r"C:\Windows\Fonts\LeelawUI.ttf", 0),
        (r"C:\Windows\Fonts\tahoma.ttf", 0),
        ("/System/Library/Fonts/Supplemental/SukhumvitSet.ttc", 2), // Text
        ("/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf", 0),
    ];
    const SEMIBOLD: &[(&str, u32)] = &[
        (r"C:\Windows\Fonts\LeelaUIb.ttf", 0),
        (r"C:\Windows\Fonts\tahomabd.ttf", 0),
        ("/System/Library/Fonts/Supplemental/SukhumvitSet.ttc", 4), // Semi Bold
        ("/usr/share/fonts/truetype/noto/NotoSansThai-SemiBold.ttf", 0),
    ];
    let load = |list: &[(&str, u32)]| {
        list.iter().find_map(|(p, i)| {
            std::fs::read(p).ok().map(|b| {
                let mut d = egui::FontData::from_owned(b);
                d.index = *i;
                d
            })
        })
    };
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let fam = |f: FontFamily| InsertFontFamily { family: f, priority: FontPriority::Highest };
    let regular = load(REGULAR);
    let semibold = load(SEMIBOLD);
    if let Some(r) = regular.clone() {
        ctx.add_font(FontInsert::new("ui-regular", r, vec![fam(FontFamily::Proportional)]));
    }
    if let Some(s) = semibold.or(regular) {
        ctx.add_font(FontInsert::new("ui-semibold", s, vec![fam(FontFamily::Name("semi".into()))]));
    }
}

fn style(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.all_styles_mut(|s| {
        let dark = s.visuals.dark_mode;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Heading, semi(20.0));
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size.y = 34.0;
        let line = if dark { Color32::from_rgb(0x2e, 0x34, 0x3a) } else { Color32::from_rgb(0xe2, 0xe6, 0xe9) };
        let field = if dark { Color32::from_rgb(0x24, 0x29, 0x2e) } else { Color32::from_rgb(0xf3, 0xf5, 0xf6) };
        let accent = if dark { Color32::from_rgb(0x2d, 0xd4, 0xbf) } else { Color32::from_rgb(0x0f, 0x76, 0x6e) };
        let v = &mut s.visuals;
        v.panel_fill = if dark { Color32::from_rgb(0x1a, 0x1e, 0x22) } else { Color32::WHITE };
        v.window_fill = v.panel_fill;
        v.extreme_bg_color = field;
        v.selection.bg_fill = accent.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, accent);
        v.window_corner_radius = CornerRadius::same(14);
        v.window_stroke = Stroke::new(1.0, line);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, line);
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = CornerRadius::same(8);
        }
        v.widgets.inactive.weak_bg_fill = field;
        v.widgets.inactive.bg_fill = field;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, line);
        v.widgets.hovered.weak_bg_fill = if dark { Color32::from_rgb(0x2c, 0x32, 0x38) } else { Color32::from_rgb(0xe9, 0xed, 0xef) };
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, line);
        v.widgets.active.weak_bg_fill = v.widgets.hovered.weak_bg_fill;
        v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    });
}

/// Window and tray icon pixels: teal rounded square with a white screen outline.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = s * 0.22;
            let cx = fx.clamp(r, s - r);
            let cy = fy.clamp(r, s - r);
            if (fx - cx).powi(2) + (fy - cy).powi(2) > r * r {
                continue;
            }
            let i = ((y * size + x) * 4) as usize;
            let mut c = [0x0f, 0x76, 0x6e];
            let (l, t, rr, b, w) = (s * 0.22, s * 0.28, s * 0.78, s * 0.66, (s * 0.07).max(1.0));
            let on_frame = (fx >= l && fx <= rr && fy >= t && fy <= b) && (fx - l < w || rr - fx < w || fy - t < w || b - fy < w);
            let stand = fx >= s * 0.38 && fx <= s * 0.62 && fy >= s * 0.74 && fy <= s * 0.74 + w;
            let dot = (fx - s * 0.5).powi(2) + (fy - s * 0.47).powi(2) <= (s * 0.07).powi(2);
            if on_frame || stand {
                c = [255, 255, 255];
            } else if dot {
                c = [0x5e, 0xea, 0xd4];
            }
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    px
}

// ---------------------------------------------------------------------------------------------
// Vector icons (crisp at any DPI, follow the text color)

#[derive(Clone, Copy)]
enum Icon {
    Copy,
    Refresh,
    Eye,
    EyeOff,
    Gear,
    Back,
    Monitor,
    Shield,
}

fn arc(c: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    (0..=24).map(|i| from + (to - from) * i as f32 / 24.0).map(|a| c + vec2(a.cos(), a.sin()) * r).collect()
}

fn paint_icon(ui: &Ui, rect: Rect, icon: Icon, color: Color32) {
    let p = ui.painter();
    let s = rect.width() / 16.0; // icons are drawn on a 16-unit grid
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let st = Stroke::new(1.5 * s.max(1.0), color);
    match icon {
        Icon::Copy => {
            p.rect_stroke(Rect::from_min_max(at(5.5, 5.5), at(13.5, 13.5)), 2.0 * s, st, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![at(10.5, 3.5), at(10.5, 2.5), at(2.5, 2.5), at(2.5, 10.5), at(3.5, 10.5)], st));
        }
        Icon::Refresh => {
            let c = at(8.0, 8.0);
            p.add(Shape::line(arc(c, 5.5 * s, -0.25 * TAU, 0.62 * TAU), st));
            let tip = c + vec2(0.0, -5.5 * s);
            p.add(Shape::line(vec![tip + vec2(-2.6, -2.2) * s, tip, tip + vec2(-2.2, 2.6) * s], st));
        }
        Icon::Eye | Icon::EyeOff => {
            let half = |sign: f32| -> Vec<Pos2> {
                (0..=20).map(|i| i as f32 / 20.0).map(|t| at(1.5 + 13.0 * t, 8.0 + sign * 4.8 * (t * TAU / 2.0).sin())).collect()
            };
            p.add(Shape::line(half(-1.0), st));
            p.add(Shape::line(half(1.0), st));
            p.circle_stroke(at(8.0, 8.0), 2.2 * s, st);
            if matches!(icon, Icon::EyeOff) {
                p.line_segment([at(2.5, 13.5), at(13.5, 2.5)], st);
            }
        }
        Icon::Gear => {
            let c = at(8.0, 8.0);
            p.circle_stroke(c, 2.2 * s, st);
            p.circle_stroke(c, 4.6 * s, st);
            for k in 0..8 {
                let a = k as f32 * TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * 4.6 * s, c + d * 6.6 * s], Stroke::new(2.2 * s.max(1.0), color));
            }
        }
        Icon::Back => {
            p.add(Shape::line(vec![at(9.5, 3.5), at(5.0, 8.0), at(9.5, 12.5)], st));
        }
        Icon::Monitor => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 2.5), at(14.5, 11.5)), 1.5 * s, st, egui::StrokeKind::Middle);
            p.line_segment([at(5.5, 14.0), at(10.5, 14.0)], st);
            p.line_segment([at(8.0, 11.5), at(8.0, 14.0)], st);
        }
        Icon::Shield => {
            p.add(Shape::line(
                vec![at(8.0, 1.8), at(13.5, 3.8), at(13.2, 8.5), at(8.0, 14.2), at(2.8, 8.5), at(2.5, 3.8), at(8.0, 1.8)],
                st,
            ));
            p.add(Shape::line(vec![at(5.5, 8.0), at(7.4, 9.9), at(10.8, 6.3)], st));
        }
    }
}

/// Square, frameless icon button with a hover background and tooltip.
fn icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8.0, if ui.visuals().dark_mode { pal.field } else { pal.line });
    }
    let color = if resp.hovered() { pal.fg } else { pal.muted };
    paint_icon(ui, Rect::from_center_size(rect.center(), vec2(18.0, 18.0)), icon, color);
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let pal = Palette::of(ui);
    let b = Button::new(RichText::new(text).font(semi(14.5)).color(pal.on_accent))
        .fill(pal.accent)
        .stroke(Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(112.0, 40.0));
    ui.add_enabled(enabled, b)
}

fn secondary(ui: &mut Ui, text: &str, color: Option<Color32>) -> Response {
    let pal = Palette::of(ui);
    let rt = RichText::new(text).font(semi(14.0)).color(color.unwrap_or(pal.fg));
    ui.add(Button::new(rt).fill(pal.bg).stroke(Stroke::new(1.0, pal.line)).corner_radius(8).min_size(vec2(0.0, 36.0)))
}

/// iOS / Windows 11 style switch.
fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let pal = Palette::of(ui);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let track = if *on { pal.accent } else if ui.visuals().dark_mode { pal.line } else { Color32::from_rgb(0xcf, 0xd5, 0xda) };
    ui.painter().rect_filled(rect, 11.0, track);
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 8.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control; returns true when the selection changed.
fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)], enabled: bool) -> bool {
    let pal = Palette::of(ui);
    let mut changed = false;
    Frame::new().fill(pal.field).corner_radius(9).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (v, label) in options {
                let selected = *value == *v;
                let text = RichText::new(*label).font(if selected { semi(13.5) } else { FontId::proportional(13.5) });
                let text = text.color(if !enabled { pal.muted.gamma_multiply(0.6) } else if selected { pal.fg } else { pal.muted });
                let b = Button::new(text)
                    .fill(if selected { pal.bg } else { Color32::TRANSPARENT })
                    .stroke(if selected { Stroke::new(1.0, pal.line) } else { Stroke::NONE })
                    .corner_radius(7)
                    .min_size(vec2(0.0, 30.0));
                if ui.add_enabled(enabled, b).clicked() && !selected {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

fn label(ui: &mut Ui, text: &str) {
    let c = Palette::of(ui).muted;
    ui.label(RichText::new(text).size(12.5).color(c));
}

fn initials(name: &str, email: &str) -> String {
    let src = if name.trim().is_empty() { email } else { name };
    src.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

fn avatar(ui: &mut Ui, text: &str, size: f32) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, pal.accent_soft);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, semi(size * 0.42), pal.accent);
}

fn elapsed(since: SystemTime) -> String {
    let s = since.elapsed().unwrap_or_default().as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string()
}

/// Opens a URL in the default browser without going through a shell.
fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url).spawn().map(|_| ())
}

// ---------------------------------------------------------------------------------------------
// App

#[derive(PartialEq)]
enum View {
    Home,
    Settings,
}

#[derive(Default)]
struct SetupForm {
    server: String,
    key: String,
    token: String,
    insecure: bool,
    error: String,
    /// Show the manual server-key form instead of signing in.
    advanced: bool,
}

#[derive(Default)]
struct LoginForm {
    email: String,
    password: String,
    totp: String,
    need_totp: bool,
    busy: bool,
    error: String,
}

/// Results of network calls made on background threads.
enum Job {
    Login { server: String, insecure: bool, result: Result<account::Login, String> },
    Book(Result<Vec<BookEntry>, (u16, String)>),
}

/// Things outside the window that may ask it to show itself (tray, second launch).
#[derive(Clone, Default)]
pub struct ShowSignal(pub Arc<AtomicBool>);

#[derive(Clone)]
pub struct Options {
    pub config_path: PathBuf,
    pub show: ShowSignal,
    pub quit: Arc<AtomicBool>,
    pub has_tray: bool,
    /// Start the network runtime (false for screenshot/demo mode).
    pub start_runtime: bool,
    /// Save a PNG of the window after a few frames, then exit (docs/tests).
    pub screenshot: Option<PathBuf>,
}

pub struct App {
    agent: Option<Arc<Agent>>,
    config_path: PathBuf,
    setup: SetupForm,
    view: View,
    approvals_tx: mpsc::UnboundedSender<ApprovalRequest>,
    approvals_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
    pending: VecDeque<(ApprovalRequest, Instant)>,
    remote_id: String,
    show_password: bool,
    login: LoginForm,
    login_modal: bool,
    jobs_tx: std::sync::mpsc::Sender<Job>,
    jobs_rx: std::sync::mpsc::Receiver<Job>,
    book: Vec<BookEntry>,
    book_loaded: bool,
    book_at: Option<Instant>,
    book_busy: bool,
    viewers: Vec<Arc<ViewerWindow>>,
    new_pw: String,
    new_pw2: String,
    autostart: bool,
    toast: Option<(String, Instant)>,
    show: ShowSignal,
    quit: Arc<AtomicBool>,
    has_tray: bool,
    start_runtime: bool,
    screenshot: Option<(PathBuf, u32)>,
    started: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options, agent: Option<Arc<Agent>>) -> Self {
        style(&cc.egui_ctx);
        let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            agent: None,
            config_path: opts.config_path,
            setup: SetupForm::default(),
            view: View::Home,
            approvals_tx,
            approvals_rx,
            pending: VecDeque::new(),
            remote_id: String::new(),
            show_password: true,
            login: LoginForm::default(),
            login_modal: false,
            jobs_tx,
            jobs_rx,
            book: Vec::new(),
            book_loaded: false,
            book_at: None,
            book_busy: false,
            viewers: Vec::new(),
            new_pw: String::new(),
            new_pw2: String::new(),
            autostart: service::is_installed(),
            toast: None,
            show: opts.show,
            quit: opts.quit,
            has_tray: opts.has_tray,
            start_runtime: opts.start_runtime,
            screenshot: opts.screenshot.map(|p| (p, 0)),
            started: Instant::now(),
        };
        if let Some(a) = agent {
            app.attach(&cc.egui_ctx, a);
        }
        app
    }

    /// Connects the UI to a running agent and starts its network runtime.
    fn attach(&mut self, ctx: &egui::Context, agent: Arc<Agent>) {
        let repaint_ctx = ctx.clone();
        *agent.ui.lock().unwrap() =
            Some(UiHooks { approvals: self.approvals_tx.clone(), repaint: Arc::new(move || repaint_ctx.request_repaint()) });
        if self.start_runtime {
            agent.rotate_temp_password();
            crate::spawn_runtime(agent.clone());
        }
        self.agent = Some(agent);
    }

    /// Demo state for screenshots; never used in normal runs.
    pub fn demo(&mut self, view: &str) {
        let Some(agent) = &self.agent else { return };
        *agent.temp_password.lock().unwrap() = "k7mq3xpd".into();
        let mut st = agent.status.lock().unwrap();
        st.server = ServerState::Online;
        if view == "home" || view == "approval" {
            st.sessions.push(SessionInfo {
                sid: "demo".into(),
                email: "somchai@kgarden.co.th".into(),
                name: "สมชาย ใจดี".into(),
                ip: "203.150.12.34".into(),
                started: SystemTime::now() - Duration::from_secs(754),
                kill: Default::default(),
            });
        }
        drop(st);
        if view == "home" {
            agent.config.lock().unwrap().account =
                Some(Account { email: "it@kgarden.co.th".into(), name: "ฝ่ายไอที".into(), token: String::new() });
            let dev = |h: &str, online: bool| Some(account::BookDevice { hostname: h.into(), alias: String::new(), os: "windows x86_64".into(), online });
            self.book = vec![
                BookEntry { device_id: "512340987".into(), alias: "บัญชี-PC01".into(), favorite: true, device: dev("ACC-PC01", true) },
                BookEntry { device_id: "734120556".into(), alias: "เครื่องประชุม ชั้น 3".into(), favorite: false, device: dev("MEET-3F", true) },
                BookEntry { device_id: "281907433".into(), alias: String::new(), favorite: false, device: dev("SRV-FILES", false) },
            ];
            self.book_loaded = true;
            self.book_at = Some(Instant::now());
        }
        if view == "settings" {
            self.view = View::Settings;
        }
        if view == "viewer" {
            // Remote window filled with a sample screen (docs/screenshots; no network).
            if let Ok(img) = image::open("docs/images/remote-sample.png").or_else(|_| image::open("../docs/images/agent-home.png")) {
                let viewport = egui::ViewportId::from_hash_of("demo-viewer");
                let v = Viewer::demo("บัญชี-PC01", "512340987", "9371:520b:f9c5:c8fc", &img.to_rgba8());
                self.viewers.push(ViewerWindow::new(v, "512340987".into(), viewport));
            }
        }
        if view == "signin" {
            self.login_modal = true;
            self.login.email = "it@kgarden.co.th".into();
        }
        if view == "approval" {
            let (reply, _) = tokio::sync::oneshot::channel();
            self.pending.push_back((
                ApprovalRequest { sid: String::new(), who: "สุดา รักงาน <suda@kgarden.co.th>".into(), ip: "10.1.20.45".into(), reply },
                Instant::now() - Duration::from_secs(8),
            ));
        }
    }

    fn account(&self) -> Option<(String, Account)> {
        let a = self.agent.as_ref()?;
        let c = a.config.lock().unwrap();
        c.account.clone().map(|acc| (c.server_url.clone(), acc))
    }

    fn spawn_job(&self, ctx: &egui::Context, f: impl FnOnce() -> Job + Send + 'static) {
        let (tx, ctx) = (self.jobs_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    fn start_login(&mut self, ctx: &egui::Context) {
        let (server, insecure) = match &self.agent {
            Some(a) => {
                let c = a.config.lock().unwrap();
                (c.server_url.clone(), c.allow_insecure_transport)
            }
            None => (self.setup.server.trim().trim_end_matches('/').to_string(), self.setup.insecure),
        };
        let (email, password, totp) = (self.login.email.clone(), self.login.password.clone(), self.login.totp.clone());
        self.login.busy = true;
        self.login.error.clear();
        self.spawn_job(ctx, move || {
            let result = account::login(&server, insecure, &email, &password, &totp).map_err(|e| format!("{e:#}"));
            Job::Login { server, insecure, result }
        });
    }

    fn refresh_book(&mut self, ctx: &egui::Context) {
        let Some((server, acc)) = self.account() else { return };
        self.book_busy = true;
        self.book_at = Some(Instant::now());
        self.spawn_job(ctx, move || Job::Book(account::address_book(&server, &acc.token)));
    }

    /// Opens a remote-control window for `id` (signing in first if needed).
    fn open_remote(&mut self, ctx: &egui::Context, id: &str, name: &str) {
        let Some(agent) = self.agent.clone() else { return };
        if agent.config.lock().unwrap().id == id {
            self.flash("นี่คือ ID ของเครื่องนี้เอง");
            return;
        }
        if let Some(w) = self.viewers.iter().find(|w| w.device_id == id && !w.closed.load(Ordering::Relaxed)) {
            ctx.send_viewport_cmd_to(w.viewport, ViewportCommand::Focus);
            return;
        }
        let Some((server, acc)) = self.account() else {
            self.flash("เข้าสู่ระบบก่อน เพื่อเชื่อมต่อเครื่องอื่น");
            self.login = LoginForm::default();
            self.login_modal = true;
            return;
        };
        let insecure = agent.config.lock().unwrap().allow_insecure_transport;
        let target = crate::viewer::Target { server, token: acc.token, insecure, device_id: id.to_string(), name: name.to_string() };
        // Frames arrive on the network thread: wake that connection's own window.
        let viewport = egui::ViewportId::from_hash_of(("remote", id, Instant::now()));
        let rctx = ctx.clone();
        let viewer = Viewer::connect(target, Arc::new(move || rctx.request_repaint_of(viewport)));
        self.viewers.push(ViewerWindow::new(viewer, id.to_string(), viewport));
    }

    fn sign_out(&mut self, ctx: &egui::Context) {
        if let Some((server, acc)) = self.account() {
            self.spawn_job(ctx, move || {
                account::logout(&server, &acc.token);
                Job::Book(Ok(Vec::new()))
            });
        }
        self.save_config(|c| c.account = None);
        self.book.clear();
        self.book_loaded = false;
        self.flash("ออกจากระบบแล้ว");
    }

    fn handle_job(&mut self, ctx: &egui::Context, job: Job) {
        match job {
            Job::Login { server, insecure, result } => {
                self.login.busy = false;
                match result {
                    Err(e) => self.login.error = e,
                    Ok(account::Login::NeedTotp) => {
                        self.login.need_totp = true;
                        self.login.error.clear();
                    }
                    Ok(account::Login::Ok(sess)) => {
                        let acc = Account { email: sess.user.email.clone(), name: sess.user.name.clone(), token: sess.token.clone() };
                        if let Err(e) = self.finish_login(ctx, &server, insecure, sess, acc) {
                            self.login.error = format!("{e:#}");
                            return;
                        }
                        self.login = LoginForm::default();
                        self.login_modal = false;
                        self.book_loaded = false;
                        self.refresh_book(ctx);
                    }
                }
            }
            Job::Book(result) => {
                self.book_busy = false;
                match result {
                    Ok(b) => {
                        self.book = b;
                        self.book_loaded = true;
                    }
                    Err((401, _)) if self.account().is_some() => {
                        self.save_config(|c| c.account = None);
                        self.book.clear();
                        self.flash("การเข้าสู่ระบบหมดอายุ กรุณาเข้าสู่ระบบใหม่");
                    }
                    Err(_) => {} // keep the last list; retried on the next refresh
                }
            }
        }
    }

    /// First sign-in creates this device's identity under the user; a later sign-in only
    /// attaches the account (and refuses a server whose key differs from the pinned one).
    fn finish_login(&mut self, ctx: &egui::Context, server: &str, insecure: bool, sess: account::Session, acc: Account) -> anyhow::Result<()> {
        match &self.agent {
            None => {
                let mut c = Config::new(server.to_string(), sess.server_key)?;
                c.enroll_token = Some(sess.enroll_token);
                c.allow_insecure_transport = insecure;
                c.account = Some(acc);
                c.ws_base()?;
                c.save(&self.config_path)?;
                let agent = Agent::new(c, self.config_path.clone(), None)?;
                self.attach(ctx, agent);
            }
            Some(agent) => {
                let mut c = agent.config.lock().unwrap();
                if c.server_key != sess.server_key {
                    anyhow::bail!("กุญแจของเซิร์ฟเวอร์ไม่ตรงกับที่เครื่องนี้ใช้อยู่ — ไม่ได้เข้าสู่ระบบ");
                }
                c.account = Some(acc);
                c.save(&agent.config_path)?;
            }
        }
        Ok(())
    }

    fn show_viewers(&mut self, ctx: &egui::Context) {
        // "Connect again" replaces a finished window's connection in place.
        for i in 0..self.viewers.len() {
            if self.viewers[i].reconnect.swap(false, Ordering::Relaxed) {
                let w = self.viewers.remove(i);
                let name = w.viewer.state.lock().unwrap().device_name.clone();
                self.open_remote(ctx, &w.device_id, &name);
                break;
            }
        }
        self.viewers.retain(|w| !w.closed.load(Ordering::Relaxed));
        for w in &self.viewers {
            let title = format!("{} — ReDesk", w.viewer.state.lock().unwrap().device_name);
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([640.0, 420.0])
                .with_icon(Arc::new(egui::IconData { rgba: icon_rgba(64), width: 64, height: 64 }));
            // REDESK_UI_VIEWER_POS="x,y" places the window (automated screenshots).
            let builder = match std::env::var("REDESK_UI_VIEWER_POS").ok().and_then(|p| {
                let (x, y) = p.split_once(',')?;
                Some([x.parse::<f32>().ok()?, y.parse::<f32>().ok()?])
            }) {
                Some(pos) => builder.with_position(pos),
                None => builder,
            };
            let win = w.clone();
            ctx.show_viewport_deferred(w.viewport, builder, move |ui, _| win.show(ui));
        }
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn bring_to_front(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn save_config(&mut self, f: impl FnOnce(&mut Config)) {
        let Some(agent) = &self.agent else { return };
        let res = {
            let mut c = agent.config.lock().unwrap();
            f(&mut c);
            c.save(&agent.config_path)
        };
        match res {
            Ok(()) => self.flash("บันทึกแล้ว"),
            Err(e) => self.flash(format!("บันทึกไม่สำเร็จ: {e}")),
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(req) = self.approvals_rx.try_recv() {
            self.pending.push_back((req, Instant::now()));
            Self::bring_to_front(ctx);
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
        }
        if let Ok(id) = std::env::var("REDESK_UI_CONNECT")
            && self.viewers.is_empty()
            && self.account().is_some()
            && !self.started.elapsed().is_zero()
            && self.started.elapsed() < Duration::from_secs(3)
        {
            self.open_remote(ctx, &id, &format_id(&id));
        }
        while let Ok(job) = self.jobs_rx.try_recv() {
            self.handle_job(ctx, job);
        }
        if self.account().is_some() && !self.book_busy && self.start_runtime && self.book_at.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.refresh_book(ctx);
        }
        // Expired prompts answer "no" by dropping the reply sender.
        self.pending.retain(|(_, at)| at.elapsed() < APPROVAL_TIMEOUT);

        if self.show.0.swap(false, Ordering::Relaxed) {
            Self::bring_to_front(ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit.load(Ordering::Relaxed) && self.has_tray {
            // Closing the window keeps the agent reachable from the tray.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        if self.quit.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if let Some((path, frames)) = &mut self.screenshot
            && std::env::var_os("REDESK_UI_CONNECT").is_none()
        {
            // REDESK_UI_SCREENSHOT_AFTER=<seconds> waits (e.g. for the server connection) before capturing.
            let wait = std::env::var("REDESK_UI_SCREENSHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
            if self.started.elapsed() >= Duration::from_secs(wait) {
                *frames += 1;
            }
            if *frames == 8 {
                let path = path.clone();
                let quit = self.quit.clone();
                let c = ctx.clone();
                ctx.request_screenshot(move |img| {
                    let px: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Some(buf) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, px) {
                        let _ = buf.save(&path);
                    }
                    quit.store(true, Ordering::Relaxed);
                    c.request_repaint();
                });
            }
            ctx.request_repaint();
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let pal = Palette::of(ui);
        match self.agent.clone() {
            None => {
                egui::CentralPanel::default().frame(Frame::new().fill(pal.side)).show(ui, |ui| self.welcome_view(ui));
            }
            Some(agent) => {
                egui::Panel::bottom("status")
                    .exact_size(34.0)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(Frame::new().fill(pal.side).inner_margin(Margin::symmetric(18, 0)).stroke(Stroke::new(1.0, pal.line)))
                    .show(ui, |ui| self.status_bar(ui, &agent));
                egui::Panel::left("this-device")
                    .exact_size(SIDEBAR_WIDTH)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(Frame::new().fill(pal.side).inner_margin(Margin { left: 28, right: 24, top: 24, bottom: 18 }))
                    .show(ui, |ui| self.sidebar(ui, &agent));
                egui::CentralPanel::default().frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(36, 28))).show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.view {
                        View::Home => self.home(ui, &agent),
                        View::Settings => self.settings(ui, &agent),
                    });
                });
            }
        }
        if self.login_modal {
            self.login_modal(ui);
        }
        self.show_viewers(ui.ctx());
        self.approval_modal(ui);
        self.toast_view(ui);
    }
}

// ---------------------------------------------------------------------------------------------
// Views

impl App {
    fn brand(ui: &mut Ui, size: f32) {
        let tex = ui.ctx().load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([128, 128], &icon_rgba(128)),
            egui::TextureOptions::LINEAR,
        );
        ui.add(egui::Image::new(&tex).fit_to_exact_size(vec2(size, size)));
    }

    fn sidebar(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let (id, verification, approve) = {
            let c = agent.config.lock().unwrap();
            (c.id.clone(), c.verification, c.approve_mode)
        };
        ui.horizontal(|ui| {
            Self::brand(ui, 30.0);
            ui.label(RichText::new("ReDesk").font(semi(19.0)).color(pal.fg));
        });
        ui.add_space(30.0);
        ui.label(RichText::new("เครื่องนี้").font(semi(17.0)).color(pal.fg));
        ui.label(RichText::new("ให้ ID และรหัสผ่านนี้กับผู้ที่จะช่วยเหลือคุณ").size(13.0).color(pal.muted));
        ui.add_space(22.0);

        label(ui, "ID ของคุณ");
        ui.horizontal(|ui| {
            ui.label(RichText::new(format_id(&id)).font(mono(27.0)).color(pal.fg).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Copy, "คัดลอก ID").clicked() {
                    ui.ctx().copy_text(id.clone());
                    self.flash("คัดลอก ID แล้ว");
                }
            });
        });
        ui.add_space(10.0);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(r, 0.0, pal.line);
        ui.add_space(10.0);

        if approve == ApproveMode::Click {
            label(ui, "การเข้าถึง");
            ui.label(RichText::new("ไม่ใช้รหัสผ่าน ต้องกดอนุญาตที่เครื่องนี้").color(pal.fg));
        } else if verification == Verification::Permanent {
            label(ui, "รหัสผ่าน");
            ui.label(RichText::new("ใช้รหัสผ่านถาวรที่ตั้งไว้").color(pal.fg));
        } else {
            label(ui, "รหัสผ่านใช้ครั้งเดียว");
            let pw = agent.temp_password.lock().unwrap().clone();
            ui.horizontal(|ui| {
                let shown = if self.show_password { pw.clone() } else { "•".repeat(pw.chars().count()) };
                ui.label(RichText::new(shown).font(mono(21.0)).color(pal.fg));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon_button(ui, Icon::Copy, "คัดลอกรหัสผ่าน").clicked() {
                        ui.ctx().copy_text(pw.clone());
                        self.flash("คัดลอกรหัสผ่านแล้ว");
                    }
                    if icon_button(ui, Icon::Refresh, "สร้างรหัสใหม่ (รหัสเดิมใช้ไม่ได้ทันที)").clicked() {
                        agent.rotate_temp_password();
                        self.flash("สร้างรหัสผ่านใหม่แล้ว");
                    }
                    let (icon, tip) = if self.show_password { (Icon::EyeOff, "ซ่อน") } else { (Icon::Eye, "แสดง") };
                    if icon_button(ui, icon, tip).clicked() {
                        self.show_password = !self.show_password;
                    }
                });
            });
            ui.label(RichText::new("เปลี่ยนใหม่อัตโนมัติหลังมีผู้เชื่อมต่อสำเร็จ").size(12.5).color(pal.muted));
        }

        if approve != ApproveMode::Password {
            ui.add_space(14.0);
            Frame::new().fill(pal.accent_soft).corner_radius(8).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    paint_icon(ui, r, Icon::Shield, pal.accent);
                    ui.label(RichText::new("ทุกการเชื่อมต่อต้องกดอนุญาตที่เครื่องนี้").size(13.0).color(pal.accent));
                });
            });
        }

        let account = agent.config.lock().unwrap().account.clone();
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            // Signed-in account (like RustDesk's account menu), or a sign-in button.
            match &account {
                Some(acc) => {
                    ui.horizontal(|ui| {
                        avatar(ui, &initials(&acc.name, &acc.email), 34.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let name = if acc.name.is_empty() { acc.email.clone() } else { acc.name.clone() };
                            ui.label(RichText::new(name).font(semi(13.5)).color(pal.fg));
                            ui.label(RichText::new(&acc.email).size(12.0).color(pal.muted));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(Button::new(RichText::new("ออกจากระบบ").size(12.5).color(pal.muted)).frame(false)).clicked() {
                                let ctx = ui.ctx().clone();
                                self.sign_out(&ctx);
                            }
                        });
                    });
                }
                None => {
                    let b = Button::new(RichText::new("เข้าสู่ระบบ").font(semi(14.0)).color(pal.accent))
                        .fill(pal.bg)
                        .stroke(Stroke::new(1.0, pal.line))
                        .corner_radius(8)
                        .min_size(vec2(ui.available_width(), 38.0));
                    // A fixed rect keeps the label centred without a nested layout eating the column.
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                    if ui.put(rect, b).clicked() {
                        self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
                        self.login_modal = true;
                    }
                }
            }
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, pal.line);
            ui.add_space(2.0);
            let (text, icon) = if self.view == View::Home { ("ตั้งค่า", Icon::Gear) } else { ("กลับหน้าหลัก", Icon::Back) };
            let resp = ui.add(
                Button::new(RichText::new(format!("      {text}")).color(pal.fg))
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::NONE)
                    .min_size(vec2(0.0, 36.0)),
            );
            paint_icon(ui, Rect::from_min_size(resp.rect.left_center() + vec2(12.0, -8.0), vec2(16.0, 16.0)), icon, pal.muted);
            if resp.clicked() {
                self.view = if self.view == View::Home { View::Settings } else { View::Home };
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let server = host_of(&agent.config.lock().unwrap().server_url);
        ui.horizontal_centered(|ui| {
            let (color, text, detail) = match &agent.status.lock().unwrap().server {
                ServerState::Online => (pal.ok, "พร้อมใช้งาน".to_string(), format!("เชื่อมต่อ {server} แล้ว · เข้ารหัสต้นทาง-ปลายทาง")),
                ServerState::Connecting => (pal.warn, "กำลังเชื่อมต่อ…".to_string(), server.clone()),
                ServerState::Error(e) => (pal.danger, "ไม่ได้เชื่อมต่อ".to_string(), e.chars().take(80).collect()),
            };
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, color);
            ui.label(RichText::new(text).size(12.5).color(pal.fg));
            ui.label(RichText::new(detail).size(12.5).color(pal.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(12.0).color(pal.muted));
            });
        });
    }

    fn home(&mut self, ui: &mut Ui, agent: &Agent) {
        permissions_ui::missing_permissions_banner(ui);
        let pal = Palette::of(ui);
        ui.label(RichText::new("ควบคุมเครื่องอื่น").font(semi(20.0)).color(pal.fg));
        ui.label(RichText::new("ใส่ ID แล้วควบคุมหน้าจอเครื่องนั้นในหน้าต่างใหม่ เข้ารหัสต้นทาง-ปลายทาง").size(13.0).color(pal.muted));
        ui.add_space(12.0);
        let digits: String = self.remote_id.chars().filter(|c| c.is_ascii_digit()).collect();
        let mut go = false;
        ui.horizontal(|ui| {
            let edit = TextEdit::singleline(&mut self.remote_id)
                .hint_text("ใส่ ID ของเครื่องปลายทาง")
                .font(mono(18.0))
                .margin(Margin::symmetric(12, 9))
                .desired_width((ui.available_width() - 130.0).clamp(160.0, 340.0));
            let r = ui.add(edit);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            if primary(ui, "เชื่อมต่อ", digits.len() == 9).clicked() {
                go = true;
            }
        });
        if go && digits.len() == 9 {
            let ctx = ui.ctx().clone();
            let name = self.book.iter().find(|e| e.device_id == digits).map(|e| e.title()).unwrap_or_else(|| format_id(&digits));
            self.open_remote(&ctx, &digits, &name);
        }

        ui.add_space(28.0);
        self.address_book(ui, agent);
        ui.add_space(28.0);
        let sessions = agent.status.lock().unwrap().sessions.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("ผู้ที่กำลังควบคุมเครื่องนี้").font(semi(16.0)).color(pal.fg));
            if !sessions.is_empty() {
                Frame::new().fill(pal.accent_soft).corner_radius(10).inner_margin(Margin::symmetric(8, 1)).show(ui, |ui| {
                    ui.label(RichText::new(sessions.len().to_string()).font(semi(12.5)).color(pal.accent));
                });
            }
        });
        ui.add_space(4.0);
        if sessions.is_empty() {
            Frame::new().fill(pal.side).corner_radius(12).inner_margin(Margin::symmetric(20, 26)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    paint_icon(ui, r, Icon::Monitor, pal.muted);
                    ui.label(RichText::new("ยังไม่มีใครเชื่อมต่อกับเครื่องนี้").color(pal.fg));
                    ui.label(RichText::new("เมื่อมีผู้ควบคุม จะแสดงที่นี่ และตัดการเชื่อมต่อได้ทุกเมื่อ").size(12.5).color(pal.muted));
                });
            });
        }
        for s in sessions {
            Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    avatar(ui, &initials(&s.name, &s.email), 40.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let who = if s.name.is_empty() { s.email.clone() } else { s.name.clone() };
                        ui.label(RichText::new(who).font(semi(14.5)).color(pal.fg));
                        ui.label(RichText::new(format!("{} · {}", s.email, s.ip)).size(12.5).color(pal.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary(ui, "ตัดการเชื่อมต่อ", Some(pal.danger)).clicked() {
                            s.kill.notify_one();
                        }
                        ui.label(RichText::new(elapsed(s.started)).font(mono(13.0)).color(pal.muted));
                    });
                });
            });
        }
    }

    fn settings(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let c = agent.config.lock().unwrap().clone();
        ui.label(RichText::new("ตั้งค่า").font(semi(20.0)).color(pal.fg));
        ui.add_space(6.0);
        if cfg!(target_os = "macos") {
            permissions_ui::permissions_card(ui);
        }

        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(12.0);
            ui.label(RichText::new(title).font(semi(13.0)).color(pal.muted));
        };
        let group = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(16, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
        };
        let row_sep = |ui: &mut Ui| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, pal.line);
        };
        // A settings row: title + description on the left, a switch on the right.
        let switch_row = |ui: &mut Ui, title: &str, desc: &str, value: &mut bool| -> bool {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.set_min_height(52.0);
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(title).color(pal.fg));
                    ui.label(RichText::new(desc).size(12.5).color(pal.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    changed = toggle(ui, value).changed();
                });
            });
            changed
        };

        section(ui, "สิ่งที่ผู้ควบคุมทำได้");
        let (mut input, mut clip, mut files) = (c.allow_input, c.allow_clipboard, c.allow_file_transfer);
        let mut changed = false;
        group(ui, &mut |ui| {
            changed |= switch_row(ui, "ควบคุมเมาส์และคีย์บอร์ด", "ถ้าปิด ผู้เชื่อมต่อจะดูหน้าจอได้อย่างเดียว", &mut input);
            row_sep(ui);
            changed |= switch_row(ui, "คลิปบอร์ด", "คัดลอกและวางข้อความระหว่างสองเครื่อง", &mut clip);
            row_sep(ui);
            changed |= switch_row(ui, "รับ-ส่งไฟล์", "ไฟล์ที่ได้รับจะอยู่ในโฟลเดอร์ Downloads/ReDesk", &mut files);
        });
        if changed {
            self.save_config(|c| {
                c.allow_input = input;
                c.allow_clipboard = clip;
                c.allow_file_transfer = files;
            });
        }

        section(ui, "ความปลอดภัย");
        let mut approve = c.approve_mode;
        let mut verification = c.verification;
        let has_perm = c.permanent_password.is_some();
        let (mut a_changed, mut v_changed, mut save_pw) = (false, false, false);
        let (pw1, pw2) = (&mut self.new_pw, &mut self.new_pw2);
        group(ui, &mut |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("วิธีอนุญาตการเชื่อมต่อ").color(pal.fg));
            a_changed = segmented(
                ui,
                &mut approve,
                &[(ApproveMode::Password, "ใช้รหัสผ่าน"), (ApproveMode::Click, "กดอนุญาต"), (ApproveMode::Both, "ทั้งสองอย่าง")],
                true,
            );
            ui.add_space(8.0);
            row_sep(ui);
            ui.add_space(8.0);
            ui.label(RichText::new("รหัสผ่านที่ยอมรับ").color(pal.fg));
            v_changed = segmented(
                ui,
                &mut verification,
                &[(Verification::Temporary, "ใช้ครั้งเดียว"), (Verification::Permanent, "ถาวร"), (Verification::Both, "ทั้งสองแบบ")],
                has_perm,
            );
            if !has_perm {
                ui.label(RichText::new("ตั้งรหัสผ่านถาวรด้านล่างก่อน จึงจะเลือกได้").size(12.5).color(pal.muted));
            }
            ui.add_space(8.0);
            row_sep(ui);
            ui.add_space(8.0);
            ui.label(RichText::new(if has_perm { "เปลี่ยนรหัสผ่านถาวร" } else { "รหัสผ่านถาวร" }).color(pal.fg));
            ui.label(RichText::new("สำหรับเข้าเครื่องโดยไม่มีคนเฝ้า เช่น เซิร์ฟเวอร์หรือเครื่องในห้องประชุม").size(12.5).color(pal.muted));
            ui.horizontal(|ui| {
                let w = ((ui.available_width() - 120.0) / 2.0).max(120.0);
                ui.add(TextEdit::singleline(pw1).password(true).hint_text("รหัสผ่านใหม่").margin(Margin::symmetric(10, 8)).desired_width(w));
                ui.add(TextEdit::singleline(pw2).password(true).hint_text("ยืนยันรหัสผ่าน").margin(Margin::symmetric(10, 8)).desired_width(w));
                save_pw = secondary(ui, "บันทึก", None).clicked();
            });
            ui.add_space(8.0);
        });
        if a_changed {
            self.save_config(|c| c.approve_mode = approve);
        }
        if v_changed {
            self.save_config(|c| c.verification = verification);
        }
        if save_pw {
            if self.new_pw.chars().count() < 8 {
                self.flash("รหัสผ่านต้องมีอย่างน้อย 8 ตัวอักษร");
            } else if self.new_pw != self.new_pw2 {
                self.flash("รหัสผ่านทั้งสองช่องไม่ตรงกัน");
            } else if let Ok(hash) = crate::hash_password(&self.new_pw) {
                self.new_pw.clear();
                self.new_pw2.clear();
                self.save_config(|c| {
                    c.permanent_password = Some(hash);
                    if c.verification == Verification::Temporary {
                        c.verification = Verification::Both;
                    }
                });
            }
        }

        section(ui, "ระบบ");
        let mut auto = self.autostart;
        let mut auto_changed = false;
        let server_fp = B64.decode(&c.server_key).map(|k| fingerprint(&k)).unwrap_or_default();
        let device_fp = fingerprint(agent.identity.verifying_key().as_bytes());
        group(ui, &mut |ui| {
            auto_changed = switch_row(ui, "เปิด ReDesk เมื่อล็อกอิน", "เริ่มทำงานในถาดระบบ (tray) โดยไม่เปิดหน้าต่าง", &mut auto);
            row_sep(ui);
            ui.add_space(6.0);
            let console = c.server_url.clone();
            ui.horizontal(|ui| {
                ui.label(RichText::new("คอนโซลบนเว็บ (จัดการผู้ใช้ อุปกรณ์ และ audit log)").size(13.0).color(pal.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(RichText::new("เปิดคอนโซล").size(13.0).color(pal.accent)).frame(false)).clicked() {
                        let _ = open_url(&console);
                    }
                });
            });
            egui::Grid::new("about").num_columns(2).spacing([20.0, 6.0]).show(ui, |ui| {
                for (k, v, m) in [
                    ("เซิร์ฟเวอร์", c.server_url.clone(), false),
                    ("กุญแจเซิร์ฟเวอร์", server_fp.clone(), true),
                    ("กุญแจเครื่องนี้", device_fp.clone(), true),
                    ("เวอร์ชัน", env!("CARGO_PKG_VERSION").to_string(), false),
                ] {
                    ui.label(RichText::new(k).size(13.0).color(pal.muted));
                    let t = RichText::new(v).color(pal.fg);
                    ui.label(if m { t.font(mono(13.0)) } else { t.size(13.0) });
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
        });
        if auto_changed {
            let res = if auto { service::install(&self.config_path) } else { service::uninstall() };
            match res {
                Ok(()) => self.autostart = auto,
                Err(e) => self.flash(format!("ตั้งค่าไม่สำเร็จ: {e}")),
            }
        }
    }

    fn address_book(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let my_id = agent.config.lock().unwrap().id.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("สมุดที่อยู่").font(semi(16.0)).color(pal.fg));
            if self.account().is_some() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Refresh, "รีเฟรช").clicked() {
                        let ctx = ui.ctx().clone();
                        self.refresh_book(&ctx);
                    }
                });
            }
        });
        if self.account().is_none() {
            Frame::new().fill(pal.side).corner_radius(12).inner_margin(Margin::symmetric(18, 16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new("เข้าสู่ระบบเพื่อดูเครื่องของคุณ").color(pal.fg));
                        ui.label(RichText::new("เครื่องในสมุดที่อยู่และสถานะออนไลน์จะแสดงที่นี่").size(12.5).color(pal.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if primary(ui, "เข้าสู่ระบบ", true).clicked() {
                            self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
                            self.login_modal = true;
                        }
                    });
                });
            });
            return;
        }
        if !self.book_loaded {
            ui.label(RichText::new("กำลังโหลด…").size(13.0).color(pal.muted));
            return;
        }
        let entries: Vec<BookEntry> = self.book.iter().filter(|e| e.device_id != my_id).cloned().collect();
        if entries.is_empty() {
            ui.label(RichText::new("ยังไม่มีเครื่องในสมุดที่อยู่ — เพิ่มได้จากคอนโซล ReDesk").size(13.0).color(pal.muted));
            return;
        }
        Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (n, e) in entries.iter().enumerate() {
                if n > 0 {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().rect_filled(r, 0.0, pal.line);
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(50.0);
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    paint_icon(ui, r, Icon::Monitor, if e.online() { pal.accent } else { pal.muted });
                    ui.painter().circle_filled(r.right_bottom() - vec2(2.0, 2.0), 4.0, if e.online() { pal.ok } else { pal.line });
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(e.title()).font(semi(14.0)).color(pal.fg));
                        let status = if e.online() { "ออนไลน์" } else { "ออฟไลน์" };
                        ui.label(RichText::new(format!("{} · {status}", format_id(&e.device_id))).size(12.5).color(pal.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let b = Button::new(RichText::new("เชื่อมต่อ").font(semi(13.5)).color(if e.online() { pal.accent } else { pal.muted }))
                            .fill(pal.bg)
                            .stroke(Stroke::new(1.0, pal.line))
                            .corner_radius(8)
                            .min_size(vec2(0.0, 32.0));
                        if ui.add_enabled(e.online(), b).clicked() {
                            let ctx = ui.ctx().clone();
                            self.open_remote(&ctx, &e.device_id, &e.title());
                        }
                    });
                });
            }
        });
    }

    /// Email / password / 2FA fields. Returns true when the form should be submitted.
    fn login_fields(&mut self, ui: &mut Ui) -> bool {
        let pal = Palette::of(ui);
        let mut submit = false;
        let enter = |ui: &Ui, r: &Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if !self.login.need_totp {
            label(ui, "อีเมล");
            let r = ui.add(TextEdit::singleline(&mut self.login.email).hint_text("name@company.com").margin(Margin::symmetric(10, 8)).desired_width(f32::INFINITY));
            submit |= enter(ui, &r);
            ui.add_space(2.0);
            label(ui, "รหัสผ่าน");
            let r = ui.add(TextEdit::singleline(&mut self.login.password).password(true).margin(Margin::symmetric(10, 8)).desired_width(f32::INFINITY));
            submit |= enter(ui, &r);
        } else {
            ui.label(RichText::new("ใส่รหัส 6 หลักจากแอป Authenticator").color(pal.fg));
            let r = ui.add(
                TextEdit::singleline(&mut self.login.totp)
                    .hint_text("000000")
                    .font(mono(20.0))
                    .char_limit(6)
                    .margin(Margin::symmetric(10, 8))
                    .desired_width(f32::INFINITY),
            );
            r.request_focus();
            submit |= enter(ui, &r);
        }
        if !self.login.error.is_empty() {
            ui.label(RichText::new(&self.login.error).size(13.0).color(pal.danger));
        }
        ui.add_space(6.0);
        let text = if self.login.busy { "กำลังเข้าสู่ระบบ…" } else if self.login.need_totp { "ยืนยัน" } else { "เข้าสู่ระบบ" };
        let ready = !self.login.busy && !self.login.email.trim().is_empty() && !self.login.password.is_empty();
        let b = Button::new(RichText::new(text).font(semi(15.0)).color(pal.on_accent))
            .fill(pal.accent)
            .stroke(Stroke::NONE)
            .corner_radius(8)
            .min_size(vec2(ui.available_width(), 42.0));
        submit |= ui.vertical_centered(|ui| ui.add_enabled(ready, b)).inner.clicked();
        submit && ready
    }

    /// First run: sign in with an organisation account (or, under "advanced", a server key).
    fn welcome_view(&mut self, ui: &mut Ui) {
        let pal = Palette::of(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            Self::brand(ui, 48.0);
            ui.label(RichText::new("เข้าสู่ระบบ ReDesk").font(semi(22.0)).color(pal.fg));
            ui.label(RichText::new("ใช้บัญชีเดียวกับคอนโซล ReDesk ขององค์กร").color(pal.muted));
            ui.add_space(12.0);
            Frame::new()
                .fill(pal.bg)
                .stroke(Stroke::new(1.0, pal.line))
                .corner_radius(14)
                .inner_margin(Margin::symmetric(24, 20))
                .show(ui, |ui| {
                    ui.set_width(400.0f32.min(ui.available_width()));
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        label(ui, "เซิร์ฟเวอร์");
                        ui.add(
                            TextEdit::singleline(&mut self.setup.server)
                                .hint_text("https://redesk.example.com")
                                .margin(Margin::symmetric(10, 8))
                                .desired_width(f32::INFINITY),
                        );
                        if self.setup.server.trim().starts_with("http://") {
                            ui.checkbox(&mut self.setup.insecure, "อนุญาตการเชื่อมต่อที่ไม่เข้ารหัส (ทดสอบเท่านั้น)");
                        }
                        ui.add_space(2.0);
                        if !self.setup.advanced {
                            if self.login_fields(ui) && !self.setup.server.trim().is_empty() {
                                let ctx = ui.ctx().clone();
                                self.start_login(&ctx);
                            }
                        } else {
                            label(ui, "กุญแจสาธารณะของเซิร์ฟเวอร์");
                            ui.add(TextEdit::singleline(&mut self.setup.key).font(mono(13.5)).hint_text("44 ตัวอักษร จากหน้า ตั้งค่าเซิร์ฟเวอร์").margin(Margin::symmetric(10, 8)).desired_width(f32::INFINITY));
                            label(ui, "Enrollment token (ถ้ามี)");
                            ui.add(TextEdit::singleline(&mut self.setup.token).font(mono(13.5)).hint_text("rdk_…").margin(Margin::symmetric(10, 8)).desired_width(f32::INFINITY));
                            if !self.setup.error.is_empty() {
                                ui.label(RichText::new(&self.setup.error).size(13.0).color(pal.danger));
                            }
                            ui.add_space(6.0);
                            let ready = !self.setup.server.trim().is_empty() && !self.setup.key.trim().is_empty();
                            let b = Button::new(RichText::new("เริ่มใช้งาน").font(semi(15.0)).color(pal.on_accent))
                                .fill(pal.accent)
                                .stroke(Stroke::NONE)
                                .corner_radius(8)
                                .min_size(vec2(ui.available_width(), 42.0));
                            if ui.vertical_centered(|ui| ui.add_enabled(ready, b)).inner.clicked() {
                                match self.finish_setup() {
                                    Ok(agent) => {
                                        let ctx = ui.ctx().clone();
                                        self.attach(&ctx, agent);
                                    }
                                    Err(e) => self.setup.error = format!("{e:#}"),
                                }
                            }
                        }
                    });
                });
            ui.add_space(8.0);
            let link = if self.setup.advanced { "กลับไปเข้าสู่ระบบด้วยบัญชี" } else { "ไม่มีบัญชี? ตั้งค่าด้วยกุญแจเซิร์ฟเวอร์" };
            if ui.add(Button::new(RichText::new(link).size(13.0).color(pal.accent)).frame(false)).clicked() {
                self.setup.advanced = !self.setup.advanced;
            }
            ui.add_space(12.0);
        }));
    }

    fn login_modal(&mut self, ui: &mut Ui) {
        let pal = Palette::of(ui);
        let server = self.agent.as_ref().map(|a| host_of(&a.config.lock().unwrap().server_url)).unwrap_or_default();
        let mut submit = false;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("login")).backdrop_color(Color32::from_black_alpha(110)).show(ui.ctx(), |ui| {
            ui.set_width(360.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("เข้าสู่ระบบ").font(semi(18.0)).color(pal.fg));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(RichText::new("ยกเลิก").size(13.0).color(pal.muted)).frame(false)).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new(format!("บัญชีบน {server}")).size(12.5).color(pal.muted));
            ui.add_space(6.0);
            submit = self.login_fields(ui);
        });
        if close {
            self.login_modal = false;
        } else if submit {
            let ctx = ui.ctx().clone();
            self.start_login(&ctx);
        }
    }

    fn finish_setup(&mut self) -> anyhow::Result<Arc<Agent>> {
        let mut c = Config::new(self.setup.server.trim().to_string(), self.setup.key.trim().to_string())?;
        let token = self.setup.token.trim();
        c.enroll_token = (!token.is_empty()).then(|| token.to_string());
        c.allow_insecure_transport = self.setup.insecure;
        c.ws_base()?;
        c.save(&self.config_path)?;
        Agent::new(c, self.config_path.clone(), None)
    }

    fn approval_modal(&mut self, ui: &mut Ui) {
        let Some((req, at)) = self.pending.front() else { return };
        let pal = Palette::of(ui);
        let left = APPROVAL_TIMEOUT.saturating_sub(at.elapsed());
        let (name, email) = match req.who.split_once(" <") {
            Some((n, e)) => (n.to_string(), e.trim_end_matches('>').to_string()),
            None => (req.who.clone(), String::new()),
        };
        let ip = req.ip.clone();
        let perms = self.agent.as_ref().map(|a| {
            let c = a.config.lock().unwrap();
            let mut p = vec![if c.allow_input { "ควบคุมเมาส์และคีย์บอร์ด" } else { "ดูหน้าจอ" }];
            if c.allow_clipboard {
                p.push("คลิปบอร์ด");
            }
            if c.allow_file_transfer {
                p.push("รับ-ส่งไฟล์");
            }
            p.join(" · ")
        });
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("approval")).backdrop_color(Color32::from_black_alpha(110)).show(ui.ctx(), |ui| {
            ui.set_width(360.0);
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                avatar(ui, &initials(&name, &email), 56.0);
                ui.add_space(4.0);
                ui.label(RichText::new("มีผู้ขอควบคุมเครื่องนี้").size(13.0).color(pal.muted));
                ui.label(RichText::new(&name).font(semi(18.0)).color(pal.fg));
                if !email.is_empty() {
                    ui.label(RichText::new(&email).size(13.0).color(pal.muted));
                }
                ui.label(RichText::new(format!("IP {ip}")).font(mono(12.5)).color(pal.muted));
                if let Some(p) = &perms {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("ถ้าอนุญาต จะทำได้: {p}")).size(12.5).color(pal.muted));
                }
            });
            ui.add_space(12.0);
            let frac = left.as_secs_f32() / APPROVAL_TIMEOUT.as_secs_f32();
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, pal.field);
            ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac, r.height())), 2.0, pal.warn);
            ui.label(RichText::new(format!("ปฏิเสธอัตโนมัติใน {} วินาที", left.as_secs())).size(12.5).color(pal.muted));
            ui.add_space(8.0);
            ui.columns(2, |cols| {
                let deny = Button::new(RichText::new("ปฏิเสธ").font(semi(14.5)).color(pal.fg))
                    .fill(pal.bg)
                    .stroke(Stroke::new(1.0, pal.line))
                    .corner_radius(8)
                    .min_size(vec2(cols[0].available_width(), 42.0));
                if cols[0].vertical_centered(|ui| ui.add(deny)).inner.clicked() {
                    answer = Some(false);
                }
                let allow = Button::new(RichText::new("อนุญาต").font(semi(14.5)).color(pal.on_accent))
                    .fill(pal.accent)
                    .stroke(Stroke::NONE)
                    .corner_radius(8)
                    .min_size(vec2(cols[1].available_width(), 42.0));
                if cols[1].vertical_centered(|ui| ui.add(allow)).inner.clicked() {
                    answer = Some(true);
                }
            });
        });
        if let Some(a) = answer {
            let (req, _) = self.pending.pop_front().unwrap();
            let _ = req.reply.send(a);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    fn toast_view(&mut self, ui: &mut Ui) {
        let Some((msg, at)) = &self.toast else { return };
        if at.elapsed() > Duration::from_secs(3) {
            self.toast = None;
            return;
        }
        let pal = Palette::of(ui);
        let msg = msg.clone();
        egui::Area::new(egui::Id::unique("toast")).anchor(Align2::CENTER_BOTTOM, [0.0, -48.0]).show(ui.ctx(), |ui| {
            Frame::new().fill(pal.fg).corner_radius(8).inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.label(RichText::new(msg).size(13.5).color(pal.bg));
            });
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Remote-control window (one native window per connection, like RustDesk)

use crate::viewer::{Phase, TransferStatus, Viewer};

const QUALITY: [(&str, u8, u32); 3] = [("คมชัด", 85, 2560), ("สมดุล", 70, 1920), ("ประหยัดเน็ต", 45, 1280)];

#[derive(Default)]
struct WinUi {
    texture: Option<egui::TextureHandle>,
    password: String,
    quality: usize,
    fullscreen: bool,
    toast: Option<(String, Instant)>,
}

pub struct ViewerWindow {
    pub viewer: Arc<Viewer>,
    pub viewport: egui::ViewportId,
    pub device_id: String,
    ui: Mutex<WinUi>,
    pub closed: AtomicBool,
    pub reconnect: AtomicBool,
}

use std::sync::Mutex;

impl ViewerWindow {
    pub fn new(viewer: Arc<Viewer>, device_id: String, viewport: egui::ViewportId) -> Arc<Self> {
        Arc::new(Self {
            viewer,
            viewport,
            device_id,
            ui: Mutex::new(WinUi { quality: 1, ..Default::default() }),
            closed: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
        })
    }

    fn flash(w: &mut WinUi, msg: impl Into<String>) {
        w.toast = Some((msg.into(), Instant::now()));
    }

    pub fn show(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if ui.input(|i| i.viewport().close_requested()) {
            self.viewer.close();
            self.closed.store(true, Ordering::Relaxed);
            return;
        }
        let pal = Palette::of(ui);
        let mut w = self.ui.lock().unwrap();

        // Pull new screen regions into the GPU texture and pick up state for this frame.
        let (phase, error, name, fp, info, fps, kbps, clip, transfers, fb_size) = {
            let mut st = self.viewer.state.lock().unwrap();
            if st.fb.w > 0 {
                let opts = egui::TextureOptions::LINEAR;
                if w.texture.is_none() || st.fb.resized {
                    let img = egui::ColorImage::from_rgba_unmultiplied([st.fb.w, st.fb.h], &st.fb.rgba);
                    w.texture = Some(ctx.load_texture(format!("remote-{}", self.device_id), img, opts));
                    st.fb.resized = false;
                    st.fb.dirty.clear();
                } else if let Some(tex) = w.texture.as_mut() {
                    let fbw = st.fb.w;
                    for [x, y, dw, dh] in std::mem::take(&mut st.fb.dirty) {
                        if dw == 0 || dh == 0 {
                            continue;
                        }
                        let mut sub = Vec::with_capacity(dw * dh * 4);
                        for row in y..y + dh {
                            let i = (row * fbw + x) * 4;
                            sub.extend_from_slice(&st.fb.rgba[i..i + dw * 4]);
                        }
                        tex.set_partial([x, y], egui::ColorImage::from_rgba_unmultiplied([dw, dh], &sub), opts);
                    }
                }
            }
            (
                st.phase.clone(),
                st.error.clone(),
                st.device_name.clone(),
                st.fingerprint.clone(),
                st.info.clone(),
                st.fps,
                st.kbps,
                st.remote_clipboard.take(),
                st.transfers.clone(),
                (st.fb.w, st.fb.h),
            )
        };
        let connected = phase == Phase::Connected;
        let remote_mac = info.as_ref().is_some_and(|i| i.os == "macos");
        self.debug_hooks(&ctx, &phase, fb_size);

        if let Some(text) = clip {
            if arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok() {
                Self::flash(&mut w, "ได้รับคลิปบอร์ดจากเครื่องปลายทาง");
            }
        }
        let dropped: Vec<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            if connected && info.as_ref().is_some_and(|i| i.files) {
                let n = dropped.len();
                for p in dropped {
                    self.viewer.upload(p);
                }
                Self::flash(&mut w, format!("กำลังส่ง {n} ไฟล์ไปที่ Downloads/ReDesk ของเครื่องปลายทาง"));
            } else {
                Self::flash(&mut w, "เครื่องปลายทางไม่อนุญาตให้รับไฟล์");
            }
        }

        // Toolbar
        egui::Panel::top("remote-bar")
            .exact_size(52.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.side).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, pal.line)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.add_space(7.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(format!("{name}  ")).font(semi(14.5)).color(pal.fg));
                        let mut sub = format_id(&self.device_id);
                        if !fp.is_empty() {
                            sub.push_str(&format!("  ·  E2E {fp}"));
                        }
                        ui.label(RichText::new(sub).font(mono(11.5)).color(pal.muted));
                    });
                    if !connected {
                        return;
                    }
                    ui.add_space(8.0);
                    Frame::new().fill(pal.field).corner_radius(6).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
                        ui.label(RichText::new(format!("{fps} fps · {kbps} kbps")).font(mono(11.5)).color(pal.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if secondary(ui, "ตัดการเชื่อมต่อ", Some(pal.danger)).clicked() {
                            self.viewer.close();
                        }
                        let fs_tip = if w.fullscreen { "ออกจากเต็มจอ" } else { "เต็มจอ" };
                        if ui.button(if w.fullscreen { "⤡" } else { "⤢" }).on_hover_text(fs_tip).clicked() {
                            w.fullscreen = !w.fullscreen;
                            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(w.fullscreen));
                        }
                        if icon_button(ui, Icon::Refresh, "รีเฟรชภาพ").clicked() {
                            self.viewer.send(serde_json::json!({"t": "refresh"}));
                        }
                        if info.as_ref().is_some_and(|i| i.input) && ui.button("Ctrl+Alt+Del").clicked() {
                            send_combo(&self.viewer, &[("ControlLeft", "Control"), ("AltLeft", "Alt")], ("Delete", "Delete"));
                        }
                        if info.as_ref().is_some_and(|i| i.clipboard) && icon_button(ui, Icon::Copy, "ส่งคลิปบอร์ดของเครื่องนี้ไปปลายทาง").clicked() {
                            match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                                Ok(t) => {
                                    self.viewer.send(serde_json::json!({"t": "clip", "s": t}));
                                    Self::flash(&mut w, "ส่งคลิปบอร์ดแล้ว");
                                }
                                Err(_) => Self::flash(&mut w, "คลิปบอร์ดว่าง"),
                            }
                        }
                        let mut q = w.quality;
                        // Right-to-left row: list in reverse so it reads คมชัด → ประหยัดเน็ต.
                        if segmented(ui, &mut q, &[(2, QUALITY[2].0), (1, QUALITY[1].0), (0, QUALITY[0].0)], true) {
                            w.quality = q;
                            self.viewer.send(serde_json::json!({"t": "cfg", "quality": QUALITY[q].1, "max_width": QUALITY[q].2}));
                        }
                        if let Some(i) = &info {
                            if i.displays.len() > 1 {
                                let current = i.displays.iter().find(|d| d.index == i.display).map(|d| d.name.clone()).unwrap_or_default();
                                egui::ComboBox::from_id_salt("display").selected_text(current).show_ui(ui, |ui| {
                                    for d in &i.displays {
                                        let label = if d.primary { format!("{} (หลัก)", d.name) } else { d.name.clone() };
                                        if ui.selectable_label(d.index == i.display, label).clicked() {
                                            self.viewer.send(serde_json::json!({"t": "cfg", "display": d.index}));
                                            if let Some(info) = self.viewer.state.lock().unwrap().info.as_mut() {
                                                info.display = d.index;
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    });
                });
            });

        // Remote screen
        egui::CentralPanel::default().frame(Frame::new().fill(Color32::BLACK)).show(ui, |ui| {
            let area = ui.max_rect();
            if let (Some(tex), (fw, fh)) = (&w.texture, fb_size) {
                let scale = (area.width() / fw as f32).min(area.height() / fh as f32);
                let rect = Rect::from_center_size(area.center(), vec2(fw as f32 * scale, fh as f32 * scale));
                ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                if connected && info.as_ref().is_some_and(|i| i.input) && ui.input(|i| i.viewport().focused != Some(false)) {
                    forward_input(&self.viewer, ui, rect, remote_mac);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                    }
                }
            } else if connected {
                ui.centered_and_justified(|ui| ui.label(RichText::new("กำลังรอภาพหน้าจอ…").color(Color32::from_gray(170))));
            }

            if !connected {
                self.overlay(ui, &mut w, &phase, error.as_deref(), area);
            }

            // File transfers (drag & drop uploads)
            let shown: Vec<_> = transfers.iter().rev().take(4).collect();
            if !shown.is_empty() {
                egui::Area::new(egui::Id::unique(("transfers", self.viewport)))
                    .anchor(Align2::LEFT_BOTTOM, [12.0, -12.0])
                    .show(ui.ctx(), |ui| {
                        Frame::new().fill(pal.bg).corner_radius(10).stroke(Stroke::new(1.0, pal.line)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                            ui.set_width(280.0);
                            for t in shown {
                                ui.label(RichText::new(&t.name).size(12.5).color(pal.fg));
                                match &t.status {
                                    TransferStatus::Active => {
                                        let frac = if t.size > 0 { t.sent as f32 / t.size as f32 } else { 0.0 };
                                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                                        ui.painter().rect_filled(r, 2.0, pal.field);
                                        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac, 4.0)), 2.0, pal.accent);
                                    }
                                    TransferStatus::Done(_) => {
                                        ui.label(RichText::new("ส่งแล้ว").size(12.0).color(pal.ok));
                                    }
                                    TransferStatus::Failed(e) => {
                                        ui.label(RichText::new(format!("ไม่สำเร็จ: {e}")).size(12.0).color(pal.danger));
                                    }
                                }
                            }
                        });
                    });
            }
        });

        if let Some((msg, at)) = &w.toast {
            if at.elapsed() > Duration::from_secs(3) {
                w.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::unique(("vtoast", self.viewport))).anchor(Align2::CENTER_BOTTOM, [0.0, -24.0]).show(&ctx, |ui| {
                    Frame::new().fill(pal.fg).corner_radius(8).inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                        ui.label(RichText::new(msg).size(13.5).color(pal.bg));
                    });
                });
            }
        }
        if !connected || !transfers.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// Automation for tests: REDESK_UI_PASSWORD answers the password prompt once.
    fn debug_hooks(&self, _ctx: &egui::Context, phase: &Phase, _fb: (usize, usize)) {
        static SUBMITTED: AtomicBool = AtomicBool::new(false);
        if *phase == Phase::Password
            && let Ok(pw) = std::env::var("REDESK_UI_PASSWORD")
            && !SUBMITTED.swap(true, Ordering::Relaxed)
        {
            self.viewer.submit_password(&pw);
        }
    }

    fn overlay(&self, ui: &mut Ui, w: &mut WinUi, phase: &Phase, error: Option<&str>, area: Rect) {
        let pal = Palette::of(ui);
        let card = Rect::from_center_size(area.center(), vec2(380.0, 230.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
            Frame::new().fill(pal.bg).corner_radius(14).inner_margin(Margin::same(24)).show(ui, |ui| {
                ui.set_width(332.0);
                ui.vertical_centered(|ui| match phase {
                    Phase::Password => {
                        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                        paint_icon(ui, r, Icon::Shield, pal.accent);
                        ui.label(RichText::new("ใส่รหัสผ่านของเครื่องปลายทาง").font(semi(16.0)).color(pal.fg));
                        ui.label(RichText::new("ดูได้ที่หน้าต่าง ReDesk ของเครื่องนั้น").size(12.5).color(pal.muted));
                        let r = ui.add(
                            TextEdit::singleline(&mut w.password)
                                .password(true)
                                .font(mono(16.0))
                                .margin(Margin::symmetric(10, 8))
                                .desired_width(f32::INFINITY),
                        );
                        if !r.has_focus() && w.password.is_empty() {
                            r.request_focus();
                        }
                        if let Some(e) = error {
                            ui.label(RichText::new(e).size(12.5).color(pal.danger));
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let ok = Button::new(RichText::new("เชื่อมต่อ").font(semi(14.5)).color(pal.on_accent))
                            .fill(pal.accent)
                            .stroke(Stroke::NONE)
                            .corner_radius(8)
                            .min_size(vec2(ui.available_width(), 38.0));
                        if (ui.add_enabled(!w.password.is_empty(), ok).clicked() || enter) && !w.password.is_empty() {
                            self.viewer.submit_password(&w.password);
                            w.password.clear();
                        }
                    }
                    Phase::Closed => {
                        let failed = error.is_some_and(|e| !e.is_empty());
                        ui.label(RichText::new(if failed { "เชื่อมต่อไม่สำเร็จ" } else { "ตัดการเชื่อมต่อแล้ว" }).font(semi(17.0)).color(pal.fg));
                        if let Some(e) = error {
                            ui.label(RichText::new(e).size(13.0).color(pal.muted));
                        }
                        ui.add_space(10.0);
                        ui.columns(2, |c| {
                            if c[0].vertical_centered(|ui| secondary(ui, "ปิดหน้าต่าง", None)).inner.clicked() {
                                self.closed.store(true, Ordering::Relaxed);
                            }
                            let again = Button::new(RichText::new("เชื่อมต่ออีกครั้ง").font(semi(14.0)).color(pal.on_accent))
                                .fill(pal.accent)
                                .stroke(Stroke::NONE)
                                .corner_radius(8)
                                .min_size(vec2(0.0, 36.0));
                            if c[1].vertical_centered(|ui| ui.add(again)).inner.clicked() {
                                self.reconnect.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                    other => {
                        ui.add_space(8.0);
                        ui.add(egui::Spinner::new().size(28.0).color(pal.accent));
                        let text = match other {
                            Phase::Requesting => "กำลังขอเชื่อมต่อ…",
                            Phase::Waiting => "รอเครื่องปลายทางตอบรับ…",
                            Phase::Handshake => "กำลังแลกเปลี่ยนกุญแจเข้ารหัส…",
                            Phase::Approval => "รอผู้ใช้ที่เครื่องปลายทางกดอนุญาต…",
                            _ => "",
                        };
                        ui.label(RichText::new(text).color(pal.fg));
                        if let Some(e) = error {
                            ui.label(RichText::new(e).size(12.5).color(pal.danger));
                        }
                    }
                });
            });
        });
    }
}

/// Presses modifiers, taps `key`, releases in reverse order.
fn send_combo(v: &Viewer, mods: &[(&str, &str)], key: (&str, &str)) {
    let k = |code: &str, name: &str, down: bool| v.send(serde_json::json!({"t": "key", "code": code, "key": name, "down": down}));
    for (c, n) in mods {
        k(c, n, true);
    }
    k(key.0, key.1, true);
    k(key.0, key.1, false);
    for (c, n) in mods.iter().rev() {
        k(c, n, false);
    }
}

/// Modifiers held on this keyboard, translated for the remote OS
/// (Cmd on a Mac viewer acts as Ctrl on a Windows/Linux machine, as in RustDesk).
fn remote_mods(m: egui::Modifiers, remote_mac: bool) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if m.ctrl || (m.mac_cmd && !remote_mac) {
        out.push(("ControlLeft", "Control"));
    }
    if m.mac_cmd && remote_mac {
        out.push(("MetaLeft", "Meta"));
    }
    if m.alt {
        out.push(("AltLeft", "Alt"));
    }
    if m.shift {
        out.push(("ShiftLeft", "Shift"));
    }
    out
}

/// Keys sent as key events (everything else that types text arrives as `Event::Text`).
fn named_key(k: egui::Key) -> Option<(&'static str, &'static str)> {
    use egui::Key::*;
    Some(match k {
        Enter => ("Enter", "Enter"),
        Tab => ("Tab", "Tab"),
        Backspace => ("Backspace", "Backspace"),
        Escape => ("Escape", "Escape"),
        Delete => ("Delete", "Delete"),
        Home => ("Home", "Home"),
        End => ("End", "End"),
        PageUp => ("PageUp", "PageUp"),
        PageDown => ("PageDown", "PageDown"),
        ArrowUp => ("ArrowUp", "ArrowUp"),
        ArrowDown => ("ArrowDown", "ArrowDown"),
        ArrowLeft => ("ArrowLeft", "ArrowLeft"),
        ArrowRight => ("ArrowRight", "ArrowRight"),
        F1 => ("F1", "F1"),
        F2 => ("F2", "F2"),
        F3 => ("F3", "F3"),
        F4 => ("F4", "F4"),
        F5 => ("F5", "F5"),
        F6 => ("F6", "F6"),
        F7 => ("F7", "F7"),
        F8 => ("F8", "F8"),
        F9 => ("F9", "F9"),
        F10 => ("F10", "F10"),
        F11 => ("F11", "F11"),
        F12 => ("F12", "F12"),
        _ => return None,
    })
}

/// Letter/digit keys for shortcuts: (KeyboardEvent.code, key).
fn shortcut_key(k: egui::Key) -> Option<(String, String)> {
    let name = k.name();
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some((format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_lowercase().to_string()));
        }
        if c.is_ascii_digit() {
            return Some((format!("Digit{c}"), c.to_string()));
        }
    }
    None
}

fn forward_input(v: &Viewer, ui: &Ui, rect: Rect, remote_mac: bool) {
    let norm = |p: Pos2| {
        let q = rect.clamp(p);
        (((q.x - rect.left()) / rect.width()) as f64, ((q.y - rect.top()) / rect.height()) as f64)
    };
    let (events, hover) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    let mut moved = None;
    for e in events {
        match e {
            egui::Event::PointerMoved(p) if rect.contains(p) => moved = Some(norm(p)),
            egui::Event::PointerButton { pos, button, pressed, .. } if rect.contains(pos) || !pressed => {
                let b = match button {
                    egui::PointerButton::Primary => 0,
                    egui::PointerButton::Middle => 1,
                    egui::PointerButton::Secondary => 2,
                    _ => continue,
                };
                let (x, y) = norm(pos);
                moved = None;
                v.send(serde_json::json!({"t": "mb", "b": b, "down": pressed, "x": x, "y": y}));
            }
            egui::Event::MouseWheel { unit, delta, .. } if hover.is_some_and(|p| rect.contains(p)) => {
                let lines = |d: f32| match unit {
                    egui::MouseWheelUnit::Line => d,
                    egui::MouseWheelUnit::Point => d / 40.0,
                    egui::MouseWheelUnit::Page => d * 10.0,
                };
                // egui: positive = content moves down (scroll up); the agent's positive = scroll down.
                let (dx, dy) = (-lines(delta.x).round() as i32, -lines(delta.y).round() as i32);
                if dx != 0 || dy != 0 {
                    v.send(serde_json::json!({"t": "wh", "dx": dx, "dy": dy}));
                }
            }
            egui::Event::Text(t) => {
                let t: String = t.chars().filter(|c| !c.is_control()).collect();
                if !t.is_empty() {
                    v.send(serde_json::json!({"t": "text", "s": t}));
                }
            }
            egui::Event::Copy => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), remote_mac), ("KeyC", "c")),
            egui::Event::Cut => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), remote_mac), ("KeyX", "x")),
            egui::Event::Paste(text) => {
                // Paste what is on this machine's clipboard: hand it over first, then press paste remotely.
                v.send(serde_json::json!({"t": "clip", "s": text}));
                send_combo(v, &remote_mods(ui.input(|i| i.modifiers), remote_mac), ("KeyV", "v"));
            }
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                let mods = remote_mods(modifiers, remote_mac);
                if let Some(k) = named_key(key) {
                    send_combo(v, &mods, k);
                } else if (modifiers.ctrl || modifiers.alt || modifiers.mac_cmd || modifiers.command)
                    && let Some((code, name)) = shortcut_key(key)
                {
                    send_combo(v, &mods, (&code, &name));
                }
            }
            _ => {}
        }
    }
    if let Some((x, y)) = moved {
        v.send(serde_json::json!({"t": "mm", "x": x, "y": y}));
    }
}
