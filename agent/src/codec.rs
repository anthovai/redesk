//! Video codecs for the screen stream.
//!
//! H.264 is preferred; JPEG rectangles remain as the fallback for viewers that cannot decode
//! H.264. AV1 is opt-in: a viewer that lists "av1" before "h264" gets it when this machine has
//! a hardware AV1 encoder. The viewer lists what it can decode in its `auth` message, best
//! first, and the agent picks the first one it can encode.
//!
//! H.264 encoders, tried in order: the platform's hardware encoder (VideoToolbox on macOS,
//! a Media Foundation hardware MFT on Windows), then OpenH264 (software, screen-content
//! tuning). All of them emit Annex B Constrained Baseline, so every viewer decodes any of them.
//!
//! AV1 is only encoded in hardware (a Media Foundation AV1 MFT on Windows: NVIDIA RTX 40,
//! Intel Arc, AMD RDNA 3 and newer). VideoToolbox has no AV1 encoder, and rav1e (software)
//! was measured too slow and too deep a pipeline for live screens (examples/codec_bench.rs).
//! The desktop viewer decodes AV1 with rav1d.

use std::ptr::NonNull;

use anyhow::{Result, anyhow, bail};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer, YUVSource};
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::DAV1D_PIXEL_LAYOUT_I420;
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib as dav1d;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Jpeg,
    H264,
    Av1,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Jpeg => "jpeg",
            Codec::H264 => "h264",
            Codec::Av1 => "av1",
        }
    }

    pub fn from_name(name: &str) -> Option<Codec> {
        match name {
            "jpeg" => Some(Codec::Jpeg),
            "h264" => Some(Codec::H264),
            "av1" => Some(Codec::Av1),
            _ => None,
        }
    }

    /// Wire id in the MSG_VIDEO header.
    pub fn id(self) -> u8 {
        match self {
            Codec::Jpeg => 0,
            Codec::H264 => 1,
            Codec::Av1 => 2,
        }
    }

    pub fn from_id(id: u8) -> Option<Codec> {
        match id {
            1 => Some(Codec::H264),
            2 => Some(Codec::Av1),
            _ => None,
        }
    }

    /// Whole frames as MSG_VIDEO rather than JPEG rectangles.
    pub fn is_video(self) -> bool {
        self != Codec::Jpeg
    }

    /// Best codec both sides support; viewers that send nothing get JPEG. AV1 needs `av1`
    /// (this machine can encode it) and must come before "h264" in the viewer's list, so a
    /// viewer that lists it only as a fallback, and every 0.4/0.5 viewer, keeps H.264.
    pub fn negotiate(viewer: &[String], av1: bool) -> Codec {
        let pos = |name: &str| viewer.iter().position(|c| c == name);
        match (pos("av1"), pos("h264")) {
            (Some(a), h) if av1 && h.is_none_or(|h| a < h) => Codec::Av1,
            (_, Some(_)) => Codec::H264,
            _ => Codec::Jpeg,
        }
    }
}

/// Target bitrate from resolution, frame rate and the viewer's quality setting (10–95).
pub fn bitrate(w: usize, h: usize, fps: u32, quality: u8) -> u32 {
    let bits_per_pixel = match quality {
        80.. => 0.10,
        60..80 => 0.06,
        _ => 0.035,
    };
    ((w * h) as f64 * fps.max(1) as f64 * bits_per_pixel).clamp(300_000.0, 12_000_000.0) as u32
}

/// One video encoder instance for a fixed frame size.
pub trait VideoEncoder {
    /// Encodes one packed RGB frame. Returns one access unit (H.264: Annex B; AV1: a temporal
    /// unit of low-overhead OBUs, with the sequence header on keyframes) and whether it is a
    /// keyframe, or None when the encoder dropped the frame.
    fn encode(&mut self, rgb: &[u8], keyframe: bool) -> Result<Option<(Vec<u8>, bool)>>;
    fn size(&self) -> (usize, usize);
    /// "videotoolbox", "mediafoundation" or "openh264" (reported to viewers).
    fn name(&self) -> &'static str;
    fn codec(&self) -> Codec {
        Codec::H264
    }
}

