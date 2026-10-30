//! Remote-control window (one native window per connection, like RustDesk): the remote screen
//! fills the window, with a collapsible floating toolbar centered on the top edge, an optional
//! quality monitor in the top-left corner and dialog-style overlays while connecting.

use super::*;

use egui::{Order, Popup, RectAlign, Shadow};

use crate::codec::Codec;
use crate::status::ChatLine;
use crate::viewer::{CallState, ClipEntry, Controls, Info, Paste, Phase, Reconnect, RemoteDir, Transfer, TransferStatus};

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
    /// How the remote screen is sized in the window.
    scale: Scale,
    /// Where a view larger than the window is panned to (0..1 on each axis).
    pan: egui::Vec2,
    /// Pan so the remote pointer stays visible (instead of following ours).
    follow_cursor: bool,
    /// Send ⌘/Ctrl as-is instead of mapping them for the remote OS.
    raw_keys: bool,
    /// Keyboard "map" mode: physical keys, down and up (feature "keymode").
    key_map: bool,
    /// Relative mouse: the cursor is captured and motion is sent as `mr` (feature "relmouse").
    relative: bool,
    /// Remote display whose modes were asked for while the monitor menu is open.
    modes_asked: Option<usize>,
    /// Top of the area this window draws in (below the tab strip when hosted in tabs).
    top: f32,
    /// Input state of the main screen.
    input: InputState,
    /// Textures and input state of extra display windows, by display.
    extra_tex: HashMap<usize, Option<egui::TextureHandle>>,
    extra_input: HashMap<usize, InputState>,
    demo_menu: Option<String>,
    demo_done: bool,
    pen: Pen,
    /// Remote file panel on the right.
    show_files: bool,
    show_hidden: bool,
    /// Name being typed for a new folder.
    new_folder: Option<String>,
    /// (remote path, new name) while renaming an entry.
    renaming: Option<(String, String)>,
    /// Remote path waiting for delete confirmation.
    confirm_delete: Option<String>,
    /// Chat panel on the right.
    show_chat: bool,
    chat_draft: String,
    /// Chat messages already announced with a toast.
    chat_seen: usize,
    /// Asking whether to let the other side control this machine.
    confirm_switch: bool,
    /// Asking whether to restart the remote machine.
    confirm_restart: bool,
}

/// What input forwarding remembers between frames for one remote screen.
#[derive(Default)]
struct InputState {
    /// Mouse buttons pressed on the remote screen (bit per button), so releases pair up.
    held: u8,
    /// A paste was seen on key-down, so the matching key-up is not a second paste.
    paste_seen: bool,
    /// Modifiers held on the remote side in map mode.
    mods_down: Vec<(&'static str, &'static str)>,
    /// Relative motion not yet sent (fractions of a unit).
    rel_rest: egui::Vec2,
}

/// The window holding remote sessions as tabs.
pub fn tabs_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("aegiron-remote-tabs")
}

/// How the remote screen is sized (RustDesk's view styles).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Scale {
    /// Fills the window, keeping the aspect ratio (enlarges small screens too).
    #[default]
    Stretch,
    /// Like stretch, but never larger than 1:1.
    Shrink,
    /// 1:1 pixels; a larger screen pans.
    Original,
}

/// Where the window is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Host {
    /// Its own native window (tabs off).
    Window,
    /// A tab of the tabbed viewer window.
    Tab,
    /// Popped out of the tabs into its own window.
    PoppedOut,
}

pub struct ViewerWindow {
    pub viewer: Arc<Viewer>,
    pub viewport: egui::ViewportId,
    pub device_id: String,
    ui: Mutex<WinUi>,
    pub closed: AtomicBool,
    pub reconnect: AtomicBool,
    /// Shown in its own window although tabs are on.
    pub popped: AtomicBool,
    /// Asked to move from its own window back into the tabs (or out of them).
    pub toggle_tab: AtomicBool,
    /// Windows to repaint when the connection has news: where it is shown and its extra
    /// display windows (kept up to date by the app).
    pub repaint_to: Arc<Mutex<Vec<egui::ViewportId>>>,
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
    encoder: &'a str,
    muted: bool,
    audio_level: Option<f32>,
    fb: (usize, usize),
    path: Option<crate::direct::Path>,
    chat_unread: usize,
    call: CallState,
    switch_pending: bool,
    display_all: bool,
    modes: Option<&'a crate::viewer::Modes>,
    host: Host,
    view_only: bool,
    controls: Controls,
}

impl ViewerWindow {
    pub fn new(viewer: Arc<Viewer>, device_id: String, viewport: egui::ViewportId, repaint_to: Arc<Mutex<Vec<egui::ViewportId>>>) -> Arc<Self> {
        Arc::new(Self {
            viewer,
            viewport,
            device_id,
            ui: Mutex::new(WinUi { quality: 1, pan: vec2(0.5, 0.5), ..Default::default() }),
            closed: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
            popped: AtomicBool::new(false),
            toggle_tab: AtomicBool::new(false),
            repaint_to,
        })
    }

    /// Viewport of extra display window `d`.
    pub fn display_viewport(&self, d: usize) -> egui::ViewportId {
        egui::ViewportId::from_hash_of((self.viewport, "display", d))
    }

    /// Displays shown in their own windows.
    pub fn extra_displays(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.viewer.state.lock().unwrap().extra.keys().copied().collect();
        v.sort();
        v
    }

    /// Short name for a tab or window title.
    pub fn title(&self) -> String {
        self.viewer.state.lock().unwrap().device_name.clone()
    }

    fn flash(w: &mut WinUi, msg: impl Into<String>) {
        w.toast = Some((msg.into(), Instant::now()));
    }

    pub fn close_window(&self) {
        self.viewer.close();
        self.closed.store(true, Ordering::Relaxed);
    }

    /// Draws the window. `host` says where: its own window, or a tab (whose host window handles
    /// closing).
    pub fn show(&self, ui: &mut Ui, host: Host) {
        let ctx = ui.ctx().clone();
        if host != Host::Tab && ui.input(|i| i.viewport().close_requested()) {
            self.close_window();
            return;
        }
        let mut guard = self.ui.lock().unwrap();
        let w = &mut *guard;
        self.demo_state(&ctx, w);
        w.top = ui.available_rect_before_wrap().top();
        if w.relative && ui.input(|i| i.viewport().focused == Some(false)) {
            // Lost focus: give the pointer back.
            self.set_relative(&ctx, w, false);
        }

        // Pull new screen regions into the GPU texture and pick up state for this frame.
        let (chat, chat_unread, call, switch_pending, switched) = {
            let st = self.viewer.state.lock().unwrap();
            (st.chat.clone(), st.chat_unread, st.call, st.switch_pending, st.switched)
        };
        if switched {
            // The other side now controls this machine (switch sides): this window is done.
            self.close_window();
            return;
        }
        let (phase, error, name, fp, info, fps, kbps, clip, transfers, fb_size, cursor, codec, path, browser, clip_offer, notice, encoder, muted, audio_level, display_all, modes, view_only, controls, reconnect) = {
            let mut st = self.viewer.state.lock().unwrap();
            sync_texture(&ctx, &mut w.texture, &mut st.fb, &format!("remote-{}", self.device_id));
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
                st.browser.clone(),
                st.clip_offer.clone(),
                st.notice.take(),
                st.encoder.clone(),
                st.muted,
                st.audio_level,
                st.display_all,
                st.modes.clone(),
                st.view_only,
                st.controls,
                st.reconnect,
            )
        };
        let connected = phase == Phase::Connected;
        let remote_mac = info.as_ref().is_some_and(|i| i.os == "macos");
        self.debug_hooks(&phase);

