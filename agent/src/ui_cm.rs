//! Incoming connections on this machine, like RustDesk's connection manager: a small
//! always-on-top window with one tab per approval request or live session. It exists only while
//! there is something to show; without native child windows the approval falls back to a modal
//! in the main window.

use std::sync::OnceLock;

use super::*;
use crate::status::{CallState, HostCmd};

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
    /// Newest in-session event (chat, call, invitation) already announced.
    attention: Option<Instant>,
    pos: Option<[f32; 2]>,
    /// Chat instead of the session details.
    chat: bool,
    /// Message being typed, per session.
    drafts: HashMap<String, String>,
}

/// A switch-sides invitation not answered within this time is declined (the server grant
/// expires after 60 s).
const SWITCH_ANSWER: Duration = Duration::from_secs(55);

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
}

enum CmAction {
    Answer(usize, bool),
    Disconnect(SessionInfo),
    /// Something for the session task (chat, call answer, mute, hang up).
    Host(Arc<SessionLink>, HostCmd),
    /// Switch sides: accept or decline the viewer's invitation.
    Switch(Arc<SessionLink>, bool),
}

#[derive(Clone, Copy)]
struct Perms {
    input: bool,
    clipboard: bool,
    files: bool,
    whiteboard: bool,
    audio: bool,
    voice: bool,
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
    Mic,
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
        Glyph::Mic => {
            p.rect_stroke(Rect::from_min_max(at(5.5, 1.5), at(10.5, 10.0)), 2.5 * s, st, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![at(3.0, 7.5), at(3.6, 10.0), at(5.5, 12.0), at(8.0, 12.8), at(10.5, 12.0), at(12.4, 10.0), at(13.0, 7.5)], st));
            p.line_segment([at(8.0, 12.8), at(8.0, 15.0)], st);
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
                Perms {
                    input: c.allow_input,
                    clipboard: c.allow_clipboard,
                    files: c.allow_file_transfer,
                    whiteboard: c.allow_whiteboard,
                    audio: c.allow_audio,
                    voice: c.allow_voice,
                }
            })
            .unwrap_or(Perms { input: false, clipboard: false, files: false, whiteboard: false, audio: false, voice: false });

        // Tabs in arrival order: live sessions, then requests waiting for an answer.
        let mut entries: Vec<Entry> = sessions
            .into_iter()
            .map(|s| Entry {
                key: egui::Id::unique(("cm-session", &s.sid)),
                name: s.name.clone(),
                email: s.email.clone(),
                ip: s.ip.clone(),
                recording: s.recording,
                kind: Kind::Session(s),
            })
            .collect();
        for (i, (req, at)) in self.pending.iter().enumerate() {
            let (name, email) = split_who(&req.who);
            let left = APPROVAL_TIMEOUT.saturating_sub(at.elapsed());
            entries.push(Entry { key: pending_key(*at), name, email, ip: req.ip.clone(), kind: Kind::Pending(i, left), recording: req.recording });
        }

        let mut st: CmState = ctx.data(|d| d.get_temp(state_id())).unwrap_or_default();
        // Unanswered switch-sides invitations expire with their grant.
        for e in &entries {
            if let Kind::Session(s) = &e.kind {
                let mut offer = s.link.switch_offer.lock().unwrap();
                if offer.as_ref().is_some_and(|(_, _, at)| at.elapsed() > SWITCH_ANSWER) {
                    *offer = None;
                    let _ = s.link.cmd.send(HostCmd::SwitchFailed("timeout".into()));
                }
            }
        }
        // A chat message, call or invitation: show that session (its chat for a message) and
        // flash the window, without taking focus from whatever the person is typing in.
        let mut notify = false;
        if let Some((a, at)) = &self.cm_attention
            && st.attention.is_none_or(|t| *at > t)
        {
            st.attention = Some(*at);
            if let Some(e) = entries.iter().find(|e| matches!(&e.kind, Kind::Session(s) if s.sid == a.sid)) {
                if self.pending.is_empty() {
                    st.selected = Some(e.key);
                }
                if let Kind::Session(s) = &e.kind {
                    st.chat = s.link.chat && s.link.unread() > 0;
                }
            }
            notify = true;
        }
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
        let (mut chat_page, mut drafts) = (st.chat, std::mem::take(&mut st.drafts));
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
            } else if notify {
                vctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                vctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
            if ui.input(|i| i.viewport().close_requested()) {
                // Closing would hide a request or a live session: keep it, out of the way.
                vctx.send_viewport_cmd(ViewportCommand::CancelClose);
                vctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            cm_window(ui, &entries, &mut selected, perms, &mut chat_page, &mut drafts)
        });
        st.selected = selected;
        st.chat = chat_page;
        st.drafts = drafts;
        ctx.data_mut(|d| d.insert_temp(state_id(), st));

        match action {
            Some(CmAction::Answer(i, allow)) => {
                if let Some((req, _)) = self.pending.remove(i) {
                    let _ = req.reply.send(allow);
                }
            }
            Some(CmAction::Disconnect(s)) => s.kill.notify_one(),
            Some(CmAction::Host(link, cmd)) => {
                let _ = link.cmd.send(cmd);
            }
            Some(CmAction::Switch(link, true)) => self.accept_switch(&ctx, link),
            Some(CmAction::Switch(link, false)) => {
                if link.switch_offer.lock().unwrap().take().is_some() {
                    let _ = link.cmd.send(HostCmd::SwitchFailed("declined".into()));
                }
            }
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
        let perms = self.agent.as_ref().map(|a| {
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
            if c.allow_voice {
                p.push("โทรคุย");
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
fn cm_window(
    ui: &mut Ui,
    entries: &[Entry],
    selected: &mut Option<egui::Id>,
    perms: Perms,
    chat_page: &mut bool,
    drafts: &mut HashMap<String, String>,
) -> Option<CmAction> {
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
                if let Some(a) = session_prompts(ui, s, &who_of(entry)) {
                    action = Some(a);
                }
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

    let chat = match &entry.kind {
        Kind::Session(s) if s.link.chat => Some(s),
        _ => None,
    };
    if chat.is_none() {
        *chat_page = false;
    }
    egui::CentralPanel::default().frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 18, right: 18, top: 12, bottom: 0 })).show(ui, |ui| {
        if let Some(s) = chat {
            page_switch(ui, chat_page, s.link.unread());
            ui.add_space(8.0);
        }
        match chat {
            Some(s) if *chat_page => {
                s.link.mark_read();
                let draft = drafts.entry(s.sid.clone()).or_default();
                if let Some(text) = chat_view(ui, &s.link, draft, &who_of(entry)) {
                    action = Some(CmAction::Host(s.link.clone(), HostCmd::Chat(text)));
                }
            }
            _ => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(entry.key).show(ui, |ui| details(ui, entry, perms));
            }
        }
    });
    drafts.retain(|sid, _| entries.iter().any(|e| matches!(&e.kind, Kind::Session(s) if &s.sid == sid)));
    action
}