#[cfg(target_os = "macos")]
#[path = "hwenc_mac.rs"]
mod hw;

#[cfg(windows)]
#[path = "hwenc_win.rs"]
mod hw;

/// Hardware encoding can be turned off with AEGIRON_HW_ENCODER=0 (diagnostics).
pub fn hardware_allowed() -> bool {
    std::env::var("AEGIRON_HW_ENCODER").map(|v| v != "0").unwrap_or(true)
}

/// The platform's hardware H.264 encoder, if there is one and it accepts this configuration.
#[allow(unused_variables)]
pub fn hardware_encoder(w: usize, h: usize, fps: u32, quality: u8) -> Result<Box<dyn VideoEncoder>> {
    if !hardware_allowed() {
        anyhow::bail!("hardware encoding disabled");
    }
    #[cfg(any(windows, target_os = "macos"))]
    return Ok(Box::new(hw::new(w, h, fps, quality)?));
    #[cfg(not(any(windows, target_os = "macos")))]
    anyhow::bail!("no hardware encoder on this platform")
}

/// The platform's hardware AV1 encoder. Only Media Foundation has one (recent GPUs).
#[allow(unused_variables)]
pub fn av1_encoder(w: usize, h: usize, fps: u32, quality: u8) -> Result<Box<dyn VideoEncoder>> {
    if !hardware_allowed() {
        bail!("hardware encoding disabled");
    }
    #[cfg(windows)]
    return Ok(Box::new(hw::new_av1(w, h, fps, quality)?));
    #[cfg(not(windows))]
    bail!("no AV1 encoder on this platform")
}

/// The AV1 encoder sessions on this machine can use, if any (probed once with a small frame).
pub fn av1_available() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *NAME.get_or_init(|| {
        let (w, h) = (640, 360);
        let rgb = vec![90u8; w * h * 3];
        match av1_encoder(w, h, 15, 70).and_then(|mut e| e.encode(&rgb, true).map(|out| (e.name(), out))) {
            Ok((name, Some(_))) => {
                tracing::info!("video: hardware AV1 encoder available ({name})");
                Some(name)
            }
            Ok((name, None)) => {
                tracing::warn!("video: {name} produced no AV1 output; AV1 is not offered");
                None
            }
            Err(e) => {
                tracing::info!("video: {e:#}; AV1 is not offered");
                None
            }
        }
    })
}

/// Whether an encoder name reported to viewers is a hardware encoder.
pub fn is_hardware(name: &str) -> bool {
    matches!(name, "videotoolbox" | "mediafoundation")
}

/// Best available H.264 encoder: hardware, else OpenH264.
pub fn h264_encoder(w: usize, h: usize, fps: u32, quality: u8, allow_hw: bool) -> Result<Box<dyn VideoEncoder>> {
    if allow_hw {
        match hardware_encoder(w, h, fps, quality) {
            Ok(e) => return Ok(e),
            Err(e) => tracing::debug!("{e:#}; using OpenH264"),
        }
    }
    Ok(Box::new(H264Encoder::new(w, h, fps, quality)?))
}

/// Which H.264 encoder sessions on this machine will use (probed once with a small frame).
pub fn preferred_encoder() -> &'static str {
    static NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let (w, h) = (640, 360);
        let rgb = vec![90u8; w * h * 3];
        match hardware_encoder(w, h, 15, 70).and_then(|mut e| e.encode(&rgb, true).map(|out| (e.name(), out))) {
            Ok((name, Some(_))) => {
                tracing::info!("video: hardware H.264 encoder available ({name})");
                name
            }
            Ok((name, None)) => {
                tracing::warn!("video: {name} produced no output; using OpenH264");
                "openh264"
            }
            Err(e) => {
                tracing::info!("video: {e:#}; using OpenH264");
                "openh264"
            }
        }
    })
}

pub struct H264Encoder {
    enc: Encoder,
    pub w: usize,
    pub h: usize,
    yuv: YUVBuffer,
}

impl VideoEncoder for H264Encoder {
    fn encode(&mut self, rgb: &[u8], keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        H264Encoder::encode(self, rgb, keyframe)
    }

    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn name(&self) -> &'static str {
        "openh264"
    }
}

