//! Whiteboard overlay: what a viewer draws on this screen, shown in a borderless, transparent,
//! click-through, always-on-top window over the display being streamed. One window per
//! session with strokes; it closes when the strokes are cleared or the session ends.

use super::*;

impl App {
    pub(super) fn whiteboard_overlay(&mut self, ctx: &egui::Context) {
        let Some(agent) = &self.agent else { return };
        let boards: Vec<crate::whiteboard::Board> =
            agent.status.lock().unwrap().boards.iter().filter(|b| !b.strokes.is_empty() && b.area[2] > 0.0).cloned().collect();
        if ctx.embed_viewports() {
            return; // no native windows on this backend
        }
        for b in boards {
            let [x, y, w, h] = b.area;
            let builder = egui::ViewportBuilder::default()
                .with_title("Aegiron — คำอธิบายบนหน้าจอ")
                .with_position([x, y])
                .with_inner_size([w, h])
                .with_decorations(false)
                .with_transparent(true)
                .with_has_shadow(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_active(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_mouse_passthrough(true);
            let id = egui::ViewportId::from_hash_of(("aegiron-whiteboard", &b.sid));
            ctx.show_viewport_immediate(id, builder, |ui, class| {
                if class == egui::ViewportClass::EmbeddedWindow {
                    return;
                }
                // The system may have moved or shrunk the window (macOS keeps windows clear of
                // the menu bar and Dock): place strokes by where the display really is.
                let inner = ui.input(|i| i.viewport().inner_rect);
                if let Some(r) = inner
                    && (r.left() - x).abs() + (r.top() - y).abs() > 1.0
                {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(pos2(x, y)));
                }
                let display = match inner {
                    Some(r) => Rect::from_min_size(pos2(x - r.left(), y - r.top()), vec2(w, h)),
                    None => Rect::from_min_size(Pos2::ZERO, vec2(w, h)),
                };
                egui::CentralPanel::default().frame(Frame::NONE.fill(Color32::TRANSPARENT)).show(ui, |ui| paint_board(ui, &b, display));
            });
        }
    }
}

/// `r`: the streamed display in this window's coordinates.
fn paint_board(ui: &mut Ui, b: &crate::whiteboard::Board, r: Rect) {
    let p = ui.painter();
    for s in &b.strokes {
        let color = Color32::from_rgb(s.color[0], s.color[1], s.color[2]);
        let width = (s.width * r.height()).max(1.5);
        let pts: Vec<Pos2> = s.pts.iter().map(|q| pos2(r.left() + q[0] * r.width(), r.top() + q[1] * r.height())).collect();
        if pts.len() == 1 {
            p.circle_filled(pts[0], width / 2.0, color);
        } else {
            // A thin dark halo keeps light colors readable on any background.
            p.add(Shape::line(pts.clone(), Stroke::new(width + 2.0, Color32::from_black_alpha(70))));
            p.add(Shape::line(pts, Stroke::new(width, color)));
        }
    }
}
