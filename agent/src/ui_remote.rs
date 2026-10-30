//! Terminal windows and TCP tunnels started from the app (docs/PROTOCOL.md §11): a terminal
//! is its own window (like a remote-control window) with a small VT renderer; tunnels are
//! opened from a dialog and listed in a panel until stopped.

use std::sync::atomic::AtomicU64;

use egui::{Align2, Order};

use super::*;
use crate::client::{self, Auth};
use crate::remote::SessionKind;
use crate::terminal::{TermCmd, TermEnd};
use crate::tunnel::{self, TunnelPhase};

const TERM_SIZE: [f32; 2] = [900.0, 560.0];
const TERM_FONT: f32 = 14.0;
const TERM_BG: Color32 = Color32::from_rgb(0x18, 0x19, 0x1e);
const TERM_FG: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xdc);

#[derive(Clone, Debug, PartialEq)]
enum TermPhase {
    Connecting,
    Password,
    Approval,
    Connected,
    Exited(Option<u32>),
    Closed,
}

struct TermState {
    phase: TermPhase,
    error: Option<String>,
    name: String,
    fingerprint: String,
    path: Option<crate::direct::Path>,
    recording: bool,
}

/// What only the window touches.
#[derive(Default)]
struct TermUi {
    password: String,
    /// Lines scrolled back into the history (0 = live).
    scroll: usize,
    focused_once: bool,
}

pub struct TerminalWindow {
    pub device_id: String,
    pub viewport: egui::ViewportId,
    pub closed: AtomicBool,
    pub reconnect: AtomicBool,
    state: Mutex<TermState>,
    screen: Mutex<vt100::Parser>,
    cmds: mpsc::UnboundedSender<TermCmd>,
    passwords: mpsc::UnboundedSender<String>,
    ui: Mutex<TermUi>,
}

impl TerminalWindow {
    pub fn open(target: client::Target, name: String, viewport: egui::ViewportId, repaint: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        let (passwords, pw_rx) = mpsc::unbounded_channel();
        let w = Arc::new(TerminalWindow {
            device_id: target.device_id.clone(),
            viewport,
            closed: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
            state: Mutex::new(TermState { phase: TermPhase::Connecting, error: None, name, fingerprint: String::new(), path: None, recording: false }),
            screen: Mutex::new(vt100::Parser::new(24, 80, 5000)),
            cmds,
            passwords,
            ui: Mutex::default(),
        });
        let task = w.clone();
        crate::runtime().spawn(async move {
            let res = task.run(target, pw_rx, cmd_rx, repaint.clone()).await;
            let mut st = task.state.lock().unwrap();
            if let Err(e) = res {
                tracing::warn!("terminal {}: {e:#}", task.device_id);
                st.error.get_or_insert(format!("{e:#}"));
                st.phase = TermPhase::Closed;
            }
            drop(st);
            repaint();
        });
        w
    }

    fn set(&self, f: impl FnOnce(&mut TermState)) {
        f(&mut self.state.lock().unwrap());
    }

    async fn run(
        &self,
        target: client::Target,
        mut passwords: mpsc::UnboundedReceiver<String>,
        cmds: mpsc::UnboundedReceiver<TermCmd>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), anyhow::Error> {
        let mut p = client::connect(&target, &SessionKind::Terminal).await?;
        self.set(|s| {
            s.fingerprint = p.fingerprint.clone();
            s.name = p.name.clone();
            s.path = Some(p.path);
        });
        let info = loop {
            let pw = if p.need_password {
                self.set(|s| s.phase = TermPhase::Password);
                repaint();
                match passwords.recv().await {
                    Some(pw) => Some(pw),
                    None => return Ok(()),
                }
            } else {
                self.set(|s| s.phase = TermPhase::Approval);
                repaint();
                None
            };
            let waiting = || {
                self.set(|s| s.phase = TermPhase::Approval);
                repaint();
            };
            match p.auth(pw.as_deref(), waiting).await? {
                Auth::Ok(info) => break info,
                Auth::Retry { left } => self.set(|s| s.error = Some(format!("รหัสผ่านไม่ถูกต้อง (เหลือ {left} ครั้ง)"))),
            }
        };
        self.set(|s| {
            s.phase = TermPhase::Connected;
            s.error = None;
            s.recording = info["recording"].as_bool().unwrap_or(false);
        });
        repaint();
        let mut session = p.into_conn();
        let (rows, cols) = self.screen.lock().unwrap().screen().size();
        let end = crate::terminal::run_client(&mut session.conn, cols, rows, cmds, |b| {
            self.screen.lock().unwrap().process(b);
            repaint();
        })
        .await?;
        self.set(|s| {
            s.phase = match end {
                TermEnd::Exited(code) => TermPhase::Exited(code),
                TermEnd::Closed => TermPhase::Closed,
            }
        });
        Ok(())
    }

