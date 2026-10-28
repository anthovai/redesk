//! Remote-control window (one native window per connection, like RustDesk): the remote screen
//! fills the window, with a collapsible floating toolbar centered on the top edge, an optional
//! quality monitor in the top-left corner and dialog-style overlays while connecting.

use super::*;

use egui::{Order, Popup, RectAlign, Shadow};

use crate::viewer::{Info, Phase, TransferStatus};

const QUALITY: [(&str, u8, u32); 3] = [("คมชัด", 85, 2560), ("สมดุล", 70, 1920), ("ประหยัดเน็ต", 45, 1280)];

/// Background around the remote screen (letterboxing), the same in light and dark mode.
const SCREEN_BG: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x21);
/// How long the unpinned toolbar stays open after the pointer leaves it (seconds).
const BAR_LINGER: f64 = 0.6;
/// The toolbar shows for this long after connecting, then collapses unless pinned.
const BAR_INTRO: f64 = 2.5;
const BAR_BUTTON: f32 = 30.0;
/// Whiteboard pen colors (name, value) and width (fraction of the remote screen height).
const PEN_COLORS: [(&str, Color32); 4] = [
    ("แดง", Color32::from_rgb(0xff, 0x3b, 0x30)),
    ("เหลือง", Color32::from_rgb(0xff, 0xd6, 0x0a)),
    ("เขียว", Color32::from_rgb(0x30, 0xd1, 0x58)),
    ("ฟ้า", Color32::from_rgb(0x0a, 0x84, 0xff)),
];
const PEN_WIDTH: f32 = 0.004;

/// A stroke drawn in this window (normalised to the remote screen), mirrored on the remote.
struct LocalStroke {
    color: Color32,
    pts: Vec<Pos2>,
}

/// Whiteboard state of one remote window.
#[derive(Default)]
struct Pen {
    /// Drawing instead of controlling.
    on: bool,
    color: usize,
    strokes: Vec<LocalStroke>,
    next_id: u32,
    /// Points of the active stroke not yet sent.
    unsent: Vec<f32>,
}

#[derive(Default)]
struct WinUi {
    texture: Option<egui::TextureHandle>,
    password: String,
    quality: usize,
    fullscreen: bool,
    toast: Option<(String, Instant)>,
    /// Toolbar stays open.
    pinned: bool,
    /// Last time (egui seconds) something kept the unpinned toolbar open.
    bar_seen: Option<f64>,
    /// Toolbar rectangle from the previous frame (hover tracking).
    bar_rect: Option<Rect>,
    show_quality: bool,
    /// Original size (1:1 pixels) instead of fit-to-window.
    original: bool,
    /// Where the 1:1 view is panned to (0..1 on each axis, follows the pointer).
    pan: egui::Vec2,
    /// Send ⌘/Ctrl as-is instead of mapping them for the remote OS.
    raw_keys: bool,
    /// Mouse buttons pressed on the remote screen (bit per button), so releases pair up.
    held: u8,
    demo_menu: Option<String>,
    demo_done: bool,
    pen: Pen,
}

pub struct ViewerWindow {
    pub viewer: Arc<Viewer>,
    pub viewport: egui::ViewportId,
    pub device_id: String,
    ui: Mutex<WinUi>,
    pub closed: AtomicBool,
    pub reconnect: AtomicBool,
}

/// What the toolbar and quality monitor need from the connection state for one frame.
struct Frame1<'a> {
    name: &'a str,
    id: &'a str,
    fp: &'a str,
    info: Option<&'a Info>,
    fps: u32,
    kbps: u32,
    codec: &'a str,
    fb: (usize, usize),
    path: Option<crate::direct::Path>,
}

impl ViewerWindow {
    pub fn new(viewer: Arc<Viewer>, device_id: String, viewport: egui::ViewportId) -> Arc<Self> {
        Arc::new(Self {
            viewer,
            viewport,
            device_id,
            ui: Mutex::new(WinUi { quality: 1, pan: vec2(0.5, 0.5), ..Default::default() }),
            closed: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
        })
    }

    fn flash(w: &mut WinUi, msg: impl Into<String>) {
        w.toast = Some((msg.into(), Instant::now()));
    }

    fn close_window(&self) {
        self.viewer.close();
        self.closed.store(true, Ordering::Relaxed);
    }

    pub fn show(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if ui.input(|i| i.viewport().close_requested()) {
            self.close_window();
            return;
        }
        let mut guard = self.ui.lock().unwrap();
        let w = &mut *guard;
        self.demo_state(&ctx, w);

        // Pull new screen regions into the GPU texture and pick up state for this frame.
        let (phase, error, name, fp, info, fps, kbps, clip, transfers, fb_size, cursor, codec, path) = {
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
                st.cursor,
                st.codec.clone(),
                st.path,
            )
        };
        let connected = phase == Phase::Connected;
        let remote_mac = info.as_ref().is_some_and(|i| i.os == "macos");
        self.debug_hooks(&phase);