fn who_of(e: &Entry) -> String {
    if e.name.is_empty() { e.email.clone() } else { e.name.clone() }
}

/// "ข้อมูล | แชท" above a session that can chat; the chat side shows unread messages.
fn page_switch(ui: &mut Ui, chat_page: &mut bool, unread: usize) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    ui.painter().rect_filled(rect, 8.0, pal.field);
    let half = rect.width() / 2.0;
    for (i, label) in ["ข้อมูล", "แชท"].into_iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(half * i as f32, 0.0), vec2(half, rect.height())).shrink(3.0);
        let on = *chat_page == (i == 1);
        let resp = ui.interact(r, egui::Id::unique(("cm-page", i)), Sense::click());
        if on {
            ui.painter().rect_filled(r, 6.0, pal.bg);
        }
        let color = if on { pal.fg } else { pal.muted };
        let font = if on { semi(13.0) } else { FontId::proportional(13.0) };
        let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
        let mut x = r.center().x - galley.size().x / 2.0;
        if i == 1 && unread > 0 {
            x -= 10.0;
            let badge = pos2(x + galley.size().x + 14.0, r.center().y);
            ui.painter().circle_filled(badge, 8.0, pal.danger);
            ui.painter().text(badge, Align2::CENTER_CENTER, unread.min(99).to_string(), semi(10.5), Color32::WHITE);
        }
        ui.painter().galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, color);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            *chat_page = i == 1;
        }
    }
}

