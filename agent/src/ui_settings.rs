//! Settings, laid out like RustDesk's desktop settings: a navigation column on the left and the
//! selected page's cards (checkboxes, radio groups, fields) on the right.

use std::sync::Mutex;

use super::*;

const NAV_WIDTH: f32 = 200.0;
const CONTENT_MAX: f32 = 680.0;

/// One entry of the navigation column.
#[derive(Clone, Copy, PartialEq, Default)]
pub(super) enum Page {
    #[default]
    General,
    Security,
    Network,
    Account,
    About,
}

impl Page {
    const ALL: [Page; 5] = [Page::General, Page::Security, Page::Network, Page::Account, Page::About];

    fn title(self) -> &'static str {
        match self {
            Page::General => "ทั่วไป",
            Page::Security => "ความปลอดภัย",
            Page::Network => "เครือข่าย",
            Page::Account => "บัญชี",
            Page::About => "เกี่ยวกับ",
        }
    }

    fn icon(self) -> NavIcon {
        match self {
            Page::General => NavIcon::Gear,
            Page::Security => NavIcon::Shield,
            Page::Network => NavIcon::Globe,
            Page::Account => NavIcon::Person,
            Page::About => NavIcon::Info,
        }
    }
}

/// Page to open on the next frame (demo screenshots; `App::demo` has no egui context).
static DEMO_PAGE: Mutex<Option<Page>> = Mutex::new(None);

pub(super) fn demo_page(page: Page) {
    *DEMO_PAGE.lock().unwrap() = Some(page);
}

fn page_id() -> egui::Id {
    egui::Id::unique("aegiron-settings-page")
}

// ---------------------------------------------------------------------------------------------
// Navigation icons (private: the shared `Icon` set lives in ui.rs)

#[derive(Clone, Copy)]
enum NavIcon {
    Gear,
    Shield,
    Globe,
    Person,
    Info,
    Back,
}

fn nav_icon(ui: &Ui, rect: Rect, icon: NavIcon, color: Color32) {
    let p = ui.painter();
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let st = Stroke::new(1.5 * s.max(1.0), color);
    match icon {
        NavIcon::Gear => paint_icon(ui, rect, Icon::Gear, color),
        NavIcon::Shield => paint_icon(ui, rect, Icon::Shield, color),
        NavIcon::Back => paint_icon(ui, rect, Icon::Back, color),
        NavIcon::Globe => {
            let c = at(8.0, 8.0);
            p.circle_stroke(c, 6.3 * s, st);
            // Meridian as an ellipse, plus the equator and two parallels.
            let ellipse: Vec<Pos2> = (0..=32).map(|i| i as f32 * TAU / 32.0).map(|a| c + vec2(2.8 * a.cos(), 6.3 * a.sin()) * s).collect();
            p.add(Shape::line(ellipse, st));
            p.line_segment([at(1.9, 8.0), at(14.1, 8.0)], st);
            p.line_segment([at(3.0, 4.8), at(13.0, 4.8)], Stroke::new(1.1 * s.max(1.0), color));
            p.line_segment([at(3.0, 11.2), at(13.0, 11.2)], Stroke::new(1.1 * s.max(1.0), color));
        }
        NavIcon::Person => {
            p.circle_stroke(at(8.0, 5.2), 3.0 * s, st);
            p.add(Shape::line(arc(at(8.0, 15.5), 6.2 * s, 0.5 * TAU + 0.18, TAU - 0.18), st));
        }
        NavIcon::Info => {
            p.circle_stroke(at(8.0, 8.0), 6.3 * s, st);
            p.line_segment([at(8.0, 7.0), at(8.0, 11.6)], st);
            p.circle_filled(at(8.0, 4.7), 1.0 * s, color);
        }
    }
}

