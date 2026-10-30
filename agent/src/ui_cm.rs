//! Incoming connections on this machine, like RustDesk's connection manager: a small
//! always-on-top window with one tab per approval request or live session. It exists only while
//! there is something to show; without native child windows the approval falls back to a modal
//! in the main window.

use std::sync::OnceLock;

use super::*;

const CM_SIZE: [f32; 2] = [320.0, 580.0];
const TAB_HEIGHT: f32 = 44.0;

/// Set once the backend turns out not to support native child windows.
static NO_VIEWPORTS: AtomicBool = AtomicBool::new(false);

fn cm_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("aegiron-connection-manager")
}

fn state_id() -> egui::Id {
    egui::Id::unique("aegiron-cm-state")
}

/// Connection-manager UI state, kept in egui memory while the window is open.
#[derive(Clone, Default)]
struct CmState {
    selected: Option<egui::Id>,
    /// Newest approval request already announced: focus is taken once per request.
    announced: Option<Instant>,
    pos: Option<[f32; 2]>,
}

enum Kind {
    /// Index into `App::pending` and time left before the automatic denial.
    Pending(usize, Duration),
    Session(SessionInfo),
}

struct Entry {
    key: egui::Id,
    name: String,
    email: String,
    ip: String,
    kind: Kind,
    /// The session is (or will be, if accepted) recorded.
    recording: bool,
    /// A tunnel or terminal session: what it does instead of remote control.
    purpose: String,
}

enum CmAction {
    Answer(usize, bool),
    Disconnect(SessionInfo),
}

#[derive(Clone, Copy)]
struct Perms {
    input: bool,
    clipboard: bool,
    files: bool,
    whiteboard: bool,
    audio: bool,
}

fn split_who(who: &str) -> (String, String) {
    match who.split_once(" <") {
        Some((n, e)) => (n.to_string(), e.trim_end_matches('>').to_string()),
        None => (who.to_string(), String::new()),
    }
}

fn pending_key(at: Instant) -> egui::Id {
    egui::Id::unique(("cm-pending", at))
}

fn window_icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| Arc::new(egui::IconData { rgba: icon_rgba(64), width: 64, height: 64 })).clone()
}

/// A stable, pleasant color per person (like RustDesk's avatar colors).
fn tint(name: &str) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(0x00, 0x71, 0xff),
        Color32::from_rgb(0x7c, 0x4d, 0xff),
        Color32::from_rgb(0x00, 0x97, 0xa7),
        Color32::from_rgb(0x2e, 0x9d, 0x5b),
        Color32::from_rgb(0xe6, 0x7e, 0x22),
        Color32::from_rgb(0xd8, 0x43, 0x6b),
        Color32::from_rgb(0x55, 0x6e, 0xe6),
        Color32::from_rgb(0x8e, 0x6c, 0x3a),
    ];
    let h = name.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    COLORS[h as usize % COLORS.len()]
}

fn round_avatar(ui: &mut Ui, letter: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, semi(size * 0.44), Color32::WHITE);
}

// ---------------------------------------------------------------------------------------------
// Permission glyphs (private: the shared `Icon` set lives in ui.rs)

#[derive(Clone, Copy)]
enum Glyph {
    Screen,
    Keyboard,
    Clipboard,
    Folder,
    Pen,
    Speaker,
}