    fn close_window(&self) {
        let _ = self.cmds.send(TermCmd::Close);
        self.closed.store(true, Ordering::Relaxed);
    }

    fn send(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.cmds.send(TermCmd::Input(bytes.into()));
    }

    pub fn show(&self, ui: &mut Ui) {
        if ui.input(|i| i.viewport().close_requested()) {
            self.close_window();
            return;
        }
        let (phase, error, name, fp, path, recording) = {
            let s = self.state.lock().unwrap();
            (s.phase.clone(), s.error.clone(), s.name.clone(), s.fingerprint.clone(), s.path, s.recording)
        };
        let pal = Palette::of(ui);
        let mut w = self.ui.lock().unwrap();

        egui::Panel::top(egui::Id::unique(("term-bar", self.viewport)))
            .exact_size(40.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(Color32::from_rgb(0x22, 0x23, 0x29)).inner_margin(Margin::symmetric(14, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let white = Color32::from_gray(235);
                    ui.label(RichText::new(">_").font(mono(14.0)).color(Color32::from_gray(150)));
                    ui.label(RichText::new(&name).font(semi(14.0)).color(white));
                    ui.label(RichText::new(format_id(&self.device_id)).font(mono(12.5)).color(Color32::from_gray(150)));
                    if let Some(p) = path {
                        ui.label(RichText::new(p.as_str()).size(12.0).color(Color32::from_gray(130)));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let btn = |t: &str, c: Color32| Button::new(RichText::new(t).size(12.5).color(c)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, Color32::from_gray(80)));
                        match &phase {
                            TermPhase::Connected => {
                                if ui.add(btn("ตัดการเชื่อมต่อ", Color32::from_rgb(0xff, 0x8a, 0x80))).clicked() {
                                    let _ = self.cmds.send(TermCmd::Close);
                                }
                            }
                            TermPhase::Exited(_) | TermPhase::Closed => {
                                if ui.add(btn("เปิดใหม่", white)).clicked() {
                                    self.reconnect.store(true, Ordering::Relaxed);
                                }
                            }
                            _ => {}
                        }
                        if ui.add(btn("คัดลอกข้อความบนจอ", white)).on_hover_text("คัดลอกข้อความทั้งหมดที่แสดงอยู่").clicked() {
                            let text = self.screen.lock().unwrap().screen().contents();
                            ui.ctx().copy_text(text);
                        }
                        if recording {
                            cm_ui::recording_chip(ui, "กำลังบันทึก");
                        }
                        if !fp.is_empty() {
                            ui.label(RichText::new(format!("E2E · {fp}")).font(mono(11.0)).color(Color32::from_gray(120)))
                                .on_hover_text("ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `aegiron-agent info` บนเครื่องนั้น");
                        }
                    });
                });
            });

        egui::CentralPanel::default().frame(Frame::new().fill(TERM_BG).inner_margin(Margin::same(8))).show(ui, |ui| {
            self.terminal_view(ui, &mut w, phase == TermPhase::Connected);
        });

        // Dialog over the terminal while not connected.
        let dialog = match &phase {
            TermPhase::Connected | TermPhase::Exited(_) => None,
            TermPhase::Closed if error.is_none() => None,
            p => Some(p.clone()),
        };
        let status = match &phase {
            TermPhase::Exited(code) => Some(format!("shell จบการทำงาน{}", code.map(|c| format!(" (รหัส {c})")).unwrap_or_default())),
            TermPhase::Closed if error.is_none() => Some("ตัดการเชื่อมต่อแล้ว".to_string()),
            _ => None,
        };
        if let Some(s) = status {
            egui::Area::new(egui::Id::unique(("term-status", self.viewport))).order(Order::Foreground).anchor(Align2::CENTER_BOTTOM, [0.0, -16.0]).show(ui.ctx(), |ui| {
                Frame::new().fill(Color32::from_black_alpha(200)).corner_radius(16).inner_margin(Margin::symmetric(14, 6)).show(ui, |ui| {
                    ui.label(RichText::new(s).size(13.0).color(Color32::WHITE));
                });
            });
        }
        let Some(dphase) = dialog else { return };
        egui::Area::new(egui::Id::unique(("term-dialog", self.viewport))).order(Order::Foreground).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ui.ctx(), |ui| {
            Frame::new()
                .fill(pal.bg)
                .corner_radius(12)
                .stroke(Stroke::new(1.0, pal.line))
                .inner_margin(Margin { left: 24, right: 24, top: 20, bottom: 20 })
                .show(ui, |ui| {
                    ui.set_width(360.0);
                    ui.spacing_mut().item_spacing.y = 10.0;
                    match dphase {
                        TermPhase::Password => {
                            ui.label(RichText::new("ใส่รหัสผ่านเพื่อเปิดเทอร์มินัล").font(semi(17.0)).color(pal.fg));
                            ui.label(RichText::new(format!("รหัสผ่านของ {name} ดูได้ที่หน้าต่าง Aegiron ของเครื่องนั้น")).size(13.0).color(pal.muted));
                            let r = ui.add(TextEdit::singleline(&mut w.password).password(true).hint_text("รหัสผ่าน").font(mono(15.0)).desired_width(f32::INFINITY));
                            if !r.has_focus() && w.password.is_empty() {
                                r.request_focus();
                            }
                            if let Some(e) = &error {
                                ui.label(RichText::new(e).size(12.5).color(pal.danger));
                            }
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let ok = primary(ui, "ตกลง", !w.password.is_empty()).clicked();
                                if (ok || enter) && !w.password.is_empty() {
                                    self.set(|s| s.error = None);
                                    let _ = self.passwords.send(std::mem::take(&mut w.password));
                                    self.set(|s| s.phase = TermPhase::Connecting);
                                }
                                if secondary(ui, "ยกเลิก", None).clicked() {
                                    self.close_window();
                                }
                            });
                        }
                        TermPhase::Closed => {
                            ui.label(RichText::new("เปิดเทอร์มินัลไม่สำเร็จ").font(semi(17.0)).color(pal.danger));
                            ui.label(RichText::new(error.unwrap_or_default()).size(13.5).color(pal.muted));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if primary(ui, "ลองอีกครั้ง", true).clicked() {
                                    self.reconnect.store(true, Ordering::Relaxed);
                                }
                                if secondary(ui, "ปิด", None).clicked() {
                                    self.closed.store(true, Ordering::Relaxed);
                                }
                            });
                        }
                        p => {
                            ui.label(RichText::new("กำลังเปิดเทอร์มินัล…").font(semi(17.0)).color(pal.fg));
                            let text = if p == TermPhase::Approval { "รอผู้ใช้ที่เครื่องปลายทางกดอนุญาต…" } else { "กำลังเชื่อมต่อและแลกเปลี่ยนกุญแจเข้ารหัส…" };
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
                                ui.label(RichText::new(text).size(14.0).color(pal.fg));
                            });
                            if let Some(e) = &error {
                                ui.label(RichText::new(e).size(12.5).color(pal.danger));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if secondary(ui, "ยกเลิก", None).clicked() {
                                    self.close_window();
                                }
                            });
                        }
                    }
                });
        });
    }

    /// The character grid: fits the window, forwards keys while focused.
    fn terminal_view(&self, ui: &mut Ui, w: &mut TermUi, live: bool) {
        let font = FontId::monospace(TERM_FONT);
        let cell = ui.painter().layout_no_wrap("M".into(), font.clone(), TERM_FG).size();
        let (cw, ch) = (cell.x.max(1.0), (cell.y + 1.0).max(1.0));
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let cols = ((rect.width() / cw).floor() as u16).clamp(2, crate::terminal::MAX_COLS);
        let rows = ((rect.height() / ch).floor() as u16).clamp(1, crate::terminal::MAX_ROWS);
        if resp.clicked() || !w.focused_once {
            resp.request_focus();
            w.focused_once = true;
        }

        let mut screen = self.screen.lock().unwrap();
        if screen.screen().size() != (rows, cols) {
            screen.screen_mut().set_size(rows, cols);
            if live {
                let _ = self.cmds.send(TermCmd::Resize(cols, rows));
            }
        }
        if live && ui.ctx().input(|i| i.viewport().focused != Some(false)) {
            let typed = self.forward_keys(ui, screen.screen());
            if typed {
                w.scroll = 0;
            }
        }
        let wheel = if resp.hovered() { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
        if wheel != 0.0 {
            let lines = (wheel / ch).round() as i64;
            w.scroll = (w.scroll as i64 + lines).max(0) as usize;
        }
        screen.screen_mut().set_scrollback(w.scroll);
        w.scroll = screen.screen().scrollback();
        let sc = screen.screen();

        let p = ui.painter_at(rect);
        for r in 0..rows {
            let y = rect.top() + r as f32 * ch;
            for c in 0..cols {
                let Some(cell) = sc.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (color(cell.fgcolor(), TERM_FG, cell.bold()), color(cell.bgcolor(), TERM_BG, false));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                let x = rect.left() + c as f32 * cw;
                let width = if cell.is_wide() { 2.0 * cw } else { cw };
                if bg != TERM_BG {
                    p.rect_filled(Rect::from_min_size(pos2(x, y), vec2(width, ch)), 0.0, bg);
                }
                let text = cell.contents();
                if !text.is_empty() && text != " " {
                    p.text(pos2(x, y), Align2::LEFT_TOP, text, font.clone(), fg);
                }
                if cell.underline() {
                    p.hline(x..=x + width, y + ch - 1.5, Stroke::new(1.0, fg));
                }
            }
        }
        if live && !sc.hide_cursor() && w.scroll == 0 {
            let (cr, cc) = sc.cursor_position();
            let at = Rect::from_min_size(pos2(rect.left() + cc as f32 * cw, rect.top() + cr as f32 * ch), vec2(cw, ch));
            if resp.has_focus() {
                p.rect_filled(at, 0.0, TERM_FG.gamma_multiply(0.55));
            } else {
                p.rect_stroke(at, 0.0, Stroke::new(1.0, TERM_FG), egui::StrokeKind::Inside);
            }
        }
        if w.scroll > 0 {
            p.text(rect.right_top() + vec2(-8.0, 4.0), Align2::RIGHT_TOP, format!("↑ {} บรรทัด", w.scroll), FontId::proportional(12.0), Color32::from_gray(160));
        }
    }

    /// Sends this frame's keyboard input to the shell; true if anything was sent.
    fn forward_keys(&self, ui: &Ui, screen: &vt100::Screen) -> bool {
        let events = ui.input(|i| i.events.clone());
        let app_cursor = screen.application_cursor();
        let bracketed = screen.bracketed_paste();
        let mut sent = false;
        for ev in events {
            let bytes: Option<Vec<u8>> = match ev {
                egui::Event::Text(t) => Some(t.into_bytes()),
                egui::Event::Paste(t) => Some(match bracketed {
                    true => format!("\x1b[200~{t}\x1b[201~").into_bytes(),
                    false => t.replace('\n', "\r").into_bytes(),
                }),
                // Windows/Linux turn Ctrl+C / Ctrl+X into copy/cut: in a terminal they are keys.
                egui::Event::Copy if !cfg!(target_os = "macos") => Some(vec![0x03]),
                egui::Event::Cut if !cfg!(target_os = "macos") => Some(vec![0x18]),
                egui::Event::Key { key, pressed: true, modifiers, .. } => key_bytes(key, modifiers, app_cursor),
                _ => None,
            };
            if let Some(b) = bytes.filter(|b| !b.is_empty()) {
                self.send(b);
                sent = true;
            }
        }
        sent
    }
}