/// A navigation row: icon + label, accent text on a soft accent background when selected.
fn nav_item(ui: &mut Ui, icon: NavIcon, text: &str, selected: bool) -> Response {
    let pal = Palette::of(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let fill = if selected {
        pal.accent_soft
    } else if resp.hovered() {
        if ui.visuals().dark_mode { pal.field } else { pal.line.gamma_multiply(0.7) }
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    if selected {
        let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 10.0), vec2(3.0, rect.height() - 20.0));
        ui.painter().rect_filled(bar, 1.5, pal.accent);
    }
    let color = if selected { pal.accent } else { pal.muted };
    nav_icon(ui, Rect::from_center_size(rect.left_center() + vec2(24.0, 0.0), vec2(18.0, 18.0)), icon, color);
    let font = if selected { semi(14.5) } else { FontId::proportional(14.5) };
    ui.painter().text(rect.left_center() + vec2(44.0, 0.0), Align2::LEFT_CENTER, text, font, if selected { pal.accent } else { pal.fg });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---------------------------------------------------------------------------------------------
// Cards and rows

fn card_title(ui: &mut Ui, title: &str) {
    let pal = Palette::of(ui);
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(semi(13.0)).color(pal.muted));
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let pal = Palette::of(ui);
    Frame::new()
        .stroke(Stroke::new(1.0, pal.line))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(16, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn sep(ui: &mut Ui) {
    let pal = Palette::of(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, pal.line);
}

/// Title + optional description to the right of a 18px control; the whole row is clickable.
fn control_row(ui: &mut Ui, title: &str, desc: &str, enabled: bool, paint: impl FnOnce(&Ui, Rect, bool)) -> Response {
    let pal = Palette::of(ui);
    let w = ui.available_width();
    let text_w = (w - 32.0).max(80.0);
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    let t = ui.painter().layout(title.to_owned(), FontId::proportional(14.0), fade(pal.fg), text_w);
    let d = (!desc.is_empty()).then(|| ui.painter().layout(desc.to_owned(), FontId::proportional(12.5), fade(pal.muted), text_w));
    let text_h = t.size().y + d.as_ref().map_or(0.0, |d| d.size().y + 1.0);
    let h = (text_h + 18.0).max(42.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
    let top = rect.top() + (h - text_h) / 2.0;
    let ctrl = Rect::from_min_size(pos2(rect.left(), top + (t.size().y - 18.0) / 2.0), vec2(18.0, 18.0));
    paint(ui, ctrl, enabled && resp.hovered());
    let x = rect.left() + 32.0;
    let th = t.size().y;
    ui.painter().galley(pos2(x, top), t, pal.fg);
    if let Some(d) = d {
        ui.painter().galley(pos2(x, top + th + 1.0), d, pal.muted);
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// RustDesk-style checkbox row.
fn check_row(ui: &mut Ui, title: &str, desc: &str, value: &mut bool) -> bool {
    let pal = Palette::of(ui);
    let on = *value;
    let resp = control_row(ui, title, desc, true, |ui, r, hovered| {
        let p = ui.painter();
        if on {
            p.rect_filled(r, 4.0, pal.accent);
            let at = |x: f32, y: f32| r.min + vec2(x, y) * (r.width() / 18.0);
            p.add(Shape::line(vec![at(4.5, 9.3), at(7.6, 12.3), at(13.6, 5.9)], Stroke::new(2.0, pal.on_accent)));
        } else {
            p.rect_filled(r, 4.0, pal.bg);
            let border = if hovered { pal.accent } else if ui.visuals().dark_mode { pal.muted } else { Color32::from_rgb(0xb8, 0xb8, 0xc0) };
            p.rect_stroke(r.shrink(0.75), 4.0, Stroke::new(1.5, border), egui::StrokeKind::Middle);
        }
    });
    if resp.clicked() {
        *value = !*value;
        true
    } else {
        false
    }
}

/// Feature on/off: title + description on the left, a switch on the right.
fn switch_row(ui: &mut Ui, title: &str, desc: &str, value: &mut bool) -> bool {
    let pal = Palette::of(ui);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.set_min_height(54.0);
        ui.vertical(|ui| {
            ui.add_space(9.0);
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).color(pal.fg));
            ui.label(RichText::new(desc).size(12.5).color(pal.muted));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed = toggle(ui, value).changed();
        });
    });
    changed
}

/// RustDesk-style radio row; returns true when it was picked (and was not already selected).
fn radio_row(ui: &mut Ui, selected: bool, enabled: bool, title: &str, desc: &str) -> bool {
    let pal = Palette::of(ui);
    let resp = control_row(ui, title, desc, enabled, |ui, r, hovered| {
        let p = ui.painter();
        let c = r.center();
        let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        if selected {
            p.circle_stroke(c, 8.0, Stroke::new(2.0, fade(pal.accent)));
            p.circle_filled(c, 4.5, fade(pal.accent));
        } else {
            let border = if hovered { pal.accent } else if ui.visuals().dark_mode { pal.muted } else { Color32::from_rgb(0xb8, 0xb8, 0xc0) };
            p.circle_stroke(c, 8.0, Stroke::new(1.5, fade(border)));
        }
    });
    resp.clicked() && !selected
}

/// Key / value line of an information card.
fn info_row(ui: &mut Ui, key: &str, value: &str, monospace: bool) {
    let pal = Palette::of(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        info_key(ui, key);
        let t = RichText::new(value).color(pal.fg);
        ui.add(egui::Label::new(if monospace { t.font(mono(13.0)) } else { t.size(13.5) }).wrap());
    });
}

fn info_key(ui: &mut Ui, key: &str) {
    let pal = Palette::of(ui);
    ui.allocate_ui_with_layout(vec2(150.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(150.0);
        ui.label(RichText::new(key).size(13.0).color(pal.muted));
    });
}