fn glyph(ui: &Ui, rect: Rect, g: Glyph, color: Color32) {
    let p = ui.painter();
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let st = Stroke::new(1.5 * s.max(1.0), color);
    match g {
        Glyph::Screen => paint_icon(ui, rect, Icon::Monitor, color),
        Glyph::Keyboard => {
            p.rect_stroke(Rect::from_min_max(at(1.0, 3.5), at(15.0, 12.5)), 2.0 * s, st, egui::StrokeKind::Middle);
            for (y, xs) in [(6.2, [3.8, 6.6, 9.4, 12.2]), (8.6, [3.8, 6.6, 9.4, 12.2])] {
                for x in xs {
                    p.rect_filled(Rect::from_center_size(at(x, y), vec2(1.5, 1.3) * s), 0.3 * s, color);
                }
            }
            p.line_segment([at(5.0, 10.8), at(11.0, 10.8)], st);
        }
        Glyph::Clipboard => {
            p.rect_stroke(Rect::from_min_max(at(3.0, 3.0), at(13.0, 15.0)), 1.6 * s, st, egui::StrokeKind::Middle);
            p.rect_filled(Rect::from_min_max(at(5.5, 1.5), at(10.5, 4.5)), 1.0 * s, color);
            p.line_segment([at(5.5, 8.0), at(10.5, 8.0)], st);
            p.line_segment([at(5.5, 11.0), at(9.0, 11.0)], st);
        }
        Glyph::Folder => {
            p.add(Shape::line(
                vec![at(1.5, 13.5), at(1.5, 3.0), at(6.0, 3.0), at(7.6, 4.8), at(14.5, 4.8), at(14.5, 13.5), at(1.5, 13.5)],
                st,
            ));
            p.line_segment([at(1.5, 7.0), at(14.5, 7.0)], st);
        }
        Glyph::Pen => {
            p.add(Shape::closed_line(vec![at(10.8, 2.2), at(13.8, 5.2), at(5.5, 13.5), at(1.8, 14.2), at(2.5, 10.5)], st));
            p.line_segment([at(9.0, 4.0), at(12.0, 7.0)], st);
        }
        Glyph::Speaker => {
            p.add(Shape::convex_polygon(
                [(1.5, 5.8), (4.5, 5.8), (8.5, 2.2), (8.5, 13.8), (4.5, 10.2), (1.5, 10.2)].iter().map(|&(x, y)| at(x, y)).collect(),
                color,
                Stroke::NONE,
            ));
            p.line_segment([at(11.0, 5.5), at(12.2, 8.0)], st);
            p.line_segment([at(12.2, 8.0), at(11.0, 10.5)], st);
            p.line_segment([at(13.4, 3.5), at(15.0, 8.0)], st);
            p.line_segment([at(15.0, 8.0), at(13.4, 12.5)], st);
        }
    }
}

