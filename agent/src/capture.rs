//! Screen capture: grabs a display, downsamples, and emits either H.264 frames or
//! JPEG-encoded dirty rectangles, plus the remote pointer position.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;
use tokio::sync::{mpsc, watch};
use xcap::Monitor;

use crate::codec::{self, Codec, VideoEncoder};
use crate::proto;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub display: usize,
    pub fps: u32,
    pub quality: u8,
    pub max_width: u32,
    pub codec: Codec,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DisplayInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Logical geometry of a display, used to map normalised pointer positions to input coordinates.
#[derive(Debug, Clone, Copy, Default)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn displays() -> Vec<DisplayInfo> {
    Monitor::all()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| DisplayInfo {
            index: i,
            name: m.friendly_name().or_else(|_| m.name()).unwrap_or_else(|_| format!("Display {}", i + 1)),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            primary: m.is_primary().unwrap_or(false),
        })
        .collect()
}

pub fn geometry(display: usize) -> Option<Geometry> {
    let monitors = Monitor::all().ok()?;
    let m = monitors.get(display).or(monitors.first())?;
    Some(Geometry { x: m.x().ok()?, y: m.y().ok()?, width: m.width().ok()?, height: m.height().ok()? })
}

/// The display in desktop coordinates as windows are placed (logical points): x, y, w, h.
pub fn logical_area(display: usize) -> Option<[f32; 4]> {
    let monitors = Monitor::all().ok()?;
    let m = monitors.get(display).or(monitors.first())?;
    let (x, y, w, h) = (m.x().ok()? as f32, m.y().ok()? as f32, m.width().ok()? as f32, m.height().ok()? as f32);
    // macOS reports points already; elsewhere the capture geometry is in physical pixels.
    let s = if cfg!(target_os = "macos") { 1.0 } else { m.scale_factor().unwrap_or(1.0).max(0.5) };
    Some([x / s, y / s, w / s, h / s])
}

/// Downsampled RGB frame.
struct Frame {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

/// Box-filter downsample by an integer factor so that width <= max_width; drops alpha.
/// `even` trims a trailing odd row/column, as 4:2:0 video needs even dimensions.
fn downsample(img: &image::RgbaImage, max_width: u32, even: bool) -> Frame {
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    let f = (sw as u32).div_ceil(max_width.max(320)).max(1) as usize;
    let (mut w, mut h) = (sw / f, sh / f);
    if even {
        (w, h) = (w & !1, h & !1);
    }
    let src = img.as_raw();
    let mut rgb = vec![0u8; w * h * 3];
    if f == 1 {
        for y in 0..h {
            for (x, px) in src[y * sw * 4..].chunks_exact(4).take(w).enumerate() {
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&px[..3]);
            }
        }
        return Frame { w, h, rgb };
    }
    let area = (f * f) as u32;
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for dy in 0..f {
                let row = ((y * f + dy) * sw + x * f) * 4;
                for dx in 0..f {
                    let p = row + dx * 4;
                    r += src[p] as u32;
                    g += src[p + 1] as u32;
                    b += src[p + 2] as u32;
                }
            }
            let o = (y * w + x) * 3;
            rgb[o] = (r / area) as u8;
            rgb[o + 1] = (g / area) as u8;
            rgb[o + 2] = (b / area) as u8;
        }
    }
    Frame { w, h, rgb }
}

/// Bounding box (x, y, w, h) of pixels that differ between two equally sized frames.
fn dirty_rect(prev: &Frame, cur: &Frame) -> Option<(usize, usize, usize, usize)> {
    let stride = cur.w * 3;
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    for y in 0..cur.h {
        let a = &prev.rgb[y * stride..(y + 1) * stride];
        let b = &cur.rgb[y * stride..(y + 1) * stride];
        if a == b {
            continue;
        }
        let first = a.chunks_exact(3).zip(b.chunks_exact(3)).position(|(p, q)| p != q).unwrap();
        let last = cur.w - 1 - a.chunks_exact(3).rev().zip(b.chunks_exact(3).rev()).position(|(p, q)| p != q).unwrap();
        y0 = y0.min(y);
        y1 = y;
        x0 = x0.min(first);
        x1 = x1.max(last);
    }
    (y0 != usize::MAX).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

fn encode_rect(f: &Frame, (x, y, w, h): (usize, usize, usize, usize), quality: u8) -> Vec<u8> {
    let mut crop = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        let start = (row * f.w + x) * 3;
        crop.extend_from_slice(&f.rgb[start..start + w * 3]);
    }
    let mut out = proto::frame_header(x as u16, y as u16, f.w as u16, f.h as u16);
    let mut enc = JpegEncoder::new_with_quality(&mut out, quality.clamp(10, 95));
    if let Err(e) = enc.encode(&crop, w as u32, h as u32, ExtendedColorType::Rgb8) {
        tracing::warn!("jpeg encode failed: {e}");
    }
    out
}

/// Tracks the local pointer so the viewer can draw it; screen captures do not include it.
struct Pointer {
    enigo: Option<enigo::Enigo>,
    last: Option<(i32, i32)>,
}

impl Pointer {
    fn new() -> Self {
        let settings = enigo::Settings { open_prompt_to_get_permissions: false, ..Default::default() };
        Self { enigo: enigo::Enigo::new(&settings).ok(), last: None }
    }

