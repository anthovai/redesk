//! Input injection on a dedicated thread (enigo handles are not Send on every platform).

use std::sync::mpsc;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use crate::capture::Geometry;

pub enum Event {
    Move { x: f64, y: f64 },
    Button { b: u8, down: bool, x: f64, y: f64 },
    Wheel { dx: i32, dy: i32 },
    Key { code: String, key: String, down: bool },
    Text(String),
    Geometry(Geometry),
}

/// Spawns the input thread. Dropping the sender ends it, and dropping Enigo releases held keys.
pub fn spawn(geometry: Geometry) -> mpsc::Sender<Event> {
    let (tx, rx) = mpsc::channel::<Event>();
    std::thread::spawn(move || {
        let mut enigo = match Enigo::new(&Settings { open_prompt_to_get_permissions: true, ..Default::default() }) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("input injection unavailable: {e} (macOS: grant Accessibility permission)");
                return;
            }
        };
        let mut geo = geometry;
        let mut modifiers = 0u8; // held Control/Meta/Alt count
        for ev in rx {
            let res = match ev {
                Event::Geometry(g) => {
                    geo = g;
                    Ok(())
                }
                Event::Move { x, y } => move_to(&mut enigo, geo, x, y),
                Event::Button { b, down, x, y } => move_to(&mut enigo, geo, x, y).and_then(|_| {
                    let button = match b {
                        1 => Button::Middle,
                        2 => Button::Right,
                        _ => Button::Left,
                    };
                    enigo.button(button, dir(down))
                }),
                Event::Wheel { dx, dy } => {
                    let r = if dy != 0 { enigo.scroll(dy, Axis::Vertical) } else { Ok(()) };
                    r.and_then(|_| if dx != 0 { enigo.scroll(dx, Axis::Horizontal) } else { Ok(()) })
                }
                Event::Text(s) => enigo.text(&s),
                Event::Key { code, key, down } => {
                    if let Some(k) = named_key(&key, &code) {
                        if matches!(k, Key::Control | Key::Meta | Key::Alt) {
                            modifiers = if down { modifiers.saturating_add(1) } else { modifiers.saturating_sub(1) };
                        }
                        enigo.key(k, dir(down))
                    } else if key.chars().count() == 1 {
                        if modifiers > 0 {
                            // Shortcut (e.g. Ctrl+C): send the physical key so held modifiers apply.
                            match physical_key(&code, &key) {
                                PhysicalKey::Raw(kc) => enigo.raw(kc, dir(down)),
                                PhysicalKey::Char(c) => enigo.key(Key::Unicode(c), dir(down)),
                            }
                        } else if down {
                            // Plain typing: inject the character itself so any layout/script (Thai etc.) works.
                            enigo.text(&key)
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(e) = res {
                tracing::debug!("input event failed: {e}");
            }
        }
    });
    tx
}

fn dir(down: bool) -> Direction {
    if down { Direction::Press } else { Direction::Release }
}

fn move_to(enigo: &mut Enigo, g: Geometry, x: f64, y: f64) -> enigo::InputResult<()> {
    let px = g.x + (x.clamp(0.0, 1.0) * (g.width.saturating_sub(1)) as f64).round() as i32;
    let py = g.y + (y.clamp(0.0, 1.0) * (g.height.saturating_sub(1)) as f64).round() as i32;
    enigo.move_mouse(px, py, Coordinate::Abs)
}

fn named_key(key: &str, _code: &str) -> Option<Key> {
    Some(match key {
        "Enter" => Key::Return,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Escape" => Key::Escape,
        "Delete" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Shift" => Key::Shift,
        "Control" => Key::Control,
        "Alt" => Key::Alt,
        "Meta" => Key::Meta,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    })
}

enum PhysicalKey {
    #[allow(dead_code)]
    Raw(u16),
    #[allow(dead_code)]
    Char(char),
}

/// Maps KeyboardEvent.code to a layout-independent key for shortcuts.
fn physical_key(code: &str, key: &str) -> PhysicalKey {
    #[cfg(target_os = "macos")]
    {
        // macOS virtual key codes (kVK_ANSI_*).
        let kc = match code {
            "KeyA" => 0x00, "KeyS" => 0x01, "KeyD" => 0x02, "KeyF" => 0x03, "KeyH" => 0x04, "KeyG" => 0x05,
            "KeyZ" => 0x06, "KeyX" => 0x07, "KeyC" => 0x08, "KeyV" => 0x09, "KeyB" => 0x0B, "KeyQ" => 0x0C,
            "KeyW" => 0x0D, "KeyE" => 0x0E, "KeyR" => 0x0F, "KeyY" => 0x10, "KeyT" => 0x11, "Digit1" => 0x12,
            "Digit2" => 0x13, "Digit3" => 0x14, "Digit4" => 0x15, "Digit6" => 0x16, "Digit5" => 0x17,
            "Equal" => 0x18, "Digit9" => 0x19, "Digit7" => 0x1A, "Minus" => 0x1B, "Digit8" => 0x1C,
            "Digit0" => 0x1D, "BracketRight" => 0x1E, "KeyO" => 0x1F, "KeyU" => 0x20, "BracketLeft" => 0x21,
            "KeyI" => 0x22, "KeyP" => 0x23, "KeyL" => 0x25, "KeyJ" => 0x26, "Quote" => 0x27, "KeyK" => 0x28,
            "Semicolon" => 0x29, "Backslash" => 0x2A, "Comma" => 0x2B, "Slash" => 0x2C, "KeyN" => 0x2D,
            "KeyM" => 0x2E, "Period" => 0x2F, "Space" => 0x31, "Backquote" => 0x32,
            _ => return PhysicalKey::Char(key.chars().next().unwrap_or(' ').to_ascii_lowercase()),
        };
        PhysicalKey::Raw(kc)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let c = code
            .strip_prefix("Key")
            .or_else(|| code.strip_prefix("Digit"))
            .and_then(|s| s.chars().next())
            .unwrap_or_else(|| key.chars().next().unwrap_or(' '));
        PhysicalKey::Char(c.to_ascii_lowercase())
    }
}