/// An on/off permission tile, like RustDesk's CM permission grid.
fn perm_tile(ui: &mut Ui, g: Glyph, text: &str, on: bool, size: egui::Vec2) {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let (fill, fg) = if on { (pal.accent, pal.on_accent) } else { (pal.field, pal.muted) };
    ui.painter().rect_filled(rect, 10.0, fill);
    if !on {
        ui.painter().rect_stroke(rect, 10.0, Stroke::new(1.0, pal.line), egui::StrokeKind::Inside);
    }
    let icon = Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(18.0, 18.0));
    glyph(ui, icon, g, fg);
    if !on {
        // Struck through when not allowed.
        ui.painter().line_segment([icon.left_bottom() + vec2(-1.0, 1.0), icon.right_top() + vec2(1.0, -1.0)], Stroke::new(1.6, pal.danger));
    }
    let font = if on { semi(13.0) } else { FontId::proportional(13.0) };
    let galley = ui.painter().layout(text.to_owned(), font, fg, rect.width() - 46.0);
    ui.painter().galley(pos2(rect.left() + 40.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    let tip = if on { "อนุญาตอยู่ · เปลี่ยนได้ที่ ตั้งค่า › ความปลอดภัย" } else { "ไม่อนุญาต · เปลี่ยนได้ที่ ตั้งค่า › ความปลอดภัย" };
    resp.on_hover_text(tip);
}

// ---------------------------------------------------------------------------------------------

impl App {
    /// Everything about incoming connections: approval requests and live sessions.
    pub(super) fn connection_manager(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let sessions = self.agent.as_ref().map(|a| a.status.lock().unwrap().sessions.clone()).unwrap_or_default();
        if self.pending.is_empty() && sessions.is_empty() {
            // Nothing to manage: the window closes (it is shown only while called).
            ctx.data_mut(|d| d.remove::<CmState>(state_id()));
            return;
        }
        if !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250)); // countdown
        }
        if ctx.embed_viewports() || NO_VIEWPORTS.load(Ordering::Relaxed) {
            self.approval_modal(ui);
            return;
        }

        let perms = self
            .agent
            .as_ref()
            .map(|a| {
                let c = a.config.lock().unwrap();
                Perms { input: c.allow_input, clipboard: c.allow_clipboard, files: c.allow_file_transfer, whiteboard: c.allow_whiteboard, audio: c.allow_audio }
            })
            .unwrap_or(Perms { input: false, clipboard: false, files: false, whiteboard: false, audio: false });

        // Tabs in arrival order: live sessions, then requests waiting for an answer.
        let mut entries: Vec<Entry> = sessions
            .into_iter()
            .map(|s| Entry {
                key: egui::Id::unique(("cm-session", &s.sid)),
                name: s.name.clone(),
                email: s.email.clone(),
                ip: s.ip.clone(),
                recording: s.recording,
                purpose: s.purpose.clone(),
                kind: Kind::Session(s),
            })
            .collect();
        for (i, (req, at)) in self.pending.iter().enumerate() {
            let (name, email) = split_who(&req.who);
            let left = APPROVAL_TIMEOUT.saturating_sub(at.elapsed());
            entries.push(Entry {
                key: pending_key(*at),
                name,
                email,
                ip: req.ip.clone(),
                kind: Kind::Pending(i, left),
                recording: req.recording,
                purpose: req.purpose.clone(),
            });
        }

        let mut st: CmState = ctx.data(|d| d.get_temp(state_id())).unwrap_or_default();
        // A new request: show its tab and bring the window forward, once.
        let mut announce = false;
        if let Some((_, at)) = self.pending.back()
            && st.announced.is_none_or(|a| *at > a)
        {
            st.announced = Some(*at);
            st.selected = Some(pending_key(*at));
            announce = true;
        }
        if !st.selected.is_some_and(|k| entries.iter().any(|e| e.key == k)) {
            // Default: the oldest waiting request, otherwise the newest session.
            st.selected = entries.iter().find(|e| matches!(e.kind, Kind::Pending(..))).or(entries.last()).map(|e| e.key);
        }
        if st.pos.is_none() {
            // AEGIRON_UI_CM_POS="x,y" places the window (automated screenshots).
            st.pos = std::env::var("AEGIRON_UI_CM_POS")
                .ok()
                .and_then(|p| {
                    let (x, y) = p.split_once(',')?;
                    Some([x.parse::<f32>().ok()?, y.parse::<f32>().ok()?])
                })
                .or_else(|| {
                    // Right edge of the screen, a little above the middle (like RustDesk).
                    ctx.input(|i| i.viewport().monitor_size)
                        .filter(|m| m.x > CM_SIZE[0] && m.y > CM_SIZE[1])
                        .map(|m| [m.x - CM_SIZE[0] - 24.0, ((m.y - CM_SIZE[1]) * 0.4).max(32.0)])
                });
        }

        let mut builder = egui::ViewportBuilder::default()
            .with_title("Aegiron — การเชื่อมต่อเข้า")
            .with_inner_size(CM_SIZE)
            .with_min_inner_size([300.0, 520.0])
            .with_maximize_button(false)
            .with_always_on_top()
            .with_transparent(false)
            // Never take focus by appearing; a new request asks for it explicitly, once.
            .with_active(false)
            .with_icon(window_icon());
        if let Some(pos) = st.pos {
            builder = builder.with_position(pos);
        }

        let mut selected = st.selected;
        let action = ctx.show_viewport_immediate(cm_viewport(), builder, |ui, class| {
            if class == egui::ViewportClass::EmbeddedWindow {
                // No native child windows here: use the modal from the next frame on.
                NO_VIEWPORTS.store(true, Ordering::Relaxed);
                return None;
            }
            let vctx = ui.ctx().clone();
            if announce {
                vctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                vctx.send_viewport_cmd(ViewportCommand::Focus);
                vctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
            }
            if ui.input(|i| i.viewport().close_requested()) {
                // Closing would hide a request or a live session: keep it, out of the way.
                vctx.send_viewport_cmd(ViewportCommand::CancelClose);
                vctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            cm_window(ui, &entries, &mut selected, perms)
        });
        st.selected = selected;
        ctx.data_mut(|d| d.insert_temp(state_id(), st));

        match action {
            Some(CmAction::Answer(i, allow)) => {
                if let Some((req, _)) = self.pending.remove(i) {
                    let _ = req.reply.send(allow);
                }
            }
            Some(CmAction::Disconnect(s)) => s.kill.notify_one(),
            None => {}
        }
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
                        if !s.purpose.is_empty() {
                            ui.label(RichText::new(&s.purpose).size(12.5).color(pal.muted));
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary(ui, "ตัดการเชื่อมต่อ", Some(pal.danger)).clicked() {
                            s.kill.notify_one();
                        }
                        ui.label(RichText::new(elapsed(s.started)).font(mono(13.0)).color(pal.muted));
                        if s.recording {
                            recording_chip(ui, "กำลังบันทึก");
                        }
                    });
                });
            });
        }
    }

    /// Fallback when native child windows are unavailable: the request as a modal.
    pub(super) fn approval_modal(&mut self, ui: &mut Ui) {
        let Some((req, at)) = self.pending.front() else { return };
        let pal = Palette::of(ui);
        let left = APPROVAL_TIMEOUT.saturating_sub(at.elapsed());
        let (name, email) = split_who(&req.who);
        let ip = req.ip.clone();
        let recording = req.recording;
        let purpose = req.purpose.clone();
        let perms = self.agent.as_ref().filter(|_| purpose.is_empty()).map(|a| {
            let c = a.config.lock().unwrap();
            let mut p = vec![if c.allow_input { "ควบคุมเมาส์และคีย์บอร์ด" } else { "ดูหน้าจอ" }];
            if c.allow_clipboard {
                p.push("คลิปบอร์ด");
            }
            if c.allow_file_transfer {
                p.push("รับ-ส่งไฟล์");
            }
            if c.allow_audio {
                p.push("เสียง");
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
                let asks = if purpose.is_empty() { "มีผู้ขอควบคุมเครื่องนี้".to_string() } else { format!("มีผู้ขอ{purpose}") };
                ui.label(RichText::new(asks).size(13.0).color(pal.muted));
                ui.label(RichText::new(&name).font(semi(18.0)).color(pal.fg));
                if !email.is_empty() {
                    ui.label(RichText::new(&email).size(13.0).color(pal.muted));
                }
                ui.label(RichText::new(format!("IP {ip}")).font(mono(12.5)).color(pal.muted));
                if let Some(p) = &perms {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("ถ้าอนุญาต จะทำได้: {p}")).size(12.5).color(pal.muted));
                }
                if recording {
                    ui.add_space(6.0);
                    recording_chip(ui, "เซสชันนี้จะถูกบันทึก");
                }
            });
            ui.add_space(12.0);
            countdown(ui, left);
            ui.add_space(8.0);
            ui.columns(2, |cols| {
                if cols[0].vertical_centered(deny_button).inner.clicked() {
                    answer = Some(false);
                }
                if cols[1].vertical_centered(allow_button).inner.clicked() {
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

fn countdown(ui: &mut Ui, left: Duration) {
    let pal = Palette::of(ui);
    let frac = left.as_secs_f32() / APPROVAL_TIMEOUT.as_secs_f32();
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, pal.field);
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac, r.height())), 2.0, pal.warn);
    ui.label(RichText::new(format!("ปฏิเสธอัตโนมัติใน {} วินาที", left.as_secs())).size(12.5).color(pal.muted));
}

