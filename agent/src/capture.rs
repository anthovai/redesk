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
    /// Index of the display (Wayland input is addressed to a portal stream, not coordinates).
    pub display: usize,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The Wayland portal session in use (screen and input come from it instead of xcap/enigo).
#[cfg(target_os = "linux")]
fn portal() -> Option<std::sync::Arc<crate::portal::Portal>> {
    crate::portal::current()
}

pub fn displays() -> Vec<DisplayInfo> {
    #[cfg(target_os = "linux")]
    if let Some(p) = portal() {
        return p.displays();
    }
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
    #[cfg(target_os = "linux")]
    if let Some(p) = portal() {
        return Some(p.geometry(display));
    }
    let monitors = Monitor::all().ok()?;
    let display = if display < monitors.len() { display } else { 0 };
    let m = monitors.get(display)?;
    Some(Geometry { display, x: m.x().ok()?, y: m.y().ok()?, width: m.width().ok()?, height: m.height().ok()? })
}

/// The display in desktop coordinates as windows are placed (logical points): x, y, w, h.
pub fn logical_area(display: usize) -> Option<[f32; 4]> {
    #[cfg(target_os = "linux")]
    if let Some(p) = portal() {
        // Portal streams are described in logical coordinates already.
        let g = p.geometry(display);
        return Some([g.x as f32, g.y as f32, g.width as f32, g.height as f32]);
    }
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
/// `force_full` asks for a full JPEG frame or a video keyframe.
/// `reported` is the encoder named in `info`; a different encoder or codec in use is announced
/// with an `encoder` message (e.g. after the hardware encoder failed and OpenH264 took over, or
/// AV1 failed and H.264 took over). `h264` is the H.264 encoder the session's probe picked.
pub fn run(params: watch::Receiver<Params>, out: mpsc::Sender<Vec<u8>>, stop: Arc<AtomicBool>, force_full: Arc<AtomicBool>, reported: &'static str, h264: &'static str) {
    let mut prev: Option<Frame> = None;
    let mut last_params = *params.borrow();
    let mut enc: Option<Box<dyn VideoEncoder>> = None;
    let (mut av1_failed, mut h264_failed) = (false, false);
    // Hardware H.264 is only tried when the session's probe picked it (and the settings allow it).
    let mut hw_failed = !codec::is_hardware(h264);
    let mut reported = (reported, last_params.codec);
    let mut pointer = Pointer::new();
    let mut warned = false;
    let mut last_seq = None;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let p = *params.borrow();
        if p != last_params {
            last_params = p;
            prev = None;
            enc = None;
        }
        let img = match grab(p.display, &mut pointer, &mut last_seq, prev.is_none() || force_full.load(Ordering::Relaxed), &mut warned) {
            Grab::Frame(img, cursor) => {
                if let Some(msg) = cursor {
                    if out.blocking_send(msg).is_err() {
                        return;
                    }
                }
                img
            }
            Grab::Unchanged => {
                std::thread::sleep(Duration::from_millis(1000 / p.fps.clamp(1, 60) as u64));
                continue;
            }
            Grab::Failed => {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        // What this frame is encoded with: the requested codec unless it failed earlier in
        // the session (AV1 -> H.264 -> JPEG).
        let mut codec = match p.codec {
            Codec::Av1 if !av1_failed => Codec::Av1,
            Codec::Av1 | Codec::H264 if !h264_failed => Codec::H264,
            _ => Codec::Jpeg,
        };
        let cur = downsample(&img, p.max_width, codec.is_video());
        let full = force_full.swap(false, Ordering::Relaxed);
        let rect = match &prev {
            Some(pf) if !full && pf.w == cur.w && pf.h == cur.h => dirty_rect(pf, &cur),
            _ => Some((0, 0, cur.w, cur.h)),
        };
        if let Some(r) = rect {
            let msg = if codec.is_video() {
                let key = full || prev.is_none();
                let mut res = encode_video(&mut enc, &cur, p, codec, key, !hw_failed);
                if let (Err(e), Codec::Av1) = (&res, codec) {
                    // Every viewer that can take AV1 also decodes H.264.
                    tracing::warn!("{e:#}; switching to H.264");
                    av1_failed = true;
                    codec = Codec::H264;
                    res = encode_video(&mut enc, &cur, p, codec, true, !hw_failed);
                }
                if res.is_err() && enc.as_ref().is_some_and(|e| e.name() != "openh264") {
                    // Hardware encoder broke mid-session: carry on in software.
                    tracing::warn!("{:#}; switching to OpenH264", res.as_ref().err().unwrap());
                    hw_failed = true;
                    enc = None;
                    res = encode_video(&mut enc, &cur, p, codec, true, false);
                }
                res.unwrap_or_else(|e| {
                    // The viewer decodes JPEG rectangles too, so degrade instead of going dark.
                    tracing::error!("{e:#}; falling back to JPEG");
                    h264_failed = true;
                    codec = Codec::Jpeg;
                    enc = None;
                    Some(encode_rect(&cur, (0, 0, cur.w, cur.h), p.quality))
                })
            } else {
                Some(encode_rect(&cur, r, p.quality))
            };
            let active = match &enc {
                Some(e) if codec.is_video() => (e.name(), codec),
                _ => ("jpeg", Codec::Jpeg),
            };
            if active != reported {
                reported = active;
                tracing::info!("video encoder: {} ({})", active.0, active.1.name());
                let note = serde_json::json!({"t": "encoder", "encoder": active.0, "codec": active.1.name()});
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

enum Grab {
    /// A screen image, and a cursor message if the pointer moved.
    Frame(Image, Option<Vec<u8>>),
    /// Nothing new since the last frame (the portal only sends frames when the screen changes).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unchanged,
    Failed,
}

/// A captured image: owned (xcap) or shared with the portal's frame slot.
enum Image {
    Owned(image::RgbaImage),
    #[allow(dead_code)]
    Shared(Arc<image::RgbaImage>),
}

impl std::ops::Deref for Image {
    type Target = image::RgbaImage;
    fn deref(&self) -> &image::RgbaImage {
        match self {
            Image::Owned(i) => i,
            Image::Shared(i) => i,
        }
    }
}

/// Captures the display: from the Wayland portal's stream when one is in use, else with xcap
/// (X11, macOS, Windows). `need` asks for an image even if nothing changed.
#[allow(unused_variables)]
fn grab(display: usize, pointer: &mut Pointer, last_seq: &mut Option<u64>, need: bool, warned: &mut bool) -> Grab {
    #[cfg(target_os = "linux")]
    if let Some(portal) = portal() {
        // The pointer is drawn into the portal's video, so no cursor messages.
        return match portal.latest(display) {
            Some((seq, img)) if need || *last_seq != Some(seq) => {
                *last_seq = Some(seq);
                Grab::Frame(Image::Shared(img), None)
            }
            Some(_) => Grab::Unchanged,
            None => Grab::Unchanged, // the first frame has not arrived yet
        };
    }
    let monitors = Monitor::all().unwrap_or_default();
    let Some(m) = monitors.get(display).or(monitors.first()) else {
        if !*warned {
            tracing::error!("screen capture: no display found{}", if cfg!(target_os = "linux") { " (is DISPLAY set? Wayland needs the desktop portal)" } else { "" });
            *warned = true;
        }
        return Grab::Failed;
    };
    let geometry = Geometry { display, x: m.x().unwrap_or(0), y: m.y().unwrap_or(0), width: m.width().unwrap_or(0), height: m.height().unwrap_or(0) };
    let cursor = pointer.poll(geometry);
    match m.capture_image() {
        Ok(i) => Grab::Frame(Image::Owned(i), cursor),
        Err(e) => {
            if !*warned {
                tracing::error!("screen capture failed: {e} (macOS: grant Screen Recording permission)");
                *warned = true;
            }
            Grab::Failed
        }
    }
}

/// Encodes a whole frame as H.264 or AV1, (re)creating the encoder when the size, codec or
/// settings change (a new encoder always starts with a keyframe). Ok(None) means the encoder
/// dropped the frame.
fn encode_video(enc: &mut Option<Box<dyn VideoEncoder>>, f: &Frame, p: Params, codec: Codec, keyframe: bool, allow_hw: bool) -> anyhow::Result<Option<Vec<u8>>> {
    let mut keyframe = keyframe;
    if enc.as_ref().is_none_or(|e| e.size() != (f.w, f.h) || e.codec() != codec) {
        *enc = None;
        *enc = Some(match codec {
            Codec::Av1 => codec::av1_encoder(f.w, f.h, p.fps, p.quality)?,
            _ => codec::h264_encoder(f.w, f.h, p.fps, p.quality, allow_hw)?,
        });
        keyframe = true;
    }
    let e = enc.as_mut().unwrap();
    Ok(e.encode(&f.rgb, keyframe)?.map(|(data, key)| proto::video(codec.id(), key, f.w as u16, f.h as u16, &data)))
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