    /// JSON message with the position normalised to the display, if it moved.
    fn poll(&mut self, g: Geometry) -> Option<Vec<u8>> {
        use enigo::Mouse;
        let pos = self.enigo.as_ref()?.location().ok()?;
        if self.last == Some(pos) || g.width == 0 || g.height == 0 {
            return None;
        }
        self.last = Some(pos);
        let x = (pos.0 - g.x) as f64 / g.width as f64;
        let y = (pos.1 - g.y) as f64 / g.height as f64;
        let inside = (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y);
        Some(proto::json(&serde_json::json!({"t": "cursor", "x": x, "y": y, "visible": inside})))
    }
}

/// Runs on a dedicated thread until `stop` is set or the receiver goes away.
/// `blocking_send` on a tiny channel paces capture to what the network can carry.
/// `force_full` asks for a full JPEG frame or an H.264 keyframe.
/// `reported` is the encoder named in `info`; a different one in use is announced with an
/// `encoder` message (e.g. after the hardware encoder failed and OpenH264 took over).
pub fn run(params: watch::Receiver<Params>, out: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>, force_full: Arc<AtomicBool>, reported: &'static str) {
    let mut prev: Option<Frame> = None;
    let mut last_params = *params.borrow();
    let mut h264: Option<Box<dyn VideoEncoder>> = None;
    let mut h264_failed = false;
    // Hardware is only tried when the session's probe picked it (and the settings allow it).
    let mut hw_failed = !codec::is_hardware(reported);
    let mut reported = reported;
    let mut pointer = Pointer::new();
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let p = *params.borrow();
        if p != last_params {
            last_params = p;
            prev = None;
            h264 = None;
        }
        let monitors = Monitor::all().unwrap_or_default();
        let Some(m) = monitors.get(p.display).or(monitors.first()) else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let geometry = Geometry {
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
        };
        if let Some(msg) = pointer.poll(geometry) {
            if out.blocking_send(msg).is_err() {
                return;
            }
        }
        let img = match m.capture_image() {
            Ok(i) => i,
            Err(e) => {
                if !warned {
                    tracing::error!("screen capture failed: {e} (macOS: grant Screen Recording permission)");
                    warned = true;
                }
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let video = p.codec == Codec::H264 && !h264_failed;
        let cur = downsample(&img, p.max_width, video);
        let full = force_full.swap(false, Ordering::Relaxed);
        let rect = match &prev {
            Some(pf) if !full && pf.w == cur.w && pf.h == cur.h => dirty_rect(pf, &cur),
            _ => Some((0, 0, cur.w, cur.h)),
        };
        if let Some(r) = rect {
            let msg = if video {
                let key = full || prev.is_none();
                let mut res = encode_video(&mut h264, &cur, p, key, !hw_failed);
                if res.is_err() && h264.as_ref().is_some_and(|e| e.name() != "openh264") {
                    // Hardware encoder broke mid-session: carry on in software.
                    tracing::warn!("{:#}; switching to OpenH264", res.as_ref().err().unwrap());
                    hw_failed = true;
                    h264 = None;
                    res = encode_video(&mut h264, &cur, p, true, false);
                }
                res.unwrap_or_else(|e| {
                    // The viewer decodes JPEG rectangles too, so degrade instead of going dark.
                    tracing::error!("{e:#}; falling back to JPEG");
                    h264_failed = true;
                    h264 = None;
                    Some(encode_rect(&cur, (0, 0, cur.w, cur.h), p.quality))
                })
            } else {
                Some(encode_rect(&cur, r, p.quality))
            };
            let active = match &h264 {
                Some(e) if video => e.name(),
                _ => "jpeg",
            };
            if active != reported {
                reported = active;
                tracing::info!("video encoder: {active}");
                let note = serde_json::json!({"t": "encoder", "encoder": active, "codec": if video { "h264" } else { "jpeg" }});
                if out.blocking_send(proto::json(&note)).is_err() {
                    return;
                }
            }
            if let Some(msg) = msg {
                if out.blocking_send(msg).is_err() {
                    return;
                }
            }
            prev = Some(cur);
        }
        let budget = Duration::from_millis(1000 / p.fps.clamp(1, 60) as u64);
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Encodes a whole frame as H.264, (re)creating the encoder when the size or settings change
/// (a new encoder always starts with a keyframe). Ok(None) means the encoder dropped the frame.
fn encode_video(enc: &mut Option<Box<dyn VideoEncoder>>, f: &Frame, p: Params, keyframe: bool, allow_hw: bool) -> anyhow::Result<Option<Vec<u8>>> {
    let mut keyframe = keyframe;
    if enc.as_ref().is_none_or(|e| e.size() != (f.w, f.h)) {
        *enc = Some(codec::h264_encoder(f.w, f.h, p.fps, p.quality, allow_hw)?);
        keyframe = true;
    }
    let e = enc.as_mut().unwrap();
    Ok(e.encode(&f.rgb, keyframe)?.map(|(data, key)| proto::video(Codec::H264.id(), key, f.w as u16, f.h as u16, &data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_rect_finds_change() {
        let a = Frame { w: 4, h: 3, rgb: vec![0; 36] };
        let mut b = Frame { w: 4, h: 3, rgb: vec![0; 36] };
        assert_eq!(dirty_rect(&a, &b), None);
        b.rgb[(1 * 4 + 2) * 3] = 9; // pixel (2,1)
        b.rgb[(2 * 4 + 1) * 3 + 2] = 9; // pixel (1,2)
        assert_eq!(dirty_rect(&a, &b), Some((1, 1, 2, 2)));
    }
}
