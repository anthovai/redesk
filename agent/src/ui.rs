//! Desktop window for the agent, laid out like the remote-support apps people already know:
//! this machine's ID and one-time password on the left, controlling another machine and the
//! live sessions on the right, a status bar along the bottom.

use std::collections::{HashMap, VecDeque};
use std::f32::consts::TAU;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eframe::egui::{
    self, Align, Align2, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Pos2, Rect, Response,
    RichText, Sense, Shape, Stroke, TextEdit, TextStyle, Ui, ViewportCommand, pos2, vec2,
};
use tokio::sync::mpsc;

use crate::account::{self, BookEntry};
use crate::config::{Account, ApproveMode, Config, Verification};
use crate::local::{self, LocalState, WindowGeom};
use crate::signal::Agent;
use crate::status::{ApprovalRequest, ServerState, SessionInfo, UiHooks};
use crate::lan::LanPeer;
use crate::viewer::Viewer;
use crate::updater::{self, Release};
use crate::{fingerprint, format_id, lan, service};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);
const SIDEBAR_WIDTH: f32 = 290.0;
const TABBAR_HEIGHT: f32 = 38.0;
const CARD_MIN_W: f32 = 190.0;
const CARD_MAX_W: f32 = 260.0;
const CARD_H: f32 = 140.0;
const CARD_HEAD: f32 = 88.0;
const VIEWER_SIZE: [f32; 2] = [1280.0, 800.0];
const PIN_MAX_TRIES: u32 = 5;
const PIN_LOCKOUT: Duration = Duration::from_secs(60);

#[path = "ui_permissions.rs"]
mod permissions_ui;
#[path = "ui_settings.rs"]
mod settings_ui;
#[path = "ui_cm.rs"]
mod cm_ui;
#[path = "ui_viewer.rs"]
mod viewer_ui;

pub use viewer_ui::ViewerWindow;

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
                bg: Color32::from_rgb(0x25, 0x25, 0x28),
                side: Color32::from_rgb(0x18, 0x19, 0x1e),
                field: Color32::from_rgb(0x2f, 0x30, 0x35),
                line: Color32::from_rgb(0x3d, 0x3e, 0x44),
                fg: Color32::from_rgb(0xf2, 0xf2, 0xf2),
                muted: Color32::from_rgb(0x99, 0x99, 0x99),
                accent: Color32::from_rgb(0x3d, 0x8b, 0xff),
                on_accent: Color32::WHITE,
                accent_soft: Color32::from_rgb(0x1b, 0x2c, 0x48),
                ok: Color32::from_rgb(0x3b, 0xc4, 0x6b),
                warn: Color32::from_rgb(0xf5, 0xa6, 0x23),
                danger: Color32::from_rgb(0xff, 0x5c, 0x5c),
            }
        } else {
            Self {
                // RustDesk's light theme: white panes, #EFEFF2 gray, #0071FF accent.
                bg: Color32::WHITE,
                side: Color32::from_rgb(0xef, 0xef, 0xf2),
                field: Color32::from_rgb(0xf4, 0xf4, 0xf6),
                line: Color32::from_rgb(0xe0, 0xe0, 0xe5),
                fg: Color32::from_rgb(0x22, 0x22, 0x22),
                muted: Color32::from_rgb(0x88, 0x88, 0x88),
                accent: Color32::from_rgb(0x00, 0x71, 0xff),
                on_accent: Color32::WHITE,
                accent_soft: Color32::from_rgb(0xe5, 0xf0, 0xff),
                ok: Color32::from_rgb(0x2e, 0xb8, 0x5c),
                warn: Color32::from_rgb(0xf0, 0x8c, 0x00),
                danger: Color32::from_rgb(0xff, 0x3b, 0x30),
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
        let line = if dark { Color32::from_rgb(0x3d, 0x3e, 0x44) } else { Color32::from_rgb(0xe0, 0xe0, 0xe5) };
        let field = if dark { Color32::from_rgb(0x2f, 0x30, 0x35) } else { Color32::from_rgb(0xf4, 0xf4, 0xf6) };
        let accent = if dark { Color32::from_rgb(0x3d, 0x8b, 0xff) } else { Color32::from_rgb(0x00, 0x71, 0xff) };
        let v = &mut s.visuals;
        v.panel_fill = if dark { Color32::from_rgb(0x25, 0x25, 0x28) } else { Color32::WHITE };
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
        v.widgets.hovered.weak_bg_fill = if dark { Color32::from_rgb(0x38, 0x39, 0x3f) } else { Color32::from_rgb(0xe8, 0xe8, 0xec) };
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, line);
        v.widgets.active.weak_bg_fill = v.widgets.hovered.weak_bg_fill;
        v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    });
}

/// Aegiron logo pixels: a white shield (aegis) with a blue "A" on a blue rounded square.
/// Drawn in code so every size (window, tray, .ico, .icns) stays sharp; 4×4 supersampled.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    // Shield outline in unit coordinates: flat top, straight sides, curved to a point.
    let mut shield = vec![(0.235f32, 0.215f32), (0.765, 0.215), (0.765, 0.47)];
    let bez = |t: f32, x0: f32| {
        let (p0, c, p1) = ((x0, 0.47f32), (x0, 0.70f32), (0.5f32, 0.835f32));
        let u = 1.0 - t;
        (u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0, u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1)
    };
    shield.extend((1..=16).map(|i| bez(i as f32 / 16.0, 0.765)));
    shield.extend((0..16).rev().map(|i| bez(i as f32 / 16.0, 0.235)));
    let in_shield = |x: f32, y: f32| {
        let mut inside = false;
        let mut j = shield.len() - 1;
        for i in 0..shield.len() {
            let ((xi, yi), (xj, yj)) = (shield[i], shield[j]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    };
    let seg = |x: f32, y: f32, a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - a.0 - t * dx).powi(2) + (y - a.1 - t * dy).powi(2)).sqrt()
    };
    let half = 0.041;
    let in_a = |x: f32, y: f32| {
        let apex = (0.5, 0.325);
        seg(x, y, apex, (0.378, 0.668)) < half || seg(x, y, apex, (0.622, 0.668)) < half || seg(x, y, (0.43, 0.56), (0.57, 0.56)) < half * 0.8
    };
    let in_square = |x: f32, y: f32| {
        let r = 0.225;
        let (cx, cy) = (x.clamp(r, 1.0 - r), y.clamp(r, 1.0 - r));
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };

    const SS: u32 = 4;
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for pxi in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (pxi as f32 + (sx as f32 + 0.5) / SS as f32) / s;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / s;
                    if !in_square(x, y) {
                        continue;
                    }
                    // Vertical gradient #2B8CFF → #0057E0.
                    let mut c = [43.0 + (0.0 - 43.0) * y, 140.0 + (87.0 - 140.0) * y, 255.0 + (224.0 - 255.0) * y];
                    if in_shield(x, y) && !in_a(x, y) {
                        c = [255.0, 255.0, 255.0];
                    } else if in_shield(x, y) {
                        c = [0.0, 113.0, 255.0];
                    }
                    r += c[0];
                    g += c[1];
                    b += c[2];
                    a += 1.0;
                }
            }
            if a > 0.0 {
                let i = ((py * size + pxi) * 4) as usize;
                let n = (SS * SS) as f32;
                px[i..i + 4].copy_from_slice(&[(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n * 255.0) as u8]);
            }
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
    StarFilled,
    Search,
    Grid,
    List,
    /// Vertical ellipsis (⋮).
    More,
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
        Icon::StarFilled => {
            let c = at(8.0, 8.6);
            let pts: Vec<Pos2> = (0..10)
                .map(|k| {
                    let a = -0.25 * TAU + k as f32 * TAU / 10.0;
                    let r = if k % 2 == 0 { 6.6 } else { 2.8 } * s;
                    c + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            // The star is concave: fill it as triangles around the centre.
            for k in 0..10 {
                p.add(Shape::convex_polygon(vec![c, pts[k], pts[(k + 1) % 10]], color, Stroke::NONE));
            }
            let mut outline = pts.clone();
            outline.push(pts[0]);
            p.add(Shape::line(outline, Stroke::new(1.2 * s.max(1.0), color)));
        }
        Icon::Search => {
            p.circle_stroke(at(6.8, 6.8), 4.3 * s, st);
            p.line_segment([at(10.0, 10.0), at(14.0, 14.0)], st);
        }
        Icon::Grid => {
            for (x, y) in [(2.0, 2.0), (9.0, 2.0), (2.0, 9.0), (9.0, 9.0)] {
                p.rect_stroke(Rect::from_min_size(at(x, y), vec2(5.0, 5.0) * s), 1.2 * s, st, egui::StrokeKind::Middle);
            }
        }
        Icon::List => {
            for y in [3.5, 8.0, 12.5] {
                p.circle_filled(at(2.5, y), 1.1 * s, color);
                p.line_segment([at(5.5, y), at(14.0, y)], st);
            }
        }
        Icon::More => {
            for y in [3.5, 8.0, 12.5] {
                p.circle_filled(at(8.0, y), 1.4 * s, color);
            }
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

/// "5 นาทีที่แล้ว" for a unix time.
fn ago(unix: u64) -> String {
    let s = local::unix_now().saturating_sub(unix);
    match s {
        0..60 => "เมื่อสักครู่".into(),
        60..3600 => format!("{} นาทีที่แล้ว", s / 60),
        3600..86400 => format!("{} ชั่วโมงที่แล้ว", s / 3600),
        86400..2_592_000 => format!("{} วันที่แล้ว", s / 86400),
        2_592_000..31_536_000 => format!("{} เดือนที่แล้ว", s / 2_592_000),
        _ => "นานกว่า 1 ปี".into(),
    }
}

/// 4-8 digits, like RustDesk's unlock PIN.
fn pin_problem(pin: &str) -> Option<&'static str> {
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        Some("PIN ต้องเป็นตัวเลข 4–8 หลัก")
    } else {
        None
    }
}

fn verify_pin(hash: &str, pin: &str) -> bool {
    !pin.is_empty() && PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(pin.as_bytes(), &h).is_ok())
}