/// Chat with the viewer of one session: messages, then a line to type in. Returns a message
/// to send.
fn chat_view(ui: &mut Ui, link: &SessionLink, draft: &mut String, who: &str) -> Option<String> {
    let pal = Palette::of(ui);
    let lines = link.lines.lock().unwrap().clone();
    let input_h = 40.0;
    let mut send = None;
    let list_h = (ui.available_height() - input_h - 12.0).max(60.0);
    egui::ScrollArea::vertical().id_salt("cm-chat").max_height(list_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        if lines.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(format!("พิมพ์ข้อความถึง {who}")).size(13.0).color(pal.muted));
                ui.label(RichText::new("ข้อความไม่ถูกเก็บไว้ที่เซิร์ฟเวอร์ และหายไปเมื่อจบการเชื่อมต่อ").size(11.5).color(pal.muted));
            });
        }
        for l in &lines {
            chat_bubble(ui, l.mine, &l.text, l.at);
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let w = ui.available_width() - 44.0;
        let edit = ui.add(
            TextEdit::singleline(draft)
                .hint_text("ข้อความ")
                .desired_width(w)
                .margin(Margin::symmetric(10, 8))
                .char_limit(crate::proto::CHAT_MAX),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let b = Button::new(RichText::new("ส่ง").font(semi(13.0)).color(pal.on_accent)).fill(pal.accent).corner_radius(8).min_size(vec2(40.0, 34.0));
        let clicked = ui.add_enabled(!draft.trim().is_empty(), b).clicked();
        if (enter || clicked) && !draft.trim().is_empty() {
            send = Some(std::mem::take(draft));
            edit.request_focus();
        }
    });
    send
}

/// One chat message: this side on the right in the accent color, the other on the left.
pub(super) fn chat_bubble(ui: &mut Ui, mine: bool, text: &str, at: SystemTime) {
    let pal = Palette::of(ui);
    let max = (ui.available_width() * 0.8).max(120.0);
    let (fill, fg) = if mine { (pal.accent, pal.on_accent) } else { (pal.field, pal.fg) };
    let layout = if mine { Layout::right_to_left(Align::Min) } else { Layout::left_to_right(Align::Min) };
    ui.with_layout(layout, |ui| {
        ui.vertical(|ui| {
            Frame::new().fill(fill).corner_radius(10).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_max_width(max);
                ui.add(egui::Label::new(RichText::new(text).size(13.5).color(fg)).wrap().selectable(true));
            });
            ui.label(RichText::new(ago(at)).size(10.5).color(pal.muted));
        });
    });
    ui.add_space(2.0);
}

/// How long ago a chat message was sent ("ตอนนี้", "5 นาทีที่แล้ว").
pub(super) fn ago(at: SystemTime) -> String {
    match at.elapsed().map(|d| d.as_secs()).unwrap_or(0) {
        0..60 => "ตอนนี้".into(),
        s @ 60..3600 => format!("{} นาทีที่แล้ว", s / 60),
        s => format!("{} ชม. ที่แล้ว", s / 3600),
    }
}

