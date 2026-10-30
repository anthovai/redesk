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
    /// Every display as one picture (feature "alldisp"); `display` is ignored.
    pub all: bool,
    pub fps: u32,
    pub quality: u8,
    pub max_width: u32,
    pub codec: Codec,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DisplayInfo {
    pub index: usize,
    pub name: String,
    /// Position in the same units as width/height (for drawing the layout).
    pub x: i32,
    pub y: i32,
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
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            primary: m.is_primary().unwrap_or(false),
        })
        .collect()
}

pub fn geometry(display: usize) -> Option<Geometry> {
    let monitors = Monitor::all().ok()?;
    monitor_geometry(monitors.get(display).or(monitors.first())?)
}

fn monitor_geometry(m: &Monitor) -> Option<Geometry> {
    Some(Geometry { x: m.x().ok()?, y: m.y().ok()?, width: m.width().ok()?, height: m.height().ok()? })
}

/// Bounding box of all displays: the input geometry of the combined view.
pub fn all_geometry() -> Option<Geometry> {
    bounding(Monitor::all().ok()?.iter().filter_map(monitor_geometry))
}

/// Input geometry of what `p` streams.
pub fn geometry_of(p: &Params) -> Option<Geometry> {
    if p.all { all_geometry() } else { geometry(p.display) }
}