// ---------------------------------------------------------------------------------------------
// Main-window pieces shaped like RustDesk's desktop home

#[derive(Clone, Copy)]
enum TabIcon {
    Logo,
    Gear,
}

/// One tab of the strip under the title bar. Returns the tab response and whether its close
/// button was clicked.
fn tab_chip(ui: &mut Ui, text: &str, icon: TabIcon, selected: bool, closable: bool) -> (Response, bool) {
    let pal = Palette::of(ui);
    let font = if selected { semi(13.0) } else { FontId::proportional(13.0) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, if selected { pal.fg } else { pal.muted });
    let w = 14.0 + 16.0 + 8.0 + galley.size().x + if closable { 30.0 } else { 16.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TABBAR_HEIGHT - 6.0), Sense::click());
    let rect = rect.translate(vec2(0.0, 3.0));
    let corners = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
    if selected {
        ui.painter().rect_filled(rect, corners, pal.bg);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect.shrink2(vec2(0.0, 3.0)), 6, pal.line.gamma_multiply(0.6));
    }
    let ic = Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(16.0, 16.0));
    match icon {
        TabIcon::Logo => {
            let tex = ui.ctx().load_texture(
                "logo-tab",
                egui::ColorImage::from_rgba_unmultiplied([64, 64], &icon_rgba(64)),
                egui::TextureOptions::LINEAR,
            );
            ui.painter().image(tex.id(), ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        TabIcon::Gear => paint_icon(ui, ic, Icon::Gear, if selected { pal.accent } else { pal.muted }),
    }
    ui.painter().galley(pos2(ic.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, pal.fg);
    let mut closed = false;
    if closable {
        let x = Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(18.0, 18.0));
        let xr = ui.interact(x, resp.id.with("close"), Sense::click());
        if xr.hovered() {
            ui.painter().rect_filled(x, 4, pal.line);
        }
        let c = x.center();
        let st = Stroke::new(1.3, if xr.hovered() { pal.fg } else { pal.muted });
        ui.painter().line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], st);
        ui.painter().line_segment([c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)], st);
        closed = xr.on_hover_text("ปิดแท็บ").clicked();
    }
    (resp.on_hover_cursor(egui::CursorIcon::PointingHand), closed)
}

/// A labelled value with RustDesk's thin accent bar on its left.
fn accent_block(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let pal = Palette::of(ui);
    let r = Frame::new()
        .inner_margin(Margin { left: 14, right: 0, top: 2, bottom: 2 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui)
        })
        .response
        .rect;
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), 1.5, pal.accent);
}

fn small_icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, if ui.visuals().dark_mode { pal.field } else { pal.line });
    }
    paint_icon(ui, Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), icon, if resp.hovered() { pal.accent } else { pal.muted });
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// White card floating on the gray pane.
fn card_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.bg)
        .corner_radius(12)
        .inner_margin(Margin::symmetric(22, 18))
        .shadow(egui::Shadow { offset: [0, 1], blur: 8, spread: 0, color: Color32::from_black_alpha(18) })
}

/// Header colors of device cards, picked from the ID so a device keeps its color.
const CARD_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x2f, 0x80, 0xed),
    Color32::from_rgb(0x9b, 0x51, 0xe0),
    Color32::from_rgb(0x27, 0xae, 0x60),
    Color32::from_rgb(0xeb, 0x57, 0x57),
    Color32::from_rgb(0xf2, 0x99, 0x4a),
    Color32::from_rgb(0x56, 0xcc, 0xf2),
    Color32::from_rgb(0x6f, 0x52, 0xed),
    Color32::from_rgb(0x21, 0x96, 0x8f),
];

fn card_color(id: &str) -> Color32 {
    let h = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    CARD_COLORS[(h % CARD_COLORS.len() as u64) as usize]
}

#[derive(Clone, Copy, PartialEq)]
enum OsKind {
    Windows,
    Mac,
    Linux,
    Unknown,
}

fn os_kind(os: &str) -> OsKind {
    let os = os.to_ascii_lowercase();
    if os.contains("win") {
        OsKind::Windows
    } else if os.contains("mac") || os.contains("darwin") {
        OsKind::Mac
    } else if os.contains("linux") {
        OsKind::Linux
    } else {
        OsKind::Unknown
    }
}

/// Plain platform glyphs (not vendor logos): four panes, a laptop, a terminal, a monitor.
fn paint_os(ui: &Ui, rect: Rect, kind: OsKind, color: Color32) {
    let p = ui.painter();
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let st = Stroke::new(1.4 * s, color);
    match kind {
        OsKind::Windows => {
            for (x, y) in [(1.5, 2.0), (8.5, 2.0), (1.5, 8.5), (8.5, 8.5)] {
                p.rect_filled(Rect::from_min_size(at(x, y), vec2(6.0, 5.5) * s), 0.6 * s, color);
            }
        }
        OsKind::Mac => {
            p.rect_stroke(Rect::from_min_max(at(3.0, 3.0), at(13.0, 10.5)), 1.2 * s, st, egui::StrokeKind::Middle);
            p.add(Shape::convex_polygon(vec![at(1.0, 12.0), at(15.0, 12.0), at(14.0, 13.6), at(2.0, 13.6)], color, Stroke::NONE));
        }
        OsKind::Linux => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 2.5), at(14.5, 13.5)), 1.5 * s, st, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![at(4.5, 6.0), at(6.8, 8.0), at(4.5, 10.0)], st));
            p.line_segment([at(8.0, 10.2), at(11.5, 10.2)], st);
        }
        OsKind::Unknown => paint_icon(ui, rect, Icon::Monitor, color),
    }
}

/// Single line of text cut with an ellipsis at `max_w`.
fn one_line(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_w, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// Underlined text tabs (RustDesk's peer tabs). Returns true when the selection changed.
fn text_tabs<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let pal = Palette::of(ui);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (v, text) in options {
            let selected = *value == *v;
            let font = if selected { semi(14.5) } else { FontId::proportional(14.5) };
            let galley = ui.painter().layout_no_wrap(text.to_string(), font, pal.fg);
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x, 30.0), Sense::click());
            let color = if selected { pal.accent } else if resp.hovered() { pal.fg } else { pal.muted };
            ui.painter().galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0 - 1.0), galley, color);
            if selected {
                ui.painter().rect_filled(Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.5), rect.right_bottom()), 1.0, pal.accent);
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !selected {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

/// Small icon toggle (grid / list view switch).
fn view_toggle(ui: &mut Ui, icon: Icon, on: bool, tip: &str) -> Response {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    if on || resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, if on { pal.accent_soft } else { pal.line });
    }
    paint_icon(ui, Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), icon, if on { pal.accent } else { pal.muted });
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// What the user picked on a device card or row.
enum PeerAction {
    Connect(String, String),
    Star(String),
    CopyId(String),
    Forget(String),
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
    /// Set while waiting for the browser SSO sign-in; storing true cancels the wait.
    sso: Option<Arc<AtomicBool>>,
}