impl H264Encoder {
    /// `w` and `h` must be even.
    pub fn new(w: usize, h: usize, fps: u32, quality: u8) -> Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate(w, h, fps, quality)))
            .max_frame_rate(FrameRate::from_hz(fps.clamp(1, 60) as f32))
            // Frames are only sent when the screen changes, so time-based GOPs make little
            // sense; keyframes come on start, on request and every ~10 s of activity.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps.max(1) * 10))
            .skip_frames(false);
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| anyhow!("h264 encoder: {e}"))?;
        Ok(Self { enc, w, h, yuv: YUVBuffer::new(w, h) })
    }

    /// Encodes one RGB frame. Returns the Annex-B bitstream and whether it is a keyframe,
    /// or None when the encoder skipped the frame.
    pub fn encode(&mut self, rgb: &[u8], keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        if keyframe {
            self.enc.force_intra_frame();
        }
        self.yuv.read_rgb8(RgbSliceU8::new(rgb, (self.w, self.h)));
        let bs = self.enc.encode(&self.yuv).map_err(|e| anyhow!("h264 encode: {e}"))?;
        let ft = bs.frame_type();
        if matches!(ft, FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        Ok(Some((bs.to_vec(), matches!(ft, FrameType::IDR | FrameType::I))))
    }
}

pub struct H264Decoder {
    dec: Decoder,
    rgba: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let dec = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new()).map_err(|e| anyhow!("h264 decoder: {e}"))?;
        Ok(Self { dec, rgba: Vec::new() })
    }

    /// Decodes one Annex-B access unit into RGBA. Ok(None) means "no picture yet".
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, &[u8])>> {
        let Some(yuv) = self.dec.decode(data).map_err(|e| anyhow!("h264 decode: {e}"))? else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        self.rgba.resize(w * h * 4, 0);
        yuv.write_rgba8(&mut self.rgba);
        Ok(Some((w, h, &self.rgba)))
    }
}

/// AV1 OBU types (AV1 spec 6.2.2).
pub const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_FRAME: u8 = 6;

/// Splits AV1 low-overhead bitstream data into OBUs: (type, whole OBU, payload).
/// Stops at the first malformed OBU.
pub fn av1_obus(data: &[u8]) -> Vec<(u8, &[u8], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let start = i;
        let header = data[i];
        i += if header & 0x04 != 0 { 2 } else { 1 }; // obu_extension_flag
        let size = if header & 0x02 != 0 {
            // obu_has_size_field: leb128
            let mut size = 0usize;
            let mut shift = 0;
            loop {
                let Some(&b) = data.get(i) else { return out };
                i += 1;
                size |= ((b & 0x7f) as usize) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
                if shift > 49 {
                    return out;
                }
            }
            size
        } else {
            data.len().saturating_sub(i)
        };
        let Some(end) = i.checked_add(size).filter(|&e| e <= data.len()) else { return out };
        out.push(((header >> 3) & 0x0f, &data[start..end], &data[i..end]));
        i = end;
    }
    out
}

/// Whether an AV1 temporal unit starts with a key frame (first frame header: not
/// show_existing_frame, frame_type KEY_FRAME). Assumes reduced_still_picture_header = 0, which
/// holds for video encoders.
pub fn av1_is_keyframe(data: &[u8]) -> bool {
    av1_obus(data)
        .into_iter()
        .find(|(t, _, _)| *t == OBU_FRAME || *t == OBU_FRAME_HEADER)
        .and_then(|(_, _, payload)| payload.first().copied())
        .is_some_and(|b| b & 0x80 == 0 && (b >> 5) & 0x03 == 0)
}

/// The sequence header OBU in an AV1 temporal unit, if there is one.
pub fn av1_sequence_header(data: &[u8]) -> Option<&[u8]> {
    av1_obus(data).into_iter().find(|(t, _, _)| *t == OBU_SEQUENCE_HEADER).map(|(_, obu, _)| obu)
}

/// AV1 decoder for the desktop viewer (rav1d, the Rust port of dav1d), one temporal unit in,
/// one RGBA picture out, without frame delay.
pub struct Av1Decoder {
    ctx: Option<Dav1dContext>,
    rgba: Vec<u8>,
}