fn deny_button(ui: &mut Ui) -> Response {
    let pal = Palette::of(ui);
    let b = Button::new(RichText::new("ปฏิเสธ").font(semi(14.5)).color(pal.fg))
        .fill(pal.bg)
        .stroke(Stroke::new(1.0, pal.line))
        .corner_radius(8)
        .min_size(vec2(ui.available_width(), 42.0));
    ui.add(b)
}

fn allow_button(ui: &mut Ui) -> Response {
    let pal = Palette::of(ui);
    let b = Button::new(RichText::new("อนุญาต").font(semi(14.5)).color(pal.on_accent))
        .fill(pal.accent)
        .stroke(Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(ui.available_width(), 42.0));
    ui.add(b)
}

/// The connection-manager window: tab strip, the selected connection, its actions.
fn cm_window(ui: &mut Ui, entries: &[Entry], selected: &mut Option<egui::Id>, perms: Perms) -> Option<CmAction> {
    let pal = Palette::of(ui);
    egui::Panel::top("cm-tabs")
        .exact_size(TAB_HEIGHT)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(pal.side))
        .show(ui, |ui| tab_strip(ui, entries, selected));
    let Some(entry) = entries.iter().find(|e| Some(e.key) == *selected).or(entries.first()) else { return None };

    let mut action = None;
    egui::Panel::bottom("cm-actions")
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 18, right: 18, top: 10, bottom: 18 }))
        .show(ui, |ui| match &entry.kind {
            Kind::Pending(i, left) => {
                countdown(ui, *left);
                ui.add_space(4.0);
                ui.columns(2, |cols| {
                    if cols[0].vertical_centered(deny_button).inner.clicked() {
                        action = Some(CmAction::Answer(*i, false));
                    }
                    if cols[1].vertical_centered(allow_button).inner.clicked() {
                        action = Some(CmAction::Answer(*i, true));
                    }
                });
            }
            Kind::Session(s) => {
                let b = Button::new(RichText::new("ตัดการเชื่อมต่อ").font(semi(14.5)).color(Color32::WHITE))
                    .fill(pal.danger)
                    .stroke(Stroke::NONE)
                    .corner_radius(8)
                    .min_size(vec2(ui.available_width(), 42.0));
                if ui.vertical_centered(|ui| ui.add(b)).inner.clicked() {
                    action = Some(CmAction::Disconnect(s.clone()));
                }
            }
        });

    egui::CentralPanel::default().frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 18, right: 18, top: 16, bottom: 0 })).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(entry.key).show(ui, |ui| details(ui, entry, perms));
    });
    action
}