        if let Some(text) = clip
            && crate::clipboard::with(|c| c.set_text(text)).is_ok()
        {
            Self::flash(w, "ได้รับคลิปบอร์ดจากเครื่องปลายทาง");
        }
        if let Some(n) = notice {
            Self::flash(w, n);
        }
        let fs2 = info.as_ref().is_some_and(Info::fs2);
        let dropped: Vec<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            if connected && info.as_ref().is_some_and(|i| i.files) {
                // Into the folder shown in the file panel, else the agent's Downloads/Aegiron.
                let dest = if w.show_files && !browser.path.is_empty() { browser.path.clone() } else { String::new() };
                let (dirs, files): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| p.is_dir());
                let n = files.len() + if fs2 { dirs.len() } else { 0 };
                for p in files.into_iter().chain(dirs.iter().filter(|_| fs2).cloned()) {
                    self.viewer.upload(p, dest.clone());
                }
                let place = if dest.is_empty() { "Downloads/Aegiron ของเครื่องปลายทาง".to_string() } else { dest.clone() };
                if !dirs.is_empty() && !fs2 {
                    Self::flash(w, "เครื่องปลายทางใช้ Aegiron รุ่นเก่า ส่งได้เฉพาะไฟล์ ไม่รองรับโฟลเดอร์");
                } else if n > 0 {
                    Self::flash(w, format!("กำลังส่ง {n} รายการไปที่ {place}"));
                }
            } else {
                Self::flash(w, "เครื่องปลายทางไม่อนุญาตให้รับไฟล์");
            }
        }
        if !(connected && info.as_ref().is_some_and(|i| i.files)) {
            w.show_files = false;
        }
        if w.show_files {
            self.files_panel(ui, w, &browser, fs2);
        }
        // Chat: a new message from the other side opens nothing by itself, but shows a toast
        // and a count on the toolbar button.
        let can_chat = connected && info.as_ref().is_some_and(Info::chat);
        if !can_chat {
            w.show_chat = false;
        }
        if chat.len() > w.chat_seen {
            if !w.show_chat
                && let Some(l) = chat[w.chat_seen..].iter().rev().find(|l| !l.mine)
            {
                let text: String = l.text.chars().take(80).collect();
                Self::flash(w, format!("ข้อความจาก {name}: {text}"));
            }
            w.chat_seen = chat.len();
        }
        if w.show_chat {
            if chat_unread > 0 {
                self.viewer.mark_chat_read();
            }
            self.chat_panel(ui, w, &chat, &name);
        }

        // Ctrl+Alt+M toggles relative mouse mode (never sent to the remote machine).
        let relmouse = connected && info.as_ref().is_some_and(|i| i.input && i.has("relmouse"));
        if relmouse && take_relative_hotkey(ui) {
            let on = !w.relative;
            self.set_relative(&ctx, w, on);
        }

        // Remote screen: the whole window.
        egui::CentralPanel::default().frame(Frame::new().fill(SCREEN_BG)).show(ui, |ui| {
            let area = ui.max_rect();
            if let (Some(tex), (fw, fh)) = (&w.texture, fb_size) {
                // A view larger than the window pans with our pointer, or with the remote one.
                let size = view_size(w.scale, area.size(), vec2(fw as f32, fh as f32), ctx.pixels_per_point());
                match cursor.filter(|_| w.follow_cursor || w.relative) {
                    Some((cx, cy)) => {
                        w.pan.x = follow(w.pan.x, size.x, area.width(), cx * size.x);
                        w.pan.y = follow(w.pan.y, size.y, area.height(), cy * size.y);
                    }
                    None => {
                        if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p)) {
                            w.pan = vec2((p.x - area.left()) / area.width(), (p.y - area.top()) / area.height());
                        }
                    }
                }
                let rect = place(area, size, w.pan);
                ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                let resp = ui.allocate_rect(rect.intersect(area), Sense::click_and_drag());
                let typing = w.new_folder.is_some() || w.renaming.is_some() || (w.show_chat && ui.ctx().egui_wants_keyboard_input());
                let drawing = connected && w.pen.on && !display_all && info.as_ref().is_some_and(|i| i.whiteboard);
                if drawing {
                    self.draw(&mut w.pen, &resp, rect);
                    w.input.held = 0;
                } else if connected && !view_only && info.as_ref().is_some_and(|i| i.input) && ui.input(|i| i.viewport().focused != Some(false)) && !typing {
                    let keys = KeyMap { remote_mac, raw: w.raw_keys, map: w.key_map, relative: w.relative };
                    forward_input(&self.viewer, ui, rect, area, keys, &mut w.input, None);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(if w.relative { egui::CursorIcon::None } else { egui::CursorIcon::Default });
                    }
                } else {
                    w.input.held = 0;
                }
                paint_strokes(ui.painter(), &w.pen.strokes, rect);
                // The remote pointer (captures do not include it), unless ours is on top of it.
                if let Some((cx, cy)) = cursor.filter(|_| !resp.hovered() || w.relative) {
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
            let f = Frame1 {
                name: &name,
                id: &self.device_id,
                fp: &fp,
                info: info.as_ref(),
                fps,
                kbps,
                codec: &codec,
                encoder: &encoder,
                muted,
                audio_level,
                fb: fb_size,
                path,
                chat_unread: if w.show_chat { 0 } else { chat_unread },
                call,
                switch_pending,
                display_all,
                modes: modes.as_ref(),
                host,
                view_only,
                controls,
            };
            if w.show_quality {
                self.quality_monitor(&ctx, &f, w.top);
            }
            if f.info.is_some_and(|i| i.recording) {
                self.recording_badge(&ctx, w.top);
            }
            self.control_badges(&ctx, &f);
            self.toolbar(&ctx, w, &f);
            if w.confirm_switch {
                self.switch_dialog(&ctx, w, &name);
            }
            if call != CallState::Idle {
                self.call_badge(&ctx, call);
            }
            if w.confirm_restart {
                self.restart_dialog(&ctx, w, &name);
            }
        } else {
            w.confirm_restart = false;
            self.dialog(&ctx, w, &phase, error.as_deref(), &name, reconnect);
        }
        self.transfers(&ctx, &transfers);
        if connected && let Some(offer) = &clip_offer {
            self.clip_offer(&ctx, offer);
        }

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

    /// Strip of remote sessions on top of the tabbed viewer window (RustDesk style). Right-click
    /// a tab to open it in its own window.
    pub fn tab_strip(ui: &mut Ui, list: &[Arc<ViewerWindow>], active: &mut Option<egui::ViewportId>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for w in list {
                let (live, unread, switched) = {
                    let st = w.viewer.state.lock().unwrap();
                    (st.phase == Phase::Connected, st.chat_unread, st.switched)
                };
                if switched {
                    // Switch sides closes the window even while its tab is in the background.
                    w.close_window();
                    continue;
                }
                let on = *active == Some(w.viewport);
                let (tab, close) = tab_chip(ui, &w.title(), TabIcon::Remote(live), on, true);
                if unread > 0 && !on {
                    // Chat messages waiting in a background tab.
                    let pal = Palette::of(ui);
                    let c = tab.rect.right_top() + vec2(-6.0, 6.0);
                    ui.painter().circle_filled(c, 7.0, pal.danger);
                    ui.painter().text(c, Align2::CENTER_CENTER, unread.min(9).to_string(), semi(9.5), Color32::WHITE);
                }
                if close {
                    w.close_window();
                } else if tab.clicked() {
                    *active = Some(w.viewport);
                }
                tab.context_menu(|ui| {
                    if ui.button("แยกเป็นหน้าต่างใหม่").clicked() {
                        w.toggle_tab.store(true, Ordering::Relaxed);
                    }
                    if ui.button("ตัดการเชื่อมต่อและปิดแท็บ").clicked() {
                        w.close_window();
                    }
                });
            }
            if let Some(w) = list.iter().find(|w| *active == Some(w.viewport)) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if bar_button(ui, Glyph::PopOut, "แยกแท็บนี้เป็นหน้าต่างใหม่", Tone::Neutral, false).clicked() {
                        w.toggle_tab.store(true, Ordering::Relaxed);
                    }
                });
            }
        });
    }

    /// Window of the extra display stream `d` (feature "multidisp"): the remote display and
    /// its pointer, with the main window's scaling and keyboard settings. Closing it stops
    /// the stream.
    pub fn show_display(&self, ui: &mut Ui, d: usize) {
        let ctx = ui.ctx().clone();
        if ui.input(|i| i.viewport().close_requested()) {
            self.viewer.set_stream(d, false);
            return;
        }
        let mut guard = self.ui.lock().unwrap();
        let w = &mut *guard;
        let (live, info, size, cursor) = {
            let mut st = self.viewer.state.lock().unwrap();
            // View-only applies to every window of the session.
            let live = st.phase == Phase::Connected && !st.view_only;
            let info = st.info.clone();
            let Some(e) = st.extra.get_mut(&d) else { return };
            sync_texture(&ctx, w.extra_tex.entry(d).or_default(), &mut e.fb, &format!("remote-{}-{d}", self.device_id));
            (live, info, vec2(e.fb.w as f32, e.fb.h as f32), e.cursor)
        };
        let remote_mac = info.as_ref().is_some_and(|i| i.os == "macos");
        egui::CentralPanel::default().frame(Frame::new().fill(SCREEN_BG)).show(ui, |ui| {
            let area = ui.max_rect();
            let Some(Some(tex)) = w.extra_tex.get(&d) else {
                ui.centered_and_justified(|ui| ui.label(RichText::new("กำลังรอภาพหน้าจอ…").color(Color32::from_gray(170))));
                return;
            };
            let view = view_size(w.scale, area.size(), size, ctx.pixels_per_point());
            let pan = match cursor {
                Some((cx, cy)) => vec2(follow(0.5, view.x, area.width(), cx * view.x), follow(0.5, view.y, area.height(), cy * view.y)),
                None => vec2(0.5, 0.5),
            };
            let rect = place(area, view, pan);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let resp = ui.allocate_rect(rect.intersect(area), Sense::click_and_drag());
            let st = w.extra_input.entry(d).or_default();
            if live && info.as_ref().is_some_and(|i| i.input) && ui.input(|i| i.viewport().focused != Some(false)) {
                let keys = KeyMap { remote_mac, raw: w.raw_keys, map: w.key_map, relative: false };
                forward_input(&self.viewer, ui, rect, area, keys, st, Some(d));
            } else {
                st.held = 0;
            }
            if let Some((cx, cy)) = cursor.filter(|_| !resp.hovered()) {
                paint_pointer(ui.painter(), pos2(rect.left() + cx * rect.width(), rect.top() + cy * rect.height()));
            }
        });
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
                .anchor(Align2::CENTER_TOP, [0.0, w.top])
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
            .anchor(Align2::CENTER_TOP, [0.0, y + w.top])
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

    /// Display choice: one display, all of them in one picture, or extra windows.
    fn monitor_menu(&self, ui: &mut Ui, w: &mut WinUi, info: &Info, f: &Frame1) {
        menu_header(ui, "หน้าจอของเครื่องปลายทาง");
        for (k, d) in info.displays.iter().enumerate() {
            let label = if d.primary { format!("{} (หลัก)", d.name) } else { d.name.clone() };
            if menu_row(ui, Lead::Check(!f.display_all && d.index == info.display), &label, &format!("{}", k + 1), true).clicked() {
                self.viewer.set_display(d.index);
            }
        }
        if info.has("alldisp") {
            let r = menu_row(ui, Lead::Check(f.display_all), "ทุกหน้าจอ", "รวมเป็นภาพเดียว", true);
            if r.clicked() && !f.display_all {
                // Annotations belong to one display.
                w.pen.on = false;
                self.viewer.set_display_all();
            }
        }
        if info.has("multidisp") {
            menu_separator(ui);
            menu_header(ui, "เปิดในหน้าต่างแยก");
            let open: Vec<usize> = self.extra_displays();
            for (k, d) in info.displays.iter().enumerate() {
                let on = open.contains(&d.index);
                if menu_row(ui, Lead::Tick(on), &format!("หน้าต่างของ {}", d.name), &format!("{}", k + 1), true).clicked() {
                    self.viewer.set_stream(d.index, !on);
                }
            }
            let others: Vec<usize> = info.displays.iter().map(|d| d.index).filter(|d| *d != info.display && !open.contains(d)).collect();
            if menu_row(ui, Lead::Glyph(Glyph::PopOut), "เปิดหน้าจออื่นทุกจอแยกหน้าต่าง", "", !others.is_empty()).clicked() {
                for d in others {
                    self.viewer.set_stream(d, true);
                }
            }
        }
    }

    /// Modes of the shown display (the agent answers `res_list` when the menu opens).
    fn resolution_menu(&self, ui: &mut Ui, display: usize, modes: Option<&crate::viewer::Modes>) {
        menu_header(ui, "ความละเอียดของหน้าจอ");
        let Some(m) = modes else {
            menu_footer(ui, "กำลังโหลด…");
            return;
        };
        egui::ScrollArea::vertical().max_height(230.0).id_salt(("modes", self.viewport)).show(ui, |ui| {
            for mode in &m.modes {
                let label = format!("{} × {}", mode.w, mode.h);
                let mut hint = if mode.hz > 0 { format!("{} Hz", mode.hz) } else { String::new() };
                if *mode == m.original && m.original != m.current {
                    hint = format!("เดิม · {hint}");
                }
                if menu_row(ui, Lead::Check(*mode == m.current), &label, hint.trim_end_matches(" · "), true).clicked() && *mode != m.current {
                    self.viewer.set_resolution(display, *mode);
                }
            }
        });
        if m.original != m.current && m.original.w > 0 {
            menu_separator(ui);
            if menu_row(ui, Lead::Glyph(Glyph::Refresh), "คืนค่าความละเอียดเดิม", &format!("{} × {}", m.original.w, m.original.h), true).clicked() {
                self.viewer.restore_resolution(display);
            }
        }
        menu_footer(ui, "คืนค่าเดิมให้เองเมื่อจบการเชื่อมต่อ");
    }

    /// Captures (or releases) the pointer for relative mouse mode.
    fn set_relative(&self, ctx: &egui::Context, w: &mut WinUi, on: bool) {
        if w.relative == on {
            return;
        }
        w.relative = on;
        w.input.rel_rest = egui::Vec2::ZERO;
        // macOS can lock the cursor in place; elsewhere winit only confines it to the window.
        let grab = if !on {
            egui::CursorGrab::None
        } else if cfg!(target_os = "macos") {
            egui::CursorGrab::Locked
        } else {
            egui::CursorGrab::Confined
        };
        ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
        ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!on));
        Self::flash(w, if on { "เมาส์แบบสัมพัทธ์: เปิด (Ctrl+Alt+M เพื่อปิด)" } else { "เมาส์แบบสัมพัทธ์: ปิด" });
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
        // Monitor: the current display's number; a menu with the displays, the combined view,
        // separate windows and the resolution.
        if let Some(info) = f.info {
            let n = info.displays.iter().position(|d| d.index == info.display).map(|p| p + 1).unwrap_or(1);
            let many = info.displays.len() > 1;
            let resize = info.can_resize() && !f.display_all;
            let tip = if many { "เลือกหน้าจอ" } else { "หน้าจอของเครื่องปลายทาง" };
            let id = egui::Id::unique(("menu-monitor", self.viewport));
            let open = Popup::is_id_open(&ctx, id);
            let resp = bar_button(ui, Glyph::Display(if f.display_all { 0 } else { n }), tip, Tone::Accent, open);
            if !open {
                w.modes_asked = None;
            } else if resize && w.modes_asked != Some(info.display) {
                w.modes_asked = Some(info.display);
                self.viewer.list_modes(info.display);
            }
            if many || resize {
                self.demo_open(&ctx, w, "monitor", id);
                menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if many {
                        self.monitor_menu(ui, w, info, f);
                    }
                    if resize {
                        if many {
                            menu_separator(ui);
                        }
                        self.resolution_menu(ui, info.display, f.modes.filter(|m| m.display == info.display));
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
            let has = |c: &str| f.info.is_some_and(|i| i.control(c));
            if has("lock") {
                // The agent locks the machine itself (and reports it for the audit log).
                if menu_row(ui, Lead::Glyph(Glyph::Lock), "ล็อกหน้าจอเครื่องปลายทาง", "", true).clicked() {
                    self.viewer.session_control("lock", true);
                }
                if menu_row(ui, Lead::Tick(f.controls.lock_after), "ล็อกเมื่อจบการเชื่อมต่อ", "", true).clicked() {
                    self.viewer.session_control("lock_after", !f.controls.lock_after);
                }
            } else if menu_row(ui, Lead::Glyph(Glyph::Lock), "ล็อกหน้าจอเครื่องปลายทาง", "", input && !f.view_only).clicked() {
                // Older agents: press the system's lock shortcut.
                if f.info.is_some_and(|i| i.os == "macos") {
                    send_combo(&self.viewer, &[("ControlLeft", "Control"), ("MetaLeft", "Meta")], ("KeyQ", "q"));
                } else {
                    send_combo(&self.viewer, &[("MetaLeft", "Meta")], ("KeyL", "l"));
                }
                Self::flash(w, "ส่งคำสั่งล็อกหน้าจอแล้ว");
            }
            if has("restart") && menu_row(ui, Lead::Glyph(Glyph::Power), "รีสตาร์ทเครื่องปลายทาง", "", true).clicked() {
                w.confirm_restart = true;
            }
            menu_separator(ui);
            if menu_row(ui, Lead::Tick(f.view_only), "ดูอย่างเดียว", "ไม่ส่งเมาส์/คีย์บอร์ด", input).clicked() {
                self.viewer.set_view_only(!f.view_only);
                Self::flash(w, if f.view_only { "กลับมาควบคุมได้แล้ว" } else { "โหมดดูอย่างเดียว: ไม่ส่งเมาส์และคีย์บอร์ด" });
            }
            if has("block_input")
                && menu_row(ui, Lead::Tick(f.controls.block_input), "บล็อกเมาส์และคีย์บอร์ดปลายทาง", "", true)
                    .on_hover_text("คนที่เครื่องปลายทางใช้เมาส์และคีย์บอร์ดไม่ได้ จนกว่าจะปิดหรือจบการเชื่อมต่อ (กด Ctrl+Alt+Shift+Esc ที่เครื่องนั้นเพื่อยกเลิก)")
                    .clicked()
            {
                self.viewer.session_control("block_input", !f.controls.block_input);
            }
            if has("privacy")
                && menu_row(ui, Lead::Tick(f.controls.privacy), "โหมดความเป็นส่วนตัว", "จอปลายทางดำ", true)
                    .on_hover_text("หน้าจอของเครื่องปลายทางเป็นสีดำสำหรับคนที่อยู่หน้าเครื่อง แต่คุณยังเห็นภาพตามปกติ และบล็อกเมาส์/คีย์บอร์ดของเครื่องนั้นด้วย")
                    .clicked()
            {
                self.viewer.session_control("privacy", !f.controls.privacy);
            }
            if menu_row(ui, Lead::Glyph(Glyph::Refresh), "รีเฟรชภาพ", "", true).clicked() {
                self.viewer.send(serde_json::json!({"t": "refresh"}));
            }
            if menu_row(ui, Lead::Glyph(Glyph::Camera), "บันทึกภาพหน้าจอ", "", f.fb.0 > 0).clicked() {
                match self.viewer.screenshot(&crate::files::default_upload_dir(), None) {
                    Ok(p) => Self::flash(w, format!("บันทึกภาพไว้ที่ {}", p.display())),
                    Err(e) => Self::flash(w, format!("บันทึกภาพไม่สำเร็จ: {e:#}")),
                }
            }
            match f.host {
                Host::Tab => {
                    if menu_row(ui, Lead::Glyph(Glyph::PopOut), "แยกเป็นหน้าต่างใหม่", "", true).clicked() {
                        self.toggle_tab.store(true, Ordering::Relaxed);
                    }
                }
                Host::PoppedOut => {
                    if menu_row(ui, Lead::Glyph(Glyph::PopOut), "ย้ายกลับเข้าแท็บ", "", true).clicked() {
                        self.toggle_tab.store(true, Ordering::Relaxed);
                    }
                }
                Host::Window => {}
            }
            menu_separator(ui);
            let files = f.info.is_some_and(|i| i.files);
            if menu_row(ui, Lead::Tick(w.show_files), "ไฟล์บนเครื่องปลายทาง", "", files)
                .on_hover_text("ดู ดาวน์โหลด และจัดการไฟล์ หรือลากไฟล์/โฟลเดอร์มาวางบนหน้าต่างนี้เพื่อส่ง")
                .clicked()
            {
                w.show_files = !w.show_files;
                if w.show_files && self.viewer.state.lock().unwrap().browser.path.is_empty() {
                    self.viewer.list_dir("");
                }
            }
            let clip = f.info.is_some_and(|i| i.clipboard);
            if menu_row(ui, Lead::Glyph(Glyph::Clipboard), "ส่งคลิปบอร์ดไปเครื่องปลายทาง", "", clip).clicked() {
                match crate::clipboard::with(|c| c.get_text()) {
                    Ok(t) => {
                        self.viewer.send(serde_json::json!({"t": "clip", "s": t}));
                        Self::flash(w, "ส่งคลิปบอร์ดแล้ว");
                    }
                    Err(_) => Self::flash(w, "คลิปบอร์ดว่าง"),
                }
            }
            // Switch sides: both ends are desktop apps and this one runs its agent.
            if f.info.is_some_and(|i| i.has(crate::proto::FEATURE_SWITCH)) {
                menu_separator(ui);
                let can = self.viewer.can_switch();
                let hint = if f.switch_pending { "รอตอบรับ…" } else { "" };
                let r = menu_row(ui, Lead::Glyph(Glyph::Swap), "สลับฝั่ง", hint, can && !f.switch_pending);
                let r = if can { r.on_hover_text("ให้คนที่เครื่องปลายทางควบคุมเครื่องนี้แทน") } else { r.on_hover_text("ต้องเข้าสู่ระบบและเชื่อมต่อเซิร์ฟเวอร์อยู่") };
                if r.clicked() {
                    w.confirm_switch = true;
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
            // AV1 only when the remote machine can encode it (info.codecs, agents from 0.6).
            menu_header(ui, "Codec วิดีโอ");
            let want = self.viewer.state.lock().unwrap().want_codec;
            let av1 = f.info.is_some_and(|i| i.codecs.iter().any(|c| c == "av1"));
            // While AV1 is unavailable the remembered AV1 choice shows as H.264, the codec in use.
            let shown = if av1 { want } else { Codec::H264 };
            for (c, label, hint, enabled) in [(Codec::H264, "H.264", "", true), (Codec::Av1, "AV1", if av1 { "" } else { "ปลายทางไม่รองรับ" }, av1)] {
                if menu_row(ui, Lead::Check(shown == c), label, hint, enabled).clicked() && want != c {
                    self.viewer.set_codec(c);
                }
            }
            menu_separator(ui);
            menu_header(ui, "ขนาดภาพ");
            for (s, label, hint) in [(Scale::Stretch, "ยืดให้เต็มหน้าต่าง", ""), (Scale::Shrink, "ย่อให้พอดี", "ไม่ขยายเกิน 1:1"), (Scale::Original, "ขนาดจริง", "1:1")] {
                if menu_row(ui, Lead::Check(w.scale == s), label, hint, true).clicked() {
                    w.scale = s;
                }
            }
            if menu_row(ui, Lead::Tick(w.follow_cursor), "เลื่อนภาพตามเคอร์เซอร์ปลายทาง", "", true)
                .on_hover_text("เมื่อภาพใหญ่กว่าหน้าต่าง ให้เลื่อนตามตัวชี้ของเครื่องปลายทางแทนตัวชี้ของเรา")
                .clicked()
            {
                w.follow_cursor = !w.follow_cursor;
            }
            menu_separator(ui);
            if menu_row(ui, Lead::Tick(w.show_quality), "แสดงข้อมูลคุณภาพการเชื่อมต่อ", "", true).clicked() {
                w.show_quality = !w.show_quality;
            }
            menu_separator(ui);
            menu_footer(ui, &format!("Codec ที่ใช้: {}", codec_label(f.codec, f.encoder)));
        });

        // Keyboard: how ⌘/Ctrl reach the remote OS.
        if input {
            let id = egui::Id::unique(("menu-keyboard", self.viewport));
            let resp = bar_button(ui, Glyph::Keyboard, "แป้นพิมพ์", Tone::Accent, Popup::is_id_open(&ctx, id));
            self.demo_open(&ctx, w, "keyboard", id);
            menu(&resp, id, pal).show(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                let remote_mac = f.info.is_some_and(|i| i.os == "macos");
                if f.info.is_some_and(|i| i.has("keymode")) {
                    menu_header(ui, "โหมดแป้นพิมพ์");
                    let modes = [(false, "พิมพ์ตามตัวอักษร", "Translate"), (true, "ตามตำแหน่งปุ่ม", "Map")];
                    for (map, label, hint) in modes {
                        let r = menu_row(ui, Lead::Check(w.key_map == map), label, hint, true).on_hover_text(if map {
                            "กดปุ่มตามตำแหน่งจริงบนแป้นพิมพ์ ใช้ภาษาที่ตั้งไว้บนเครื่องปลายทาง (เหมาะกับเกม)"
                        } else {
                            "ส่งตัวอักษรที่พิมพ์ ใช้ได้ทุกภาษาโดยไม่ต้องตั้งภาษาบนเครื่องปลายทาง"
                        });
                        if r.clicked() && w.key_map != map {
                            w.key_map = map;
                            w.input.mods_down.clear();
                            self.viewer.set_key_map(map);
                        }
                    }
                    menu_separator(ui);
                }
                menu_header(ui, "ปุ่ม ⌘ และ Ctrl");
                let hint = if remote_mac { "" } else { "⌘ → Ctrl" };
                if menu_row(ui, Lead::Check(!w.raw_keys), "แมปตามระบบปลายทาง", hint, true).clicked() {
                    w.raw_keys = false;
                }
                if menu_row(ui, Lead::Check(w.raw_keys), "ส่งปุ่มตามจริง", "", true).clicked() {
                    w.raw_keys = true;
                }
                menu_separator(ui);
                if f.info.is_some_and(|i| i.has("relmouse")) {
                    if menu_row(ui, Lead::Tick(w.relative), "เมาส์แบบสัมพัทธ์ (เกม/3D)", "Ctrl+Alt+M", true).clicked() {
                        let on = !w.relative;
                        self.set_relative(&ctx, w, on);
                    }
                    menu_separator(ui);
                }
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

        // Chat with the person at the remote machine, with the number of unread messages.
        if f.info.is_some_and(Info::chat) {
            let resp = bar_button(ui, Glyph::Chat, "แชท", if w.show_chat { Tone::Neutral } else { Tone::Accent }, false);
            if f.chat_unread > 0 {
                let c = resp.rect.right_top() + vec2(-3.0, 3.0);
                ui.painter().circle_filled(c, 7.5, pal.danger);
                ui.painter().text(c, Align2::CENTER_CENTER, f.chat_unread.min(9).to_string(), semi(10.0), Color32::WHITE);
            }
            if resp.clicked() {
                w.show_chat = !w.show_chat;
            }
        }

        // Voice call: call, then (once accepted) mute the microphone and hang up.
        if f.info.is_some_and(Info::voice) {
            match f.call {
                CallState::Idle => {
                    if bar_button(ui, Glyph::Phone, "โทรคุยด้วยเสียง", Tone::Accent, false).clicked() {
                        self.viewer.call();
                    }
                }
                CallState::Calling(_) => {
                    if bar_button(ui, Glyph::PhoneOff, "กำลังรอปลายทางรับสาย · คลิกเพื่อยกเลิก", Tone::Danger, false).clicked() {
                        self.viewer.hangup();
                    }
                }
                CallState::Active { muted, .. } => {
                    let (g, tip) = if muted { (Glyph::MicOff, "เปิดไมค์") } else { (Glyph::Mic, "ปิดไมค์") };
                    if bar_button(ui, g, tip, if muted { Tone::Neutral } else { Tone::Accent }, false).clicked() {
                        self.viewer.set_mic_muted(!muted);
                    }
                    if bar_button(ui, Glyph::PhoneOff, "วางสาย", Tone::Danger, false).clicked() {
                        self.viewer.hangup();
                    }
                }
            }
        }

        // Speaker: only when the agent streams audio to us.
        if f.info.is_some_and(|i| i.audio) {
            let (g, tip) = if f.muted { (Glyph::SpeakerOff, "เปิดเสียง") } else { (Glyph::Speaker, "ปิดเสียง") };
            if bar_button(ui, g, tip, if f.muted { Tone::Neutral } else { Tone::Accent }, false).clicked() {
                self.viewer.set_muted(!f.muted);
            }
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
    /// Chat with the person at the remote machine (this session only; nothing is stored).
    fn chat_panel(&self, ui: &mut Ui, w: &mut WinUi, chat: &[ChatLine], name: &str) {
        let pal = Palette::of(ui);
        egui::Panel::right(egui::Id::unique(("chat-panel", self.viewport)))
            .exact_size(300.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(12, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    glyph(ui, r, Glyph::Chat, pal.accent);
                    ui.label(RichText::new("แชท").font(semi(14.0)).color(pal.fg));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_glyph_button(ui, Glyph::Close, "ปิด").clicked() {
                            w.show_chat = false;
                        }
                    });
                });
                ui.add_space(4.0);
                let list_h = (ui.available_height() - 52.0).max(60.0);
                egui::ScrollArea::vertical()
                    .id_salt(("chat-list", self.viewport))
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if chat.is_empty() {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new(format!("พิมพ์ข้อความถึงคนที่ {name}")).size(13.0).color(pal.muted));
                                ui.label(RichText::new("ข้อความเข้ารหัสต้นทาง-ปลายทาง ไม่ถูกเก็บไว้ที่เซิร์ฟเวอร์").size(11.5).color(pal.muted));
                            });
                        }
                        for l in chat {
                            cm_ui::chat_bubble(ui, l.mine, &l.text, l.at);
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut w.chat_draft)
                            .hint_text("ข้อความ")
                            .desired_width(ui.available_width() - 48.0)
                            .margin(Margin::symmetric(10, 8))
                            .char_limit(crate::proto::CHAT_MAX),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let send = dialog_button(ui, "ส่ง", true, !w.chat_draft.trim().is_empty()).clicked();
                    if (enter || send) && !w.chat_draft.trim().is_empty() {
                        self.viewer.send_chat(&std::mem::take(&mut w.chat_draft));
                        edit.request_focus();
                    }
                });
            });
    }

    /// Switch sides: the person here confirms before the other side may control this machine.
    fn switch_dialog(&self, ctx: &egui::Context, w: &mut WinUi, name: &str) {
        let mut answer = None;
        egui::Modal::new(egui::Id::unique(("switch-confirm", self.viewport))).backdrop_color(Color32::from_black_alpha(110)).show(ctx, |ui| {
            let pal = Palette::of(ui);
            ui.set_width(380.0);
            ui.spacing_mut().item_spacing.y = 10.0;
            dialog_title(ui, Glyph::Swap, "สลับฝั่ง", pal.accent);
            ui.label(
                RichText::new(format!(
                    "คนที่ใช้ {name} จะได้รับคำเชิญให้ควบคุมเครื่องนี้ ถ้าเขาตอบรับ หน้าต่างนี้จะปิดและเขาจะควบคุมเครื่องนี้ได้ทันที โดยไม่ต้องใช้รหัสผ่าน (คำเชิญใช้ได้ครั้งเดียว ภายใน 1 นาที)"
                ))
                .size(13.5)
                .color(pal.muted),
            );
            ui.label(RichText::new("ตัดการเชื่อมต่อได้ทุกเมื่อจากหน้าต่างการเชื่อมต่อเข้า").size(12.5).color(pal.muted));
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if dialog_button(ui, "สลับฝั่ง", true, true).clicked() {
                    answer = Some(true);
                }
                if dialog_button(ui, "ยกเลิก", false, true).clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(yes) = answer {
            w.confirm_switch = false;
            if yes {
                self.viewer.switch_sides();
                Self::flash(w, "ส่งคำเชิญแล้ว รออีกฝ่ายตอบรับ");
            }
        }
    }

    /// Call status under the toolbar while calling or in a call.
    fn call_badge(&self, ctx: &egui::Context, call: CallState) {
        let text = match call {
            CallState::Calling(_) => "กำลังโทร… รอปลายทางรับสาย".to_string(),
            CallState::Active { since, muted, remote_muted } => {
                let t = since.elapsed().as_secs();
                let mut s = format!("กำลังคุย  {:02}:{:02}", t / 60, t % 60);
                if muted {
                    s.push_str(" · ไมค์ของคุณปิดอยู่");
                }
                if remote_muted {
                    s.push_str(" · อีกฝ่ายปิดไมค์");
                }
                s
            }
            CallState::Idle => return,
        };
        egui::Area::new(egui::Id::unique(("call-badge", self.viewport)))
            .order(Order::Foreground)
            .interactable(false)
            .anchor(Align2::RIGHT_TOP, [-12.0, 12.0])
            .show(ctx, |ui| {
                let pal = Palette::of(ui);
                Frame::new().fill(pal.ok.gamma_multiply(0.92)).corner_radius(14).inner_margin(Margin::symmetric(12, 5)).show(ui, |ui| {
                    ui.label(RichText::new(text).font(semi(12.5)).color(Color32::WHITE));
                });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn recording_badge(&self, ctx: &egui::Context, top: f32) {
        egui::Area::new(egui::Id::unique(("recording", self.viewport)))
            .order(Order::Middle)
            .interactable(true)
            .anchor(Align2::RIGHT_TOP, [-12.0, 10.0 + top])
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

    fn quality_monitor(&self, ctx: &egui::Context, f: &Frame1, top: f32) {
        egui::Area::new(egui::Id::unique(("quality", self.viewport)))
            .order(Order::Middle)
            .interactable(false)
            .anchor(Align2::LEFT_TOP, [10.0, 10.0 + top])
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
                        ui.label(val(codec_label(f.codec, "")));
                        ui.end_row();
                        ui.label(key("ตัวเข้ารหัส"));
                        ui.label(val(encoder_label(f.encoder)));
                        ui.end_row();
                        ui.label(key("เสียง"));
                        ui.label(val(audio_label(f.info.is_some_and(|i| i.audio), f.muted, f.audio_level)));
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

    fn transfers(&self, ctx: &egui::Context, transfers: &[Transfer]) {
        let shown: Vec<_> = transfers.iter().rev().take(4).collect();
        if shown.is_empty() {
            return;
        }
        let finished = transfers.iter().any(|t| t.status != TransferStatus::Active);
        egui::Area::new(egui::Id::unique(("transfers", self.viewport)))
            .order(Order::Middle)
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
                        ui.set_width(300.0);
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.spacing_mut().interact_size.y = 18.0;
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            glyph(ui, r, Glyph::Folder, pal.accent);
                            ui.label(RichText::new("การโอนไฟล์").font(semi(13.5)).color(pal.fg));
                            if finished {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let clear = ui.add(Button::new(RichText::new("ล้างที่เสร็จแล้ว").size(12.0).color(pal.muted)).frame(false));
                                    if clear.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        self.viewer.clear_finished();
                                    }
                                });
                            }
                        });
                        for t in shown {
                            self.transfer_row(ui, t, pal);
                        }
                    });
            });
    }

    fn transfer_row(&self, ui: &mut Ui, t: &Transfer, pal: Palette) {
        let pct = |done: u64, size: u64| (done.min(size) * 100).checked_div(size).unwrap_or(0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
            glyph(ui, r, if t.up { Glyph::Upload } else { Glyph::Download }, pal.muted);
            let name = if t.folder { format!("{}/", t.name) } else { t.name.clone() };
            ui.add(egui::Label::new(RichText::new(name).size(12.5).color(pal.fg)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = match &t.status {
                    TransferStatus::Active => (format!("{}%", pct(t.sent, t.size)), pal.muted),
                    TransferStatus::Done(_) if t.failed > 0 => (format!("ข้าม {} ไฟล์", t.failed), pal.warn),
                    TransferStatus::Done(_) => ("เสร็จแล้ว".into(), pal.ok),
                    TransferStatus::Failed(_) => ("ไม่สำเร็จ".into(), pal.danger),
                };
                if t.status == TransferStatus::Active {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                    glyph(ui, r.shrink(2.0), Glyph::Close, if resp.hovered() { pal.danger } else { pal.muted });
                    if resp.on_hover_text("ยกเลิก").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.viewer.cancel(t.id);
                    }
                }
                ui.label(RichText::new(text).size(12.0).color(color));
            });
        });
        let bar = |ui: &mut Ui, frac: f32, h: f32| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
            ui.painter().rect_filled(r, h / 2.0, pal.field);
            ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * frac.clamp(0.0, 1.0), h)), h / 2.0, pal.accent);
        };
        match &t.status {
            TransferStatus::Active => {
                bar(ui, if t.size > 0 { t.sent as f32 / t.size as f32 } else { 0.0 }, 4.0);
                if t.folder {
                    // Per-file progress under the total.
                    let done = format!("{} / {} ไฟล์", t.files_done, t.files);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(done).size(11.5).color(pal.muted));
                        if !t.current.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&t.current).size(11.5).color(pal.muted)).truncate());
                        }
                    });
                    if t.cur_size > 0 {
                        bar(ui, t.cur_sent as f32 / t.cur_size as f32, 2.0);
                    }
                }
            }
            TransferStatus::Failed(e) => {
                ui.add(egui::Label::new(RichText::new(e).size(11.5).color(pal.danger)).truncate());
            }
            TransferStatus::Done(path) => {
                if !t.up && !path.is_empty() {
                    ui.add(egui::Label::new(RichText::new(path).size(11.0).color(pal.muted)).truncate()).on_hover_text(path);
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Dialogs (connecting, password, closed)

    fn dialog(&self, ctx: &egui::Context, w: &mut WinUi, phase: &Phase, error: Option<&str>, name: &str, reconnect: Option<Reconnect>) {
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
                            Phase::Reconnecting => {
                                dialog_title(ui, Glyph::Power, "เครื่องปลายทางกำลังรีสตาร์ท", pal.accent);
                                let text = match reconnect {
                                    Some(r) => {
                                        let left = r.next.saturating_duration_since(Instant::now()).as_secs();
                                        if left > 0 {
                                            format!("รอให้ {name} กลับมาออนไลน์ · ลองครั้งที่ {} ใน {left} วินาที", r.attempt)
                                        } else {
                                            format!("กำลังเชื่อมต่อกับ {name} อีกครั้ง (ครั้งที่ {})…", r.attempt)
                                        }
                                    }
                                    None => format!("รอให้ {name} กลับมาออนไลน์…"),
                                };
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(18.0).color(pal.accent));
                                    ui.label(RichText::new(text).size(14.0).color(pal.fg));
                                });
                                ui.label(RichText::new("จะเชื่อมต่อให้อัตโนมัติโดยไม่ต้องใส่รหัสผ่าน (ภายใน 15 นาที)").size(12.5).color(pal.muted));
                                ui.add_space(6.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if dialog_button(ui, "เลิกรอ", false, true).clicked() {
                                        self.close_window();
                                    }
                                });
                            }
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

    /// "Restart the remote machine?" — it goes away for a minute or two, so ask first.
    fn restart_dialog(&self, ctx: &egui::Context, w: &mut WinUi, name: &str) {
        egui::Area::new(egui::Id::unique(("restart-confirm", self.viewport)))
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
                        dialog_title(ui, Glyph::Power, "รีสตาร์ทเครื่องปลายทาง?", pal.danger);
                        let body = format!("{name} จะปิดโปรแกรมทั้งหมดแล้วเริ่มใหม่ งานที่ยังไม่บันทึกอาจหายไป เมื่อเครื่องกลับมาออนไลน์ หน้าต่างนี้จะเชื่อมต่อให้อีกครั้ง");
                        ui.label(RichText::new(body).size(13.5).color(pal.muted));
                        ui.add_space(6.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if dialog_button(ui, "รีสตาร์ท", true, true).clicked() {
                                self.viewer.session_control("restart", true);
                                w.confirm_restart = false;
                            }
                            if dialog_button(ui, "ยกเลิก", false, true).clicked() {
                                w.confirm_restart = false;
                            }
                        });
                    });
            });
    }

    /// Chips in the bottom-left corner for the controls in effect on the remote machine.
    fn control_badges(&self, ctx: &egui::Context, f: &Frame1) {
        let chips: Vec<&str> = [
            (f.view_only, "ดูอย่างเดียว"),
            (f.controls.privacy, "จอปลายทางถูกซ่อน"),
            (f.controls.block_input && !f.controls.privacy, "บล็อกเมาส์/คีย์บอร์ดปลายทาง"),
            (f.controls.lock_after, "ล็อกเมื่อจบ"),
        ]
        .into_iter()
        .filter_map(|(on, t)| on.then_some(t))
        .collect();
        if chips.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::unique(("controls", self.viewport)))
            .order(Order::Middle)
            .interactable(false)
            .anchor(Align2::LEFT_BOTTOM, [12.0, -12.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for c in chips {
                        Frame::new().fill(Color32::from_black_alpha(175)).corner_radius(12).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                            ui.label(RichText::new(c).font(semi(12.5)).color(Color32::WHITE));
                        });
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
    /// original, shrink, map, cursor, dark, password, connecting, closed).
    fn demo_state(&self, ctx: &egui::Context, w: &mut WinUi) {
        if w.demo_done {
            return;
        }
        w.demo_done = true;
        let Ok(spec) = std::env::var("AEGIRON_UI_VIEWER_STATE") else { return };
        let mut st = self.viewer.state.lock().unwrap();
        if st.codec.is_empty() {
            st.codec = "h264".into();
            st.encoder = "videotoolbox".into();
        }
        for flag in spec.split(',').map(str::trim) {
            match flag {
                "pinned" => w.pinned = true,
                "collapsed" => w.bar_seen = Some(-100.0),
                "quality" => w.show_quality = true,
                "original" => w.scale = Scale::Original,
                "shrink" => w.scale = Scale::Shrink,
                "map" => w.key_map = true,
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
                "files" => {
                    w.show_files = true;
                    let e = |name: &str, dir: bool, size: u64| crate::viewer::RemoteEntry { name: name.into(), dir, size };
                    st.browser = RemoteDir {
                        path: "C:\\Users\\บัญชี\\Documents".into(),
                        parent: Some("C:\\Users\\บัญชี".into()),
                        sep: "\\".into(),
                        entries: vec![e("งบประมาณ 2569", true, 0), e("รูปภาพ", true, 0), e("รายงานประจำเดือน.xlsx", false, 482_133), e("สัญญา.pdf", false, 2_301_442)],
                        ..Default::default()
                    };
                    st.transfers = vec![crate::viewer::Transfer {
                        id: 2,
                        name: "ภาพถ่าย".into(),
                        up: true,
                        folder: true,
                        size: 84 << 20,
                        sent: 51 << 20,
                        files: 120,
                        files_done: 71,
                        current: "2025/ทริป/IMG_0412.jpg".into(),
                        cur_size: 3 << 20,
                        cur_sent: 1 << 20,
                        ..Default::default()
                    }];
                    st.clip_offer = Some(vec![crate::viewer::ClipEntry { name: "งบประมาณ 2569".into(), path: String::new(), dir: true, size: 0 }]);
                }
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

fn codec_label(codec: &str, encoder: &str) -> String {
    match (codec, encoder) {
        ("", _) => "–".into(),
        ("h264", e) if !e.is_empty() => format!("H264 · {}", encoder_label(e)),
        ("av1", e) if !e.is_empty() => format!("AV1 · {}", encoder_label(e)),
        (c, _) => c.to_uppercase(),
    }
}

fn encoder_label(encoder: &str) -> String {
    match encoder {
        "" => "–".into(),
        "videotoolbox" => "VideoToolbox (ฮาร์ดแวร์)".into(),
        "mediafoundation" => "Media Foundation (ฮาร์ดแวร์)".into(),
        "openh264" => "OpenH264 (ซอฟต์แวร์)".into(),
        "jpeg" => "JPEG".into(),
        e => e.to_string(),
    }
}

fn audio_label(on: bool, muted: bool, level: Option<f32>) -> String {
    match (on, muted, level) {
        (false, _, _) => "ไม่มี".into(),
        (true, true, _) => "Opus · ปิดเสียงอยู่".into(),
        (true, false, None) => "Opus".into(),
        // RMS -> rough dBFS so quiet sound still shows.
        (true, false, Some(l)) => format!("Opus · {:.0} dB", 20.0 * l.max(1e-5).log10()),
    }
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
    Download,
    Upload,
    Trash,
    Pencil,
    FolderPlus,
    Up,
    File,
    Speaker,
    SpeakerOff,
    Chat,
    Phone,
    PhoneOff,
    Mic,
    MicOff,
    Swap,
    Power,
    Camera,
    /// A window with an arrow out of it (open in a new window).
    PopOut,
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
            p.text(at(8.0, 6.9), Align2::CENTER_CENTER, if n == 0 { "*".to_string() } else { n.to_string() }, semi(8.5 * s), color);
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
        Glyph::Power => {
            p.add(Shape::line(arc(at(8.0, 8.8), 5.6 * s, -0.17 * TAU, 0.67 * TAU), st));
            line(&[(8.0, 1.6), (8.0, 7.6)]);
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
        Glyph::Download | Glyph::Upload => {
            if g == Glyph::Download {
                line(&[(8.0, 1.5), (8.0, 10.5)]);
                line(&[(4.2, 6.8), (8.0, 10.6), (11.8, 6.8)]);
            } else {
                line(&[(8.0, 10.5), (8.0, 1.5)]);
                line(&[(4.2, 5.2), (8.0, 1.4), (11.8, 5.2)]);
            }
            line(&[(2.0, 11.0), (2.0, 14.2), (14.0, 14.2), (14.0, 11.0)]);
        }
        Glyph::Trash => {
            line(&[(2.0, 3.8), (14.0, 3.8)]);
            line(&[(6.0, 3.8), (6.0, 1.8), (10.0, 1.8), (10.0, 3.8)]);
            line(&[(3.5, 3.8), (4.5, 14.5), (11.5, 14.5), (12.5, 3.8)]);
            line(&[(6.6, 6.5), (6.9, 11.8)]);
            line(&[(9.4, 6.5), (9.1, 11.8)]);
        }
        Glyph::Pencil => {
            line(&[(2.0, 14.0), (2.5, 10.8), (11.2, 2.1), (13.9, 4.8), (5.2, 13.5), (2.0, 14.0)]);
            line(&[(9.4, 3.9), (12.1, 6.6)]);
        }
        Glyph::FolderPlus => {
            line(&[(1.5, 13.5), (1.5, 2.8), (6.0, 2.8), (7.6, 4.6), (14.5, 4.6), (14.5, 13.5), (1.5, 13.5)]);
            line(&[(8.0, 6.8), (8.0, 11.6)]);
            line(&[(5.6, 9.2), (10.4, 9.2)]);
        }
        Glyph::Up => {
            line(&[(8.0, 14.0), (8.0, 2.5)]);
            line(&[(3.5, 7.0), (8.0, 2.4), (12.5, 7.0)]);
        }
        Glyph::File => {
            line(&[(3.0, 1.5), (9.5, 1.5), (13.0, 5.0), (13.0, 14.5), (3.0, 14.5), (3.0, 1.5)]);
            line(&[(9.5, 1.5), (9.5, 5.0), (13.0, 5.0)]);
        }
        Glyph::Chat => {
            p.add(Shape::closed_line(
                [(2.0, 3.0), (14.0, 3.0), (14.0, 11.0), (7.5, 11.0), (4.0, 14.0), (4.5, 11.0), (2.0, 11.0)].iter().map(|&(x, y)| at(x, y)).collect(),
                st,
            ));
            line(&[(5.0, 6.2), (11.0, 6.2)]);
            line(&[(5.0, 8.4), (9.0, 8.4)]);
        }
        Glyph::Phone | Glyph::PhoneOff => {
            // A handset; hung up it lies flat and red buttons get it.
            let pts: &[(f32, f32)] = if g == Glyph::Phone {
                &[(3.2, 2.0), (5.8, 2.0), (6.8, 5.2), (5.2, 6.6), (9.4, 10.8), (10.8, 9.2), (14.0, 10.2), (14.0, 12.8), (12.6, 14.2), (2.0, 3.4), (3.2, 2.0)]
            } else {
                &[(1.5, 9.5), (2.5, 7.0), (8.0, 5.5), (13.5, 7.0), (14.5, 9.5), (13.5, 11.0), (10.6, 10.2), (10.4, 8.4), (5.6, 8.4), (5.4, 10.2), (2.5, 11.0), (1.5, 9.5)]
            };
            line(pts);
        }
        Glyph::Mic | Glyph::MicOff => {
            p.rect_stroke(Rect::from_min_max(at(5.5, 1.5), at(10.5, 10.0)), 2.5 * s, st, egui::StrokeKind::Middle);
            line(&[(3.0, 7.5), (3.6, 10.0), (5.5, 12.0), (8.0, 12.8), (10.5, 12.0), (12.4, 10.0), (13.0, 7.5)]);
            line(&[(8.0, 12.8), (8.0, 15.0)]);
            if g == Glyph::MicOff {
                line(&[(2.0, 2.0), (14.0, 14.5)]);
            }
        }
        Glyph::Swap => {
            line(&[(2.0, 5.0), (13.5, 5.0)]);
            line(&[(10.5, 2.0), (13.5, 5.0), (10.5, 8.0)]);
            line(&[(14.0, 11.0), (2.5, 11.0)]);
            line(&[(5.5, 8.0), (2.5, 11.0), (5.5, 14.0)]);
        }
        Glyph::Camera => {
            p.rect_stroke(Rect::from_min_max(at(1.0, 4.0), at(15.0, 13.5)), 2.0 * s, st, egui::StrokeKind::Middle);
            line(&[(5.0, 4.0), (6.3, 2.0), (9.7, 2.0), (11.0, 4.0)]);
            p.circle_stroke(at(8.0, 8.8), 2.7 * s, st);
        }
        Glyph::PopOut => {
            line(&[(7.0, 2.0), (2.0, 2.0), (2.0, 14.0), (14.0, 14.0), (14.0, 9.0)]);
            line(&[(8.0, 8.0), (14.5, 1.5)]);
            line(&[(10.0, 1.5), (14.5, 1.5), (14.5, 6.0)]);
        }
        Glyph::Speaker | Glyph::SpeakerOff => {
            p.add(Shape::convex_polygon(
                [(1.5, 5.8), (4.5, 5.8), (8.5, 2.2), (8.5, 13.8), (4.5, 10.2), (1.5, 10.2)].iter().map(|&(x, y)| at(x, y)).collect(),
                color,
                Stroke::NONE,
            ));
            if g == Glyph::Speaker {
                p.add(Shape::line(arc(at(8.5, 8.0), 3.2 * s, -0.16 * TAU, 0.16 * TAU), st));
                p.add(Shape::line(arc(at(8.5, 8.0), 6.0 * s, -0.16 * TAU, 0.16 * TAU), st));
            } else {
                line(&[(10.8, 5.7), (15.0, 10.3)]);
                line(&[(15.0, 5.7), (10.8, 10.3)]);
            }
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

/// How keys and the pointer reach the remote machine.
#[derive(Clone, Copy)]
struct KeyMap {
    remote_mac: bool,
    /// Send Ctrl as Ctrl and ⌘ as Meta, whatever the remote OS.
    raw: bool,
    /// "map" mode: physical keys, each press and release (otherwise characters are typed).
    map: bool,
    /// Relative mouse: raw motion as `mr`, buttons without a position.
    relative: bool,
}

/// Size of the remote screen (`frame`, in pixels) in a window area of `avail` points.
fn view_size(scale: Scale, avail: egui::Vec2, frame: egui::Vec2, ppp: f32) -> egui::Vec2 {
    let fit = (avail.x / frame.x).min(avail.y / frame.y);
    let one = 1.0 / ppp;
    frame
        * match scale {
            Scale::Stretch => fit,
            Scale::Shrink => fit.min(one),
            Scale::Original => one,
        }
}

/// Where a view of `size` goes in `area`: centered on an axis where it fits, else panned
/// (`pan` 0 = start, 1 = end).
fn place(area: Rect, size: egui::Vec2, pan: egui::Vec2) -> Rect {
    let mut min = area.center() - size / 2.0;
    if size.x > area.width() {
        min.x = area.left() - pan.x.clamp(0.0, 1.0) * (size.x - area.width());
    }
    if size.y > area.height() {
        min.y = area.top() - pan.y.clamp(0.0, 1.0) * (size.y - area.height());
    }
    Rect::from_min_size(min.round(), size)
}

/// Pan (0..1) along one axis so a point at `at` (in view units from its start) stays at least
/// a margin inside a window `view` long; moves only as far as needed.
fn follow(pan: f32, size: f32, view: f32, at: f32) -> f32 {
    let room = size - view;
    if room <= 0.0 {
        return pan;
    }
    let margin = (view * 0.15).min(80.0);
    let mut offset = pan.clamp(0.0, 1.0) * room;
    if at - offset < margin {
        offset = at - margin;
    } else if at - offset > view - margin {
        offset = at - (view - margin);
    }
    (offset / room).clamp(0.0, 1.0)
}

/// Uploads changed parts of `fb` into `tex` (the whole picture after a resize, or when many
/// changes piled up while the window or tab was not drawn).
fn sync_texture(ctx: &egui::Context, tex: &mut Option<egui::TextureHandle>, fb: &mut crate::viewer::Framebuffer, name: &str) {
    if fb.w == 0 {
        return;
    }
    let opts = egui::TextureOptions::LINEAR;
    if tex.is_none() || fb.resized || fb.dirty.len() > 64 {
        let img = egui::ColorImage::from_rgba_unmultiplied([fb.w, fb.h], &fb.rgba);
        *tex = Some(ctx.load_texture(name, img, opts));
        fb.resized = false;
        fb.dirty.clear();
    } else if let Some(t) = tex.as_mut() {
        for [x, y, dw, dh] in std::mem::take(&mut fb.dirty) {
            if dw == 0 || dh == 0 {
                continue;
            }
            let mut sub = Vec::with_capacity(dw * dh * 4);
            for row in y..y + dh {
                let i = (row * fb.w + x) * 4;
                sub.extend_from_slice(&fb.rgba[i..i + dw * 4]);
            }
            t.set_partial([x, y], egui::ColorImage::from_rgba_unmultiplied([dw, dh], &sub), opts);
        }
    }
}

/// Ctrl+Alt+M was pressed (relative mouse on/off); the key is taken out of this frame's input.
fn take_relative_hotkey(ui: &Ui) -> bool {
    ui.input_mut(|i| {
        let hit = |e: &egui::Event| matches!(e, egui::Event::Key { key: egui::Key::M, modifiers, .. } if modifiers.ctrl && modifiers.alt);
        let pressed = i.events.iter().any(|e| hit(e) && matches!(e, egui::Event::Key { pressed: true, repeat: false, .. }));
        i.events.retain(|e| !hit(e));
        // Ctrl+Alt+M may also type a character (e.g. µ on a Mac): drop it.
        if pressed {
            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
        }
        pressed
    })
}

/// Presses modifiers, taps `key`, releases in reverse order.
fn send_combo(v: &Viewer, mods: &[(&str, &str)], key: (&str, &str)) {
    combo_events(mods, key).into_iter().for_each(|e| v.send(e));
}

fn combo_events(mods: &[(&str, &str)], key: (&str, &str)) -> Vec<serde_json::Value> {
    let k = |code: &str, name: &str, down: bool| serde_json::json!({"t": "key", "code": code, "key": name, "down": down});
    let mut out: Vec<_> = mods.iter().map(|(c, n)| k(c, n, true)).collect();
    out.push(k(key.0, key.1, true));
    out.push(k(key.0, key.1, false));
    out.extend(mods.iter().rev().map(|(c, n)| k(c, n, false)));
    out
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

/// Every key that types something, by its position (map mode): (KeyboardEvent.code, key).
fn physical_code(k: egui::Key) -> Option<(String, String)> {
    use egui::Key::*;
    if let Some((code, name)) = named_key(k) {
        return Some((code.into(), name.into()));
    }
    if let Some(c) = shortcut_key(k) {
        return Some(c);
    }
    let (code, ch) = match k {
        Space => ("Space", " "),
        Minus => ("Minus", "-"),
        Equals => ("Equal", "="),
        OpenBracket => ("BracketLeft", "["),
        CloseBracket => ("BracketRight", "]"),
        Backslash => ("Backslash", "\\"),
        Semicolon => ("Semicolon", ";"),
        Quote => ("Quote", "'"),
        Backtick => ("Backquote", "`"),
        Comma => ("Comma", ","),
        Period => ("Period", "."),
        Slash => ("Slash", "/"),
        _ => return None,
    };
    Some((code.into(), ch.into()))
}

/// Presses and releases remote modifiers so exactly `want` is held (map mode).
fn sync_mods(v: &Viewer, st: &mut InputState, want: Vec<(&'static str, &'static str)>) {
    let k = |(code, name): (&str, &str), down: bool| serde_json::json!({"t": "key", "code": code, "key": name, "down": down});
    for m in st.mods_down.iter().filter(|m| !want.contains(m)) {
        v.send(k(*m, false));
    }
    for m in want.iter().filter(|m| !st.mods_down.contains(m)) {
        v.send(k(*m, true));
    }
    st.mods_down = want;
}

/// Sends pointer and keyboard input on the remote screen. `rect` is where the remote screen is
/// drawn, `visible` the part of the window showing it, `display` the extra display stream it
/// shows (None = the main one). Pointer events over the toolbar, its menus or other overlays
/// stay local (hit-tested against egui's layers from the last frame), and nothing is
/// forwarded while a menu is open.
fn forward_input(v: &Viewer, ui: &Ui, rect: Rect, visible: Rect, keys: KeyMap, st: &mut InputState, display: Option<usize>) {
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
    // Pointer messages of an extra display window say which display they are for.
    let with_d = |mut m: serde_json::Value| {
        if let Some(d) = display {
            m["d"] = serde_json::json!(d);
        }
        m
    };
    let (events, hover) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    if !keys.map && !st.mods_down.is_empty() {
        sync_mods(v, st, Vec::new());
    }
    let mut moved = None;
    let mut rel = egui::Vec2::ZERO;
    for e in events {
        match e {
            // Relative mode: raw motion, wherever the (hidden, captured) cursor is.
            egui::Event::MouseMoved(d) if keys.relative && !menu_open => rel += d,
            egui::Event::PointerMoved(p) if !keys.relative && (on_screen(p) || st.held != 0) => moved = Some(norm(p)),
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                let b: u8 = match button {
                    egui::PointerButton::Primary => 0,
                    egui::PointerButton::Middle => 1,
                    egui::PointerButton::Secondary => 2,
                    _ => continue,
                };
                // Presses must land on the screen; a release goes out only if its press did.
                let lands = if keys.relative { !menu_open } else { on_screen(pos) };
                if pressed && !lands || !pressed && st.held & (1 << b) == 0 {
                    continue;
                }
                if pressed {
                    st.held |= 1 << b;
                } else {
                    st.held &= !(1 << b);
                }
                moved = None;
                if keys.relative {
                    v.send(serde_json::json!({"t": "mb", "b": b, "down": pressed}));
                } else {
                    let (x, y) = norm(pos);
                    v.send(with_d(serde_json::json!({"t": "mb", "b": b, "down": pressed, "x": x, "y": y})));
                }
            }
            egui::Event::MouseWheel { unit, delta, .. } if keys.relative || hover.is_some_and(on_screen) => {
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
            // Map mode: every key by position, down and up; characters are not typed.
            egui::Event::Text(_) if keys.map => {}
            egui::Event::Key { key, physical_key, pressed, modifiers, .. } if keys.map => {
                sync_mods(v, st, remote_mods(modifiers, keys));
                if let Some((code, name)) = physical_code(physical_key.unwrap_or(key)) {
                    v.send(serde_json::json!({"t": "key", "code": code, "key": name, "down": pressed}));
                }
            }
            egui::Event::Copy | egui::Event::Cut if keys.map => {
                sync_mods(v, st, remote_mods(ui.input(|i| i.modifiers), keys));
                let k = if matches!(e, egui::Event::Copy) { ("KeyC", "c") } else { ("KeyX", "x") };
                send_combo(v, &[], k);
            }
            egui::Event::Text(t) => {
                let t: String = t.chars().filter(|c| !c.is_control()).collect();
                if !t.is_empty() {
                    v.send(serde_json::json!({"t": "text", "s": t}));
                }
            }
            egui::Event::Copy => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), keys), ("KeyC", "c")),
            egui::Event::Cut => send_combo(v, &remote_mods(ui.input(|i| i.modifiers), keys), ("KeyX", "x")),
            egui::Event::Paste(text) => {
                // Paste what is on this machine's clipboard: files are uploaded and put on the
                // remote clipboard first; text is handed over; then paste is pressed remotely.
                // In map mode the modifiers are already held remotely.
                st.paste_seen = true;
                let mods = if keys.map {
                    sync_mods(v, st, remote_mods(ui.input(|i| i.modifiers), keys));
                    Vec::new()
                } else {
                    remote_mods(ui.input(|i| i.modifiers), keys)
                };
                let combo = combo_events(&mods, ("KeyV", "v"));
                match v.paste_files(combo.clone()) {
                    Paste::Text => {
                        v.send(serde_json::json!({"t": "clip", "s": text}));
                        combo.into_iter().for_each(|e| v.send(e));
                    }
                    Paste::KeysOnly => combo.into_iter().for_each(|e| v.send(e)),
                    Paste::Files => {}
                }
            }
            // Cmd/Ctrl+V with no text on the clipboard (e.g. files copied in Explorer) produces
            // no Paste event, only this key-up.
            egui::Event::Key { key: egui::Key::V, pressed: false, modifiers, .. } if modifiers.command_only() => {
                if std::mem::take(&mut st.paste_seen) {
                    continue;
                }
                let combo = combo_events(&remote_mods(modifiers, keys), ("KeyV", "v"));
                if v.paste_files(combo.clone()) == Paste::KeysOnly {
                    combo.into_iter().for_each(|e| v.send(e));
                }
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
    if keys.map && !menu_open {
        // Modifier keys alone (Shift for running in a game) produce no key event.
        sync_mods(v, st, remote_mods(ui.input(|i| i.modifiers), keys));
    }
    if keys.relative {
        st.rel_rest += rel;
        let (dx, dy) = (st.rel_rest.x.trunc(), st.rel_rest.y.trunc());
        if dx != 0.0 || dy != 0.0 {
            st.rel_rest -= vec2(dx, dy);
            v.send(serde_json::json!({"t": "mr", "dx": dx as i32, "dy": dy as i32}));
        }
    } else if let Some((x, y)) = moved {
        v.send(with_d(serde_json::json!({"t": "mm", "x": x, "y": y})));
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

// ---------------------------------------------------------------------------------------------
// Remote file panel and clipboard offers

impl ViewerWindow {
    /// Right-hand panel: browse the remote machine, download, and (with "fs2") manage files.
    fn files_panel(&self, ui: &mut Ui, w: &mut WinUi, b: &RemoteDir, fs2: bool) {
        let pal = Palette::of(ui);
        egui::Panel::right(egui::Id::unique(("files-panel", self.viewport)))
            .exact_size(340.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(12, 10)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    glyph(ui, r, Glyph::Folder, pal.accent);
                    ui.label(RichText::new("ไฟล์บนเครื่องปลายทาง").font(semi(14.0)).color(pal.fg));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_glyph_button(ui, Glyph::Close, "ปิด").clicked() {
                            w.show_files = false;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if let Some(parent) = &b.parent
                        && small_glyph_button(ui, Glyph::Up, "โฟลเดอร์แม่").clicked()
                    {
                        self.viewer.list_dir(parent);
                    }
                    if small_glyph_button(ui, Glyph::Refresh, "รีเฟรช").clicked() {
                        self.viewer.list_dir(&b.path);
                    }
                    if fs2 && small_glyph_button(ui, Glyph::FolderPlus, "โฟลเดอร์ใหม่").clicked() {
                        w.new_folder = Some(String::new());
                        w.renaming = None;
                    }
                    let path = if b.path.is_empty() { "…".to_string() } else { b.path.clone() };
                    ui.add(egui::Label::new(RichText::new(&path).font(mono(11.5)).color(pal.muted)).truncate()).on_hover_text(&path);
                });
                if let Some(name) = w.new_folder.as_mut() {
                    let mut done = None;
                    ui.horizontal(|ui| {
                        let edit = ui.add(egui::TextEdit::singleline(name).hint_text("ชื่อโฟลเดอร์ใหม่").desired_width(170.0));
                        edit.request_focus();
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("สร้าง").clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("ยกเลิก").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            done = Some(false);
                        }
                    });
                    match done {
                        Some(true) if !name.trim().is_empty() => {
                            self.viewer.fs_op(serde_json::json!({"t": "mkdir", "dir": b.path, "name": name.trim()}));
                            w.new_folder = None;
                        }
                        Some(false) => w.new_folder = None,
                        _ => {}
                    }
                }
                if let Some(e) = &b.error {
                    ui.label(RichText::new(e).size(12.0).color(pal.danger));
                }
                if b.loading && b.entries.is_empty() {
                    ui.label(RichText::new("กำลังโหลด…").size(12.5).color(pal.muted));
                }

                let footer = 52.0;
                egui::ScrollArea::vertical().max_height((ui.available_height() - footer).max(60.0)).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let hidden = w.show_hidden;
                    for e in b.entries.iter().filter(|e| hidden || !e.name.starts_with('.')) {
                        self.file_row(ui, w, b, e, fs2, pal);
                    }
                });

                ui.separator();
                ui.checkbox(&mut w.show_hidden, RichText::new("แสดงไฟล์ซ่อน").size(12.0));
                let hint = if fs2 {
                    "ลากไฟล์หรือโฟลเดอร์มาวางเพื่อส่งมาที่โฟลเดอร์นี้ · ดาวน์โหลดไปที่ Downloads/Aegiron"
                } else {
                    "ลากไฟล์มาวางเพื่อส่งมาที่โฟลเดอร์นี้ · ดาวน์โหลดไปที่ Downloads/Aegiron"
                };
                ui.label(RichText::new(hint).size(11.5).color(pal.muted));
            });
    }

    fn file_row(&self, ui: &mut Ui, w: &mut WinUi, b: &RemoteDir, e: &crate::viewer::RemoteEntry, fs2: bool, pal: Palette) {
        let path = if b.path.ends_with(&b.sep) { format!("{}{}", b.path, e.name) } else { format!("{}{}{}", b.path, b.sep, e.name) };

        // Rename in place.
        if let Some((target, name)) = w.renaming.as_mut().filter(|(p, _)| *p == path) {
            let mut done = None;
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(name).desired_width(190.0));
                edit.request_focus();
                if ui.button("ตกลง").clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    done = Some(true);
                }
                if ui.button("ยกเลิก").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
            match done {
                Some(true) if !name.trim().is_empty() && name.trim() != e.name => {
                    self.viewer.fs_op(serde_json::json!({"t": "mv", "path": target, "name": name.trim()}));
                    w.renaming = None;
                }
                Some(_) => w.renaming = None,
                None => {}
            }
            return;
        }
        // Delete confirmation.
        if w.confirm_delete.as_deref() == Some(path.as_str()) {
            ui.horizontal(|ui| {
                let what = if e.dir { "ลบโฟลเดอร์และทุกอย่างข้างใน" } else { "ลบ" };
                ui.add(egui::Label::new(RichText::new(format!("{what} “{}”?", e.name)).size(12.5).color(pal.danger)).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("ยกเลิก").clicked() {
                        w.confirm_delete = None;
                    }
                    if ui.add(Button::new(RichText::new("ลบ").color(Color32::WHITE)).fill(pal.danger)).clicked() {
                        self.viewer.fs_op(serde_json::json!({"t": "rm", "path": path}));
                        w.confirm_delete = None;
                    }
                });
            });
            return;
        }

        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        if hovered {
            ui.painter().rect_filled(rect, 5.0, pal.field);
        }
        let icon = Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(14.0, 14.0));
        glyph(ui, icon, if e.dir { Glyph::Folder } else { Glyph::File }, if e.dir { pal.accent } else { pal.muted });
        // Action buttons on hover, the size otherwise.
        let mut right = rect.right() - 4.0;
        let mut clicked_action = false;
        if hovered {
            let mut button = |g: Glyph, tip: &str| {
                let r = Rect::from_center_size(pos2(right - 11.0, rect.center().y), vec2(22.0, 22.0));
                right -= 24.0;
                let id = ui.scope_id().with(("fa", &path, tip));
                let resp = ui.interact(r, id, Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 4.0, pal.line);
                }
                glyph(ui, r.shrink(4.0), g, if g == Glyph::Trash && resp.hovered() { pal.danger } else { pal.fg });
                resp.on_hover_text(tip).clicked()
            };
            if fs2 && button(Glyph::Trash, "ลบ") {
                w.confirm_delete = Some(path.clone());
                clicked_action = true;
            }
            if fs2 && button(Glyph::Pencil, "เปลี่ยนชื่อ") {
                w.renaming = Some((path.clone(), e.name.clone()));
                w.new_folder = None;
                clicked_action = true;
            }
            if (!e.dir || fs2) && button(Glyph::Download, if e.dir { "ดาวน์โหลดทั้งโฟลเดอร์" } else { "ดาวน์โหลด" }) {
                self.viewer.download(path.clone(), e.dir);
                clicked_action = true;
            }
        } else if !e.dir {
            let size = human_size(e.size);
            let g = ui.painter().layout_no_wrap(size, mono(11.0), pal.muted);
            right -= g.size().x + 6.0;
            ui.painter().galley(pos2(right + 6.0, rect.center().y - g.size().y / 2.0), g, pal.muted);
        }
        let name_w = (right - rect.left() - 30.0).max(20.0);
        let g = one_line(ui, &e.name, FontId::proportional(13.0), pal.fg, name_w);
        ui.painter().galley(pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0), g, pal.fg);
        if e.dir && !clicked_action && resp.clicked() {
            self.viewer.list_dir(&path);
        }
        if e.dir && hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Files copied on the remote machine that are too big to fetch without asking.
    fn clip_offer(&self, ctx: &egui::Context, offer: &[ClipEntry]) {
        egui::Area::new(egui::Id::unique(("clip-offer", self.viewport)))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, [-12.0, -12.0])
            .show(ctx, |ui| {
                let pal = Palette::of(ui);
                Frame::new()
                    .fill(pal.bg)
                    .corner_radius(10)
                    .stroke(Stroke::new(1.0, pal.line))
                    .shadow(Shadow { offset: [0, 3], blur: 14, spread: 0, color: Color32::from_black_alpha(60) })
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(280.0);
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            glyph(ui, r, Glyph::Clipboard, pal.accent);
                            ui.label(RichText::new(format!("เครื่องปลายทางคัดลอก {} รายการ", offer.len())).font(semi(13.5)).color(pal.fg));
                        });
                        let names: Vec<_> = offer.iter().take(3).map(|e| if e.dir { format!("{}/", e.name) } else { e.name.clone() }).collect();
                        let more = if offer.len() > 3 { format!(" และอีก {}", offer.len() - 3) } else { String::new() };
                        ui.add(egui::Label::new(RichText::new(format!("{}{more}", names.join(", "))).size(12.0).color(pal.muted)).truncate());
                        ui.label(RichText::new("รับไฟล์มาไว้ที่ Downloads/Aegiron แล้ววางในเครื่องนี้ได้").size(12.0).color(pal.muted));
                        ui.horizontal(|ui| {
                            if dialog_button(ui, "รับไฟล์", true, true).clicked() {
                                self.viewer.accept_clip_offer();
                            }
                            if dialog_button(ui, "ไม่รับ", false, true).clicked() {
                                self.viewer.state.lock().unwrap().clip_offer = None;
                            }
                        });
                    });
            });
    }
}