/// Device list tabs on the home view (like RustDesk's Recent / Favorites / Address book).
#[derive(Clone, Copy, PartialEq)]
enum HomeTab {
    Recent,
    Favorites,
    Book,
    /// Agents answering a broadcast on the local network.
    Lan,
}

/// Agent self-update, driven from Settings. Nothing installs without the user.
#[derive(Default)]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    Downloaded(PathBuf),
    Failed(String),
}

/// What a correct unlock PIN lets through.
enum PinAction {
    OpenSettings,
    SignOut,
    /// Replace the PIN with this argon2 hash.
    Change(String),
    Remove,
}

#[derive(Default)]
struct PinState {
    prompt: Option<PinAction>,
    input: String,
    error: String,
    fails: u32,
    locked_until: Option<Instant>,
    new_pin: String,
    new_pin2: String,
}

/// One row of the device list.
struct DeviceRow {
    id: String,
    title: String,
    detail: String,
    /// Known only for devices in the address book.
    online: Option<bool>,
    tags: Vec<String>,
    /// "windows x86_64" etc., empty when unknown.
    os: String,
    host: String,
}

/// Results of network calls made on background threads.
enum Job {
    Login { server: String, insecure: bool, result: Result<account::Login, String> },
    Book(Result<Vec<BookEntry>, (u16, String)>),
    Lan(Vec<LanPeer>),
    UpdateCheck(Result<Option<Release>, String>),
    UpdateDownload(Result<PathBuf, String>),
}

/// Things outside the window that may ask it to show itself (tray, second launch).
#[derive(Clone, Default)]
pub struct ShowSignal(pub Arc<AtomicBool>);

/// Device IDs from `aegiron://` links (command line or a second launch), waiting to be opened.
#[derive(Clone, Default)]
pub struct OpenRequests(pub Arc<Mutex<VecDeque<String>>>);

