//! Remote-control window (one native window per connection, like RustDesk).

use super::*;

use crate::viewer::{Phase, TransferStatus};

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
        let (phase, error, name, fp, info, fps, kbps, clip, transfers, fb_size, cursor, codec) = {
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
                        ui.label(RichText::new(format!("{} · {fps} fps · {kbps} kbps", codec.to_uppercase())).font(mono(11.5)).color(pal.muted));
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
                // The remote pointer (captures do not include it), unless ours is on top of it.
                if let Some((cx, cy)) = cursor.filter(|_| !resp.hovered()) {
                    paint_pointer(ui.painter(), pos2(rect.left() + cx * rect.width(), rect.top() + cy * rect.height()));
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

/// Arrow pointer with its tip at `tip`, drawn over the remote screen.
fn paint_pointer(p: &egui::Painter, tip: egui::Pos2) {
    let pts = [(0.0, 0.0), (0.0, 16.0), (4.2, 12.3), (7.0, 18.5), (9.6, 17.4), (6.9, 11.3), (12.0, 11.3)];
    let at = |i: &[usize]| i.iter().map(|&i| tip + vec2(pts[i].0, pts[i].1)).collect::<Vec<_>>();
    // Head and tail are filled separately: convex_polygon needs convex input.
    p.add(egui::Shape::convex_polygon(at(&[0, 1, 2, 5, 6]), Color32::BLACK, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(at(&[2, 3, 4, 5]), Color32::BLACK, Stroke::NONE));
    p.add(egui::Shape::closed_line(at(&[0, 1, 2, 3, 4, 5, 6]), Stroke::new(1.2, Color32::WHITE)));
}