        if let Some(text) = clip
            && arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok()
        {
            Self::flash(w, "ได้รับคลิปบอร์ดจากเครื่องปลายทาง");
        }
        let dropped: Vec<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            if connected && info.as_ref().is_some_and(|i| i.files) {
                let n = dropped.len();
                for p in dropped {
                    self.viewer.upload(p);
                }
                Self::flash(w, format!("กำลังส่ง {n} ไฟล์ไปที่ Downloads/Aegiron ของเครื่องปลายทาง"));
            } else {
                Self::flash(w, "เครื่องปลายทางไม่อนุญาตให้รับไฟล์");
            }
        }

        // Remote screen: the whole window.
        egui::CentralPanel::default().frame(Frame::new().fill(SCREEN_BG)).show(ui, |ui| {
            let area = ui.max_rect();
            if let (Some(tex), (fw, fh)) = (&w.texture, fb_size) {
                let rect = if w.original {
                    // 1:1 pixels; when larger than the window the view pans with the pointer.
                    let size = vec2(fw as f32, fh as f32) / ctx.pixels_per_point();
                    if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p)) {
                        w.pan = vec2((p.x - area.left()) / area.width(), (p.y - area.top()) / area.height());
                    }
                    let mut min = area.center() - size / 2.0;
                    if size.x > area.width() {
                        min.x = area.left() - w.pan.x * (size.x - area.width());
                    }
                    if size.y > area.height() {
                        min.y = area.top() - w.pan.y * (size.y - area.height());
                    }
                    Rect::from_min_size(min.round(), size)
                } else {
                    let scale = (area.width() / fw as f32).min(area.height() / fh as f32);
                    Rect::from_center_size(area.center(), vec2(fw as f32 * scale, fh as f32 * scale))
                };
                ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                let resp = ui.allocate_rect(rect.intersect(area), Sense::click_and_drag());
                let drawing = connected && w.pen.on && info.as_ref().is_some_and(|i| i.whiteboard);
                if drawing {
                    self.draw(&mut w.pen, &resp, rect);
                    w.held = 0;
                } else if connected && info.as_ref().is_some_and(|i| i.input) && ui.input(|i| i.viewport().focused != Some(false)) {
                    let keys = KeyMap { remote_mac, raw: w.raw_keys };
                    forward_input(&self.viewer, ui, rect, area, keys, &mut w.held);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                    }
                } else {
                    w.held = 0;
                }
                paint_strokes(ui.painter(), &w.pen.strokes, rect);
                // The remote pointer (captures do not include it), unless ours is on top of it.
                if let Some((cx, cy)) = cursor.filter(|_| !resp.hovered()) {
                    paint_pointer(ui.painter(), pos2(rect.left() + cx * rect.width(), rect.top() + cy * rect.height()));
                }
            } else if connected {
                ui.centered_and_justified(|ui| ui.label(RichText::new("กำลังรอภาพหน้าจอ…").color(Color32::from_gray(170))));
            }
            if !connected {
                // Dim whatever is behind the dialog, as RustDesk does.
                ui.painter().rect_filled(area, 0.0, Color32::from_black_alpha(if w.texture.is_some() { 120 } else { 40 }));
            }
        });

        if connected {
            let f = Frame1 { name: &name, id: &self.device_id, fp: &fp, info: info.as_ref(), fps, kbps, codec: &codec, fb: fb_size, path };
            if w.show_quality {
                self.quality_monitor(&ctx, &f);
            }
            if f.info.is_some_and(|i| i.recording) {
                self.recording_badge(&ctx);
            }
            self.toolbar(&ctx, w, &f);
        } else {
            self.dialog(&ctx, w, &phase, error.as_deref(), &name);
        }
        self.transfers(&ctx, &transfers);

        if let Some((msg, at)) = &w.toast {
            if at.elapsed() > Duration::from_secs(3) {
                w.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::unique(("vtoast", self.viewport)))
                    .order(Order::Tooltip)
                    .interactable(false)
                    .anchor(Align2::CENTER_BOTTOM, [0.0, -28.0])
                    .show(&ctx, |ui| {
                        Frame::new().fill(Color32::from_black_alpha(205)).corner_radius(8).inner_margin(Margin::symmetric(16, 9)).show(ui, |ui| {
                            ui.label(RichText::new(msg).size(13.5).color(Color32::WHITE));
                        });
                    });
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
        if !connected || !transfers.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Floating toolbar

    fn toolbar(&self, ctx: &egui::Context, w: &mut WinUi, f: &Frame1) {
        let now = ctx.input(|i| i.time);
        let hover = ctx.input(|i| i.pointer.hover_pos());
        let seen = *w.bar_seen.get_or_insert(now + BAR_INTRO);
        let over_bar = hover.is_some_and(|p| w.bar_rect.is_some_and(|r| r.expand(14.0).contains(p)));
        let was_open = w.pinned || now - seen < BAR_LINGER;
        if w.pinned || Popup::is_any_open(ctx) || (was_open && over_bar) {
            w.bar_seen = Some(seen.max(now));
        }
        let open = w.pinned || now - w.bar_seen.unwrap_or(now) < BAR_LINGER;
        let t = ctx.animate_bool_with_time(egui::Id::unique(("bar-open", self.viewport)), open, 0.14);
        if open && !w.pinned {
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        // Collapsed: a small tab with a chevron; hovering or clicking it opens the toolbar.
        if t < 1.0 {
            let resp = egui::Area::new(egui::Id::unique(("bar-handle", self.viewport)))
                .order(Order::Foreground)
                .anchor(Align2::CENTER_TOP, [0.0, 0.0])
                .show(ctx, |ui| {
                    let pal = Palette::of(ui);
                    let (rect, resp) = ui.allocate_exact_size(vec2(54.0, 18.0), Sense::click());
                    let a = 1.0 - t;
                    let fill = if resp.hovered() { pal.accent } else { bar_fill(ui) };
                    let corners = CornerRadius { nw: 0, ne: 0, sw: 9, se: 9 };
                    ui.painter().rect_filled(rect.translate(vec2(0.0, 1.5)), corners, Color32::from_black_alpha((60.0 * a) as u8));
                    ui.painter().rect_filled(rect, corners, fill.gamma_multiply(a));
                    let fg = if resp.hovered() { Color32::WHITE } else { pal.fg };
                    glyph(ui, Rect::from_center_size(rect.center(), vec2(12.0, 12.0)), Glyph::ChevronDown, fg.gamma_multiply(a));
                    resp.on_hover_text("แสดงแถบเครื่องมือ").on_hover_cursor(egui::CursorIcon::PointingHand)
                })
                .inner;
            if resp.hovered() || resp.clicked() {
                w.bar_seen = Some(now);
            }
        }
        if t <= 0.0 {
            w.bar_rect = None;
            return;
        }

        let height = BAR_BUTTON + 12.0;
        let y = 6.0 - (1.0 - t) * (height + 8.0);
        let resp = egui::Area::new(egui::Id::unique(("bar", self.viewport)))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, [0.0, y])
            .constrain(false)
            .show(ctx, |ui| {
                let pal = Palette::of(ui);
                Frame::new()
                    .fill(bar_fill(ui))
                    .corner_radius(10)
                    .stroke(Stroke::new(1.0, if ui.visuals().dark_mode { pal.line } else { Color32::from_black_alpha(18) }))
                    .shadow(Shadow { offset: [0, 3], blur: 14, spread: 0, color: Color32::from_black_alpha(70) })
                    .inner_margin(Margin::same(6))
                    .multiply_with_opacity(t)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(5.0, 0.0);
                        ui.horizontal(|ui| self.toolbar_buttons(ui, w, f));
                    });
            });
        w.bar_rect = Some(resp.response.rect);
    }

    fn toolbar_buttons(&self, ui: &mut Ui, w: &mut WinUi, f: &Frame1) {
        let pal = Palette::of(ui);
        let ctx = ui.ctx().clone();
        let input = f.info.is_some_and(|i| i.input);

        // Pin
        let pin_tip = if w.pinned { "เลิกปักหมุดแถบเครื่องมือ" } else { "ปักหมุดแถบเครื่องมือ" };
        if bar_button(ui, Glyph::Pin, pin_tip, if w.pinned { Tone::Accent } else { Tone::Neutral }, false).clicked() {
            w.pinned = !w.pinned;
        }
        bar_divider(ui);

        // Monitor: the current display's number; a menu when there are several.
        if let Some(info) = f.info {
            let n = info.displays.iter().position(|d| d.index == info.display).map(|p| p + 1).unwrap_or(1);
            let many = info.displays.len() > 1;
            let tip = if many { "เลือกหน้าจอ" } else { "หน้าจอของเครื่องปลายทาง" };
            let id = egui::Id::unique(("menu-monitor", self.viewport));
            let open = Popup::is_id_open(&ctx, id);
            let resp = bar_button(ui, Glyph::Display(n), tip, Tone::Accent, open);
            if many {
                self.demo_open(&ctx, w, "monitor", id);
                menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    menu_header(ui, "หน้าจอของเครื่องปลายทาง");
                    for (k, d) in info.displays.iter().enumerate() {
                        let label = if d.primary { format!("{} (หลัก)", d.name) } else { d.name.clone() };
                        if menu_row(ui, Lead::Check(d.index == info.display), &label, &format!("{}", k + 1), true).clicked() {
                            self.viewer.send(serde_json::json!({"t": "cfg", "display": d.index}));
                            if let Some(info) = self.viewer.state.lock().unwrap().info.as_mut() {
                                info.display = d.index;
                            }
                        }
                    }
                });
            }
        }

        // Actions
        let id = egui::Id::unique(("menu-actions", self.viewport));
        let resp = bar_button(ui, Glyph::Bolt, "การทำงาน", Tone::Accent, Popup::is_id_open(&ctx, id));
        self.demo_open(&ctx, w, "actions", id);
        menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
            identity_header(ui, f);
            menu_separator(ui);
            if menu_row(ui, Lead::Glyph(Glyph::Keyboard), "ส่ง Ctrl + Alt + Del", "", input).clicked() {
                send_combo(&self.viewer, &[("ControlLeft", "Control"), ("AltLeft", "Alt")], ("Delete", "Delete"));
            }
            if menu_row(ui, Lead::Glyph(Glyph::Lock), "ล็อกหน้าจอเครื่องปลายทาง", "", input).clicked() {
                if f.info.is_some_and(|i| i.os == "macos") {
                    send_combo(&self.viewer, &[("ControlLeft", "Control"), ("MetaLeft", "Meta")], ("KeyQ", "q"));
                } else {
                    send_combo(&self.viewer, &[("MetaLeft", "Meta")], ("KeyL", "l"));
                }
                Self::flash(w, "ส่งคำสั่งล็อกหน้าจอแล้ว");
            }
            if menu_row(ui, Lead::Glyph(Glyph::Refresh), "รีเฟรชภาพ", "", true).clicked() {
                self.viewer.send(serde_json::json!({"t": "refresh"}));
            }
            menu_separator(ui);
            let files = f.info.is_some_and(|i| i.files);
            if menu_row(ui, Lead::Glyph(Glyph::Folder), "ส่งไฟล์", "", files).on_hover_text("ลากไฟล์มาวางบนหน้าต่างนี้").clicked() {
                Self::flash(w, "ลากไฟล์มาวางบนหน้าต่างนี้ ไฟล์จะไปอยู่ที่ Downloads/Aegiron ของเครื่องปลายทาง");
            }
            let clip = f.info.is_some_and(|i| i.clipboard);
            if menu_row(ui, Lead::Glyph(Glyph::Clipboard), "ส่งคลิปบอร์ดไปเครื่องปลายทาง", "", clip).clicked() {
                match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    Ok(t) => {
                        self.viewer.send(serde_json::json!({"t": "clip", "s": t}));
                        Self::flash(w, "ส่งคลิปบอร์ดแล้ว");
                    }
                    Err(_) => Self::flash(w, "คลิปบอร์ดว่าง"),
                }
            }
        });

        // Display settings
        let id = egui::Id::unique(("menu-display", self.viewport));
        let resp = bar_button(ui, Glyph::Tune, "การแสดงผล", Tone::Accent, Popup::is_id_open(&ctx, id));
        self.demo_open(&ctx, w, "display", id);
        menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
            menu_header(ui, "คุณภาพของภาพ");
            for q in 0..QUALITY.len() {
                if menu_row(ui, Lead::Check(w.quality == q), QUALITY[q].0, "", true).clicked() && w.quality != q {
                    w.quality = q;
                    self.viewer.send(serde_json::json!({"t": "cfg", "quality": QUALITY[q].1, "max_width": QUALITY[q].2}));
                }
            }
            menu_separator(ui);
            menu_header(ui, "ขนาดภาพ");
            if menu_row(ui, Lead::Check(!w.original), "พอดีหน้าต่าง", "", true).clicked() {
                w.original = false;
            }
            if menu_row(ui, Lead::Check(w.original), "ขนาดจริง", "1:1", true).clicked() {
                w.original = true;
            }
            menu_separator(ui);
            if menu_row(ui, Lead::Tick(w.show_quality), "แสดงข้อมูลคุณภาพการเชื่อมต่อ", "", true).clicked() {
                w.show_quality = !w.show_quality;
            }
            menu_separator(ui);
            menu_footer(ui, &format!("Codec ที่ใช้: {}", codec_label(f.codec)));
        });

        // Keyboard: how ⌘/Ctrl reach the remote OS.
        if input {
            let id = egui::Id::unique(("menu-keyboard", self.viewport));
            let resp = bar_button(ui, Glyph::Keyboard, "แป้นพิมพ์", Tone::Accent, Popup::is_id_open(&ctx, id));
            self.demo_open(&ctx, w, "keyboard", id);
            menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                let remote_mac = f.info.is_some_and(|i| i.os == "macos");
                menu_header(ui, "โหมดแป้นพิมพ์");
                let hint = if remote_mac { "" } else { "⌘ → Ctrl" };
                if menu_row(ui, Lead::Check(!w.raw_keys), "แมปตามระบบปลายทาง", hint, true).clicked() {
                    w.raw_keys = false;
                }
                if menu_row(ui, Lead::Check(w.raw_keys), "ส่งปุ่มตามจริง", "", true).clicked() {
                    w.raw_keys = true;
                }
                menu_separator(ui);
                if menu_row(ui, Lead::Glyph(Glyph::Keyboard), "ส่ง Ctrl + Alt + Del", "", true).clicked() {
                    send_combo(&self.viewer, &[("ControlLeft", "Control"), ("AltLeft", "Alt")], ("Delete", "Delete"));
                }
                menu_separator(ui);
                menu_footer(ui, &format!("เครื่องปลายทาง: {}", os_label(f.info.map(|i| i.os.as_str()).unwrap_or(""))));
            });
        }

        // Whiteboard: draw on the remote screen (only when the remote machine shows annotations).
        if f.info.is_some_and(|i| i.whiteboard) {
            let id = egui::Id::unique(("menu-pen", self.viewport));
            let tone = if w.pen.on { Tone::Danger } else { Tone::Accent };
            let resp = bar_button(ui, Glyph::Pen, "วาดบนหน้าจอ", tone, Popup::is_id_open(&ctx, id));
            self.demo_open(&ctx, w, "pen", id);
            menu(&resp, id, pal).show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                menu_header(ui, "วาดบนหน้าจอปลายทาง");
                if menu_row(ui, Lead::Tick(w.pen.on), "ปากกา", "เมาส์ใช้วาดแทนการควบคุม", true).clicked() {
                    w.pen.on = !w.pen.on;
                }
                menu_separator(ui);
                menu_header(ui, "สี");
                for (k, (name, color)) in PEN_COLORS.iter().enumerate() {
                    let r = menu_row(ui, Lead::Check(w.pen.color == k), name, "", true);
                    ui.painter().circle_filled(pos2(r.rect.right() - 20.0, r.rect.center().y), 6.0, *color);
                    if r.clicked() {
                        w.pen.color = k;
                        w.pen.on = true;
                    }
                }
                menu_separator(ui);
                if menu_row(ui, Lead::Glyph(Glyph::Close), "ล้างทั้งหมด", "", !w.pen.strokes.is_empty()).clicked() {
                    w.pen.strokes.clear();
                    self.viewer.send(serde_json::json!({"t": "wb", "op": "clear"}));
                }
                menu_footer(ui, "เส้นจะหายไปเมื่อจบการเชื่อมต่อ");
            });
        }

        // Fullscreen
        let (g, tip) = if w.fullscreen { (Glyph::Shrink, "ออกจากเต็มจอ") } else { (Glyph::Expand, "เต็มจอ") };
        if bar_button(ui, g, tip, Tone::Accent, false).clicked() {
            w.fullscreen = !w.fullscreen;
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(w.fullscreen));
        }
        bar_divider(ui);

        // Disconnect
        if bar_button(ui, Glyph::Close, "ตัดการเชื่อมต่อ", Tone::Danger, false).clicked() {
            self.viewer.close();
        }
    }

    /// Pen mode: a drag on the remote screen becomes a stroke, sent as it is drawn.
    fn draw(&self, pen: &mut Pen, resp: &Response, rect: Rect) {
        let norm = |p: Pos2| {
            let q = rect.clamp(p);
            pos2((q.x - rect.left()) / rect.width(), (q.y - rect.top()) / rect.height())
        };
        if resp.hovered() {
            resp.ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.drag_started_by(egui::PointerButton::Primary) {
            pen.next_id += 1;
            pen.unsent.clear();
            pen.strokes.push(LocalStroke { color: PEN_COLORS[pen.color].1, pts: Vec::new() });
        }
        let active = resp.dragged_by(egui::PointerButton::Primary) || resp.drag_stopped_by(egui::PointerButton::Primary);
        if active
            && let Some(p) = resp.interact_pointer_pos().map(norm)
            && let Some(s) = pen.strokes.last_mut()
            && s.pts.last().is_none_or(|l| (*l - p).length() > 0.0015)
        {
            s.pts.push(p);
            pen.unsent.extend([p.x, p.y]);
        }
        // One message per frame (and at the end of the stroke).
        if !pen.unsent.is_empty() {
            let c = PEN_COLORS[pen.color].1;
            self.viewer.send(serde_json::json!({
                "t": "wb", "op": "line", "id": pen.next_id,
                "c": format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()),
                "w": PEN_WIDTH, "p": std::mem::take(&mut pen.unsent),
            }));
        }
    }

    /// "Recording" badge in the top-right corner while the remote machine records the session.
    fn recording_badge(&self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::unique(("recording", self.viewport)))
            .order(Order::Middle)
            .interactable(true)
            .anchor(Align2::RIGHT_TOP, [-12.0, 10.0])
            .show(ctx, |ui| {
                Frame::new().fill(Color32::from_black_alpha(175)).corner_radius(12).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let a = 0.55 + 0.45 * (ui.input(|i| i.time) * 3.0).sin().abs() as f32;
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.5, Color32::from_rgb(0xff, 0x45, 0x3a).gamma_multiply(a));
                        ui.label(RichText::new("กำลังบันทึก").font(semi(12.5)).color(Color32::WHITE));
                    })
                    .response
                    .on_hover_text("องค์กรกำหนดให้บันทึกหน้าจอของเซสชันนี้ไว้ตรวจสอบย้อนหลัง");
                });
            });
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    /// Demo/screenshot hook: opens the named toolbar menu once.
    fn demo_open(&self, ctx: &egui::Context, w: &mut WinUi, name: &str, id: egui::Id) {
        if w.demo_menu.as_deref() == Some(name) {
            w.demo_menu = None;
            Popup::open_id(ctx, id);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Quality monitor, transfers

    fn quality_monitor(&self, ctx: &egui::Context, f: &Frame1) {
        egui::Area::new(egui::Id::unique(("quality", self.viewport)))
            .order(Order::Middle)
            .interactable(false)
            .anchor(Align2::LEFT_TOP, [10.0, 10.0])
            .show(ctx, |ui| {
                Frame::new().fill(Color32::from_black_alpha(175)).corner_radius(8).inner_margin(Margin::symmetric(12, 9)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(14.0, 3.0);
                    ui.spacing_mut().interact_size.y = 16.0;
                    let key = |t: &str| RichText::new(t).size(12.0).color(Color32::from_gray(185));
                    let val = |t: String| RichText::new(t).font(mono(12.0)).color(Color32::WHITE);
                    ui.label(RichText::new(f.name).font(semi(13.0)).color(Color32::WHITE));
                    egui::Grid::new(("quality-grid", self.viewport)).num_columns(2).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                        ui.label(key("ID"));
                        ui.label(val(format_id(&self.device_id)));
                        ui.end_row();
                        ui.label(key("เส้นทาง"));
                        ui.label(val(f.path.map_or("–", |p| p.label()).to_string()));
                        ui.end_row();
                        ui.label(key("ความเร็ว"));
                        ui.label(val(format!("{} kbps", f.kbps)));
                        ui.end_row();
                        ui.label(key("FPS"));
                        ui.label(val(f.fps.to_string()));
                        ui.end_row();
                        ui.label(key("Codec"));
                        ui.label(val(codec_label(f.codec)));
                        ui.end_row();
                        ui.label(key("ความละเอียด"));
                        ui.label(val(if f.fb.0 > 0 { format!("{} × {}", f.fb.0, f.fb.1) } else { "–".into() }));
                        ui.end_row();
                        if !f.fp.is_empty() {
                            ui.label(key("E2E"));
                            ui.label(val(f.fp.to_string()));
                            ui.end_row();
                        }
                    });
                });
            });
    }

    fn transfers(&self, ctx: &egui::Context, transfers: &[crate::viewer::Transfer]) {
        let shown: Vec<_> = transfers.iter().rev().take(4).collect();
        if shown.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::unique(("transfers", self.viewport)))
            .order(Order::Middle)
            .interactable(false)
            .anchor(Align2::LEFT_BOTTOM, [12.0, -12.0])
            .show(ctx, |ui| {
                let pal = Palette::of(ui);
                Frame::new()
                    .fill(pal.bg)
                    .corner_radius(10)
                    .stroke(Stroke::new(1.0, pal.line))
                    .shadow(Shadow { offset: [0, 3], blur: 14, spread: 0, color: Color32::from_black_alpha(60) })
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(290.0);
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.spacing_mut().interact_size.y = 18.0;
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            glyph(ui, r, Glyph::Folder, pal.accent);
                            ui.label(RichText::new("ส่งไฟล์ไปเครื่องปลายทาง").font(semi(13.5)).color(pal.fg));
                        });
                        for t in shown {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&t.name).size(12.5).color(pal.fg));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let (text, color) = match &t.status {
                                        TransferStatus::Active => {
                                            let pct = if t.size > 0 { t.sent * 100 / t.size } else { 0 };
                                            (format!("{pct}%"), pal.muted)
                                        }
                                        TransferStatus::Done(_) => ("ส่งแล้ว".into(), pal.ok),
                                        TransferStatus::Failed(_) => ("ไม่สำเร็จ".into(), pal.danger),
                                    };
                                    ui.label(RichText::new(text).size(12.0).color(color));
                                });
                            });
                            match &t.status {
                                TransferStatus::Active => {
                                    let frac = if t.size > 0 { t.sent as f32 / t.size as f32 } else { 0.0 };
                                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                                    ui.painter().rect_filled(r, 2.0, pal.field);
                                    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac, 4.0)), 2.0, pal.accent);
                                }
                                TransferStatus::Failed(e) => {
                                    ui.label(RichText::new(e).size(11.5).color(pal.danger));
                                }
                                TransferStatus::Done(_) => {}
                            }
                        }
                    });
            });
    }

    // -----------------------------------------------------------------------------------------
    // Dialogs (connecting, password, closed)

    fn dialog(&self, ctx: &egui::Context, w: &mut WinUi, phase: &Phase, error: Option<&str>, name: &str) {
        egui::Area::new(egui::Id::unique(("dialog", self.viewport)))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let pal = Palette::of(ui);
                Frame::new()
                    .fill(pal.bg)
                    .corner_radius(12)
                    .stroke(Stroke::new(1.0, if ui.visuals().dark_mode { pal.line } else { Color32::TRANSPARENT }))
                    .shadow(Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(90) })
                    .inner_margin(Margin { left: 24, right: 24, top: 20, bottom: 20 })
                    .show(ui, |ui| {
                        ui.set_width(360.0);
                        ui.spacing_mut().item_spacing.y = 10.0;
                        match phase {
                            Phase::Password => self.password_dialog(ui, w, error, name),
                            Phase::Closed => {
                                let failed = error.is_some_and(|e| !e.is_empty());
                                let title = if failed { "เชื่อมต่อไม่สำเร็จ" } else { "ตัดการเชื่อมต่อแล้ว" };
                                dialog_title(ui, if failed { Glyph::Warn } else { Glyph::Info }, title, if failed { pal.danger } else { pal.accent });
                                let body = match error.filter(|e| !e.is_empty()) {
                                    Some(e) => e.to_string(),
                                    None => format!("การเชื่อมต่อกับ {name} สิ้นสุดแล้ว"),
                                };
                                ui.label(RichText::new(body).size(13.5).color(pal.muted));
                                ui.add_space(6.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if dialog_button(ui, "เชื่อมต่ออีกครั้ง", true, true).clicked() {
                                        self.reconnect.store(true, Ordering::Relaxed);
                                    }
                                    if dialog_button(ui, "ปิด", false, true).clicked() {
                                        self.closed.store(true, Ordering::Relaxed);
                                    }
                                });
                            }
                            other => {
                                dialog_title(ui, Glyph::Link, "กำลังเชื่อมต่อ…", pal.accent);
                                let text = match other {
                                    Phase::Requesting => "กำลังขอเชื่อมต่อ…",
                                    Phase::Waiting => "รอเครื่องปลายทางตอบรับ…",
                                    Phase::Handshake => "กำลังแลกเปลี่ยนกุญแจเข้ารหัส…",
                                    Phase::Approval => "รอผู้ใช้ที่เครื่องปลายทางกดอนุญาต…",
                                    _ => "",
                                };
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
                                    ui.label(RichText::new(text).size(14.0).color(pal.fg));
                                });
                                if let Some(e) = error {
                                    ui.label(RichText::new(e).size(12.5).color(pal.danger));
                                }
                                ui.add_space(6.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if dialog_button(ui, "ยกเลิก", false, true).clicked() {
                                        self.close_window();
                                    }
                                });
                            }
                        }
                    });
            });
    }

    fn password_dialog(&self, ui: &mut Ui, w: &mut WinUi, error: Option<&str>, name: &str) {
        let pal = Palette::of(ui);
        dialog_title(ui, Glyph::Lock, "ต้องใส่รหัสผ่าน", pal.accent);
        ui.label(RichText::new(format!("ใส่รหัสผ่านของ {name} ดูได้ที่หน้าต่าง Aegiron ของเครื่องนั้น")).size(13.0).color(pal.muted));
        ui.add_space(2.0);
        let r = ui.add(
            TextEdit::singleline(&mut w.password)
                .password(true)
                .hint_text("รหัสผ่าน")
                .font(mono(15.0))
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
        ui.add_space(6.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ok = dialog_button(ui, "ตกลง", true, !w.password.is_empty()).clicked();
            if (ok || enter) && !w.password.is_empty() {
                self.viewer.submit_password(&w.password);
                w.password.clear();
            }
            if dialog_button(ui, "ยกเลิก", false, true).clicked() {
                self.close_window();
            }
        });
    }

    // -----------------------------------------------------------------------------------------
    // Test and screenshot hooks

    /// Automation for tests: AEGIRON_UI_PASSWORD answers the password prompt once.
    fn debug_hooks(&self, phase: &Phase) {
        static SUBMITTED: AtomicBool = AtomicBool::new(false);
        if *phase == Phase::Password
            && let Ok(pw) = std::env::var("AEGIRON_UI_PASSWORD")
            && !SUBMITTED.swap(true, Ordering::Relaxed)
        {
            self.viewer.submit_password(&pw);
        }
    }

    /// Screenshots: AEGIRON_UI_VIEWER_STATE="menu=display,quality,dark,..." sets up the demo
    /// viewer once (flags: menu=monitor|actions|display|keyboard|pen, pinned, collapsed, quality, pen,
    /// original, cursor, dark, password, connecting, closed).
    fn demo_state(&self, ctx: &egui::Context, w: &mut WinUi) {
        if w.demo_done {
            return;
        }
        w.demo_done = true;
        let Ok(spec) = std::env::var("AEGIRON_UI_VIEWER_STATE") else { return };
        let mut st = self.viewer.state.lock().unwrap();
        if st.codec.is_empty() {
            st.codec = "h264".into();
        }
        for flag in spec.split(',').map(str::trim) {
            match flag {
                "pinned" => w.pinned = true,
                "collapsed" => w.bar_seen = Some(-100.0),
                "quality" => w.show_quality = true,
                "original" => w.original = true,
                "cursor" => st.cursor = Some((0.62, 0.55)),
                "pen" => {
                    w.pen.on = true;
                    let ring = (0..=48).map(|i| {
                        let a = i as f32 / 48.0 * TAU;
                        pos2(0.42 + 0.08 * a.cos(), 0.4 + 0.11 * a.sin())
                    });
                    w.pen.strokes.push(LocalStroke { color: PEN_COLORS[0].1, pts: ring.collect() });
                }
                "dark" => ctx.set_theme(egui::Theme::Dark),
                "password" => {
                    st.phase = Phase::Password;
                    st.error = Some("รหัสผ่านไม่ถูกต้อง (เหลือ 2 ครั้ง)".into());
                }
                "connecting" => st.phase = Phase::Approval,
                "closed" => {
                    st.phase = Phase::Closed;
                    st.error = Some("เครื่องปลายทางปิดการเชื่อมต่อ".into());
                }
                f if f.starts_with("menu=") => {
                    w.demo_menu = Some(f[5..].to_string());
                    w.pinned = true;
                }
                _ => {}
            }
        }
    }
}

