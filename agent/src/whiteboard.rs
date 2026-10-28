//! Whiteboard / annotation (docs/PROTOCOL.md §8): the viewer draws on the remote screen and
//! the controlled machine shows the strokes in a transparent, click-through, always-on-top
//! overlay (desktop app only; the headless agent does not offer it).
//!
//! Strokes arrive as `{"t":"wb","op":"line","id","c":"#rrggbb","w","p":[x,y,...]}` (points
//! appended to stroke `id`, normalised 0-1 over the streamed display; `w` is the pen width as
//! a fraction of the display height) and `{"t":"wb","op":"clear"}`. They are kept until the
//! viewer clears them or the session ends.

/// Limits that keep a misbehaving viewer from exhausting memory or the overlay.
const MAX_STROKES: usize = 300;
const MAX_POINTS: usize = 60_000;
const MAX_POINTS_PER_MSG: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub id: u32,
    pub color: [u8; 3],
    /// Pen width as a fraction of the display height.
    pub width: f32,
    /// Points normalised to the display.
    pub pts: Vec<[f32; 2]>,
}

/// Annotations of one session.
#[derive(Clone, Debug, Default)]
pub struct Board {
    pub sid: String,
    /// The streamed display in desktop coordinates (logical points): x, y, width, height.
    pub area: [f32; 4],
    pub strokes: Vec<Stroke>,
}

pub fn parse_color(c: &str) -> [u8; 3] {
    let hex = c.strip_prefix('#').unwrap_or(c);
    if hex.len() == 6
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    }
    [0xff, 0x3b, 0x30]
}

impl Board {
    pub fn new(sid: &str, area: [f32; 4]) -> Self {
        Self { sid: sid.to_string(), area, strokes: Vec::new() }
    }

    fn points(&self) -> usize {
        self.strokes.iter().map(|s| s.pts.len()).sum()
    }

    /// Applies one viewer message. Returns true if anything visible changed.
    pub fn apply(&mut self, op: &str, id: u32, color: &str, width: f32, p: &[f32]) -> bool {
        match op {
            "clear" => {
                let had = !self.strokes.is_empty();
                self.strokes.clear();
                had
            }
            "line" => {
                let pts: Vec<[f32; 2]> = p
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(MAX_POINTS_PER_MSG)
                    .filter(|c| c[0].is_finite() && c[1].is_finite())
                    .map(|c| [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0)])
                    .collect();
                if pts.is_empty() {
                    return false;
                }
                // Continue the newest stroke with this id, or start a new one.
                match self.strokes.iter_mut().rev().find(|s| s.id == id) {
                    Some(s) => s.pts.extend(pts),
                    None => {
                        let width = if width.is_finite() { width.clamp(0.001, 0.05) } else { 0.004 };
                        self.strokes.push(Stroke { id, color: parse_color(color), width, pts });
                    }
                }
                // Oldest strokes go first when over the limits.
                while self.strokes.len() > MAX_STROKES || (self.points() > MAX_POINTS && self.strokes.len() > 1) {
                    self.strokes.remove(0);
                }
                if let Some(s) = self.strokes.last_mut()
                    && s.pts.len() > MAX_POINTS
                {
                    let cut = s.pts.len() - MAX_POINTS;
                    s.pts.drain(..cut);
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_append_clear_and_stay_bounded() {
        let mut b = Board::new("s", [0.0, 0.0, 100.0, 100.0]);
        assert!(b.apply("line", 1, "#00ff00", 0.01, &[0.1, 0.1, 0.2, 0.2]));
        assert!(b.apply("line", 1, "", 0.0, &[0.3, 2.0]));
        assert!(b.apply("line", 2, "bad", f32::NAN, &[0.5, 0.5, f32::NAN, 0.1]));
        assert_eq!(b.strokes.len(), 2);
        assert_eq!(b.strokes[0].color, [0, 255, 0]);
        assert_eq!(b.strokes[0].pts, vec![[0.1, 0.1], [0.2, 0.2], [0.3, 1.0]]);
        assert_eq!(b.strokes[1].color, [0xff, 0x3b, 0x30]);
        assert_eq!(b.strokes[1].width, 0.004);
        assert_eq!(b.strokes[1].pts.len(), 1);
        assert!(!b.apply("line", 3, "", 0.01, &[]));
        assert!(!b.apply("erase-everything", 0, "", 0.0, &[]));
        assert!(b.apply("clear", 0, "", 0.0, &[]));
        assert!(b.strokes.is_empty() && !b.apply("clear", 0, "", 0.0, &[]));

        for i in 0..(MAX_STROKES as u32 + 50) {
            b.apply("line", i, "", 0.01, &[0.0; 2048]);
        }
        assert!(b.strokes.len() <= MAX_STROKES && b.points() <= MAX_POINTS);
        assert_eq!(b.strokes.last().unwrap().id, MAX_STROKES as u32 + 49);
    }
}