/// Voice call and switch-sides prompts above the disconnect button.
fn session_prompts(ui: &mut Ui, s: &SessionInfo, who: &str) -> Option<CmAction> {
    let pal = Palette::of(ui);
    let link = &s.link;
    let mut action = None;
    let card = |ui: &mut Ui, tint: Color32, body: &mut dyn FnMut(&mut Ui)| {
        Frame::new().fill(tint.gamma_multiply(0.12)).corner_radius(10).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
        ui.add_space(8.0);
    };
    if link.switch_offer.lock().unwrap().is_some() {
        card(ui, pal.accent, &mut |ui| {
            ui.label(RichText::new("สลับฝั่ง").font(semi(13.5)).color(pal.fg));
            ui.label(RichText::new(format!("{who} เชิญให้คุณควบคุมเครื่องของเขา")).size(12.5).color(pal.muted));
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                if cols[0].vertical_centered(small_deny("ปฏิเสธ")).inner.clicked() {
                    action = Some(CmAction::Switch(link.clone(), false));
                }
                if cols[1].vertical_centered(small_allow("ควบคุม")).inner.clicked() {
                    action = Some(CmAction::Switch(link.clone(), true));
                }
            });
        });
    }
    match link.call_state() {
        CallState::Ringing(at) => {
            let left = crate::session::RING_TIMEOUT.saturating_sub(at.elapsed()).as_secs();
            card(ui, pal.ok, &mut |ui| {
                ui.label(RichText::new("สายเรียกเข้า (เสียง)").font(semi(13.5)).color(pal.fg));
                ui.label(RichText::new(format!("{who} ขอโทรคุย · ปฏิเสธอัตโนมัติใน {left} วินาที")).size(12.5).color(pal.muted));
                ui.add_space(6.0);
                ui.columns(2, |cols| {
                    if cols[0].vertical_centered(small_deny("ปฏิเสธ")).inner.clicked() {
                        action = Some(CmAction::Host(link.clone(), HostCmd::Answer(false)));
                    }
                    if cols[1].vertical_centered(small_allow("รับสาย")).inner.clicked() {
                        action = Some(CmAction::Host(link.clone(), HostCmd::Answer(true)));
                    }
                });
            });
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        CallState::Active { since, muted, remote_muted } => {
            card(ui, pal.ok, &mut |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let t = since.elapsed().as_secs();
                        ui.label(RichText::new(format!("กำลังโทร  {:02}:{:02}", t / 60, t % 60)).font(semi(13.5)).color(pal.fg));
                        if remote_muted {
                            ui.label(RichText::new("อีกฝ่ายปิดไมค์อยู่").size(12.0).color(pal.muted));
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new(RichText::new("วางสาย").color(Color32::WHITE)).fill(pal.danger).corner_radius(8)).clicked() {
                            action = Some(CmAction::Host(link.clone(), HostCmd::Hangup));
                        }
                        let label = if muted { "เปิดไมค์" } else { "ปิดไมค์" };
                        if ui.add(Button::new(label).corner_radius(8)).clicked() {
                            action = Some(CmAction::Host(link.clone(), HostCmd::Mute(!muted)));
                        }
                    });
                });
            });
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        CallState::Idle => {}
    }
    action
}

fn small_deny(text: &'static str) -> impl FnOnce(&mut Ui) -> Response {
    move |ui| {
        let pal = Palette::of(ui);
        ui.add(Button::new(RichText::new(text).font(semi(13.5)).color(pal.fg)).fill(pal.bg).stroke(Stroke::new(1.0, pal.line)).corner_radius(8).min_size(vec2(ui.available_width(), 34.0)))
    }
}

fn small_allow(text: &'static str) -> impl FnOnce(&mut Ui) -> Response {
    move |ui| {
        let pal = Palette::of(ui);
        ui.add(Button::new(RichText::new(text).font(semi(13.5)).color(pal.on_accent)).fill(pal.accent).stroke(Stroke::NONE).corner_radius(8).min_size(vec2(ui.available_width(), 34.0)))
    }
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
                let waiting = match &e.kind {
                    Kind::Pending(..) => true,
                    Kind::Session(s) => matches!(s.link.call_state(), CallState::Ringing(_)) || s.link.switch_offer.lock().unwrap().is_some(),
                };
                let unread = match &e.kind {
                    Kind::Session(s) => s.link.unread(),
                    Kind::Pending(..) => 0,
                };
                if unread > 0 {
                    // Unread chat messages: a red count on the avatar.
                    let b = c + vec2(9.0, -8.0);
                    p.circle_filled(b, 7.5, pal.side);
                    p.circle_filled(b, 6.5, pal.danger);
                    p.text(b, Align2::CENTER_CENTER, unread.min(9).to_string(), semi(9.5), Color32::WHITE);
                } else if waiting {
                    // Waiting for an answer (approval, call, invitation): an orange dot.
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
    ui.horizontal(|ui| {
        perm_tile(ui, Glyph::Mic, "โทรคุย (ต้องรับสาย)", perms.voice, size);
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