/// Bytes a terminal sends for a key that produces no text (xterm conventions).
fn key_bytes(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key as K;
    // Ctrl+letter (the Control key on every OS; ⌘ stays with the app on macOS).
    if m.ctrl && !m.alt {
        let name = key.name();
        if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
            return Some(vec![name.as_bytes()[0].to_ascii_uppercase() - b'A' + 1]);
        }
        match key {
            K::OpenBracket => return Some(vec![0x1b]),
            K::Backslash => return Some(vec![0x1c]),
            K::CloseBracket => return Some(vec![0x1d]),
            K::Space => return Some(vec![0x00]),
            _ => {}
        }
    }
    let arrow = |c: char| Some(if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") }.into_bytes());
    let s: &str = match key {
        K::Enter => "\r",
        K::Backspace => "\x7f",
        K::Tab if m.shift => "\x1b[Z",
        K::Tab => "\t",
        K::Escape => "\x1b",
        K::ArrowUp => return arrow('A'),
        K::ArrowDown => return arrow('B'),
        K::ArrowRight => return arrow('C'),
        K::ArrowLeft => return arrow('D'),
        K::Home => return arrow('H'),
        K::End => return arrow('F'),
        K::PageUp => "\x1b[5~",
        K::PageDown => "\x1b[6~",
        K::Delete => "\x1b[3~",
        K::Insert => "\x1b[2~",
        K::F1 => "\x1bOP",
        K::F2 => "\x1bOQ",
        K::F3 => "\x1bOR",
        K::F4 => "\x1bOS",
        K::F5 => "\x1b[15~",
        K::F6 => "\x1b[17~",
        K::F7 => "\x1b[18~",
        K::F8 => "\x1b[19~",
        K::F9 => "\x1b[20~",
        K::F10 => "\x1b[21~",
        K::F11 => "\x1b[23~",
        K::F12 => "\x1b[24~",
        _ => return None,
    };
    Some(s.as_bytes().to_vec())
}