#[derive(Clone)]
pub struct Options {
    pub config_path: PathBuf,
    pub show: ShowSignal,
    pub open: OpenRequests,
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
    lan: Vec<LanPeer>,
    lan_busy: bool,
    update: UpdateState,
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
    open: OpenRequests,
    /// Deep link waiting for the user to confirm.
    link_prompt: Option<String>,
    /// Connection to open once sign-in succeeds.
    after_login: Option<(String, String)>,
    local: LocalState,
    /// None in demo mode: never touch the user's real file.
    local_path: Option<PathBuf>,
    local_dirty: Option<Instant>,
    tab: HomeTab,
    search: String,
    pin: PinState,
    /// Initial size of each open viewer window (a changing builder would resize it).
    viewer_init: HashMap<egui::ViewportId, [f32; 2]>,
    viewer_size_seen: Arc<Mutex<Option<[f32; 2]>>>,
    /// Demo screenshots of the lower part of a view.
    demo_scroll: Option<f32>,
    /// The settings tab stays in the tab strip until closed.
    settings_tab: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options, agent: Option<Arc<Agent>>) -> Self {
        style(&cc.egui_ctx);
        let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel();
        let local_path = opts.start_runtime.then(|| LocalState::path_for(&opts.config_path));
        let local = local_path.as_deref().map(LocalState::load).unwrap_or_default();
        let tab = if local.recent.is_empty() { HomeTab::Book } else { HomeTab::Recent };
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
            lan: Vec::new(),
            lan_busy: false,
            update: UpdateState::default(),
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
            open: opts.open,
            link_prompt: None,
            after_login: None,
            local,
            local_path,
            local_dirty: None,
            tab,
            search: String::new(),
            pin: PinState::default(),
            viewer_init: HashMap::new(),
            viewer_size_seen: Arc::default(),
            demo_scroll: None,
            settings_tab: false,
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
        // home | home-fav | home-book: the same data, another device-list tab.
        let (view, tab) = match view {
            "home-fav" => ("home", HomeTab::Favorites),
            "home-book" => ("home", HomeTab::Book),
            "home-lan" => {
                let peer = |id: &str, host: &str, ip: [u8; 4], os: &str| LanPeer { id: id.into(), hostname: host.into(), os: os.into(), addr: ip.into() };
                self.lan = vec![
                    peer("512340987", "ACC-PC01", [192, 168, 1, 21], "windows x86_64"),
                    peer("734120556", "meeting-3f", [192, 168, 1, 40], "macos aarch64"),
                ];
                ("home", HomeTab::Lan)
            }
            "settings-update" => {
                settings_ui::demo_page(settings_ui::Page::About);
                self.update = UpdateState::Available(Release::demo());
                ("settings", HomeTab::Recent)
            }
            "settings-pin" => {
                settings_ui::demo_page(settings_ui::Page::Security);
                self.demo_scroll = Some(760.0); // the PIN card, at the bottom
                ("settings", HomeTab::Recent)
            }
            "settings-security" | "settings-network" | "settings-account" => {
                settings_ui::demo_page(match view {
                    "settings-security" => settings_ui::Page::Security,
                    "settings-network" => settings_ui::Page::Network,
                    _ => settings_ui::Page::Account,
                });
                ("settings", HomeTab::Recent)
            }
            v => (v, HomeTab::Recent),
        };
        self.tab = tab;
        *agent.temp_password.lock().unwrap() = "k7mq3xpd".into();
        let mut st = agent.status.lock().unwrap();
        st.server = ServerState::Online;
        if view == "home" || view == "approval" || view == "pin" || view == "link" {
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
        let now = local::unix_now();
        self.local.recent = vec![
            local::Recent { id: "512340987".into(), name: "บัญชี-PC01".into(), last: now - 300, count: 12 },
            local::Recent { id: "604118275".into(), name: String::new(), last: now - 3 * 3600, count: 1 },
            local::Recent { id: "734120556".into(), name: "เครื่องประชุม ชั้น 3".into(), last: now - 2 * 86400, count: 3 },
        ];
        self.local.favorites = vec!["512340987".into(), "281907433".into()];
        if view == "settings" || view == "pin" {
            agent.config.lock().unwrap().unlock_pin = crate::hash_password("1234").ok();
        }
        if view == "pin" {
            self.pin.prompt = Some(PinAction::OpenSettings);
            self.pin.input = "12".into();
        }
        if view == "link" {
            self.open.0.lock().unwrap().push_back("734120556".into());
        }
        if view == "home" || view == "pin" || view == "link" {
            agent.config.lock().unwrap().account =
                Some(Account { email: "it@kgarden.co.th".into(), name: "ฝ่ายไอที".into(), token: String::new() });
            let dev = |h: &str, online: bool| Some(account::BookDevice { hostname: h.into(), alias: String::new(), os: "windows x86_64".into(), online });
            let tags = |t: &[&str]| t.iter().map(|s| s.to_string()).collect();
            self.book = vec![
                BookEntry { device_id: "512340987".into(), alias: "บัญชี-PC01".into(), favorite: true, tags: tags(&["บัญชี"]), device: dev("ACC-PC01", true) },
                BookEntry { device_id: "734120556".into(), alias: "เครื่องประชุม ชั้น 3".into(), favorite: false, tags: tags(&["ประชุม"]), device: dev("MEET-3F", true) },
                BookEntry { device_id: "281907433".into(), alias: String::new(), favorite: false, tags: tags(&["server"]), device: dev("SRV-FILES", false) },
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

    /// Server to sign in to: this device's, or the one typed on the welcome screen.
    fn login_target(&self) -> (String, bool) {
        match &self.agent {
            Some(a) => {
                let c = a.config.lock().unwrap();
                (c.server_url.clone(), c.allow_insecure_transport)
            }
            None => (self.setup.server.trim().trim_end_matches('/').to_string(), self.setup.insecure),
        }
    }

    /// Organisation SSO: the browser signs in, the app polls for the resulting session.
    fn start_sso(&mut self, ctx: &egui::Context) {
        let (server, insecure) = self.login_target();
        let cancel = Arc::new(AtomicBool::new(false));
        self.login.sso = Some(cancel.clone());
        self.login.busy = true;
        self.login.error.clear();
        let device = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        self.spawn_job(ctx, move || {
            let result = (|| {
                let (code, url) = account::oidc_start(&server, insecure, &device)?;
                open_url(&url).map_err(|e| anyhow::anyhow!("เปิดเบราว์เซอร์ไม่ได้: {e}"))?;
                let deadline = Instant::now() + Duration::from_secs(300);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_secs(2));
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    if let Some(sess) = account::oidc_poll(&server, insecure, &code)? {
                        return Ok(Some(account::Login::Ok(sess)));
                    }
                }
                anyhow::bail!("หมดเวลารอการเข้าสู่ระบบในเบราว์เซอร์")
            })();
            match result {
                // Cancelled by the user: nothing to report.
                Ok(None) => Job::Login { server, insecure, result: Err(String::new()) },
                Ok(Some(login)) => Job::Login { server, insecure, result: Ok(login) },
                Err(e) => Job::Login { server, insecure, result: Err(format!("{e:#}")) },
            }
        });
    }

    fn cancel_sso(&mut self) {
        if let Some(c) = self.login.sso.take() {
            c.store(true, Ordering::Relaxed);
            self.login.busy = false;
        }
    }

    fn discover_lan(&mut self, ctx: &egui::Context) {
        if self.lan_busy {
            return;
        }
        self.lan_busy = true;
        self.spawn_job(ctx, || Job::Lan(lan::discover(Duration::from_millis(1500))));
    }

    fn check_update(&mut self, ctx: &egui::Context, server: String, insecure: bool) {
        self.update = UpdateState::Checking;
        self.spawn_job(ctx, move || Job::UpdateCheck(updater::check(&server, insecure).map_err(|e| format!("{e:#}"))));
    }

    fn start_login(&mut self, ctx: &egui::Context) {
        let (server, insecure) = self.login_target();
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
            self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
            self.login_modal = true;
            self.after_login = Some((id.to_string(), name.to_string()));
            return;
        };
        let insecure = agent.config.lock().unwrap().allow_insecure_transport;
        let direct = agent.config.lock().unwrap().direct_enabled;
        let target = crate::viewer::Target { server, token: acc.token, insecure, device_id: id.to_string(), name: name.to_string(), direct, only: None };
        // Frames arrive on the network thread: wake that connection's own window.
        let viewport = egui::ViewportId::from_hash_of(("remote", id, Instant::now()));
        let rctx = ctx.clone();
        let viewer = Viewer::connect(target, Arc::new(move || rctx.request_repaint_of(viewport)));
        self.viewers.push(ViewerWindow::new(viewer, id.to_string(), viewport));
        // A bare formatted ID is not a name worth remembering.
        let name = if name == format_id(id) { "" } else { name };
        self.local.record(id, name, local::unix_now());
        self.mark_local();
    }

    fn mark_local(&mut self) {
        if self.local_dirty.is_none() {
            self.local_dirty = Some(Instant::now());
        }
    }

    /// Writes local.toml at most about once a second (window drags change it every frame).
    fn flush_local(&mut self, force: bool) {
        let Some(at) = self.local_dirty else { return };
        if !force && at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.local_dirty = None;
        if let Some(path) = &self.local_path
            && let Err(e) = self.local.save(path)
        {
            tracing::warn!("cannot save {}: {e:#}", path.display());
        }
    }

    /// Window geometry to remember: skipped while minimized, maximized or full screen.
    fn track_geometry(&mut self, ctx: &egui::Context) {
        let geom = ctx.input(|i| {
            let v = i.viewport();
            if v.minimized == Some(true) || v.maximized == Some(true) || v.fullscreen == Some(true) {
                return None;
            }
            let inner = v.inner_rect?;
            Some(WindowGeom { pos: v.outer_rect.map(|r| [r.min.x, r.min.y]), size: [inner.width(), inner.height()] })
        });
        if let Some(g) = geom
            && self.local.main_window != Some(g)
        {
            self.local.main_window = Some(g);
            self.mark_local();
        }
        let seen = self.viewer_size_seen.lock().unwrap().take();
        if let Some(s) = seen
            && self.local.viewer_size != Some(s)
        {
            self.local.viewer_size = Some(s);
            self.mark_local();
        }
    }

    fn has_pin(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| a.config.lock().unwrap().unlock_pin.is_some())
    }

    /// Runs `action` now, or after the unlock PIN when one is set.
    fn guarded(&mut self, ctx: &egui::Context, action: PinAction) {
        if self.has_pin() {
            self.pin.input.clear();
            self.pin.error.clear();
            self.pin.prompt = Some(action);
        } else {
            self.run_pin_action(ctx, action);
        }
    }

    fn run_pin_action(&mut self, ctx: &egui::Context, action: PinAction) {
        match action {
            PinAction::OpenSettings => self.view = View::Settings,
            PinAction::SignOut => self.sign_out(ctx),
            PinAction::Change(hash) => {
                self.pin.new_pin.clear();
                self.pin.new_pin2.clear();
                self.save_config(|c| c.unlock_pin = Some(hash));
            }
            PinAction::Remove => self.save_config(|c| c.unlock_pin = None),
        }
    }

    fn pin_locked_for(&self) -> Option<Duration> {
        self.pin.locked_until.and_then(|t| t.checked_duration_since(Instant::now())).filter(|d| !d.is_zero())
    }

    fn submit_pin(&mut self, ctx: &egui::Context) {
        if self.pin_locked_for().is_some() {
            return;
        }
        let hash = self.agent.as_ref().and_then(|a| a.config.lock().unwrap().unlock_pin.clone());
        let ok = hash.is_none_or(|h| verify_pin(&h, &self.pin.input));
        self.pin.input.clear();
        if ok {
            self.pin.fails = 0;
            self.pin.locked_until = None;
            self.pin.error.clear();
            if let Some(action) = self.pin.prompt.take() {
                self.run_pin_action(ctx, action);
            }
            return;
        }
        // After the first lockout every further wrong PIN locks again, until a correct one.
        self.pin.fails += 1;
        if self.pin.fails >= PIN_MAX_TRIES {
            self.pin.locked_until = Some(Instant::now() + PIN_LOCKOUT);
            self.pin.error = "ใส่ PIN ผิดหลายครั้งเกินไป".into();
        } else {
            self.pin.error = format!("PIN ไม่ถูกต้อง (ลองได้อีก {} ครั้ง)", PIN_MAX_TRIES - self.pin.fails);
        }
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
                self.login.sso = None;
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
                        if let Some((id, name)) = self.after_login.take() {
                            self.open_remote(ctx, &id, &name);
                        }
                    }
                }
            }
            Job::Lan(peers) => {
                self.lan_busy = false;
                self.lan = peers;
            }
            Job::UpdateCheck(r) => {
                self.update = match r {
                    Ok(Some(rel)) => UpdateState::Available(rel),
                    Ok(None) => UpdateState::UpToDate,
                    Err(e) => UpdateState::Failed(e),
                }
            }
            Job::UpdateDownload(r) => {
                self.update = match r {
                    Ok(path) => UpdateState::Downloaded(path),
                    Err(e) => UpdateState::Failed(e),
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
        let live: Vec<_> = self.viewers.iter().map(|w| w.viewport).collect();
        self.viewer_init.retain(|v, _| live.contains(v));
        for w in &self.viewers {
            let title = format!("{} — Aegiron", w.viewer.state.lock().unwrap().device_name);
            let size = *self.viewer_init.entry(w.viewport).or_insert(self.local.viewer_size.unwrap_or(VIEWER_SIZE));
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(size)
                .with_min_inner_size([640.0, 420.0])
                .with_icon(Arc::new(egui::IconData { rgba: icon_rgba(64), width: 64, height: 64 }));
            // AEGIRON_UI_VIEWER_POS="x,y" places the window (automated screenshots).
            let builder = match std::env::var("AEGIRON_UI_VIEWER_POS").ok().and_then(|p| {
                let (x, y) = p.split_once(',')?;
                Some([x.parse::<f32>().ok()?, y.parse::<f32>().ok()?])
            }) {
                Some(pos) => builder.with_position(pos),
                None => builder,
            };
            let (win, seen) = (w.clone(), self.viewer_size_seen.clone());
            ctx.show_viewport_deferred(w.viewport, builder, move |ui, _| {
                let size = ui.input(|i| {
                    let v = i.viewport();
                    // The focused window only: several open viewers must not fight over one value.
                    let normal = v.focused == Some(true)
                        && v.minimized != Some(true)
                        && v.maximized != Some(true)
                        && v.fullscreen != Some(true);
                    v.inner_rect.filter(|_| normal).map(|r| [r.width(), r.height()])
                });
                if size.is_some() {
                    *seen.lock().unwrap() = size;
                }
                win.show(ui)
            });
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
        if let Ok(id) = std::env::var("AEGIRON_UI_CONNECT")
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
        let link = self.open.0.lock().unwrap().pop_back();
        if let Some(id) = link {
            self.open.0.lock().unwrap().clear(); // a burst of links: only the newest counts
            self.link_prompt = Some(id);
            Self::bring_to_front(ctx);
        }
        if self.local_path.is_some() {
            self.track_geometry(ctx);
            self.flush_local(false);
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
            && std::env::var_os("AEGIRON_UI_CONNECT").is_none()
        {
            // AEGIRON_UI_SCREENSHOT_AFTER=<seconds> waits (e.g. for the server connection) before capturing.
            let wait = std::env::var("AEGIRON_UI_SCREENSHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_local(true);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let pal = Palette::of(ui);
        match self.agent.clone() {
            None => {
                egui::CentralPanel::default().frame(Frame::new().fill(pal.side)).show(ui, |ui| self.welcome_view(ui));
            }
            Some(agent) => {
                egui::Panel::top("tabs")
                    .exact_size(TABBAR_HEIGHT)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(Frame::new().fill(pal.side).inner_margin(Margin { left: 8, right: 10, top: 0, bottom: 0 }))
                    .show(ui, |ui| self.tab_bar(ui));
                match self.view {
                    // Settings take the whole window (own navigation and scrolling).
                    View::Settings => {
                        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg)).show(ui, |ui| self.settings(ui, &agent));
                    }
                    View::Home => {
                        egui::Panel::left("this-device")
                            .exact_size(SIDEBAR_WIDTH)
                            .resizable(false)
                            .show_separator_line(true)
                            .frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 24, right: 20, top: 22, bottom: 16 }))
                            .show(ui, |ui| self.sidebar(ui, &agent));
                        egui::CentralPanel::default().frame(Frame::new().fill(pal.side)).show(ui, |ui| {
                            egui::Panel::bottom("status")
                                .exact_size(32.0)
                                .resizable(false)
                                .show_separator_line(false)
                                .frame(Frame::new().inner_margin(Margin::symmetric(24, 0)))
                                .show(ui, |ui| self.status_bar(ui, &agent));
                            egui::CentralPanel::default()
                                .frame(Frame::new().inner_margin(Margin { left: 24, right: 24, top: 20, bottom: 0 }))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                                        // Room for card shadows at the edges.
                                        Frame::new().inner_margin(Margin { left: 2, right: 6, top: 2, bottom: 12 }).show(ui, |ui| self.home(ui, &agent));
                                    });
                                });
                        });
                    }
                }
            }
        }
        if self.login_modal {
            self.login_modal(ui);
        }
        if self.agent.is_some() {
            if self.pin.prompt.is_some() {
                self.pin_modal(ui);
            } else if self.link_prompt.is_some() && !self.login_modal {
                self.link_modal(ui);
            }
        }
        self.show_viewers(ui.ctx());
        self.connection_manager(ui);
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

    /// RustDesk-style tab strip under the title bar: the home tab, a closable settings tab,
    /// and the settings gear on the right.
    fn tab_bar(&mut self, ui: &mut Ui) {
        let pal = Palette::of(ui);
        if self.view == View::Settings {
            self.settings_tab = true;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let (home, _) = tab_chip(ui, "Aegiron", TabIcon::Logo, self.view == View::Home, false);
            if home.clicked() {
                self.view = View::Home;
            }
            if self.settings_tab {
                let (tab, close) = tab_chip(ui, "ตั้งค่า", TabIcon::Gear, self.view == View::Settings, true);
                if close {
                    self.settings_tab = false;
                    self.view = View::Home;
                } else if tab.clicked() && self.view != View::Settings {
                    let ctx = ui.ctx().clone();
                    self.guarded(&ctx, PinAction::OpenSettings);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Gear, "ตั้งค่า").clicked() && self.view != View::Settings {
                    let ctx = ui.ctx().clone();
                    self.guarded(&ctx, PinAction::OpenSettings);
                }
                let _ = pal;
            });
        });
    }

    /// Left pane: this machine's ID and password, like RustDesk's "Your Desktop".
    fn sidebar(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let (id, verification, approve) = {
            let c = agent.config.lock().unwrap();
            (c.id.clone(), c.verification, c.approve_mode)
        };
        ui.label(RichText::new("เครื่องของคุณ").font(semi(19.0)).color(pal.fg));
        ui.label(RichText::new("ผู้อื่นควบคุมเครื่องนี้ได้ด้วย ID และรหัสผ่านด้านล่าง").size(12.5).color(pal.muted));
        ui.add_space(18.0);

        accent_block(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ID").size(13.0).color(pal.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if small_icon_button(ui, Icon::Copy, "คัดลอก ID").clicked() {
                        ui.ctx().copy_text(id.clone());
                        self.flash("คัดลอก ID แล้ว");
                    }
                });
            });
            ui.label(RichText::new(format_id(&id)).font(semi(25.0)).color(pal.fg));
        });
        ui.add_space(16.0);

        accent_block(ui, |ui| {
            if approve == ApproveMode::Click {
                ui.label(RichText::new("การเข้าถึง").size(13.0).color(pal.muted));
                ui.label(RichText::new("ไม่ใช้รหัสผ่าน ต้องกดอนุญาตที่เครื่องนี้").color(pal.fg));
            } else if verification == Verification::Permanent {
                ui.label(RichText::new("รหัสผ่าน").size(13.0).color(pal.muted));
                ui.label(RichText::new("ใช้รหัสผ่านถาวรที่ตั้งไว้").color(pal.fg));
            } else {
                let pw = agent.temp_password.lock().unwrap().clone();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("รหัสผ่านใช้ครั้งเดียว").size(13.0).color(pal.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if small_icon_button(ui, Icon::Copy, "คัดลอกรหัสผ่าน").clicked() {
                            ui.ctx().copy_text(pw.clone());
                            self.flash("คัดลอกรหัสผ่านแล้ว");
                        }
                        if small_icon_button(ui, Icon::Refresh, "สร้างรหัสใหม่ (รหัสเดิมใช้ไม่ได้ทันที)").clicked() {
                            agent.rotate_temp_password();
                            self.flash("สร้างรหัสผ่านใหม่แล้ว");
                        }
                        let (icon, tip) = if self.show_password { (Icon::EyeOff, "ซ่อน") } else { (Icon::Eye, "แสดง") };
                        if small_icon_button(ui, icon, tip).clicked() {
                            self.show_password = !self.show_password;
                        }
                    });
                });
                let shown = if self.show_password { pw.clone() } else { "•".repeat(pw.chars().count()) };
                ui.label(RichText::new(shown).font(semi(22.0)).color(pal.fg));
            }
        });
        if approve == ApproveMode::Password && verification != Verification::Permanent {
            ui.add_space(4.0);
            ui.label(RichText::new("เปลี่ยนใหม่อัตโนมัติหลังมีผู้เชื่อมต่อสำเร็จ").size(12.0).color(pal.muted));
        }

        if approve != ApproveMode::Password {
            ui.add_space(14.0);
            Frame::new().fill(pal.accent_soft).corner_radius(8).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    paint_icon(ui, r, Icon::Shield, pal.accent);
                    ui.label(RichText::new("ทุกการเชื่อมต่อต้องกดอนุญาตที่เครื่องนี้").size(12.5).color(pal.accent));
                });
            });
        }

        let account = agent.config.lock().unwrap().account.clone();
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| match &account {
            Some(acc) => {
                ui.horizontal(|ui| {
                    avatar(ui, &initials(&acc.name, &acc.email), 34.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let name = if acc.name.is_empty() { acc.email.clone() } else { acc.name.clone() };
                        ui.add(egui::Label::new(RichText::new(name).font(semi(13.5)).color(pal.fg)).truncate());
                        ui.add(egui::Label::new(RichText::new(&acc.email).size(12.0).color(pal.muted)).truncate());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new(RichText::new("ออกจากระบบ").size(12.5).color(pal.muted)).frame(false)).clicked() {
                            let ctx = ui.ctx().clone();
                            self.guarded(&ctx, PinAction::SignOut);
                        }
                    });
                });
            }
            None => {
                let b = Button::new(RichText::new("เข้าสู่ระบบ").font(semi(14.0)).color(pal.accent))
                    .fill(pal.bg)
                    .stroke(Stroke::new(1.0, pal.line))
                    .corner_radius(8);
                // A fixed rect keeps the label centred without a nested layout eating the column.
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                if ui.put(rect, b).clicked() {
                    self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
                    self.login_modal = true;
                }
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let server = host_of(&agent.config.lock().unwrap().server_url);
        let incoming = agent.status.lock().unwrap().sessions.len();
        ui.horizontal_centered(|ui| {
            let (color, text, detail) = match &agent.status.lock().unwrap().server {
                ServerState::Online => (pal.ok, "พร้อมใช้งาน".to_string(), format!("{server} · เข้ารหัสต้นทาง-ปลายทาง")),
                ServerState::Connecting => (pal.warn, "กำลังเชื่อมต่อ…".to_string(), server.clone()),
                ServerState::Error(e) => (pal.danger, "ไม่ได้เชื่อมต่อ".to_string(), e.chars().take(80).collect()),
            };
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, color);
            ui.label(RichText::new(text).size(12.5).color(pal.fg));
            ui.add(egui::Label::new(RichText::new(detail).size(12.5).color(pal.muted)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(12.0).color(pal.muted));
                if incoming > 0 {
                    ui.label(RichText::new(format!("มีผู้ควบคุมเครื่องนี้ {incoming} คน")).size(12.5).color(pal.accent));
                }
            });
        });
    }

    /// Right pane: the "control a remote desktop" card, then the device tabs.
    fn home(&mut self, ui: &mut Ui, agent: &Agent) {
        permissions_ui::missing_permissions_banner(ui);
        let pal = Palette::of(ui);
        let digits: String = self.remote_id.chars().filter(|c| c.is_ascii_digit()).collect();
        let mut go = false;
        card_frame(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("ควบคุมเครื่องอื่น").font(semi(18.0)).color(pal.fg));
            ui.add_space(6.0);
            let edit = TextEdit::singleline(&mut self.remote_id)
                .hint_text(RichText::new("ใส่ ID ของเครื่องปลายทาง").size(17.0))
                .font(FontId::proportional(21.0))
                .align(Align2::LEFT_CENTER)
                .min_size(vec2(0.0, 46.0))
                .margin(Margin::symmetric(14, 6))
                .desired_width(f32::INFINITY);
            let r = ui.add(edit);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = true;
            }
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 40.0), Layout::right_to_left(Align::Center), |ui| {
                if primary(ui, "เชื่อมต่อ", digits.len() == 9).clicked() {
                    go = true;
                }
            });
        });
        if go && digits.len() == 9 {
            let ctx = ui.ctx().clone();
            let name = self.device_title(&digits);
            self.open_remote(&ctx, &digits, &name);
        }
        ui.add_space(18.0);
        self.address_book(ui, agent);
        // Without separate windows there is no connection-manager window: list sessions here.
        if ui.ctx().embed_viewports() {
            ui.add_space(24.0);
            self.sessions_list(ui, agent);
        }
    }

    fn book_entry(&self, id: &str) -> Option<&BookEntry> {
        self.book.iter().find(|e| e.device_id == id)
    }

    /// Best known name: address book, then the name remembered from the last connection.
    fn device_title(&self, id: &str) -> String {
        self.book_entry(id).map(|e| e.title()).or_else(|| self.local.name_of(id).map(str::to_string)).unwrap_or_else(|| format_id(id))
    }

    fn known_row(&self, id: &str, when: Option<u64>) -> DeviceRow {
        let entry = self.book_entry(id);
        let online = entry.map(|e| e.online());
        let title = self.device_title(id);
        let mut parts = Vec::new();
        // An unnamed device already shows its ID as the title.
        if title != format_id(id) {
            parts.push(format_id(id));
        }
        if let Some(on) = online {
            parts.push((if on { "ออนไลน์" } else { "ออฟไลน์" }).to_string());
        }
        if let Some(t) = when {
            parts.push(ago(t));
        }
        if parts.is_empty() {
            parts.push("ไม่อยู่ในสมุดที่อยู่".into());
        }
        let dev = entry.and_then(|e| e.device.as_ref());
        DeviceRow {
            id: id.to_string(),
            title,
            detail: parts.join(" · "),
            online,
            tags: entry.map(|e| e.tags.clone()).unwrap_or_default(),
            os: dev.map(|d| d.os.clone()).unwrap_or_default(),
            host: dev.map(|d| d.hostname.clone()).unwrap_or_default(),
        }
    }

    /// Recent / favorites / address book, with search and a favorite star on every row.
    fn address_book(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let my_id = agent.config.lock().unwrap().id.clone();
        let signed_in = self.account().is_some();
        ui.horizontal(|ui| {
            let mut tab = self.tab;
            text_tabs(ui, &mut tab, &[(HomeTab::Recent, "ล่าสุด"), (HomeTab::Favorites, "รายการโปรด"), (HomeTab::Lan, "ในเครือข่าย"), (HomeTab::Book, "สมุดที่อยู่")]);
            if tab == HomeTab::Lan && self.tab != HomeTab::Lan {
                let ctx = ui.ctx().clone();
                self.discover_lan(&ctx);
            }
            self.tab = tab;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if view_toggle(ui, Icon::List, self.local.peer_list, "มุมมองรายการ").clicked() && !self.local.peer_list {
                    self.local.peer_list = true;
                    self.mark_local();
                }
                if view_toggle(ui, Icon::Grid, !self.local.peer_list, "มุมมองการ์ด").clicked() && self.local.peer_list {
                    self.local.peer_list = false;
                    self.mark_local();
                }
                if self.tab == HomeTab::Book && signed_in && small_icon_button(ui, Icon::Refresh, "รีเฟรช").clicked() {
                    let ctx = ui.ctx().clone();
                    self.refresh_book(&ctx);
                }
                if self.tab == HomeTab::Lan && !self.lan_busy && small_icon_button(ui, Icon::Refresh, "ค้นหาอีกครั้ง").clicked() {
                    let ctx = ui.ctx().clone();
                    self.discover_lan(&ctx);
                }
                ui.add_space(4.0);
                let edit = TextEdit::singleline(&mut self.search)
                    .hint_text("ค้นหา")
                    .margin(Margin { left: 28, right: 8, top: 5, bottom: 5 })
                    .desired_width(ui.available_width().clamp(90.0, 170.0));
                let r = ui.add(edit);
                let icon = Rect::from_center_size(r.rect.left_center() + vec2(15.0, 0.0), vec2(13.0, 13.0));
                paint_icon(ui, icon, Icon::Search, pal.muted);
            });
        });
        ui.add_space(10.0);

        let rows: Vec<DeviceRow> = match self.tab {
            HomeTab::Recent => self.local.recent.iter().filter(|r| r.id != my_id).map(|r| self.known_row(&r.id, Some(r.last))).collect(),
            HomeTab::Favorites => self.local.favorites.iter().filter(|f| **f != my_id).map(|f| self.known_row(f, None)).collect(),
            HomeTab::Lan => {
                if self.lan_busy {
                    ui.label(RichText::new("กำลังค้นหาเครื่องในเครือข่าย…").size(13.0).color(pal.muted));
                    return;
                }
                self.lan
                    .iter()
                    .filter(|p| p.id != my_id)
                    .map(|p| DeviceRow {
                        id: p.id.clone(),
                        title: if p.hostname.is_empty() { format_id(&p.id) } else { p.hostname.clone() },
                        detail: format!("{} · {} · {}", format_id(&p.id), p.addr, p.os),
                        online: Some(true),
                        tags: Vec::new(),
                        os: p.os.clone(),
                        host: p.hostname.clone(),
                    })
                    .collect()
            }
            HomeTab::Book => {
                if !signed_in {
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
                self.book
                    .iter()
                    .filter(|e| e.device_id != my_id)
                    .map(|e| {
                        let status = if e.online() { "ออนไลน์" } else { "ออฟไลน์" };
                        DeviceRow {
                            id: e.device_id.clone(),
                            title: e.title(),
                            detail: format!("{} · {status}", format_id(&e.device_id)),
                            online: Some(e.online()),
                            tags: e.tags.clone(),
                            os: e.device.as_ref().map(|d| d.os.clone()).unwrap_or_default(),
                            host: e.device.as_ref().map(|d| d.hostname.clone()).unwrap_or_default(),
                        }
                    })
                    .collect()
            }
        };
        if rows.is_empty() {
            let msg = match self.tab {
                HomeTab::Recent => "ยังไม่มีประวัติการเชื่อมต่อ — เครื่องที่คุณเคยควบคุมจะแสดงที่นี่",
                HomeTab::Favorites => "ยังไม่มีรายการโปรด — เพิ่มได้จากเมนู ⋮ ของแต่ละเครื่อง",
                HomeTab::Book => "ยังไม่มีเครื่องในสมุดที่อยู่ — เพิ่มได้จากคอนโซล Aegiron",
                HomeTab::Lan => "ไม่พบเครื่อง Aegiron อื่นในเครือข่ายเดียวกัน",
            };
            ui.label(RichText::new(msg).size(13.0).color(pal.muted));
            return;
        }
        let q = self.search.trim().to_lowercase();
        let q_id: String = q.chars().filter(|c| *c != ' ').collect();
        let matches = |r: &DeviceRow| {
            q.is_empty()
                || r.title.to_lowercase().contains(&q)
                || (!q_id.is_empty() && q_id.bytes().all(|b| b.is_ascii_digit()) && r.id.contains(&q_id))
                || r.tags.iter().any(|t| t.to_lowercase().contains(&q))
        };
        let rows: Vec<DeviceRow> = rows.into_iter().filter(|r| matches(r)).collect();
        if rows.is_empty() {
            ui.label(RichText::new(format!("ไม่พบเครื่องที่ตรงกับ “{}”", self.search.trim())).size(13.0).color(pal.muted));
            return;
        }

        let tab = self.tab;
        let action = if self.local.peer_list { self.peer_list(ui, &rows, tab) } else { self.peer_grid(ui, &rows, tab) };
        match action {
            Some(PeerAction::Connect(id, name)) => {
                let ctx = ui.ctx().clone();
                self.open_remote(&ctx, &id, &name);
            }
            Some(PeerAction::Star(id)) => {
                self.local.toggle_favorite(&id);
                self.mark_local();
            }
            Some(PeerAction::CopyId(id)) => {
                ui.ctx().copy_text(id);
                self.flash("คัดลอก ID แล้ว");
            }
            Some(PeerAction::Forget(id)) => {
                self.local.recent.retain(|r| r.id != id);
                self.mark_local();
            }
            None => {}
        }
        if self.tab == HomeTab::Recent && self.search.trim().is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.add(Button::new(RichText::new("ล้างประวัติ").size(12.5).color(pal.muted)).frame(false)).clicked() {
                    self.local.recent.clear();
                    self.mark_local();
                }
            });
        }
    }

    /// Menu shared by cards (⋮ and right click) and list rows.
    fn peer_menu(&self, ui: &mut Ui, r: &DeviceRow, tab: HomeTab, action: &mut Option<PeerAction>) {
        ui.set_min_width(190.0);
        let offline = r.online == Some(false);
        if ui.add_enabled(!offline, Button::new("เชื่อมต่อ")).clicked() {
            *action = Some(PeerAction::Connect(r.id.clone(), r.title.clone()));
        }
        let fav = if self.local.is_favorite(&r.id) { "เอาออกจากรายการโปรด" } else { "เพิ่มในรายการโปรด" };
        if ui.button(fav).clicked() {
            *action = Some(PeerAction::Star(r.id.clone()));
        }
        if ui.button("คัดลอก ID").clicked() {
            *action = Some(PeerAction::CopyId(r.id.clone()));
        }
        if tab == HomeTab::Recent {
            ui.separator();
            if ui.button(RichText::new("ลบออกจากประวัติ").color(Palette::of(ui).danger)).clicked() {
                *action = Some(PeerAction::Forget(r.id.clone()));
            }
        }
    }

    /// Device cards in a responsive grid (RustDesk's default peer view).
    fn peer_grid(&self, ui: &mut Ui, rows: &[DeviceRow], tab: HomeTab) -> Option<PeerAction> {
        const GAP: f32 = 14.0;
        let avail = ui.available_width();
        let cols = ((avail + GAP) / (CARD_MIN_W + GAP)).floor().max(1.0) as usize;
        let w = ((avail - GAP * (cols - 1) as f32) / cols as f32).min(CARD_MAX_W);
        let mut action = None;
        for chunk in rows.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for r in chunk {
                    self.peer_card(ui, r, w, tab, &mut action);
                }
            });
            ui.add_space(GAP - ui.spacing().item_spacing.y);
        }
        action
    }

    fn peer_card(&self, ui: &mut Ui, r: &DeviceRow, w: f32, tab: HomeTab, action: &mut Option<PeerAction>) {
        let pal = Palette::of(ui);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, CARD_H), Sense::click());
        let head = Rect::from_min_size(rect.min, vec2(w, CARD_HEAD));
        let offline = r.online == Some(false);
        let p = ui.painter();
        p.add(egui::Shadow { offset: [0, 1], blur: if resp.hovered() { 12 } else { 6 }, spread: 0, color: Color32::from_black_alpha(22) }.as_shape(rect, 10));
        p.rect_filled(rect, 10, pal.bg);
        let color = if offline { card_color(&r.id).gamma_multiply(0.55) } else { card_color(&r.id) };
        p.rect_filled(head, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, color);
        if resp.hovered() {
            p.rect_stroke(rect, 10, Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
        }
        // Header: platform glyph and host name in white.
        let lift = if r.host.is_empty() { 0.0 } else { 11.0 };
        let glyph = Rect::from_center_size(head.center() - vec2(0.0, lift), vec2(34.0, 34.0));
        paint_os(ui, glyph, os_kind(&r.os), Color32::WHITE);
        if !r.host.is_empty() {
            let g = one_line(ui, &r.host, FontId::proportional(12.5), Color32::WHITE, w - 24.0);
            ui.painter().galley(pos2(head.center().x - g.size().x / 2.0, head.bottom() - 26.0), g, Color32::WHITE);
        }
        if self.local.is_favorite(&r.id) {
            paint_icon(ui, Rect::from_min_size(head.right_top() + vec2(-24.0, 8.0), vec2(14.0, 14.0)), Icon::StarFilled, Color32::WHITE);
        }
        // Footer: status dot, name and ID, menu.
        let foot = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
        let dot = match r.online {
            Some(true) => pal.ok,
            Some(false) => pal.muted.gamma_multiply(0.6),
            None => pal.line,
        };
        ui.painter().circle_filled(pos2(foot.left() + 15.0, foot.top() + 17.0), 4.0, dot);
        let tw = w - 26.0 - 34.0;
        let title = one_line(ui, &r.title, semi(13.5), pal.fg, tw);
        ui.painter().galley(pos2(foot.left() + 26.0, foot.top() + 8.0), title, pal.fg);
        let id = one_line(ui, &format_id(&r.id), FontId::proportional(12.0), pal.muted, tw);
        ui.painter().galley(pos2(foot.left() + 26.0, foot.top() + 27.0), id, pal.muted);
        let more = Rect::from_center_size(pos2(foot.right() - 18.0, foot.center().y), vec2(26.0, 26.0));
        let more_resp = ui.interact(more, resp.id.with("more"), Sense::click());
        if more_resp.hovered() {
            ui.painter().rect_filled(more, 6, pal.line);
        }
        paint_icon(ui, Rect::from_center_size(more.center(), vec2(15.0, 15.0)), Icon::More, if more_resp.hovered() { pal.fg } else { pal.muted });
        egui::Popup::menu(&more_resp).show(|ui| self.peer_menu(ui, r, tab, action));
        resp.context_menu(|ui| self.peer_menu(ui, r, tab, action));
        let resp = resp.on_hover_text(format!("{}\n{}\nดับเบิลคลิกเพื่อเชื่อมต่อ", r.title, r.detail));
        if resp.double_clicked() && !offline {
            *action = Some(PeerAction::Connect(r.id.clone(), r.title.clone()));
        }
    }

    /// Compact list view: one row per device with a connect button.
    fn peer_list(&self, ui: &mut Ui, rows: &[DeviceRow], tab: HomeTab) -> Option<PeerAction> {
        let pal = Palette::of(ui);
        let mut action = None;
        card_frame(&pal).inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (n, r) in rows.iter().enumerate() {
                if n > 0 {
                    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().rect_filled(sep, 0.0, pal.line);
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(50.0);
                    let offline = r.online == Some(false);
                    let (ic, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                    ui.painter().rect_filled(ic, 7, if offline { card_color(&r.id).gamma_multiply(0.55) } else { card_color(&r.id) });
                    paint_os(ui, ic.shrink(7.0), os_kind(&r.os), Color32::WHITE);
                    if let Some(on) = r.online {
                        ui.painter().circle_filled(ic.right_bottom() - vec2(1.0, 1.0), 4.5, pal.bg);
                        ui.painter().circle_filled(ic.right_bottom() - vec2(1.0, 1.0), 3.5, if on { pal.ok } else { pal.muted });
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let more = small_icon_button(ui, Icon::More, "เพิ่มเติม");
                        egui::Popup::menu(&more).show(|ui| self.peer_menu(ui, r, tab, &mut action));
                        let b = Button::new(RichText::new("เชื่อมต่อ").font(semi(13.5)).color(if offline { pal.muted } else { pal.accent }))
                            .fill(pal.bg)
                            .stroke(Stroke::new(1.0, pal.line))
                            .corner_radius(8)
                            .min_size(vec2(0.0, 32.0));
                        if ui.add_enabled(!offline, b).clicked() {
                            action = Some(PeerAction::Connect(r.id.clone(), r.title.clone()));
                        }
                        // Name and details take the rest of the row, left aligned.
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(6.0);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.spacing_mut().interact_size.y = 0.0; // compact tag chips
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if self.local.is_favorite(&r.id) {
                                        let (s, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                                        paint_icon(ui, s, Icon::StarFilled, pal.warn);
                                    }
                                    ui.add(egui::Label::new(RichText::new(&r.title).font(semi(14.0)).color(pal.fg)).truncate());
                                    for t in r.tags.iter().take(3) {
                                        Frame::new().fill(pal.accent_soft).corner_radius(6).inner_margin(Margin::symmetric(6, 0)).show(ui, |ui| {
                                            ui.label(RichText::new(t).size(11.5).color(pal.accent));
                                        });
                                    }
                                });
                                ui.add(egui::Label::new(RichText::new(&r.detail).size(12.5).color(pal.muted)).truncate());
                            });
                        });
                    });
                });
            }
        });
        action
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
        if !self.login.need_totp {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                if self.login.sso.is_some() {
                    ui.label(RichText::new("ดำเนินการเข้าสู่ระบบต่อในเบราว์เซอร์…").size(13.0).color(pal.muted));
                    if ui.add(Button::new(RichText::new("ยกเลิก").size(13.0).color(pal.accent)).frame(false)).clicked() {
                        self.cancel_sso();
                    }
                } else {
                    let has_server = self.agent.is_some() || !self.setup.server.trim().is_empty();
                    let b = Button::new(RichText::new("เข้าสู่ระบบด้วย SSO ขององค์กร").size(13.5).color(pal.accent)).frame(false);
                    if ui.add_enabled(!self.login.busy && has_server, b).clicked() {
                        let ctx = ui.ctx().clone();
                        self.start_sso(&ctx);
                    }
                }
            });
        }
        submit && ready
    }

    /// First run: sign in with an organisation account (or, under "advanced", a server key).
    fn welcome_view(&mut self, ui: &mut Ui) {
        let pal = Palette::of(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            Self::brand(ui, 48.0);
            ui.label(RichText::new("เข้าสู่ระบบ Aegiron").font(semi(22.0)).color(pal.fg));
            ui.label(RichText::new("ใช้บัญชีเดียวกับคอนโซล Aegiron ขององค์กร").color(pal.muted));
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
                                .hint_text("https://aegiron.example.com")
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
            self.cancel_sso();
            self.login_modal = false;
            self.after_login = None;
        } else if submit {
            let ctx = ui.ctx().clone();
            self.start_login(&ctx);
        }
    }

    fn pin_modal(&mut self, ui: &mut Ui) {
        let pal = Palette::of(ui);
        let what = match self.pin.prompt {
            Some(PinAction::OpenSettings) => "ใส่ PIN เพื่อเปิดหน้าตั้งค่า",
            Some(PinAction::SignOut) => "ใส่ PIN เพื่อออกจากระบบ",
            Some(PinAction::Change(_)) => "ใส่ PIN ปัจจุบัน เพื่อเปลี่ยนเป็น PIN ใหม่",
            Some(PinAction::Remove) => "ใส่ PIN ปัจจุบัน เพื่อลบ PIN",
            None => return,
        };
        let locked = self.pin_locked_for();
        let (mut submit, mut close) = (false, false);
        egui::Modal::new(egui::Id::unique("unlock-pin")).backdrop_color(Color32::from_black_alpha(110)).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                paint_icon(ui, r, Icon::Shield, pal.accent);
                ui.label(RichText::new("ปลดล็อก").font(semi(18.0)).color(pal.fg));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(RichText::new("ยกเลิก").size(13.0).color(pal.muted)).frame(false)).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new(what).size(13.0).color(pal.muted));
            ui.add_space(6.0);
            let r = ui.add_enabled(
                locked.is_none(),
                TextEdit::singleline(&mut self.pin.input)
                    .password(true)
                    .char_limit(8)
                    .hint_text("PIN")
                    .font(mono(20.0))
                    .align(Align2::CENTER_CENTER)
                    .margin(Margin::symmetric(10, 8))
                    .desired_width(f32::INFINITY),
            );
            if locked.is_none() && !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            self.pin.input.retain(|c| c.is_ascii_digit());
            if let Some(left) = locked {
                ui.label(RichText::new(format!("ใส่ PIN ผิดหลายครั้งเกินไป ลองใหม่ได้ใน {} วินาที", left.as_secs() + 1)).size(13.0).color(pal.danger));
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            } else if !self.pin.error.is_empty() {
                ui.label(RichText::new(&self.pin.error).size(13.0).color(pal.danger));
            }
            ui.add_space(6.0);
            let ready = locked.is_none() && self.pin.input.len() >= 4;
            let b = Button::new(RichText::new("ยืนยัน").font(semi(15.0)).color(pal.on_accent))
                .fill(pal.accent)
                .stroke(Stroke::NONE)
                .corner_radius(8)
                .min_size(vec2(ui.available_width(), 42.0));
            submit |= ui.vertical_centered(|ui| ui.add_enabled(ready, b)).inner.clicked();
            submit &= ready;
        });
        if close {
            self.pin.prompt = None;
            self.pin.input.clear();
            self.pin.error.clear();
        } else if submit {
            let ctx = ui.ctx().clone();
            self.submit_pin(&ctx);
        }
    }

    /// Confirms a `aegiron://` link before connecting: a web page must not be able to make this
    /// app announce the user to an arbitrary device without a click.
    fn link_modal(&mut self, ui: &mut Ui) {
        let Some(id) = self.link_prompt.clone() else { return };
        let pal = Palette::of(ui);
        let title = self.device_title(&id);
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("open-link")).backdrop_color(Color32::from_black_alpha(110)).show(ui.ctx(), |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                paint_icon(ui, r, Icon::Monitor, pal.accent);
                ui.label(RichText::new("เปิดการเชื่อมต่อจากลิงก์").size(13.0).color(pal.muted));
                ui.label(RichText::new(&title).font(semi(18.0)).color(pal.fg));
                if title != format_id(&id) {
                    ui.label(RichText::new(format_id(&id)).font(mono(14.0)).color(pal.muted));
                }
                ui.add_space(4.0);
                ui.label(RichText::new("เครื่องปลายทางจะเห็นชื่อและอีเมลของคุณ เชื่อมต่อเฉพาะเครื่องที่คุณตั้งใจเท่านั้น").size(12.5).color(pal.muted));
            });
            ui.add_space(10.0);
            ui.columns(2, |cols| {
                let cancel = Button::new(RichText::new("ยกเลิก").font(semi(14.5)).color(pal.fg))
                    .fill(pal.bg)
                    .stroke(Stroke::new(1.0, pal.line))
                    .corner_radius(8)
                    .min_size(vec2(cols[0].available_width(), 42.0));
                if cols[0].vertical_centered(|ui| ui.add(cancel)).inner.clicked() {
                    answer = Some(false);
                }
                let go = Button::new(RichText::new("เชื่อมต่อ").font(semi(14.5)).color(pal.on_accent))
                    .fill(pal.accent)
                    .stroke(Stroke::NONE)
                    .corner_radius(8)
                    .min_size(vec2(cols[1].available_width(), 42.0));
                if cols[1].vertical_centered(|ui| ui.add(go)).inner.clicked() {
                    answer = Some(true);
                }
            });
        });
        if let Some(yes) = answer {
            self.link_prompt = None;
            if yes {
                let ctx = ui.ctx().clone();
                self.open_remote(&ctx, &id, &title);
            }
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