fn link(ui: &mut Ui, text: &str) -> Response {
    let pal = Palette::of(ui);
    ui.add(Button::new(RichText::new(text).size(13.5).color(pal.accent)).frame(false)).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn hint(ui: &mut Ui, text: &str) {
    let pal = Palette::of(ui);
    ui.label(RichText::new(text).size(12.5).color(pal.muted));
}

// ---------------------------------------------------------------------------------------------

impl App {
    /// Gets the whole window (below the tab bar, once there is one).
    pub(super) fn settings(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let id = page_id();
        if let Some(p) = DEMO_PAGE.lock().unwrap().take() {
            ui.ctx().data_mut(|d| d.insert_temp(id, p));
        }
        let mut page: Page = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
        let mut back = false;

        egui::Panel::left("settings-nav")
            .exact_size(NAV_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.side).inner_margin(Margin { left: 12, right: 12, top: 22, bottom: 14 }))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("ตั้งค่า").font(semi(20.0)).color(pal.fg));
                });
                ui.add_space(14.0);
                for p in Page::ALL {
                    if nav_item(ui, p.icon(), p.title(), page == p).clicked() {
                        page = p;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    back = nav_item(ui, NavIcon::Back, "กลับหน้าหลัก", false).clicked();
                    ui.add_space(2.0);
                    sep(ui);
                });
            });
        ui.ctx().data_mut(|d| d.insert_temp(id, page));
        if back {
            self.view = View::Home;
            return;
        }

        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg)).show(ui, |ui| {
            let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("settings-scroll", page as u8));
            if let Some(y) = self.demo_scroll {
                scroll = scroll.vertical_scroll_offset(y);
            }
            scroll.show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 36, right: 36, top: 26, bottom: 28 }).show(ui, |ui| {
                    ui.set_max_width(CONTENT_MAX.min(ui.available_width()));
                    ui.label(RichText::new(page.title()).font(semi(20.0)).color(pal.fg));
                    ui.add_space(2.0);
                    let c = agent.config.lock().unwrap().clone();
                    match page {
                        Page::General => self.page_general(ui, &c),
                        Page::Security => self.page_security(ui, &c),
                        Page::Network => self.page_network(ui, agent, &c),
                        Page::Account => self.page_account(ui, &c),
                        Page::About => self.page_about(ui, agent, &c),
                    }
                });
            });
        });
    }

    fn page_general(&mut self, ui: &mut Ui, c: &Config) {
        card_title(ui, "การเริ่มต้นระบบ");
        let mut auto = self.autostart;
        let auto_changed = card(ui, |ui| switch_row(ui, "เปิด Aegiron เมื่อล็อกอิน", "เริ่มทำงานในถาดระบบ (tray) โดยไม่เปิดหน้าต่าง", &mut auto));
        if auto_changed {
            let res = if auto { service::install(&self.config_path) } else { service::uninstall() };
            match res {
                Ok(()) => self.autostart = auto,
                Err(e) => self.flash(format!("ตั้งค่าไม่สำเร็จ: {e}")),
            }
        }

        card_title(ui, "เครื่องนี้");
        let host = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        card(ui, |ui| {
            ui.add_space(4.0);
            info_row(ui, "ID", &format_id(&c.id), true);
            info_row(ui, "ชื่อเครื่อง", &host, false);
            ui.add_space(4.0);
        });

        card_title(ui, "การเข้ารหัสวิดีโอ");
        let picked = card(ui, |ui| {
            let mut picked = None;
            let opts = [
                (EncoderPref::Auto, "อัตโนมัติ", "ใช้ตัวเข้ารหัส H.264 ฮาร์ดแวร์ของเครื่อง (VideoToolbox / Media Foundation) ถ้าใช้ได้ ไม่เช่นนั้นใช้ OpenH264"),
                (EncoderPref::Software, "ซอฟต์แวร์เท่านั้น", "ใช้ OpenH264 เสมอ (ใช้ CPU มากกว่า เลือกเมื่อภาพจากฮาร์ดแวร์มีปัญหา)"),
            ];
            for (i, (pref, title, desc)) in opts.into_iter().enumerate() {
                if i > 0 {
                    sep(ui);
                }
                if radio_row(ui, c.video_encoder == pref, true, title, desc) {
                    picked = Some(pref);
                }
            }
            picked
        });
        if let Some(pref) = picked {
            self.save_config(|c| c.video_encoder = pref);
        }

        card_title(ui, "Codec เมื่อควบคุมเครื่องอื่น");
        let picked = card(ui, |ui| {
            let mut picked = None;
            let opts = [
                (ViewerCodec::H264, "H.264", "ใช้ได้กับทุกเครื่อง หน่วงต่ำ (แนะนำ)"),
                (ViewerCodec::Av1, "AV1", "ใช้แบนด์วิดท์น้อยกว่าที่คุณภาพเท่ากัน เมื่อเครื่องปลายทางมีตัวเข้ารหัส AV1 ฮาร์ดแวร์ (Windows กับ NVIDIA RTX 40, Intel Arc, AMD RX 7000 ขึ้นไป) ไม่เช่นนั้นใช้ H.264"),
            ];
            for (i, (pref, title, desc)) in opts.into_iter().enumerate() {
                if i > 0 {
                    sep(ui);
                }
                if radio_row(ui, c.viewer_codec == pref, true, title, desc) {
                    picked = Some(pref);
                }
            }
            picked
        });
        if let Some(pref) = picked {
            self.save_config(|c| c.viewer_codec = pref);
        }

        card_title(ui, "หน้าต่างควบคุมเครื่องอื่น");
        let mut tabs = self.local.viewer_tabs;
        let changed = card(ui, |ui| {
            check_row(ui, "เปิดเป็นแท็บในหน้าต่างเดียว", "แต่ละการเชื่อมต่อเป็นแท็บหนึ่ง คลิกขวาที่แท็บเพื่อแยกเป็นหน้าต่างใหม่ (ถ้าปิด จะเปิดหน้าต่างละเครื่อง)", &mut tabs)
        });
        if changed {
            self.local.viewer_tabs = tabs;
            self.mark_local();
        }

        if cfg!(target_os = "macos") {
            permissions_ui::permissions_card(ui);
        }
    }

    fn page_security(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);

        card_title(ui, "สิทธิ์ของผู้ควบคุม");
        let (mut input, mut clip, mut files, mut board, mut sound, mut voice) =
            (c.allow_input, c.allow_clipboard, c.allow_file_transfer, c.allow_whiteboard, c.allow_audio, c.allow_voice);
        let mut res = c.allow_resolution;
        let changed = card(ui, |ui| {
            let mut changed = check_row(ui, "ควบคุมเมาส์และคีย์บอร์ด", "ถ้าปิด ผู้เชื่อมต่อจะดูหน้าจอได้อย่างเดียว", &mut input);
            sep(ui);
            changed |= check_row(ui, "คลิปบอร์ด", "คัดลอกและวางข้อความระหว่างสองเครื่อง", &mut clip);
            sep(ui);
            changed |= check_row(ui, "รับ-ส่งไฟล์", "ไฟล์ที่ได้รับจะอยู่ในโฟลเดอร์ Downloads/Aegiron", &mut files);
            sep(ui);
            changed |= check_row(ui, "วาดบนหน้าจอ", "ผู้ควบคุมขีดเขียนชี้จุดบนหน้าจอนี้ได้ (คลิกทะลุ ไม่บังหน้าจอ หายไปเมื่อจบเซสชัน)", &mut board);
            sep(ui);
            let hint = if crate::audio::capture_supported() {
                "ส่งเสียงที่เครื่องนี้กำลังเล่นไปให้ผู้ควบคุม"
            } else {
                "ส่งเสียงที่เครื่องนี้กำลังเล่นไปให้ผู้ควบคุม (ต้องใช้ macOS 13 ขึ้นไป หรือ Windows)"
            };
            changed |= check_row(ui, "อนุญาตเสียง", hint, &mut sound);
            sep(ui);
            let hint = if crate::audio::voice_supported() {
                "ผู้ควบคุมขอโทรคุยได้ ต้องกดรับสายที่เครื่องนี้ทุกครั้ง (ใช้ไมโครโฟนของเครื่องนี้)"
            } else {
                "ผู้ควบคุมขอโทรคุยได้ ต้องกดรับสายที่เครื่องนี้ทุกครั้ง (ไม่พบไมโครโฟนหรือลำโพง)"
            };
            changed |= check_row(ui, "อนุญาตการโทรคุย", hint, &mut voice);
            let hint = if crate::resolution::supported() {
                "ผู้ควบคุมเปลี่ยนความละเอียดของหน้าจอนี้ได้ และคืนค่าเดิมเมื่อจบเซสชัน"
            } else {
                "ผู้ควบคุมเปลี่ยนความละเอียดของหน้าจอนี้ได้ (macOS และ Windows เท่านั้น)"
            };
            changed |= check_row(ui, "อนุญาตให้เปลี่ยนความละเอียด", hint, &mut res);
            changed
        });
        if changed {
            self.save_config(|c| {
                c.allow_input = input;
                c.allow_clipboard = clip;
                c.allow_file_transfer = files;
                c.allow_whiteboard = board;
                c.allow_audio = sound;
                c.allow_voice = voice;
                c.allow_resolution = res;
            });
        }

        card_title(ui, "TCP tunnel และเทอร์มินัล");
        let (mut tunnel, mut lan, mut term) = (c.allow_tunnel, c.allow_tunnel_lan, c.allow_terminal);
        let changed = card(ui, |ui| {
            let mut changed = check_row(ui, "อนุญาต TCP tunnel", "ผู้เชื่อมต่อส่งต่อพอร์ตจากเครื่องของเขามายังพอร์ตบนเครื่องนี้ (เช่น RDP, SSH, ฐานข้อมูล)", &mut tunnel);
            if tunnel {
                sep(ui);
                changed |= check_row(
                    ui,
                    "ให้ tunnel ไปเครื่องอื่นในเครือข่ายภายในได้",
                    "ถ้าปิด tunnel ไปได้เฉพาะเครื่องนี้ (localhost) ไม่ว่าจะเปิดหรือปิด ก็ไม่ไปอินเทอร์เน็ต",
                    &mut lan,
                );
            }
            sep(ui);
            changed |= check_row(ui, "อนุญาตเทอร์มินัล", "ผู้เชื่อมต่อเปิด shell บนเครื่องนี้ได้ ทำงานด้วยสิทธิ์ของผู้ใช้ที่เปิด Aegiron", &mut term);
            changed
        });
        if changed {
            self.save_config(|c| {
                c.allow_tunnel = tunnel;
                c.allow_tunnel_lan = lan;
                c.allow_terminal = term;
            });
        }

        card_title(ui, "การยอมรับการเชื่อมต่อ");
        let picked = card(ui, |ui| {
            let mut picked = None;
            let opts = [
                (ApproveMode::Password, "ใช้รหัสผ่าน", "ใส่รหัสผ่านถูกต้องแล้วเข้าได้ทันที เหมาะกับเครื่องที่ไม่มีคนเฝ้า"),
                (ApproveMode::Click, "กดอนุญาต", "ไม่ต้องใช้รหัสผ่าน แต่ต้องมีคนกดอนุญาตที่เครื่องนี้ทุกครั้ง"),
                (ApproveMode::Both, "ทั้งสองอย่าง", "ต้องใส่รหัสผ่านถูกต้อง และมีคนกดอนุญาตที่เครื่องนี้"),
            ];
            for (i, (mode, title, desc)) in opts.into_iter().enumerate() {
                if i > 0 {
                    sep(ui);
                }
                if radio_row(ui, c.approve_mode == mode, true, title, desc) {
                    picked = Some(mode);
                }
            }
            picked
        });
        if let Some(mode) = picked {
            self.save_config(|c| c.approve_mode = mode);
        }

        card_title(ui, "รหัสผ่าน");
        let has_perm = c.permanent_password.is_some();
        let (pw1, pw2) = (&mut self.new_pw, &mut self.new_pw2);
        let (verification, save_pw) = card(ui, |ui| {
            let mut picked = None;
            ui.add_space(8.0);
            ui.label(RichText::new("รหัสผ่านที่ยอมรับ").font(semi(14.0)).color(pal.fg));
            if !has_perm {
                hint(ui, "ตั้งรหัสผ่านถาวรด้านล่างก่อน จึงจะเลือกได้");
            }
            let opts = [
                (Verification::Temporary, "รหัสผ่านใช้ครั้งเดียว", "เปลี่ยนใหม่อัตโนมัติหลังมีผู้เชื่อมต่อสำเร็จ"),
                (Verification::Permanent, "รหัสผ่านถาวร", "สำหรับเข้าเครื่องโดยไม่มีคนเฝ้า เช่น เซิร์ฟเวอร์หรือเครื่องในห้องประชุม"),
                (Verification::Both, "ทั้งสองแบบ", "รับได้ทั้งรหัสใช้ครั้งเดียวและรหัสถาวร"),
            ];
            for (v, title, desc) in opts {
                if radio_row(ui, c.verification == v, has_perm, title, desc) {
                    picked = Some(v);
                }
            }
            ui.add_space(4.0);
            sep(ui);
            ui.add_space(10.0);
            ui.label(RichText::new(if has_perm { "เปลี่ยนรหัสผ่านถาวร" } else { "ตั้งรหัสผ่านถาวร" }).font(semi(14.0)).color(pal.fg));
            hint(ui, "อย่างน้อย 8 ตัวอักษร");
            let save = ui
                .horizontal(|ui| {
                    let w = ((ui.available_width() - 100.0) / 2.0).max(120.0);
                    ui.add(TextEdit::singleline(pw1).password(true).hint_text("รหัสผ่านใหม่").margin(Margin::symmetric(10, 8)).desired_width(w));
                    ui.add(TextEdit::singleline(pw2).password(true).hint_text("ยืนยันรหัสผ่าน").margin(Margin::symmetric(10, 8)).desired_width(w));
                    secondary(ui, "บันทึก", None).clicked()
                })
                .inner;
            ui.add_space(10.0);
            (picked, save)
        });
        if let Some(v) = verification {
            self.save_config(|c| c.verification = v);
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

        self.pin_card(ui, c);
    }

    /// "ล็อกการตั้งค่า": the unlock PIN; changing or removing it goes through `guarded`.
    fn pin_card(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);
        card_title(ui, "ล็อกการตั้งค่า");
        let has_pin = c.unlock_pin.is_some();
        let (p1, p2) = (&mut self.pin.new_pin, &mut self.pin.new_pin2);
        let (set_pin, remove_pin) = card(ui, |ui| {
            ui.add_space(10.0);
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 0.0; // compact status chip
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PIN ปลดล็อก").font(semi(14.0)).color(pal.fg));
                    if has_pin {
                        Frame::new().fill(pal.accent_soft).corner_radius(6).inner_margin(Margin::symmetric(8, 1)).show(ui, |ui| {
                            ui.label(RichText::new("เปิดใช้อยู่").size(12.0).color(pal.accent));
                        });
                    }
                });
            });
            hint(ui, "ต้องใส่ PIN ก่อนเปิดหน้าตั้งค่าหรือออกจากระบบ กันคนอื่นแก้การตั้งค่าเครื่องนี้");
            let r = ui
                .horizontal(|ui| {
                    let w = ((ui.available_width() - if has_pin { 200.0 } else { 100.0 }) / 2.0).max(96.0);
                    fn field<'a>(hint: &str, v: &'a mut String, w: f32) -> TextEdit<'a> {
                        TextEdit::singleline(v).password(true).char_limit(8).hint_text(hint).margin(Margin::symmetric(10, 8)).desired_width(w)
                    }
                    ui.add(field(if has_pin { "PIN ใหม่" } else { "PIN 4–8 หลัก" }, p1, w));
                    ui.add(field("ยืนยัน PIN", p2, w));
                    let set = secondary(ui, if has_pin { "เปลี่ยน" } else { "ตั้ง PIN" }, None).clicked();
                    let remove = has_pin && secondary(ui, "ลบ PIN", Some(pal.danger)).clicked();
                    (set, remove)
                })
                .inner;
            ui.add_space(10.0);
            r
        });
        if set_pin {
            if let Some(problem) = pin_problem(&self.pin.new_pin) {
                self.flash(problem);
            } else if self.pin.new_pin != self.pin.new_pin2 {
                self.flash("PIN ทั้งสองช่องไม่ตรงกัน");
            } else if let Ok(hash) = crate::hash_password(&self.pin.new_pin) {
                if has_pin {
                    // Changing an existing PIN asks for the current one first.
                    let ctx = ui.ctx().clone();
                    self.guarded(&ctx, PinAction::Change(hash));
                } else {
                    self.pin.new_pin.clear();
                    self.pin.new_pin2.clear();
                    self.save_config(|c| c.unlock_pin = Some(hash));
                }
            }
        }
        if remove_pin {
            let ctx = ui.ctx().clone();
            self.guarded(&ctx, PinAction::Remove);
        }
    }

    fn page_network(&mut self, ui: &mut Ui, agent: &Agent, c: &Config) {
        let pal = Palette::of(ui);
        card_title(ui, "เซิร์ฟเวอร์");
        let server_fp = B64.decode(&c.server_key).map(|k| fingerprint(&k)).unwrap_or_default();
        let state = agent.status.lock().unwrap().server.clone();
        card(ui, |ui| {
            ui.add_space(4.0);
            info_row(ui, "ที่อยู่เซิร์ฟเวอร์", &c.server_url, false);
            ui.horizontal(|ui| {
                ui.set_min_height(30.0);
                info_key(ui, "สถานะ");
                let (color, text) = match &state {
                    ServerState::Online => (pal.ok, "เชื่อมต่อแล้ว".to_string()),
                    ServerState::Connecting => (pal.warn, "กำลังเชื่อมต่อ…".to_string()),
                    ServerState::Error(e) => (pal.danger, format!("ไม่ได้เชื่อมต่อ: {}", e.chars().take(60).collect::<String>())),
                };
                let (d, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(d.center(), 4.0, color);
                ui.label(RichText::new(text).size(13.5).color(pal.fg));
            });
            info_row(ui, "กุญแจเซิร์ฟเวอร์", &server_fp, true);
            info_row(
                ui,
                "การเข้ารหัสระหว่างทาง",
                if c.allow_insecure_transport { "ปิด TLS (ใช้ทดสอบเท่านั้น)" } else { "TLS" },
                false,
            );
            ui.add_space(2.0);
            sep(ui);
            ui.horizontal(|ui| {
                ui.set_min_height(40.0);
                hint(ui, "จัดการผู้ใช้ อุปกรณ์ และ audit log บนเว็บ");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if link(ui, "เปิดคอนโซล ›").clicked() {
                        let _ = open_url(&c.server_url);
                    }
                });
            });
        });

        card_title(ui, "เครือข่ายท้องถิ่น (LAN)");
        let mut lan_on = c.lan_discovery;
        let lan_changed = card(ui, |ui| {
            switch_row(ui, "ให้เครื่องอื่นในเครือข่ายค้นพบ", "ตอบ ID และชื่อเครื่องแก่เครื่องในวงแลนเดียวกัน (UDP 21120)", &mut lan_on)
        });
        if lan_changed {
            self.save_config(|c| c.lan_discovery = lan_on);
        }

        card_title(ui, "การเชื่อมต่อโดยตรง (P2P)");
        let mut direct_on = c.direct_enabled;
        let direct_changed = card(ui, |ui| {
            let changed = switch_row(
                ui,
                "อนุญาตการเชื่อมต่อโดยตรง",
                "เชื่อมต่อตรงในวงแลน หรือ P2P ผ่าน UDP hole punching แทนการส่งผ่าน relay ของเซิร์ฟเวอร์ ข้อมูลยังเข้ารหัสต้นทาง-ปลายทางเหมือนเดิม ปิดเพื่อบังคับใช้ relay เสมอ (นโยบายองค์กร)",
                &mut direct_on,
            );
            sep(ui);
            info_row(ui, "พอร์ตเชื่อมต่อตรง", &format!("TCP {}", c.direct_port), false);
            let nat = crate::direct::nat_type().map(|n| n.label()).unwrap_or("ยังไม่ได้ตรวจ");
            info_row(ui, "ประเภท NAT", nat, false);
            ui.add_space(4.0);
            changed
        });
        if direct_changed {
            self.save_config(|c| c.direct_enabled = direct_on);
        }

        self.proxy_card(ui, c);
    }

    /// SOCKS5 / HTTP CONNECT proxy for every connection to the server. The form lives in egui's
    /// temporary memory; a blank password keeps the stored one.
    fn proxy_card(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);
        card_title(ui, "พร็อกซี");
        let fid = egui::Id::unique("aegiron-settings-proxy-form");
        let mut f: ProxyForm = ui.ctx().data(|d| d.get_temp(fid)).unwrap_or_else(|| ProxyForm::from_config(c.proxy.as_ref()));
        let save = card(ui, |ui| {
            ui.add_space(8.0);
            hint(ui, "ใช้กับทุกการเชื่อมต่อไปยังเซิร์ฟเวอร์ (signaling, relay และ HTTPS API) TLS ยังตรวจใบรับรองเต็มรูปแบบเหมือนเดิม การเชื่อมต่อโดยตรง/P2P ไม่ผ่านพร็อกซี");
            text_tabs(ui, &mut f.kind, &[(0u8, "ไม่ใช้"), (1, "SOCKS5"), (2, "HTTP")]);
            if f.kind != 0 {
                fn field<'a>(hint: &str, v: &'a mut String, w: f32, pw: bool) -> TextEdit<'a> {
                    TextEdit::singleline(v).password(pw).hint_text(hint).margin(Margin::symmetric(10, 8)).desired_width(w).char_limit(255)
                }
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 110.0).max(120.0);
                    ui.add(field("โฮสต์ เช่น proxy.example.com", &mut f.host, w, false));
                    ui.add(field("พอร์ต", &mut f.port, 90.0, false));
                });
                ui.horizontal(|ui| {
                    let w = ((ui.available_width() - 8.0) / 2.0).max(100.0);
                    ui.add(field("ชื่อผู้ใช้ (ถ้ามี)", &mut f.user, w, false));
                    let pw_hint = if f.has_password { "รหัสผ่าน (เว้นว่าง = ใช้รหัสเดิม)" } else { "รหัสผ่าน (ถ้ามี)" };
                    ui.add(field(pw_hint, &mut f.pass, w, true));
                });
            }
            let save = ui
                .horizontal(|ui| {
                    ui.set_min_height(40.0);
                    let save = secondary(ui, "บันทึกพร็อกซี", None).clicked();
                    let now = c.proxy.as_ref().map(|p| format!("ใช้อยู่: {}", p.display())).unwrap_or_else(|| "ไม่ได้ใช้พร็อกซี".into());
                    ui.label(RichText::new(now).size(12.5).color(pal.muted));
                    save
                })
                .inner;
            ui.add_space(4.0);
            save
        });
        if save {
            match f.to_config(c.proxy.as_ref()) {
                Ok(p) => {
                    self.save_config(|c| c.proxy = p);
                    f = ProxyForm::from_config(agent_proxy(self).as_ref());
                }
                Err(e) => self.flash(e),
            }
        }
        ui.ctx().data_mut(|d| d.insert_temp(fid, f));
    }

    fn page_account(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);
        card_title(ui, "บัญชีที่เข้าสู่ระบบ");
        let mut sign_in = false;
        let mut sign_out = false;
        card(ui, |ui| {
            ui.add_space(10.0);
            match &c.account {
                Some(acc) => {
                    ui.horizontal(|ui| {
                        avatar(ui, &initials(&acc.name, &acc.email), 48.0);
                        ui.vertical(|ui| {
                            ui.add_space(3.0);
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let name = if acc.name.is_empty() { acc.email.clone() } else { acc.name.clone() };
                            ui.label(RichText::new(name).font(semi(16.0)).color(pal.fg));
                            ui.label(RichText::new(&acc.email).size(13.0).color(pal.muted));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            sign_out = secondary(ui, "ออกจากระบบ", Some(pal.danger)).clicked();
                        });
                    });
                    ui.add_space(6.0);
                    sep(ui);
                    info_row(ui, "เซิร์ฟเวอร์", &host_of(&c.server_url), false);
                }
                None => {
                    ui.label(RichText::new("ยังไม่ได้เข้าสู่ระบบ").font(semi(15.0)).color(pal.fg));
                    hint(ui, "เข้าสู่ระบบเพื่อเชื่อมต่อเครื่องอื่น และใช้สมุดที่อยู่ขององค์กร");
                    ui.add_space(6.0);
                    sign_in = ui.horizontal(|ui| primary(ui, "เข้าสู่ระบบ", true)).inner.clicked();
                }
            }
            ui.add_space(10.0);
        });
        if sign_out {
            let ctx = ui.ctx().clone();
            self.guarded(&ctx, PinAction::SignOut);
        }
        if sign_in {
            self.login = LoginForm { email: self.login.email.clone(), ..Default::default() };
            self.login_modal = true;
        }
    }

    fn page_about(&mut self, ui: &mut Ui, agent: &Agent, c: &Config) {
        let pal = Palette::of(ui);
        ui.add_space(12.0);
        card(ui, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                Self::brand(ui, 52.0);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new("Aegiron").font(semi(19.0)).color(pal.fg));
                    ui.label(RichText::new(format!("เวอร์ชัน {}", env!("CARGO_PKG_VERSION"))).size(13.0).color(pal.muted));
                    ui.label(RichText::new("รีโมตเดสก์ท็อปบนเซิร์ฟเวอร์ขององค์กรเอง เข้ารหัสต้นทาง-ปลายทาง").size(12.5).color(pal.muted));
                });
            });
            ui.add_space(10.0);
        });

        self.update_section(ui, c);

        card_title(ui, "ข้อมูลเครื่องนี้");
        let device_fp = fingerprint(agent.identity.verifying_key().as_bytes());
        card(ui, |ui| {
            ui.add_space(4.0);
            info_row(ui, "ID", &format_id(&c.id), true);
            info_row(ui, "กุญแจเครื่องนี้", &device_fp, true);
            info_row(ui, "ระบบปฏิบัติการ", &format!("{} {}", std::env::consts::OS, std::env::consts::ARCH), false);
            ui.add_space(4.0);
        });
    }

    /// "อัปเดต": check the server for a newer agent, download it after the user asks, verify
    /// it (size + SHA-256, in updater::download) and show it; installing stays a user action.
    pub(super) fn update_section(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);
        card_title(ui, "อัปเดต");
        let mut action: Option<u8> = None; // 1 check, 2 download, 3 show file
        Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (text, color) = match &self.update {
                    UpdateState::Idle => (format!("เวอร์ชันปัจจุบัน {}", env!("CARGO_PKG_VERSION")), pal.fg),
                    UpdateState::Checking => ("กำลังตรวจหาอัปเดต…".into(), pal.muted),
                    UpdateState::UpToDate => (format!("เป็นเวอร์ชันล่าสุดแล้ว ({})", env!("CARGO_PKG_VERSION")), pal.fg),
                    UpdateState::Available(r) => (format!("มีเวอร์ชันใหม่ {} ({:.1} MB)", r.version, r.size as f64 / 1e6), pal.accent),
                    UpdateState::Downloading(r) => (format!("กำลังดาวน์โหลด {}…", r.version), pal.muted),
                    UpdateState::Downloaded(p) => (format!("ดาวน์โหลดและตรวจ SHA-256 แล้ว: {}", p.file_name().unwrap_or_default().to_string_lossy()), pal.fg),
                    UpdateState::Failed(e) => (e.clone(), pal.danger),
                };
                ui.label(RichText::new(text).size(13.5).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &self.update {
                    UpdateState::Available(_) => {
                        if primary(ui, "ดาวน์โหลด", true).clicked() {
                            action = Some(2);
                        }
                    }
                    UpdateState::Downloaded(_) => {
                        if secondary(ui, "แสดงไฟล์", None).clicked() {
                            action = Some(3);
                        }
                    }
                    UpdateState::Checking | UpdateState::Downloading(_) => {}
                    _ => {
                        if secondary(ui, "ตรวจหาอัปเดต", None).clicked() {
                            action = Some(1);
                        }
                    }
                });
            });
            if let UpdateState::Available(r) = &self.update
                && !r.notes.trim().is_empty()
            {
                ui.label(RichText::new(r.notes.trim()).size(12.5).color(pal.muted));
            }
        });
        let ctx = ui.ctx().clone();
        match action {
            Some(1) => self.check_update(&ctx, c.server_url.clone(), c.allow_insecure_transport),
            Some(2) => {
                if let UpdateState::Available(rel) = std::mem::take(&mut self.update) {
                    let dir = dirs::download_dir().unwrap_or_else(std::env::temp_dir).join("Aegiron");
                    self.update = UpdateState::Downloading(rel.clone());
                    self.spawn_job(&ctx, move || Job::UpdateDownload(updater::download(&rel, &dir).map_err(|e| format!("{e:#}"))));
                }
            }
            Some(3) => {
                if let UpdateState::Downloaded(p) = &self.update
                    && let Some(dir) = p.parent()
                {
                    let _ = open_url(&dir.to_string_lossy());
                }
            }
            _ => {}
        }
    }
}