// SAFETY: the context is an Arc'd, internally synchronised decoder; it is only used through
// &mut self, so moving it to another thread is fine.
unsafe impl Send for Av1Decoder {}

impl Av1Decoder {
    pub fn new() -> Result<Self> {
        let mut s = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: dav1d_default_settings initialises every field.
        let mut s = unsafe {
            dav1d::dav1d_default_settings(NonNull::new_unchecked(s.as_mut_ptr()));
            s.assume_init()
        };
        s.n_threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4)) as i32;
        // One frame in flight: every temporal unit comes out as soon as it is decoded.
        s.max_frame_delay = 1;
        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: both pointers are valid for the call.
        let r = unsafe { dav1d::dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(&mut s))) };
        if r.0 != 0 || ctx.is_none() {
            bail!("av1 decoder: error {}", r.0);
        }
        Ok(Self { ctx, rgba: Vec::new() })
    }

    /// Decodes one temporal unit into RGBA. Ok(None) means "no picture yet".
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, &[u8])>> {
        let eagain = -libc::EAGAIN;
        let mut d = Dav1dData::default();
        // SAFETY: dav1d_data_create allocates `data.len()` bytes owned by `d`.
        let buf = unsafe { dav1d::dav1d_data_create(Some(NonNull::from(&mut d)), data.len()) };
        if buf.is_null() {
            bail!("av1 decode: cannot allocate {} bytes", data.len());
        }
        // SAFETY: `buf` points to `data.len()` writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len()) };
        let mut size = None;
        let res = (|| -> Result<()> {
            loop {
                // SAFETY: the context is open and `d` is a valid Dav1dData.
                let sent = unsafe { dav1d::dav1d_send_data(self.ctx, Some(NonNull::from(&mut d))) }.0;
                if sent != 0 && sent != eagain {
                    bail!("av1 decode: error {sent}");
                }
                let mut pic = Dav1dPicture::default();
                // SAFETY: as above; `pic` is written on success and released below.
                let got = unsafe { dav1d::dav1d_get_picture(self.ctx, Some(NonNull::from(&mut pic))) }.0;
                if got == 0 {
                    let r = self.convert(&pic);
                    // SAFETY: `pic` came from dav1d_get_picture.
                    unsafe { dav1d::dav1d_picture_unref(Some(NonNull::from(&mut pic))) };
                    size = Some(r?);
                } else if got != eagain {
                    bail!("av1 decode: error {got}");
                }
                if d.sz == 0 {
                    return Ok(());
                }
                if sent == eagain && got == eagain {
                    bail!("av1 decode: decoder stalled");
                }
            }
        })();
        // SAFETY: releases whatever of `d` the decoder did not take (no-op when empty).
        unsafe { dav1d::dav1d_data_unref(Some(NonNull::from(&mut d))) };
        res?;
        Ok(size.map(|(w, h)| (w, h, &self.rgba[..])))
    }

    /// 8-bit 4:2:0 picture -> RGBA (BT.601 limited range, like every encoder here).
    fn convert(&mut self, pic: &Dav1dPicture) -> Result<(usize, usize)> {
        let (w, h) = (pic.p.w.max(0) as usize, pic.p.h.max(0) as usize);
        if pic.p.layout != DAV1D_PIXEL_LAYOUT_I420 || pic.p.bpc != 8 {
            bail!("av1 decode: unsupported picture format (layout {}, {} bit)", pic.p.layout, pic.p.bpc);
        }
        let [Some(y), Some(u), Some(v)] = pic.data else { bail!("av1 decode: picture without planes") };
        let (ys, cs) = (pic.stride[0] as usize, pic.stride[1] as usize);
        // SAFETY: dav1d guarantees `stride * rows` readable bytes per plane.
        let (y, u, v) = unsafe {
            (
                std::slice::from_raw_parts(y.as_ptr() as *const u8, ys * h),
                std::slice::from_raw_parts(u.as_ptr() as *const u8, cs * h.div_ceil(2)),
                std::slice::from_raw_parts(v.as_ptr() as *const u8, cs * h.div_ceil(2)),
            )
        };
        self.rgba.resize(w * h * 4, 0);
        for row in 0..h {
            let (yr, ur, vr) = (&y[row * ys..], &u[(row / 2) * cs..], &v[(row / 2) * cs..]);
            for (x, px) in self.rgba[row * w * 4..(row + 1) * w * 4].chunks_exact_mut(4).enumerate() {
                let c = 298 * (yr[x] as i32 - 16);
                let (d, e) = (ur[x / 2] as i32 - 128, vr[x / 2] as i32 - 128);
                px[0] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
                px[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
                px[2] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
                px[3] = 255;
            }
        }
        Ok((w, h))
    }
}