fn codec_label(codec: &str) -> String {
    if codec.is_empty() { "–".into() } else { codec.to_uppercase() }
}

fn os_label(os: &str) -> &str {
    match os {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        "" => "ไม่ทราบ",
        other => other,
    }
}

// ---------------------------------------------------------------------------------------------
// Toolbar and menu widgets

fn bar_fill(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_rgb(0x2b, 0x2c, 0x31) } else { Color32::from_rgb(0xf7, 0xf7, 0xf9) }
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Accent,
    Neutral,
    Danger,
}

/// Small filled toolbar button (RustDesk-style: blue square with a white glyph).
fn bar_button(ui: &mut Ui, g: Glyph, tip: &str, tone: Tone, open: bool) -> Response {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(BAR_BUTTON, BAR_BUTTON), Sense::click());
    let dark = ui.visuals().dark_mode;
    let base = match tone {
        Tone::Accent => pal.accent,
        Tone::Danger => pal.danger,
        Tone::Neutral => {
            if dark {
                pal.field
            } else {
                Color32::from_rgb(0xe4, 0xe4, 0xe9)
            }
        }
    };
    let fill = if open || resp.is_pointer_button_down_on() {
        base.lerp_to_gamma(Color32::BLACK, 0.18)
    } else if resp.hovered() {
        base.lerp_to_gamma(Color32::WHITE, if tone == Tone::Neutral && !dark { 0.0 } else { 0.14 })
            .lerp_to_gamma(Color32::BLACK, if tone == Tone::Neutral && !dark { 0.06 } else { 0.0 })
    } else {
        base
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    let fg = if tone == Tone::Neutral { pal.fg } else { Color32::WHITE };
    glyph(ui, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), g, fg);
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if open { resp } else { resp.on_hover_text(tip) }
}