fn tab_strip(ui: &mut Ui, entries: &[Entry], selected: &mut Option<egui::Id>) {
    let pal = Palette::of(ui);
    let full = ui.max_rect();
    egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            let w = (full.width() / entries.len() as f32).clamp(96.0, 160.0);
            for e in entries {
                let on = Some(e.key) == *selected;
                let (rect, resp) = ui.allocate_exact_size(vec2(w, TAB_HEIGHT), Sense::click());
                let p = ui.painter();
                if on {
                    p.rect_filled(rect, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, pal.bg);
                    p.rect_filled(Rect::from_min_size(rect.left_bottom() - vec2(0.0, 2.0), vec2(w, 2.0)), 0.0, pal.accent);
                } else if resp.hovered() {
                    p.rect_filled(rect.shrink2(vec2(2.0, 4.0)), 6.0, if ui.visuals().dark_mode { pal.field } else { pal.line.gamma_multiply(0.7) });
                }
                let who = if e.name.is_empty() { e.email.as_str() } else { e.name.as_str() };
                let c = rect.left_center() + vec2(22.0, 0.0);
                p.circle_filled(c, 11.0, tint(who));
                p.text(c, Align2::CENTER_CENTER, initials(&e.name, &e.email), semi(11.5), Color32::WHITE);
                if matches!(e.kind, Kind::Pending(..)) {
                    // Waiting for an answer: an orange dot on the avatar.
                    p.circle_filled(c + vec2(8.0, -8.0), 4.5, pal.side);
                    p.circle_filled(c + vec2(8.0, -8.0), 3.2, pal.warn);
                }
                let font = if on { semi(13.0) } else { FontId::proportional(13.0) };
                let max = w - 46.0;
                let mut text: String = who.to_string();
                let mut galley = p.layout_no_wrap(text.clone(), font.clone(), if on { pal.fg } else { pal.muted });
                while galley.size().x > max && text.chars().count() > 2 {
                    text.pop();
                    let t = format!("{}…", text.trim_end());
                    galley = p.layout_no_wrap(t, font.clone(), if on { pal.fg } else { pal.muted });
                }
                p.galley(pos2(rect.left() + 39.0, rect.center().y - galley.size().y / 2.0), galley, pal.fg);
                if resp.on_hover_text(who).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    *selected = Some(e.key);
                }
            }
        });
    });
}

