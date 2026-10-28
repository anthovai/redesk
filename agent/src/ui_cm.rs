//! Incoming connections on this machine: approval requests and live sessions.

use super::*;

impl App {
    /// Everything about incoming connections: approval requests and live sessions.
    pub(super) fn connection_manager(&mut self, ui: &mut Ui) {
        self.approval_modal(ui);
    }

    /// Who is controlling this machine right now, with a disconnect button each.
    pub(super) fn sessions_list(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
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

    pub(super) fn approval_modal(&mut self, ui: &mut Ui) {
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
}