fn bar_divider(ui: &mut Ui) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(5.0, BAR_BUTTON), Sense::hover());
    ui.painter().line_segment([pos2(rect.center().x, rect.top() + 6.0), pos2(rect.center().x, rect.bottom() - 6.0)], Stroke::new(1.0, pal.line));
}

/// Dropdown attached below a toolbar button, styled like RustDesk's menus.
fn menu<'a>(resp: &Response, id: egui::Id, pal: Palette) -> Popup<'a> {
    Popup::menu(resp)
        .id(id)
        .align(RectAlign::BOTTOM)
        .align_alternatives(&[RectAlign::BOTTOM_START, RectAlign::BOTTOM_END])
        .gap(8.0)
        .width(250.0)
        .frame(
            Frame::new()
                .fill(pal.bg)
                .corner_radius(10)
                .stroke(Stroke::new(1.0, pal.line))
                .shadow(Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(55) })
                .inner_margin(Margin::same(6)),
        )
}

enum Lead {
    Glyph(Glyph),
    /// Radio-style option: a check mark when selected.
    Check(bool),
    /// On/off option: a check box.
    Tick(bool),
}

fn menu_row(ui: &mut Ui, lead: Lead, text: &str, hint: &str, enabled: bool) -> Response {
    let pal = Palette::of(ui);
    let width = ui.available_width().max(230.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 32.0), if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 6.0, if ui.visuals().dark_mode { pal.field } else { pal.accent_soft });
    }
    let fg = if !enabled { pal.muted.gamma_multiply(0.7) } else if hovered && !ui.visuals().dark_mode { pal.accent } else { pal.fg };
    let icon = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(15.0, 15.0));
    match lead {
        Lead::Glyph(g) => glyph(ui, icon, g, if enabled { if hovered { fg } else { pal.muted } } else { fg }),
        Lead::Check(true) => glyph(ui, icon, Glyph::Check, pal.accent),
        Lead::Check(false) => {}
        Lead::Tick(on) => {
            let b = Rect::from_center_size(icon.center(), vec2(15.0, 15.0));
            if on {
                ui.painter().rect_filled(b, 4.0, pal.accent);
                glyph(ui, b.shrink(2.0), Glyph::Check, Color32::WHITE);
            } else {
                ui.painter().rect_stroke(b, 4.0, Stroke::new(1.5, pal.muted), egui::StrokeKind::Inside);
            }
        }
    }
    ui.painter().text(pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(14.0), fg);
    if !hint.is_empty() {
        ui.painter().text(pos2(rect.right() - 12.0, rect.center().y), Align2::RIGHT_CENTER, hint, FontId::proportional(12.5), pal.muted);
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

fn menu_header(ui: &mut Ui, text: &str) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(rect.left() + 12.0, rect.bottom() - 6.0), Align2::LEFT_BOTTOM, text, semi(12.0), pal.muted);
}

