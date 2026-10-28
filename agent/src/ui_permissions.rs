//! macOS privacy permissions in the desktop window: a card in Settings and a warning banner on
//! the home view. A child module of `ui` so it shares the palette and widgets.

use std::sync::Mutex;

use super::*;
use crate::permissions::{self, Kind, Permission};

const RECHECK: Duration = Duration::from_secs(2);

struct Cache {
    at: Option<Instant>,
    list: Vec<Permission>,
    /// Asked once from this process: macOS only prompts the first time, so the next click
    /// opens System Settings instead.
    asked: Vec<Kind>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { at: None, list: Vec::new(), asked: Vec::new() });

/// Status of every permission, re-checked at most every couple of seconds.
fn current() -> (Vec<Permission>, Vec<Kind>) {
    let mut c = CACHE.lock().unwrap();
    if c.at.is_none_or(|t| t.elapsed() >= RECHECK) {
        c.list = permissions::status();
        c.at = Some(Instant::now());
    }
    (c.list.clone(), c.asked.clone())
}

/// Asks the first time, opens the System Settings pane afterwards.
fn act(ctx: &egui::Context, kind: Kind, asked: bool) {
    if asked {
        let _ = kind.open_settings();
        return;
    }
    let mut c = CACHE.lock().unwrap();
    c.asked.push(kind);
    c.at = None;
    drop(c);
    let ctx = ctx.clone();
    // The request may show a system dialog; keep it off the UI thread.
    std::thread::spawn(move || {
        kind.request();
        CACHE.lock().unwrap().at = None;
        ctx.request_repaint();
    });
}

fn action_label(asked: bool) -> &'static str {
    if asked { "เปิดการตั้งค่า" } else { "ขอสิทธิ์" }
}

fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), semi(12.5), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(18.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, color.gamma_multiply(0.14));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// "สิทธิ์ของระบบ" section for Settings: one row per permission with its status and an action.
pub fn permissions_card(ui: &mut Ui) {
    let pal = Palette::of(ui);
    let (list, asked) = current();
    if list.iter().any(|p| !p.granted) {
        ui.ctx().request_repaint_after(RECHECK);
    }
    ui.add_space(12.0);
    ui.label(RichText::new("สิทธิ์ของระบบ").font(semi(13.0)).color(pal.muted));
    Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(16, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        for (i, p) in list.iter().enumerate() {
            if i > 0 {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(r, 0.0, pal.line);
            }
            let color = if p.granted { pal.ok } else if p.required { pal.warn } else { pal.muted };
            ui.horizontal(|ui| {
                ui.set_min_height(56.0);
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                paint_icon(ui, r, Icon::Shield, color);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add_space(9.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(p.kind.name()).color(pal.fg));
                    ui.label(RichText::new(p.kind.reason()).size(12.5).color(pal.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !p.granted {
                        let was_asked = asked.contains(&p.kind);
                        if secondary(ui, action_label(was_asked), None).clicked() {
                            act(ui.ctx(), p.kind, was_asked);
                        }
                    }
                    chip(ui, if p.granted { "ได้รับสิทธิ์" } else { "ยังไม่ได้รับ" }, color);
                });
            });
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("macOS ผูกสิทธิ์กับลายเซ็นของแอป ถ้าติดตั้ง ReDesk เวอร์ชันใหม่อาจต้องอนุญาตอีกครั้ง")
                .size(12.0)
                .color(pal.muted),
        );
        ui.add_space(6.0);
    });
}

/// Compact warning row, shown only while a required permission is missing.
pub fn missing_permissions_banner(ui: &mut Ui) {
    let (list, asked) = current();
    let missing: Vec<Kind> = list.iter().filter(|p| p.required && !p.granted).map(|p| p.kind).collect();
    let Some(&first) = missing.first() else { return };
    ui.ctx().request_repaint_after(RECHECK);
    let pal = Palette::of(ui);
    let names = missing.iter().map(|k| k.name()).collect::<Vec<_>>().join(" และ ");
    Frame::new()
        .fill(pal.warn.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, pal.warn.gamma_multiply(0.45)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                paint_icon(ui, r, Icon::Shield, pal.warn);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(format!("ยังไม่ได้รับสิทธิ์ {names}")).font(semi(13.5)).color(pal.fg));
                    ui.label(RichText::new("ผู้ควบคุมจะยังเห็นหรือใช้งานเครื่องนี้ไม่ได้").size(12.5).color(pal.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let was_asked = asked.contains(&first);
                    if secondary(ui, action_label(was_asked), None).clicked() {
                        act(ui.ctx(), first, was_asked);
                    }
                });
            });
        });
    ui.add_space(14.0);
}