impl Drop for Av1Decoder {
    fn drop(&mut self) {
        // SAFETY: closes the context opened in new() and sets it to None.
        unsafe { dav1d::dav1d_close(Some(NonNull::from(&mut self.ctx))) };
    }
}

/// The viewer's decoder for whichever video codec the agent is sending.
pub enum VideoDecoder {
    H264(H264Decoder),
    Av1(Av1Decoder),
}

impl VideoDecoder {
    pub fn new(codec: Codec) -> Result<Self> {
        Ok(match codec {
            Codec::H264 => VideoDecoder::H264(H264Decoder::new()?),
            Codec::Av1 => VideoDecoder::Av1(Av1Decoder::new()?),
            Codec::Jpeg => bail!("JPEG is not a video codec"),
        })
    }

    pub fn codec(&self) -> Codec {
        match self {
            VideoDecoder::H264(_) => Codec::H264,
            VideoDecoder::Av1(_) => Codec::Av1,
        }
    }

    /// Decodes one access unit into RGBA. Ok(None) means "no picture yet".
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, &[u8])>> {
        match self {
            VideoDecoder::H264(d) => d.decode(data),
            VideoDecoder::Av1(d) => d.decode(data),
        }
    }
}

/// Packed RGB -> planar 4:2:0 (Y, U, V; BT.601 limited range), for software encoders that
/// take planes (rav1e in tests and examples/codec_bench.rs). `w` and `h` must be even.
pub fn rgb_to_i420(rgb: &[u8], w: usize, h: usize) -> [Vec<u8>; 3] {
    let mut y = vec![0u8; w * h];
    let (mut u, mut v) = (vec![0u8; w * h / 4], vec![0u8; w * h / 4]);
    for (i, p) in rgb.chunks_exact(3).take(w * h).enumerate() {
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        y[i] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
    }
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = &rgb[((cy * 2 + dy) * w + cx * 2 + dx) * 3..];
                r += p[0] as i32;
                g += p[1] as i32;
                b += p[2] as i32;
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            u[cy * (w / 2) + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            v[cy * (w / 2) + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    [y, u, v]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn negotiation_prefers_h264() {
        for av1 in [false, true] {
            assert_eq!(Codec::negotiate(&list(&["jpeg", "h264"]), av1), Codec::H264);
            assert_eq!(Codec::negotiate(&list(&["h264", "jpeg"]), av1), Codec::H264);
            assert_eq!(Codec::negotiate(&list(&["jpeg"]), av1), Codec::Jpeg);
            assert_eq!(Codec::negotiate(&[], av1), Codec::Jpeg);
            // AV1 listed as a fallback only: H.264 stays.
            assert_eq!(Codec::negotiate(&list(&["h264", "av1", "jpeg"]), av1), Codec::H264);
        }
    }

    #[test]
    fn negotiation_av1_is_opt_in() {
        assert_eq!(Codec::negotiate(&list(&["av1", "h264", "jpeg"]), true), Codec::Av1);
        assert_eq!(Codec::negotiate(&list(&["av1", "jpeg"]), true), Codec::Av1);
        // No AV1 encoder on this machine: the viewer's next choice.
        assert_eq!(Codec::negotiate(&list(&["av1", "h264", "jpeg"]), false), Codec::H264);
        assert_eq!(Codec::negotiate(&list(&["av1", "jpeg"]), false), Codec::Jpeg);
        for c in [Codec::Jpeg, Codec::H264, Codec::Av1] {
            assert_eq!(Codec::from_name(c.name()), Some(c));
        }
        assert_eq!(Codec::from_id(Codec::Av1.id()), Some(Codec::Av1));
    }

    #[test]
    fn av1_obu_parsing() {
        // Temporal delimiter, then a sequence header and a frame with a key frame header.
        let tu = [0x12, 0x00, 0x0a, 0x02, 0xaa, 0xbb, 0x32, 0x03, 0x10, 0x00, 0x00];
        let obus = av1_obus(&tu);
        assert_eq!(obus.iter().map(|o| o.0).collect::<Vec<_>>(), [2, OBU_SEQUENCE_HEADER, OBU_FRAME]);
        assert_eq!(av1_sequence_header(&tu), Some(&tu[2..6]));
        assert!(av1_is_keyframe(&tu));
        // frame_type INTER_FRAME (1).
        let inter = [0x12, 0x00, 0x32, 0x02, 0x30, 0x00];
        assert!(!av1_is_keyframe(&inter));
        assert_eq!(av1_sequence_header(&inter), None);
        // Truncated size: parsing stops instead of reading past the end.
        assert_eq!(av1_obus(&[0x32, 0x09, 0x00]).len(), 0);
    }

    #[test]
    fn h264_round_trip() {
        round_trip(&mut H264Encoder::new(320, 240, 15, 70).unwrap());
    }

    /// The hardware encoder's output must decode with the viewers' decoder (OpenH264) and be
    /// Constrained Baseline. Skipped where there is no hardware encoder (CI, VMs).
    #[test]
    fn hardware_round_trip() {
        let mut enc = match hardware_encoder(320, 240, 15, 70) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("no hardware encoder here: {e:#}");
                return;
            }
        };
        let (key, _) = enc.encode(&vec![128u8; 320 * 240 * 3], true).unwrap().expect("keyframe");
        // SPS: start code, NAL type 7, profile_idc 66 (Baseline), constraint_set1 (constrained).
        let sps = key.windows(5).position(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7).expect("SPS in keyframe");
        assert_eq!(key[sps + 5], 66, "profile_idc must be Baseline");
        assert!(key[sps + 6] & 0x40 != 0, "constraint_set1_flag (Constrained Baseline)");
        round_trip(&mut *hardware_encoder(320, 240, 15, 70).unwrap());
    }

    /// The Media Foundation encoder's media types, NV12 input, output handling and keyframe
    /// detection, with Microsoft's software H.264 MFT (present on every Windows, including CI
    /// runners without a GPU). Only the asynchronous event pump is left to hardware_round_trip.
    #[cfg(windows)]
    #[test]
    fn mediafoundation_software_round_trip() {
        let mut enc = match hw::new_software(320, 240, 15, 70) {
            Ok(e) => e,
            // Windows N/KN editions and Server Core ship without Media Foundation's codecs.
            Err(e) if std::env::var_os("CI").is_none() => {
                eprintln!("no software Media Foundation H.264 encoder here: {e:#}");
                return;
            }
            Err(e) => panic!("Media Foundation H.264 encoder: {e:#}"),
        };
        let (key, _) = enc.encode(&vec![128u8; 320 * 240 * 3], true).unwrap().expect("keyframe");
        let sps = key.windows(5).position(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7).expect("SPS in keyframe");
        assert_eq!(key[sps + 5], 66, "profile_idc must be Baseline");
        round_trip(&mut hw::new_software(320, 240, 15, 70).unwrap());
    }

    /// AV1 from a hardware encoder must decode with the desktop viewer's decoder (rav1d), and
    /// keyframes must carry the sequence header (WebCodecs is configured without a
    /// description). Skipped where there is no AV1 encoder (everywhere but recent Windows GPUs).
    #[test]
    fn hardware_av1_round_trip() {
        let mut enc = match av1_encoder(320, 240, 15, 70) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("no AV1 encoder here: {e:#}");
                return;
            }
        };
        assert_eq!(enc.codec(), Codec::Av1);
        round_trip(&mut *enc);
    }

    /// Software AV1 (rav1e, test-only) through the same checks: the bitstream layout the
    /// viewers expect, rav1d decoding, and key/delta detection.
    #[test]
    fn av1_round_trip() {
        round_trip(&mut Rav1e::new(320, 240));
    }

    /// rav1e as a VideoEncoder for tests. It holds frames back (lookahead), so every call
    /// re-encodes the whole stream so far and flushes; rav1e is deterministic, so the frames
    /// before the last one are the same the decoder already has. Fine for two frames.
    struct Rav1e {
        w: usize,
        h: usize,
        frames: Vec<Vec<u8>>,
    }

    impl Rav1e {
        fn new(w: usize, h: usize) -> Self {
            Self { w, h, frames: Vec::new() }
        }
    }

    impl VideoEncoder for Rav1e {
        fn encode(&mut self, rgb: &[u8], _keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
            use rav1e::prelude::*;
            self.frames.push(rgb.to_vec());
            let mut cfg = EncoderConfig::with_speed_preset(10);
            (cfg.width, cfg.height, cfg.low_latency) = (self.w, self.h, true);
            cfg.speed_settings.rdo_lookahead_frames = 1;
            let mut ctx: Context<u8> = Config::new().with_encoder_config(cfg).with_threads(2).new_context()?;
            for rgb in &self.frames {
                let mut f = ctx.new_frame();
                for (plane, data) in f.planes.iter_mut().zip(rgb_to_i420(rgb, self.w, self.h)) {
                    let stride = plane.cfg.width;
                    plane.copy_from_raw_u8(&data, stride, 1);
                }
                ctx.send_frame(f)?;
            }
            ctx.flush();
            let mut last = None;
            loop {
                match ctx.receive_packet() {
                    Ok(p) => last = Some(p.data),
                    Err(EncoderStatus::Encoded) => {}
                    Err(EncoderStatus::LimitReached) => break,
                    Err(e) => bail!("rav1e: {e:?}"),
                }
            }
            let data = last.ok_or_else(|| anyhow!("rav1e: no packet"))?;
            let key = av1_is_keyframe(&data);
            Ok(Some((data, key)))
        }

        fn size(&self) -> (usize, usize) {
            (self.w, self.h)
        }

        fn name(&self) -> &'static str {
            "rav1e"
        }

        fn codec(&self) -> Codec {
            Codec::Av1
        }
    }

    fn round_trip(enc: &mut dyn VideoEncoder) {
        let (w, h) = enc.size();
        // A gradient frame, then the same frame with a block changed.
        let mut rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, 128]).collect();
        let mut dec = VideoDecoder::new(enc.codec()).unwrap();

        let (key, is_key) = enc.encode(&rgb, true).unwrap().expect("first frame is encoded");
        assert!(is_key, "first frame must be a keyframe");
        match enc.codec() {
            Codec::Av1 => {
                assert!(av1_is_keyframe(&key));
                assert!(av1_sequence_header(&key).is_some(), "AV1 keyframe must carry the sequence header for WebCodecs");
            }
            _ => assert_eq!(&key[..4], &[0, 0, 0, 1], "Annex-B start code for WebCodecs"),
        }
        let (dw, dh, px) = dec.decode(&key).unwrap().expect("keyframe decodes to a picture");
        assert_eq!((dw, dh), (w, h));
        // Lossy, but a mid-gray-blue pixel should come back close.
        let p = &px[(100 * w + 100) * 4..(100 * w + 100) * 4 + 3];
        assert!((p[0] as i32 - 100).abs() < 12 && (p[1] as i32 - 100).abs() < 12 && (p[2] as i32 - 128).abs() < 12, "{p:?}");

        for y in 50..90 {
            for x in 50..90 {
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[255, 0, 0]);
            }
        }
        let (delta, is_key) = enc.encode(&rgb, false).unwrap().expect("changed frame is encoded");
        assert!(!is_key);
        if enc.codec() == Codec::Av1 {
            assert!(!av1_is_keyframe(&delta));
        }
        assert!(delta.len() < key.len(), "{} P-frame ({}) should be smaller than the keyframe ({})", enc.name(), delta.len(), key.len());
        let (_, _, px) = dec.decode(&delta).unwrap().expect("delta decodes");
        let p = &px[(70 * w + 70) * 4..(70 * w + 70) * 4 + 3];
        assert!(p[0] > 200 && p[1] < 60, "changed block should be red, got {p:?}");
    }
}