fn menu_footer(ui: &mut Ui, text: &str) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.5), pal.muted);
}

fn menu_separator(ui: &mut Ui) {
    let pal = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().line_segment([pos2(rect.left() + 6.0, rect.center().y), pos2(rect.right() - 6.0, rect.center().y)], Stroke::new(1.0, pal.line));
}

/// Device name, ID and E2E fingerprint at the top of the actions menu.
fn identity_header(ui: &mut Ui, f: &Frame1) {
    let pal = Palette::of(ui);
    Frame::new().inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(f.name).font(semi(14.5)).color(pal.fg));
        ui.label(RichText::new(format!("ID {}", format_id(f.id))).font(mono(11.5)).color(pal.muted));
        if !f.fp.is_empty() {
            ui.label(RichText::new(format!("E2E {}", f.fp)).font(mono(11.5)).color(pal.muted))
                .on_hover_text("ลายนิ้วมือการเข้ารหัสต้นทาง-ปลายทาง ตรวจให้ตรงกับที่เครื่องปลายทางแสดง");
        }
    });
}

fn dialog_title(ui: &mut Ui, g: Glyph, text: &str, color: Color32) {
    let pal = Palette::of(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        glyph(ui, r, g, color);
        ui.label(RichText::new(text).font(semi(17.0)).color(pal.fg));
    });
}