fn small_glyph_button(ui: &mut Ui, g: Glyph, tip: &str) -> Response {
    let pal = Palette::of(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 5.0, pal.field);
    }
    glyph(ui, r.shrink(5.0), g, if resp.hovered() { pal.fg } else { pal.muted });
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_modes() {
        let frame = vec2(1920.0, 1080.0);
        // A big window: stretch enlarges, shrink and original stay at 1:1 (2x display: 960 pt).
        let big = vec2(2400.0, 1600.0);
        assert_eq!(view_size(Scale::Stretch, big, frame, 1.0), vec2(2400.0, 1350.0));
        assert_eq!(view_size(Scale::Shrink, big, frame, 1.0), frame);
        assert_eq!(view_size(Scale::Original, big, frame, 2.0), vec2(960.0, 540.0));
        // A small window: stretch and shrink fit, original overflows.
        let small = vec2(960.0, 800.0);
        assert_eq!(view_size(Scale::Stretch, small, frame, 1.0), vec2(960.0, 540.0));
        assert_eq!(view_size(Scale::Shrink, small, frame, 1.0), vec2(960.0, 540.0));
        assert_eq!(view_size(Scale::Original, small, frame, 1.0), frame);
    }

    #[test]
    fn placing_and_following() {
        let area = Rect::from_min_size(pos2(0.0, 40.0), vec2(800.0, 600.0));
        // Fits: centered.
        assert_eq!(place(area, vec2(400.0, 300.0), vec2(0.0, 1.0)).min, pos2(200.0, 190.0));
        // Larger: panned.
        assert_eq!(place(area, vec2(1600.0, 600.0), vec2(1.0, 0.5)).min, pos2(-800.0, 40.0));
        // The remote pointer near the right edge of a 1600-wide view pulls the pan along.
        let pan = follow(0.0, 1600.0, 800.0, 1500.0);
        let offset = pan * 800.0;
        assert!(1500.0 - offset <= 800.0 - 80.0 + 0.01 && pan > 0.0);
        // Already visible: no movement; and nothing to pan when it fits.
        assert_eq!(follow(0.5, 1600.0, 800.0, 800.0), 0.5);
        assert_eq!(follow(0.3, 600.0, 800.0, 590.0), 0.3);
    }

    #[test]
    fn map_mode_codes() {
        assert_eq!(physical_code(egui::Key::A), Some(("KeyA".into(), "a".into())));
        assert_eq!(physical_code(egui::Key::Num7), Some(("Digit7".into(), "7".into())));
        assert_eq!(physical_code(egui::Key::Semicolon), Some(("Semicolon".into(), ";".into())));
        assert_eq!(physical_code(egui::Key::Enter), Some(("Enter".into(), "Enter".into())));
        assert_eq!(physical_code(egui::Key::Copy), None);
    }
}
