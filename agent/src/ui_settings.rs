//! Settings page of the main window.

use super::*;

impl App {
    /// Gets the whole window below the tab bar.
    pub(super) fn settings(&mut self, ui: &mut Ui, agent: &Agent) {
        Frame::new().inner_margin(Margin::symmetric(36, 28)).show(ui, |ui| {
            if ui.add(Button::new(RichText::new("‹ กลับหน้าหลัก").color(Palette::of(ui).accent)).frame(false)).clicked() {
                self.view = View::Home;
            }
            let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(y) = self.demo_scroll {
                scroll = scroll.vertical_scroll_offset(y);
            }
            scroll.show(ui, |ui| self.settings_body(ui, agent));
        });
    }

    fn settings_body(&mut self, ui: &mut Ui, agent: &Agent) {
        let pal = Palette::of(ui);
        let c = agent.config.lock().unwrap().clone();
        ui.label(RichText::new("ตั้งค่า").font(semi(20.0)).color(pal.fg));
        ui.add_space(6.0);
        if cfg!(target_os = "macos") {
            permissions_ui::permissions_card(ui);
        }

        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(12.0);
            ui.label(RichText::new(title).font(semi(13.0)).color(pal.muted));
        };
        let group = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            Frame::new().stroke(Stroke::new(1.0, pal.line)).corner_radius(12).inner_margin(Margin::symmetric(16, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
        };
        let row_sep = |ui: &mut Ui| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, pal.line);
        };
        // A settings row: title + description on the left, a switch on the right.
        let switch_row = |ui: &mut Ui, title: &str, desc: &str, value: &mut bool| -> bool {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.set_min_height(52.0);
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(title).color(pal.fg));
                    ui.label(RichText::new(desc).size(12.5).color(pal.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    changed = toggle(ui, value).changed();
                });
            });
            changed
        };

        section(ui, "สิ่งที่ผู้ควบคุมทำได้");
        let (mut input, mut clip, mut files) = (c.allow_input, c.allow_clipboard, c.allow_file_transfer);
        let mut changed = false;
        group(ui, &mut |ui| {
            changed |= switch_row(ui, "ควบคุมเมาส์และคีย์บอร์ด", "ถ้าปิด ผู้เชื่อมต่อจะดูหน้าจอได้อย่างเดียว", &mut input);
            row_sep(ui);
            changed |= switch_row(ui, "คลิปบอร์ด", "คัดลอกและวางข้อความระหว่างสองเครื่อง", &mut clip);
            row_sep(ui);
            changed |= switch_row(ui, "รับ-ส่งไฟล์", "ไฟล์ที่ได้รับจะอยู่ในโฟลเดอร์ Downloads/ReDesk", &mut files);
        });
        if changed {
            self.save_config(|c| {
                c.allow_input = input;
                c.allow_clipboard = clip;
                c.allow_file_transfer = files;
            });
        }

        section(ui, "ความปลอดภัย");
        let mut approve = c.approve_mode;
        let mut verification = c.verification;
        let has_perm = c.permanent_password.is_some();
        let (mut a_changed, mut v_changed, mut save_pw) = (false, false, false);
        let (pw1, pw2) = (&mut self.new_pw, &mut self.new_pw2);
        group(ui, &mut |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("วิธีอนุญาตการเชื่อมต่อ").color(pal.fg));
            a_changed = segmented(
                ui,
                &mut approve,
                &[(ApproveMode::Password, "ใช้รหัสผ่าน"), (ApproveMode::Click, "กดอนุญาต"), (ApproveMode::Both, "ทั้งสองอย่าง")],
                true,
            );
            ui.add_space(8.0);
            row_sep(ui);
            ui.add_space(8.0);
            ui.label(RichText::new("รหัสผ่านที่ยอมรับ").color(pal.fg));
            v_changed = segmented(
                ui,
                &mut verification,
                &[(Verification::Temporary, "ใช้ครั้งเดียว"), (Verification::Permanent, "ถาวร"), (Verification::Both, "ทั้งสองแบบ")],
                has_perm,
            );
            if !has_perm {
                ui.label(RichText::new("ตั้งรหัสผ่านถาวรด้านล่างก่อน จึงจะเลือกได้").size(12.5).color(pal.muted));
            }
            ui.add_space(8.0);
            row_sep(ui);
            ui.add_space(8.0);
            ui.label(RichText::new(if has_perm { "เปลี่ยนรหัสผ่านถาวร" } else { "รหัสผ่านถาวร" }).color(pal.fg));
            ui.label(RichText::new("สำหรับเข้าเครื่องโดยไม่มีคนเฝ้า เช่น เซิร์ฟเวอร์หรือเครื่องในห้องประชุม").size(12.5).color(pal.muted));
            ui.horizontal(|ui| {
                let w = ((ui.available_width() - 120.0) / 2.0).max(120.0);
                ui.add(TextEdit::singleline(pw1).password(true).hint_text("รหัสผ่านใหม่").margin(Margin::symmetric(10, 8)).desired_width(w));
                ui.add(TextEdit::singleline(pw2).password(true).hint_text("ยืนยันรหัสผ่าน").margin(Margin::symmetric(10, 8)).desired_width(w));
                save_pw = secondary(ui, "บันทึก", None).clicked();
            });
            ui.add_space(8.0);
        });
        if a_changed {
            self.save_config(|c| c.approve_mode = approve);
        }
        if v_changed {
            self.save_config(|c| c.verification = verification);
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

        section(ui, "ล็อกการตั้งค่า");
        let has_pin = c.unlock_pin.is_some();
        let (mut set_pin, mut remove_pin) = (false, false);
        let (p1, p2) = (&mut self.pin.new_pin, &mut self.pin.new_pin2);
        group(ui, &mut |ui| {
            ui.add_space(10.0);
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 0.0; // compact status chip
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PIN ปลดล็อก").color(pal.fg));
                    if has_pin {
                        Frame::new().fill(pal.accent_soft).corner_radius(6).inner_margin(Margin::symmetric(8, 1)).show(ui, |ui| {
                            ui.label(RichText::new("เปิดใช้อยู่").size(12.0).color(pal.accent));
                        });
                    }
                });
            });
            ui.label(RichText::new("ต้องใส่ PIN ก่อนเปิดหน้าตั้งค่าหรือออกจากระบบ กันคนอื่นแก้การตั้งค่าเครื่องนี้").size(12.5).color(pal.muted));
            ui.horizontal(|ui| {
                let w = ((ui.available_width() - if has_pin { 230.0 } else { 120.0 }) / 2.0).max(96.0);
                fn field<'a>(hint: &str, v: &'a mut String, w: f32) -> TextEdit<'a> {
                    TextEdit::singleline(v).password(true).char_limit(8).hint_text(hint).margin(Margin::symmetric(10, 8)).desired_width(w)
                }
                ui.add(field(if has_pin { "PIN ใหม่" } else { "PIN 4–8 หลัก" }, p1, w));
                ui.add(field("ยืนยัน PIN", p2, w));
                set_pin = secondary(ui, if has_pin { "เปลี่ยน" } else { "ตั้ง PIN" }, None).clicked();
                if has_pin {
                    remove_pin = secondary(ui, "ลบ PIN", Some(pal.danger)).clicked();
                }
            });
            ui.add_space(8.0);
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

        section(ui, "ระบบ");
        let mut auto = self.autostart;
        let mut auto_changed = false;
        let (mut lan_on, mut lan_changed) = (c.lan_discovery, false);
        let server_fp = B64.decode(&c.server_key).map(|k| fingerprint(&k)).unwrap_or_default();
        let device_fp = fingerprint(agent.identity.verifying_key().as_bytes());
        group(ui, &mut |ui| {
            auto_changed = switch_row(ui, "เปิด ReDesk เมื่อล็อกอิน", "เริ่มทำงานในถาดระบบ (tray) โดยไม่เปิดหน้าต่าง", &mut auto);
            row_sep(ui);
            lan_changed = switch_row(ui, "ให้เครื่องอื่นในเครือข่ายค้นพบ", "ตอบ ID และชื่อเครื่องแก่เครื่องในวงแลนเดียวกัน (UDP 21120)", &mut lan_on);
            row_sep(ui);
            ui.add_space(6.0);
            let console = c.server_url.clone();
            ui.horizontal(|ui| {
                ui.label(RichText::new("คอนโซลบนเว็บ (จัดการผู้ใช้ อุปกรณ์ และ audit log)").size(13.0).color(pal.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(RichText::new("เปิดคอนโซล").size(13.0).color(pal.accent)).frame(false)).clicked() {
                        let _ = open_url(&console);
                    }
                });
            });
            egui::Grid::new("about").num_columns(2).spacing([20.0, 6.0]).show(ui, |ui| {
                for (k, v, m) in [
                    ("เซิร์ฟเวอร์", c.server_url.clone(), false),
                    ("กุญแจเซิร์ฟเวอร์", server_fp.clone(), true),
                    ("กุญแจเครื่องนี้", device_fp.clone(), true),
                    ("เวอร์ชัน", env!("CARGO_PKG_VERSION").to_string(), false),
                ] {
                    ui.label(RichText::new(k).size(13.0).color(pal.muted));
                    let t = RichText::new(v).color(pal.fg);
                    ui.label(if m { t.font(mono(13.0)) } else { t.size(13.0) });
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
        });
        if lan_changed {
            self.save_config(|c| c.lan_discovery = lan_on);
        }
        self.update_section(ui, &c);
        if auto_changed {
            let res = if auto { service::install(&self.config_path) } else { service::uninstall() };
            match res {
                Ok(()) => self.autostart = auto,
                Err(e) => self.flash(format!("ตั้งค่าไม่สำเร็จ: {e}")),
            }
        }
    }

    /// "อัปเดต": check the server for a newer agent, download it after the user asks, verify
    /// it (size + SHA-256, in updater::download) and show it; installing stays a user action.
    pub(super) fn update_section(&mut self, ui: &mut Ui, c: &Config) {
        let pal = Palette::of(ui);
        ui.add_space(12.0);
        ui.label(RichText::new("อัปเดต").font(semi(13.0)).color(pal.muted));
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
                    let dir = dirs::download_dir().unwrap_or_else(std::env::temp_dir).join("ReDesk");
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