fn details(ui: &mut Ui, e: &Entry, perms: Perms) {
    let pal = Palette::of(ui);
    let who = if e.name.is_empty() { e.email.clone() } else { e.name.clone() };
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        round_avatar(ui, &initials(&e.name, &e.email), tint(&who), 64.0);
        ui.add_space(6.0);
        ui.label(RichText::new(&who).font(semi(18.0)).color(pal.fg));
        if !e.email.is_empty() && e.email != who {
            ui.label(RichText::new(&e.email).size(13.0).color(pal.muted));
        }
        ui.label(RichText::new(format!("IP {}", e.ip)).font(mono(12.5)).color(pal.muted));
        ui.add_space(8.0);
        let (dot, text) = match &e.kind {
            Kind::Pending(..) if !e.purpose.is_empty() => (pal.warn, format!("ขอ{}", e.purpose)),
            Kind::Pending(..) => (pal.warn, "ขอควบคุมเครื่องนี้".to_string()),
            Kind::Session(s) => (pal.ok, format!("เชื่อมต่อแล้ว  {}", elapsed(s.started))),
        };
        // Status chip, centered: colored dot + text on a soft tint.
        let galley = ui.painter().layout_no_wrap(text, semi(13.0), pal.fg);
        let (r, _) = ui.allocate_exact_size(galley.size() + vec2(36.0, 8.0), Sense::hover());
        ui.painter().rect_filled(r, r.height() / 2.0, dot.gamma_multiply(0.14));
        ui.painter().circle_filled(r.left_center() + vec2(14.0, 0.0), 4.0, dot);
        ui.painter().galley(pos2(r.left() + 24.0, r.center().y - galley.size().y / 2.0), galley, pal.fg);
        if e.recording {
            ui.add_space(6.0);
            let text = if matches!(e.kind, Kind::Pending(..)) { "เซสชันนี้จะถูกบันทึก" } else { "กำลังบันทึก" };
            recording_chip(ui, text);
        }
    });
    ui.add_space(14.0);
    if !e.purpose.is_empty() {
        // Tunnels and terminals do not see or control the screen: say what they do instead.
        let title = if matches!(e.kind, Kind::Pending(..)) { "ถ้าอนุญาต ผู้เชื่อมต่อจะทำได้" } else { "การเชื่อมต่อนี้" };
        ui.label(RichText::new(title).font(semi(13.0)).color(pal.muted));
        ui.add_space(2.0);
        ui.label(RichText::new(&e.purpose).size(14.0).color(pal.fg));
        if e.purpose.contains("เทอร์มินัล") {
            ui.label(RichText::new("คำสั่งจะทำงานด้วยสิทธิ์ของผู้ใช้ที่เปิด Aegiron บนเครื่องนี้").size(12.5).color(pal.muted));
        }
        return;
    }
    let title = if matches!(e.kind, Kind::Pending(..)) { "ถ้าอนุญาต ผู้ควบคุมจะทำได้" } else { "สิทธิ์ของผู้ควบคุม" };
    ui.label(RichText::new(title).font(semi(13.0)).color(pal.muted));
    ui.add_space(2.0);
    let gap = 8.0;
    let w = ((ui.available_width() - gap) / 2.0).floor();
    let size = vec2(w, 40.0);
    ui.spacing_mut().item_spacing = vec2(gap, gap);
    ui.horizontal(|ui| {
        perm_tile(ui, Glyph::Screen, "ดูหน้าจอ", true, size);
        perm_tile(ui, Glyph::Keyboard, "เมาส์และคีย์บอร์ด", perms.input, size);
    });
    ui.horizontal(|ui| {
        perm_tile(ui, Glyph::Clipboard, "คลิปบอร์ด", perms.clipboard, size);
        perm_tile(ui, Glyph::Folder, "รับ-ส่งไฟล์", perms.files, size);
    });
    ui.horizontal(|ui| {
        perm_tile(ui, Glyph::Pen, "วาดบนหน้าจอ", perms.whiteboard, size);
        perm_tile(ui, Glyph::Speaker, "เสียง", perms.audio, size);
    });
}

/// Red "recording" chip: the person at this machine always sees when a session is recorded.
pub(super) fn recording_chip(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::of(ui);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), semi(13.0), pal.danger);
    let (r, resp) = ui.allocate_exact_size(galley.size() + vec2(34.0, 8.0), Sense::hover());
    ui.painter().rect_filled(r, r.height() / 2.0, pal.danger.gamma_multiply(0.14));
    // Pulsing dot.
    let t = ui.input(|i| i.time);
    let a = 0.55 + 0.45 * (t * 3.0).sin().abs() as f32;
    ui.painter().circle_filled(r.left_center() + vec2(13.0, 0.0), 4.5, pal.danger.gamma_multiply(a));
    ui.painter().galley(pos2(r.left() + 23.0, r.center().y - galley.size().y / 2.0), galley, pal.danger);
    ui.ctx().request_repaint_after(Duration::from_millis(100));
    resp.on_hover_text("องค์กรกำหนดให้บันทึกหน้าจอของเซสชันนี้ ผู้ดูแลระบบดูย้อนหลังได้ที่เว็บคอนโซล")
}