/// xterm's 256 colours (16 system, 6×6×6 cube, 24 greys) and true colour.
fn color(c: vt100::Color, default: Color32, bold: bool) -> Color32 {
    const SYS: [(u8, u8, u8); 16] = [
        (0x1d, 0x1f, 0x21),
        (0xcc, 0x34, 0x2b),
        (0x19, 0x8f, 0x4c),
        (0xc4, 0x9a, 0x00),
        (0x3b, 0x78, 0xff),
        (0xa3, 0x5e, 0xc4),
        (0x0f, 0x9e, 0xb5),
        (0xc5, 0xc8, 0xc6),
        (0x6b, 0x6e, 0x76),
        (0xff, 0x6b, 0x60),
        (0x4e, 0xd4, 0x7f),
        (0xf5, 0xd2, 0x4b),
        (0x76, 0xa3, 0xff),
        (0xd5, 0x8f, 0xf2),
        (0x4f, 0xd6, 0xe8),
        (0xff, 0xff, 0xff),
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => {
            let i = if bold && i < 8 { i + 8 } else { i };
            match i {
                0..=15 => {
                    let (r, g, b) = SYS[i as usize];
                    Color32::from_rgb(r, g, b)
                }
                16..=231 => {
                    let n = i - 16;
                    let v = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
                    Color32::from_rgb(v(n / 36), v((n / 6) % 6), v(n % 6))
                }
                _ => {
                    let g = 8 + (i - 232) * 10;
                    Color32::from_gray(g)
                }
            }
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

// ---------------------------------------------------------------------------------------------
// Tunnels

/// The "TCP tunnel" dialog.
pub(super) struct TunnelForm {
    id: String,
    name: String,
    local: String,
    host: String,
    port: String,
    password: String,
    /// Windows: start Remote Desktop (mstsc) on the local port once the tunnel is open.
    rdp: bool,
    busy: bool,
    error: Arc<Mutex<Option<String>>>,
    done: Arc<AtomicBool>,
}

impl TunnelForm {
    pub(super) fn new(id: &str, name: &str) -> Self {
        TunnelForm {
            id: id.to_string(),
            name: name.to_string(),
            local: "13389".into(),
            host: "localhost".into(),
            port: "3389".into(),
            password: String::new(),
            rdp: cfg!(windows),
            busy: false,
            error: Arc::default(),
            done: Arc::default(),
        }
    }
}

static TUNNEL_SEQ: AtomicU64 = AtomicU64::new(1);

impl App {
    /// Opens a terminal window for `id` (signing in first if needed).
    pub(super) fn open_terminal(&mut self, ctx: &egui::Context, id: &str, name: &str) {
        let Some(agent) = self.agent.clone() else { return };
        if agent.config.lock().unwrap().id == id {
            self.flash("นี่คือ ID ของเครื่องนี้เอง");
            return;
        }
        let target = match client::Target::from_config(&agent.config.lock().unwrap(), id) {
            Ok(t) => t,
            Err(_) => {
                self.flash("เข้าสู่ระบบก่อน เพื่อเปิดเทอร์มินัลของเครื่องอื่น");
                self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
                self.login_modal = true;
                return;
            }
        };
        let viewport = egui::ViewportId::from_hash_of(("terminal", id, Instant::now()));
        let rctx = ctx.clone();
        self.terminals.push(TerminalWindow::open(target, name.to_string(), viewport, Arc::new(move || rctx.request_repaint_of(viewport))));
    }

    pub(super) fn show_terminals(&mut self, ctx: &egui::Context) {
        for i in 0..self.terminals.len() {
            if self.terminals[i].reconnect.swap(false, Ordering::Relaxed) {
                let w = self.terminals.remove(i);
                let name = w.state.lock().unwrap().name.clone();
                self.open_terminal(ctx, &w.device_id, &name);
                break;
            }
        }
        self.terminals.retain(|w| !w.closed.load(Ordering::Relaxed));
        for w in &self.terminals {
            let title = format!("เทอร์มินัล — {} — Aegiron", w.state.lock().unwrap().name);
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(TERM_SIZE)
                .with_min_inner_size([420.0, 260.0])
                .with_transparent(false)
                .with_icon(Arc::new(egui::IconData { rgba: icon_rgba(64), width: 64, height: 64 }));
            let win = w.clone();
            ctx.show_viewport_deferred(w.viewport, builder, move |ui, _| win.show(ui));
        }
    }

    pub(super) fn tunnel_dialog(&mut self, ui: &mut Ui) {
        let Some(form) = self.tunnel_form.as_mut() else { return };
        if form.done.load(Ordering::Relaxed) {
            self.tunnel_form = None;
            return;
        }
        let pal = Palette::of(ui);
        let (mut submit, mut close) = (false, false);
        let error = form.error.lock().unwrap().clone();
        if error.is_some() {
            form.busy = false;
        }
        egui::Modal::new(egui::Id::unique("tcp-tunnel")).backdrop_color(Color32::from_black_alpha(110)).show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new("TCP tunnel").font(semi(18.0)).color(pal.fg));
            ui.label(RichText::new(format!("ส่งต่อพอร์ตบนเครื่องนี้ไปยังพอร์ตที่ {} มองเห็น ({})", form.name, format_id(&form.id))).size(12.5).color(pal.muted));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (label, port) in [("RDP", 3389u16), ("SSH", 22), ("VNC", 5900), ("HTTP", 80)] {
                    if ui.add(Button::new(RichText::new(label).size(12.5)).corner_radius(6)).clicked() {
                        form.port = port.to_string();
                        form.local = (port as u32 + 10000).min(65535).to_string();
                        form.rdp = cfg!(windows) && port == 3389;
                    }
                }
            });
            let field = |ui: &mut Ui, label: &str, v: &mut String, w: f32| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(12.5).color(pal.muted));
                    ui.add(TextEdit::singleline(v).font(mono(14.0)).desired_width(w));
                });
            };
            ui.horizontal(|ui| {
                field(ui, "พอร์ตบนเครื่องนี้", &mut form.local, 90.0);
                ui.label(RichText::new("→").size(16.0).color(pal.muted));
                field(ui, "โฮสต์ปลายทาง", &mut form.host, 170.0);
                field(ui, "พอร์ต", &mut form.port, 70.0);
            });
            ui.label(
                RichText::new("โฮสต์ตามที่เครื่องปลายทางมองเห็น: localhost คือเครื่องนั้นเอง เครื่องอื่นในวงแลนได้เฉพาะเมื่อเครื่องนั้นอนุญาต")
                    .size(12.0)
                    .color(pal.muted),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new("รหัสผ่านของเครื่องปลายทาง").size(12.5).color(pal.muted));
                ui.add(TextEdit::singleline(&mut form.password).password(true).hint_text("เว้นว่างถ้าเครื่องนั้นใช้การกดอนุญาต").font(mono(14.0)).desired_width(f32::INFINITY));
            });
            if cfg!(windows) {
                ui.checkbox(&mut form.rdp, "เปิด Remote Desktop (mstsc) เมื่อ tunnel พร้อม");
            }
            if let Some(e) = &error {
                ui.label(RichText::new(e).size(12.5).color(pal.danger));
            }
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                submit = primary(ui, if form.busy { "กำลังเปิด…" } else { "เปิด tunnel" }, !form.busy).clicked();
                close = secondary(ui, "ยกเลิก", None).clicked();
            });
        });
        if close {
            self.tunnel_form = None;
            return;
        }
        if !submit {
            return;
        }
        let Some(agent) = self.agent.clone() else { return };
        let form = self.tunnel_form.as_mut().unwrap();
        let parsed = (form.local.trim().parse::<u16>().ok().filter(|p| *p > 0), form.port.trim().parse::<u16>().ok().filter(|p| *p > 0));
        let (Some(local), Some(port)) = parsed else {
            *form.error.lock().unwrap() = Some("พอร์ตต้องเป็นตัวเลข 1–65535".into());
            return;
        };
        let host = form.host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
        if let Err(e) = crate::remote::parse_target(&crate::remote::join_target(&host, port)) {
            *form.error.lock().unwrap() = Some(format!("โฮสต์ไม่ถูกต้อง: {e}"));
            return;
        }
        let target = match client::Target::from_config(&agent.config.lock().unwrap(), &form.id) {
            Ok(t) => t,
            Err(e) => {
                *form.error.lock().unwrap() = Some(format!("{e}"));
                return;
            }
        };
        form.busy = true;
        *form.error.lock().unwrap() = None;
        let (name, password, rdp) = (form.name.clone(), Some(form.password.clone()).filter(|p| !p.is_empty()), form.rdp);
        let (error, done, list) = (form.error.clone(), form.done.clone(), self.tunnels.clone());
        let ctx = ui.ctx().clone();
        crate::runtime().spawn(async move {
            let repaint: Arc<dyn Fn() + Send + Sync> = { let c = ctx.clone(); Arc::new(move || c.request_repaint()) };
            match tunnel::open(target, name, local, &host, port, password, repaint).await {
                Ok((t, _task)) => {
                    list.lock().unwrap().push((TUNNEL_SEQ.fetch_add(1, Ordering::Relaxed), t.clone()));
                    done.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                    if rdp {
                        // Remote Desktop once the remote side accepted.
                        for _ in 0..600 {
                            match t.phase() {
                                TunnelPhase::Open => {
                                    let _ = std::process::Command::new("mstsc").arg(format!("/v:127.0.0.1:{local}")).spawn();
                                    break;
                                }
                                TunnelPhase::Closed => break,
                                _ => tokio::time::sleep(Duration::from_millis(100)).await,
                            }
                        }
                    }
                }
                Err(e) => {
                    *error.lock().unwrap() = Some(format!("{e:#}"));
                    ctx.request_repaint();
                }
            }
        });
    }

    /// Open tunnels, bottom right of the main window, while there are any.
    pub(super) fn tunnels_panel(&mut self, ui: &mut Ui) {
        let tunnels = self.tunnels.lock().unwrap().clone();
        if tunnels.is_empty() {
            return;
        }
        let pal = Palette::of(ui);
        let mut remove = None;
        egui::Window::new(RichText::new(format!("TCP tunnel ({})", tunnels.len())).font(semi(14.0)))
            .id(egui::Id::unique("tunnels-panel"))
            .anchor(Align2::RIGHT_BOTTOM, [-16.0, -44.0])
            .resizable(false)
            .collapsible(true)
            .default_width(360.0)
            .show(ui.ctx(), |ui| {
                for (i, (key, t)) in tunnels.iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let phase = t.phase();
                    let (dot, state) = match phase {
                        TunnelPhase::Connecting => (pal.warn, "กำลังเชื่อมต่อ".to_string()),
                        TunnelPhase::Approval => (pal.warn, "รอเครื่องปลายทางกดอนุญาต".to_string()),
                        TunnelPhase::Open => {
                            let c = &t.counters;
                            (
                                pal.ok,
                                format!(
                                    "เปิดอยู่ · {} การเชื่อมต่อ · ↑ {} ↓ {}",
                                    c.active.load(Ordering::Relaxed),
                                    human_bytes(c.up.load(Ordering::Relaxed)),
                                    human_bytes(c.down.load(Ordering::Relaxed))
                                ),
                            )
                        }
                        TunnelPhase::Closed => (pal.muted, t.error.lock().unwrap().clone().unwrap_or_else(|| "ปิดแล้ว".into())),
                    };
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, dot);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(format!("{} → {}", t.local, t.target)).font(mono(13.0)).color(pal.fg));
                            ui.label(RichText::new(format!("บน {} ({})", t.name, format_id(&t.device_id))).size(12.0).color(pal.muted));
                            ui.label(RichText::new(state).size(12.0).color(if phase == TunnelPhase::Closed && t.error.lock().unwrap().is_some() { pal.danger } else { pal.muted }));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if phase == TunnelPhase::Closed {
                                if ui.small_button("ลบ").clicked() {
                                    remove = Some(*key);
                                }
                            } else {
                                if ui.small_button("หยุด").clicked() {
                                    t.stop();
                                }
                                if ui.small_button("คัดลอก").on_hover_text("คัดลอกที่อยู่บนเครื่องนี้").clicked() {
                                    ui.ctx().copy_text(t.local.to_string());
                                }
                            }
                        });
                    });
                }
            });
        if let Some(k) = remove {
            self.tunnels.lock().unwrap().retain(|(key, _)| *key != k);
        }
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.2} GB", n as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let ctrl = egui::Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(key_bytes(egui::Key::C, ctrl, false), Some(vec![3]));
        assert_eq!(key_bytes(egui::Key::D, ctrl, false), Some(vec![4]));
        assert_eq!(key_bytes(egui::Key::Enter, Default::default(), false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(egui::Key::ArrowUp, Default::default(), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(egui::Key::ArrowUp, Default::default(), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(egui::Key::A, Default::default(), false), None); // comes as Text
    }

    #[test]
    fn colors() {
        assert_eq!(color(vt100::Color::Default, TERM_FG, false), TERM_FG);
        assert_eq!(color(vt100::Color::Idx(1), TERM_FG, true), color(vt100::Color::Idx(9), TERM_FG, false));
        assert_eq!(color(vt100::Color::Idx(16), TERM_FG, false), Color32::from_rgb(0, 0, 0));
        assert_eq!(color(vt100::Color::Idx(231), TERM_FG, false), Color32::from_rgb(255, 255, 255));
        assert_eq!(color(vt100::Color::Rgb(1, 2, 3), TERM_FG, false), Color32::from_rgb(1, 2, 3));
    }

    /// The renderer's model: UTF-8 split across reads, Thai combining marks stay in their cell.
    #[test]
    fn vt_screen_handles_split_utf8() {
        let mut p = vt100::Parser::new(5, 20, 0);
        let s = "ok สวัสดี\r\n".as_bytes();
        p.process(&s[..5]);
        p.process(&s[5..]);
        assert!(p.screen().contents().starts_with("ok สวัสดี"));
    }
}