fn bounding(it: impl Iterator<Item = Geometry>) -> Option<Geometry> {
    let mut b: Option<(i32, i32, i32, i32)> = None;
    for g in it {
        let (x1, y1) = (g.x + g.width as i32, g.y + g.height as i32);
        b = Some(match b {
            None => (g.x, g.y, x1, y1),
            Some((a, c, d, e)) => (a.min(g.x), c.min(g.y), d.max(x1), e.max(y1)),
        });
    }
    b.map(|(x0, y0, x1, y1)| Geometry { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
}

/// All displays in desktop coordinates as windows are placed (bounding box of `logical_area`).
pub fn logical_area_all() -> Option<[f32; 4]> {
    let n = Monitor::all().ok()?.len();
    let areas: Vec<[f32; 4]> = (0..n).filter_map(logical_area).collect();
    let x0 = areas.iter().map(|a| a[0]).reduce(f32::min)?;
    let y0 = areas.iter().map(|a| a[1]).reduce(f32::min)?;
    let x1 = areas.iter().map(|a| a[0] + a[2]).reduce(f32::max)?;
    let y1 = areas.iter().map(|a| a[1] + a[3]).reduce(f32::max)?;
    Some([x0, y0, x1 - x0, y1 - y0])
}

/// Largest combined picture; denser displays are scaled down to fit.
const MAX_COMPOSITE: u32 = 8192;

/// Places each display's capture at its position in one picture covering the bounding box.
/// The scale (pixels per geometry unit) is the densest display's, capped at MAX_COMPOSITE wide;
/// captures at another density are resized.
fn composite(parts: &[(Geometry, image::RgbaImage)]) -> Option<image::RgbaImage> {
    let b = bounding(parts.iter().map(|(g, _)| *g))?;
    let densest = parts.iter().filter(|(g, _)| g.width > 0).map(|(g, i)| i.width() as f32 / g.width as f32).fold(1.0f32, f32::max);
    let scale = densest.min(MAX_COMPOSITE as f32 / b.width.max(1) as f32);
    let (w, h) = ((b.width as f32 * scale).round() as u32, (b.height as f32 * scale).round() as u32);
    let mut out = image::RgbaImage::new(w.max(1), h.max(1));
    for (g, img) in parts {
        let (pw, ph) = ((g.width as f32 * scale).round() as u32, (g.height as f32 * scale).round() as u32);
        if pw == 0 || ph == 0 {
            continue;
        }
        let x = ((g.x - b.x) as f32 * scale).round() as i64;
        let y = ((g.y - b.y) as f32 * scale).round() as i64;
        if img.width() == pw && img.height() == ph {
            image::imageops::replace(&mut out, img, x, y);
        } else {
            let resized = image::imageops::resize(img, pw, ph, image::imageops::FilterType::Triangle);
            image::imageops::replace(&mut out, &resized, x, y);
        }
    }
    Some(out)
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

/// One screenshot of a display. While privacy mode shows its black windows on macOS, they are
/// left out of the picture (see `privacy`); everywhere else the capture is xcap's.
fn capture_monitor(m: &Monitor) -> anyhow::Result<image::RgbaImage> {
    #[cfg(target_os = "macos")]
    {
        let hidden = crate::privacy::excluded_windows();
        if !hidden.is_empty() {
            let rect = [m.x()? as f64, m.y()? as f64, m.width()? as f64, m.height()? as f64];
            return mac::capture_without(rect, &hidden);
        }
    }
    Ok(m.capture_image()?)
}

#[cfg(target_os = "macos")]
pub mod mac {
    use std::ffi::c_void;

    use anyhow::{Result, anyhow};

    type CFRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCreate(option: u32, relative_to: u32) -> CFRef;
        fn CGWindowListCreateImageFromArray(bounds: CGRect, windows: CFRef, option: u32) -> CFRef;
        fn CGImageGetWidth(image: CFRef) -> usize;
        fn CGImageGetHeight(image: CFRef) -> usize;
        fn CGImageGetBytesPerRow(image: CFRef) -> usize;
        fn CGImageGetDataProvider(image: CFRef) -> CFRef;
        fn CGDataProviderCopyData(provider: CFRef) -> CFRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFRef, i: isize) -> *const c_void;
        fn CFArrayCreate(alloc: CFRef, values: *const *const c_void, n: isize, callbacks: *const c_void) -> CFRef;
        fn CFDataGetBytePtr(data: CFRef) -> *const u8;
        fn CFDataGetLength(data: CFRef) -> isize;
        fn CFRelease(cf: CFRef);
    }

    const ON_SCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
    const IMAGE_DEFAULT: u32 = 0;

    /// On-screen windows other than `hidden`, front to back.
    pub(super) fn keep(all: &[u32], hidden: &[u32]) -> Vec<u32> {
        all.iter().copied().filter(|w| !hidden.contains(w)).collect()
    }

    /// Every on-screen window, front to back.
    pub fn on_screen() -> Vec<u32> {
        // SAFETY: the list is created, read and released here.
        unsafe {
            let list = CGWindowListCreate(ON_SCREEN_ONLY, 0);
            if list.is_null() {
                return Vec::new();
            }
            // The array holds CGWindowIDs stored as pointer-sized values.
            let all = (0..CFArrayGetCount(list)).map(|i| CFArrayGetValueAtIndex(list, i) as usize as u32).collect();
            CFRelease(list);
            all
        }
    }

    /// The display area `rect` (x, y, w, h in global points) composited from every on-screen
    /// window except `hidden`, as RGBA like xcap returns it.
    pub fn capture_without(rect: [f64; 4], hidden: &[u32]) -> Result<image::RgbaImage> {
        let all = on_screen();
        if all.is_empty() {
            return Err(anyhow!("cannot list windows"));
        }
        // SAFETY: CoreGraphics/CoreFoundation calls; every object created here is released.
        unsafe {
            let ids: Vec<*const c_void> = keep(&all, hidden).into_iter().map(|w| w as usize as *const c_void).collect();
            let array = CFArrayCreate(std::ptr::null(), ids.as_ptr(), ids.len() as isize, std::ptr::null());
            let bounds = CGRect { x: rect[0], y: rect[1], w: rect[2], h: rect[3] };
            let image = CGWindowListCreateImageFromArray(bounds, array, IMAGE_DEFAULT);
            CFRelease(array);
            if image.is_null() {
                return Err(anyhow!("capture failed"));
            }
            let (w, h, stride) = (CGImageGetWidth(image), CGImageGetHeight(image), CGImageGetBytesPerRow(image));
            let data = CGDataProviderCopyData(CGImageGetDataProvider(image));
            CFRelease(image);
            if data.is_null() {
                return Err(anyhow!("capture failed"));
            }
            let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize);
            let mut rgba = Vec::with_capacity(w * h * 4);
            for row in bytes.chunks_exact(stride.max(1)).take(h) {
                rgba.extend_from_slice(&row[..(w * 4).min(row.len())]);
            }
            CFRelease(data);
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2); // BGRA -> RGBA
            }
            image::RgbaImage::from_raw(w as u32, h as u32, rgba).ok_or_else(|| anyhow!("unexpected image size"))
        }
    }
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
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let p = *params.borrow();
        if p != last_params {
            last_params = p;
            prev = None;
            enc = None;
        }
        let monitors = Monitor::all().unwrap_or_default();
        let Some(m) = monitors.get(p.display).or(monitors.first()) else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let geometry = match p.all {
            true => bounding(monitors.iter().filter_map(monitor_geometry)).unwrap_or_default(),
            false => monitor_geometry(m).unwrap_or_default(),
        };
        if let Some(msg) = pointer.poll(geometry) {
            if out.blocking_send(msg).is_err() {
                return;
            }
        }
        // Privacy mode's black windows are left out on macOS (capture_monitor).
        let captured = match p.all {
            // A display that fails to capture is left black rather than ending the picture.
            true => {
                let parts: Vec<_> = monitors.iter().filter_map(|m| Some((monitor_geometry(m)?, capture_monitor(m).ok()?))).collect();
                composite(&parts).ok_or_else(|| anyhow::anyhow!("no display could be captured"))
            }
            false => capture_monitor(m),
        };
        let img = match captured {
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
    #[cfg(target_os = "macos")]
    fn privacy_windows_are_left_out() {
        assert_eq!(super::mac::keep(&[40, 7, 12, 3], &[7, 3]), [40, 12]);
        assert_eq!(super::mac::keep(&[1, 2], &[]), [1, 2]);
    }

    /// The capture privacy mode uses: the same picture as xcap with nothing hidden, and nothing
    /// of a window once it is hidden. Needs Screen Recording: `cargo test -- --ignored
    /// capture_without`.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn capture_without_hides_windows() {
        let m = Monitor::all().unwrap().into_iter().next().unwrap();
        let rect = [m.x().unwrap() as f64, m.y().unwrap() as f64, m.width().unwrap() as f64, m.height().unwrap() as f64];
        let xcap = m.capture_image().unwrap();
        let all = super::mac::capture_without(rect, &[]).unwrap();
        assert_eq!(all.dimensions(), xcap.dimensions());
        let lit = all.pixels().filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30).count();
        assert!(lit > 0, "nothing captured (Screen Recording permission?)");
        let differ = |a: &image::RgbaImage, b: &image::RgbaImage| {
            a.pixels().zip(b.pixels()).filter(|(p, q)| (0..3).any(|c| (p[c] as i32 - q[c] as i32).abs() > 24)).count() as f64 / a.pixels().len() as f64
        };
        assert!(differ(&all, &xcap) < 0.05, "not the same picture as xcap");
        // Hiding every window leaves only the desktop picture (drawn by the window server).
        let none = super::mac::capture_without(rect, &super::mac::on_screen()).unwrap();
        assert!(differ(&all, &none) > 0.1, "hidden windows still visible");
    }

    #[test]
    fn dirty_rect_finds_change() {
        let a = Frame { w: 4, h: 3, rgb: vec![0; 36] };
        let mut b = Frame { w: 4, h: 3, rgb: vec![0; 36] };
        assert_eq!(dirty_rect(&a, &b), None);
        b.rgb[(1 * 4 + 2) * 3] = 9; // pixel (2,1)
        b.rgb[(2 * 4 + 1) * 3 + 2] = 9; // pixel (1,2)
        assert_eq!(dirty_rect(&a, &b), Some((1, 1, 2, 2)));
    }

    #[test]
    fn composite_places_displays() {
        let solid = |w, h, v: u8| image::RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255]));
        // A 2x "Retina" display at (0,0) 4x3 points and a 1x display to its right, lower down.
        let parts = vec![
            (Geometry { x: 0, y: 0, width: 4, height: 3 }, solid(8, 6, 200)),
            (Geometry { x: 4, y: 1, width: 2, height: 2 }, solid(2, 2, 50)),
        ];
        let out = composite(&parts).unwrap();
        assert_eq!((out.width(), out.height()), (12, 6));
        assert_eq!(out.get_pixel(0, 0)[0], 200);
        assert_eq!(out.get_pixel(9, 3)[0], 50); // scaled up to 4x4 at (8,2)
        assert_eq!(out.get_pixel(9, 0)[3], 0); // outside every display
        let b = bounding(parts.iter().map(|(g, _)| *g)).unwrap();
        assert_eq!((b.x, b.y, b.width, b.height), (0, 0, 6, 3));
    }
}