fn dialog_button(ui: &mut Ui, text: &str, primary: bool, enabled: bool) -> Response {
    let pal = Palette::of(ui);
    let b = if primary {
        Button::new(RichText::new(text).font(semi(14.0)).color(pal.on_accent)).fill(pal.accent).stroke(Stroke::NONE)
    } else {
        Button::new(RichText::new(text).font(semi(14.0)).color(pal.accent)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, pal.accent))
    };
    ui.add_enabled(enabled, b.corner_radius(6).min_size(vec2(88.0, 34.0)))
}

// ---------------------------------------------------------------------------------------------
// Glyphs used only by the remote window (drawn on a 16-unit grid like `paint_icon`)

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Pin,
    /// Monitor with the display number inside.
    Display(usize),
    Bolt,
    Tune,
    Keyboard,
    Expand,
    Shrink,
    Close,
    Check,
    ChevronDown,
    Lock,
    Folder,
    Clipboard,
    Refresh,
    Link,
    Info,
    Warn,
    Pen,
}

fn glyph(ui: &Ui, rect: Rect, g: Glyph, color: Color32) {
    let p = ui.painter();
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let st = Stroke::new(1.5 * s.max(1.0), color);
    let line = |pts: &[(f32, f32)]| p.add(Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    match g {
        Glyph::Pin => {
            line(&[(5.0, 1.8), (11.0, 1.8)]);
            line(&[(6.2, 1.8), (6.2, 6.5), (3.8, 9.5), (12.2, 9.5), (9.8, 6.5), (9.8, 1.8)]);
            line(&[(8.0, 9.5), (8.0, 14.8)]);
        }
        Glyph::Display(n) => {
            p.rect_stroke(Rect::from_min_max(at(1.0, 1.8), at(15.0, 11.8)), 1.5 * s, st, egui::StrokeKind::Middle);
            line(&[(5.0, 14.5), (11.0, 14.5)]);
            line(&[(8.0, 11.8), (8.0, 14.5)]);
            p.text(at(8.0, 6.9), Align2::CENTER_CENTER, n.to_string(), semi(8.5 * s), color);
        }
        Glyph::Bolt => {
            let poly = |pts: &[(f32, f32)]| p.add(Shape::convex_polygon(pts.iter().map(|&(x, y)| at(x, y)).collect(), color, Stroke::NONE));
            poly(&[(9.8, 0.8), (3.2, 9.2), (7.6, 9.2), (8.7, 6.8)]);
            poly(&[(7.6, 9.2), (6.3, 15.2), (12.8, 6.8), (8.7, 6.8)]);
        }
        Glyph::Tune => {
            for (y, k) in [(3.5, 10.5), (8.0, 5.0), (12.5, 9.0)] {
                line(&[(1.5, y), (14.5, y)]);
                p.circle_filled(at(k, y), 2.3 * s, color);
            }
        }
        Glyph::Keyboard => {
            p.rect_stroke(Rect::from_min_max(at(1.0, 3.5), at(15.0, 12.5)), 1.8 * s, st, egui::StrokeKind::Middle);
            for y in [6.2, 8.4] {
                for x in [4.0, 6.7, 9.3, 12.0] {
                    p.rect_filled(Rect::from_center_size(at(x, y), vec2(1.5, 1.4) * s), 0.3 * s, color);
                }
            }
            line(&[(5.0, 10.6), (11.0, 10.6)]);
        }
        Glyph::Expand => {
            line(&[(1.5, 5.5), (1.5, 1.5), (5.5, 1.5)]);
            line(&[(10.5, 1.5), (14.5, 1.5), (14.5, 5.5)]);
            line(&[(14.5, 10.5), (14.5, 14.5), (10.5, 14.5)]);
            line(&[(5.5, 14.5), (1.5, 14.5), (1.5, 10.5)]);
        }
        Glyph::Shrink => {
            line(&[(5.5, 1.5), (5.5, 5.5), (1.5, 5.5)]);
            line(&[(10.5, 1.5), (10.5, 5.5), (14.5, 5.5)]);
            line(&[(14.5, 10.5), (10.5, 10.5), (10.5, 14.5)]);
            line(&[(5.5, 14.5), (5.5, 10.5), (1.5, 10.5)]);
        }
        Glyph::Close => {
            let st = Stroke::new(1.9 * s.max(1.0), color);
            p.line_segment([at(3.5, 3.5), at(12.5, 12.5)], st);
            p.line_segment([at(12.5, 3.5), at(3.5, 12.5)], st);
        }
        Glyph::Check => {
            p.add(Shape::line(vec![at(2.5, 8.5), at(6.3, 12.2), at(13.5, 4.2)], Stroke::new(1.9 * s.max(1.0), color)));
        }
        Glyph::ChevronDown => {
            line(&[(3.5, 5.8), (8.0, 10.3), (12.5, 5.8)]);
        }
        Glyph::Lock => {
            p.rect_filled(Rect::from_min_max(at(2.8, 7.0), at(13.2, 14.6)), 2.0 * s, color);
            p.add(Shape::line(arc(at(8.0, 7.0), 3.4 * s, 0.5 * TAU, TAU), st));
            p.line_segment([at(4.6, 7.0), at(4.6, 6.0)], st);
            p.line_segment([at(11.4, 7.0), at(11.4, 6.0)], st);
        }
        Glyph::Folder => {
            line(&[(1.5, 13.5), (1.5, 2.8), (6.0, 2.8), (7.6, 4.6), (14.5, 4.6), (14.5, 13.5), (1.5, 13.5)]);
            line(&[(1.5, 7.0), (14.5, 7.0)]);
        }
        Glyph::Clipboard => paint_icon(ui, rect, Icon::Copy, color),
        Glyph::Refresh => paint_icon(ui, rect, Icon::Refresh, color),
        Glyph::Link => {
            p.rect_stroke(Rect::from_min_max(at(1.0, 1.8), at(15.0, 11.8)), 1.5 * s, st, egui::StrokeKind::Middle);
            line(&[(5.0, 14.5), (11.0, 14.5)]);
            line(&[(8.0, 11.8), (8.0, 14.5)]);
            line(&[(4.5, 7.8), (7.0, 5.3), (9.0, 7.3), (11.5, 4.8)]);
        }
        Glyph::Pen => {
            p.add(Shape::closed_line(vec![at(10.8, 2.2), at(13.8, 5.2), at(5.5, 13.5), at(1.8, 14.2), at(2.5, 10.5)], st));
            line(&[(9.0, 4.0), (12.0, 7.0)]);
        }
        Glyph::Info | Glyph::Warn => {
            p.circle_stroke(at(8.0, 8.0), 6.6 * s, st);
            if g == Glyph::Info {
                p.circle_filled(at(8.0, 4.9), 1.0 * s, color);
                line(&[(8.0, 7.2), (8.0, 11.8)]);
            } else {
                line(&[(8.0, 4.2), (8.0, 9.0)]);
                p.circle_filled(at(8.0, 11.6), 1.0 * s, color);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Input forwarding

/// How modifier keys are translated for the remote OS.
#[derive(Clone, Copy)]
struct KeyMap {
    remote_mac: bool,
    /// Send Ctrl as Ctrl and ⌘ as Meta, whatever the remote OS.
    raw: bool,
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
fn remote_mods(m: egui::Modifiers, keys: KeyMap) -> Vec<(&'static str, &'static str)> {
    let cmd_is_meta = keys.remote_mac || keys.raw;
    let mut out = Vec::new();
    if m.ctrl || (m.mac_cmd && !cmd_is_meta) {
        out.push(("ControlLeft", "Control"));
    }
    if m.mac_cmd && cmd_is_meta {
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

/// Sends pointer and keyboard input on the remote screen. `rect` is where the remote screen is
/// drawn, `visible` the part of the window showing it. Pointer events over the toolbar, its
/// menus or other overlays stay local (hit-tested against egui's layers from the last frame),
/// and nothing is forwarded while a menu is open.
fn forward_input(v: &Viewer, ui: &Ui, rect: Rect, visible: Rect, keys: KeyMap, held: &mut u8) {
    let ctx = ui.ctx();
    let menu_open = Popup::is_any_open(ctx);
    let ours = ui.layer_id();
    let on_screen = |p: Pos2| {
        rect.contains(p) && visible.contains(p) && !menu_open && ctx.layer_id_at(p).is_none_or(|l| l == ours || l.order == Order::Background)
    };
    let norm = |p: Pos2| {
        let q = rect.clamp(p);
        (((q.x - rect.left()) / rect.width()) as f64, ((q.y - rect.top()) / rect.height()) as f64)
    };
    let (events, hover) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    let mut moved = None;
    for e in events {
        match e {
            egui::Event::PointerMoved(p) if on_screen(p) || *held != 0 => moved = Some(norm(p)),
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                let b: u8 = match button {
                    egui::PointerButton::Primary => 0,
                    egui::PointerButton::Middle => 1,
                    egui::PointerButton::Secondary => 2,
                    _ => continue,
                };
                // Presses must land on the screen; a release goes out only if its press did.
                if pressed && !on_screen(pos) || !pressed && *held & (1 << b) == 0 {
                    continue;
                }
                if pressed {
                    *held |= 1 << b;
                } else {
                    *held &= !(1 << b);
                }
                let (x, y) = norm(pos);
                moved = None;
                v.send(serde_json::json!({"t": "mb", "b": b, "down": pressed, "x": x, "y": y}));
            }
            egui::Event::MouseWheel { unit, delta, .. } if hover.is_some_and(on_screen) => {
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
            // Keyboard input belongs to an open menu (Esc closes it).
            _ if menu_open => {}
            egui::Event::Text(t) => {
                let t: String = t.chars().filter(|c| !c.is_control()).collect();
                if !t.is_empty() {
                    v.send(serde_json::json!({"t": "text", "s": t}));
                }
            }
            egui::Event::Copy => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), keys), ("KeyC", "c")),
            egui::Event::Cut => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), keys), ("KeyX", "x")),
            egui::Event::Paste(text) => {
                // Paste what is on this machine's clipboard: hand it over first, then press paste remotely.
                v.send(serde_json::json!({"t": "clip", "s": text}));
                send_combo(v, &remote_mods(ui.input(|i| i.modifiers), keys), ("KeyV", "v"));
            }
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                let mods = remote_mods(modifiers, keys);
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

/// Whiteboard strokes drawn in this window, over the remote screen at `rect`.
fn paint_strokes(p: &egui::Painter, strokes: &[LocalStroke], rect: Rect) {
    let width = (PEN_WIDTH * rect.height()).max(1.5);
    for s in strokes {
        let pts: Vec<Pos2> = s.pts.iter().map(|q| pos2(rect.left() + q.x * rect.width(), rect.top() + q.y * rect.height())).collect();
        match pts.len() {
            0 => {}
            1 => {
                p.circle_filled(pts[0], width / 2.0, s.color);
            }
            _ => {
                p.add(Shape::line(pts, Stroke::new(width, s.color)));
            }
        }
    }
}

/// Arrow pointer with its tip at `tip`, drawn over the remote screen.
fn paint_pointer(p: &egui::Painter, tip: egui::Pos2) {
    let pts = [(0.0, 0.0), (0.0, 16.0), (4.2, 12.3), (7.0, 18.5), (9.6, 17.4), (6.9, 11.3), (12.0, 11.3)];
    let at = |i: &[usize]| i.iter().map(|&i| tip + vec2(pts[i].0, pts[i].1)).collect::<Vec<_>>();
    // Head and tail are filled separately: convex_polygon needs convex input.
    p.add(egui::Shape::convex_polygon(at(&[0, 1, 2, 5, 6]), Color32::BLACK, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(at(&[2, 3, 4, 5]), Color32::BLACK, Stroke::NONE));
    p.add(egui::Shape::closed_line(at(&[0, 1, 2, 3, 4, 5, 6]), Stroke::new(1.2, Color32::WHITE)));
}