fn agent_proxy(app: &App) -> Option<crate::net::ProxyConfig> {
    app.agent.as_ref().and_then(|a| a.config.lock().unwrap().proxy.clone())
}

/// Proxy fields being edited in Settings → Network (0 = none, 1 = SOCKS5, 2 = HTTP).
#[derive(Clone, Default)]
struct ProxyForm {
    kind: u8,
    host: String,
    port: String,
    user: String,
    pass: String,
    has_password: bool,
}

impl ProxyForm {
    fn from_config(p: Option<&crate::net::ProxyConfig>) -> Self {
        use crate::net::ProxyKind;
        let Some(p) = p else { return Self::default() };
        Self {
            kind: if p.kind == ProxyKind::Socks5 { 1 } else { 2 },
            host: p.host.clone(),
            port: p.port.to_string(),
            user: p.username.clone().unwrap_or_default(),
            pass: String::new(),
            has_password: p.password.as_ref().is_some_and(|s| !s.is_empty()),
        }
    }

    /// The proxy to store; a blank password keeps `old`'s for the same user.
    fn to_config(&self, old: Option<&crate::net::ProxyConfig>) -> Result<Option<crate::net::ProxyConfig>, String> {
        use crate::net::{ProxyConfig, ProxyKind};
        if self.kind == 0 {
            return Ok(None);
        }
        let port = self.port.trim().parse::<u16>().ok().filter(|p| *p > 0).ok_or("พอร์ตพร็อกซีไม่ถูกต้อง")?;
        let user = self.user.trim().to_string();
        let password = if !self.pass.is_empty() {
            Some(self.pass.clone())
        } else {
            old.filter(|o| o.username.as_deref() == Some(user.as_str())).and_then(|o| o.password.clone())
        };
        let p = ProxyConfig {
            kind: if self.kind == 1 { ProxyKind::Socks5 } else { ProxyKind::Http },
            host: self.host.trim().trim_start_matches('[').trim_end_matches(']').to_string(),
            port,
            password: password.filter(|_| !user.is_empty()),
            username: (!user.is_empty()).then_some(user),
        };
        p.validate().map_err(|_| "ที่อยู่พร็อกซีไม่ถูกต้อง".to_string())?;
        Ok(Some(p))
    }
}
