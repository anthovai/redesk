//! Hardware H.264 encoding with VideoToolbox (macOS).
//!
//! Configured for what every Aegiron viewer already decodes: Constrained Baseline (falls back
//! to Baseline on systems without that profile constant), no frame reordering, real-time rate
//! control, BT.601 colour, output converted from AVCC to Annex B with SPS/PPS in front of every
//! keyframe. Only the hardware encoder is accepted; the software path is OpenH264.

use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};

use super::{VideoEncoder, bitrate};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type OSStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const CM_TIME_INVALID: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
const PIXEL_BGRA: i32 = i32::from_be_bytes(*b"BGRA");
const CF_NUMBER_SINT32: isize = 3;
const CF_NUMBER_FLOAT64: isize = 6;
const CF_UTF8: u32 = 0x0800_0100;
const ATTACH_SHOULD_PROPAGATE: u32 = 1;

type OutputCallback = extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *const c_void);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(a: CFTypeRef, k: *const CFTypeRef, v: *const CFTypeRef, n: isize, kc: *const c_void, vc: *const c_void) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFNumberCreate(a: CFTypeRef, t: isize, v: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithCString(a: CFTypeRef, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_YCbCrMatrix: CFStringRef;
    static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: CFStringRef;
    static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    fn VTCompressionSessionCreate(
        a: CFTypeRef,
        w: i32,
        h: i32,
        codec: u32,
        spec: CFTypeRef,
        src_attrs: CFTypeRef,
        comp_alloc: CFTypeRef,
        cb: OutputCallback,
        refcon: *mut c_void,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTSessionSetProperty(s: CFTypeRef, k: CFStringRef, v: CFTypeRef) -> OSStatus;
    fn VTSessionCopyProperty(s: CFTypeRef, k: CFStringRef, a: CFTypeRef, out: *mut CFTypeRef) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(s: CFTypeRef) -> OSStatus;
    fn VTCompressionSessionGetPixelBufferPool(s: CFTypeRef) -> CFTypeRef;
    fn VTCompressionSessionEncodeFrame(s: CFTypeRef, img: CFTypeRef, pts: CMTime, dur: CMTime, props: CFTypeRef, frame_refcon: *mut c_void, flags_out: *mut u32) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(s: CFTypeRef, until: CMTime) -> OSStatus;
    fn VTCompressionSessionInvalidate(s: CFTypeRef);
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    static kCVImageBufferYCbCrMatrixKey: CFStringRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_601_4: CFStringRef;
    fn CVPixelBufferPoolCreatePixelBuffer(a: CFTypeRef, pool: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn CVPixelBufferLockBaseAddress(pb: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: CFTypeRef) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(pb: CFTypeRef) -> usize;
    fn CVBufferSetAttachment(b: CFTypeRef, k: CFStringRef, v: CFTypeRef, mode: u32);
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    static kCMSampleAttachmentKey_NotSync: CFStringRef;
    fn CMSampleBufferGetDataBuffer(sb: CFTypeRef) -> CFTypeRef;
    fn CMSampleBufferGetFormatDescription(sb: CFTypeRef) -> CFTypeRef;
    fn CMSampleBufferGetSampleAttachmentsArray(sb: CFTypeRef, create: u8) -> CFTypeRef;
    fn CMBlockBufferGetDataLength(bb: CFTypeRef) -> usize;
    fn CMBlockBufferCopyDataBytes(bb: CFTypeRef, offset: usize, len: usize, dest: *mut c_void) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: CFTypeRef,
        index: usize,
        ps: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_header_len: *mut i32,
    ) -> OSStatus;
}

/// Owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn num_i32(v: i32) -> Cf {
    Cf(unsafe { CFNumberCreate(std::ptr::null(), CF_NUMBER_SINT32, &v as *const i32 as *const c_void) })
}

fn num_f64(v: f64) -> Cf {
    Cf(unsafe { CFNumberCreate(std::ptr::null(), CF_NUMBER_FLOAT64, &v as *const f64 as *const c_void) })
}

fn cfstr(s: &std::ffi::CStr) -> Cf {
    Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), CF_UTF8) })
}

fn dict(pairs: &[(CFTypeRef, CFTypeRef)]) -> Cf {
    let k: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
    let v: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
    Cf(unsafe {
        CFDictionaryCreate(std::ptr::null(), k.as_ptr(), v.as_ptr(), pairs.len() as isize, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
    })
}

type Output = Mutex<Vec<Result<(Vec<u8>, bool), OSStatus>>>;

extern "C" fn on_output(refcon: *mut c_void, _frame: *mut c_void, status: OSStatus, _flags: u32, sb: *const c_void) {
    let out = unsafe { &*(refcon as *const Output) };
    let res = if status != 0 || sb.is_null() { Err(status) } else { unsafe { annex_b(sb) }.ok_or(-1) };
    out.lock().unwrap().push(res);
}

/// AVCC sample buffer -> Annex B access unit (+ SPS/PPS on keyframes).
unsafe fn annex_b(sb: CFTypeRef) -> Option<(Vec<u8>, bool)> {
    unsafe {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, 0);
        let mut key = true;
        if !attachments.is_null() && CFArrayGetCount(attachments) > 0 {
            let a = CFArrayGetValueAtIndex(attachments, 0);
            let not_sync = CFDictionaryGetValue(a, kCMSampleAttachmentKey_NotSync);
            key = not_sync.is_null() || CFBooleanGetValue(not_sync) == 0;
        }
        let desc = CMSampleBufferGetFormatDescription(sb);
        let (mut count, mut nal_len) = (0usize, 4i32);
        let (mut ptr, mut size) = (std::ptr::null(), 0usize);
        if desc.is_null() || CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, 0, &mut ptr, &mut size, &mut count, &mut nal_len) != 0 {
            return None;
        }
        let mut out = Vec::new();
        if key {
            for i in 0..count {
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, i, &mut ptr, &mut size, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(std::slice::from_raw_parts(ptr, size));
                }
            }
        }
        let bb = CMSampleBufferGetDataBuffer(sb);
        if bb.is_null() {
            return None;
        }
        let mut data = vec![0u8; CMBlockBufferGetDataLength(bb)];
        if CMBlockBufferCopyDataBytes(bb, 0, data.len(), data.as_mut_ptr() as *mut c_void) != 0 {
            return None;
        }
        let n = nal_len.clamp(1, 4) as usize;
        let mut i = 0;
        while i + n <= data.len() {
            let len = data[i..i + n].iter().fold(0usize, |acc, b| (acc << 8) | *b as usize);
            i += n;
            if len == 0 || i + len > data.len() {
                break;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&data[i..i + len]);
            i += len;
        }
        Some((out, key))
    }
}

pub struct VtEncoder {
    session: CFTypeRef,
    output: Box<Output>,
    w: usize,
    h: usize,
    started: Instant,
}

impl VtEncoder {
    pub fn new(w: usize, h: usize, fps: u32, quality: u8) -> Result<Self> {
        let output: Box<Output> = Box::new(Mutex::new(Vec::new()));
        unsafe {
            let spec = dict(&[(kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, kCFBooleanTrue)]);
            let empty = dict(&[]);
            let (fmt, cw, ch) = (num_i32(PIXEL_BGRA), num_i32(w as i32), num_i32(h as i32));
            let attrs = dict(&[
                (kCVPixelBufferPixelFormatTypeKey, fmt.0),
                (kCVPixelBufferWidthKey, cw.0),
                (kCVPixelBufferHeightKey, ch.0),
                (kCVPixelBufferIOSurfacePropertiesKey, empty.0),
            ]);
            let mut session: CFTypeRef = std::ptr::null();
            let st = VTCompressionSessionCreate(
                std::ptr::null(),
                w as i32,
                h as i32,
                CODEC_H264,
                spec.0,
                attrs.0,
                std::ptr::null(),
                on_output,
                &*output as *const Output as *mut c_void,
                &mut session,
            );
            if st != 0 || session.is_null() {
                bail!("VideoToolbox: no hardware H.264 encoder ({st})");
            }
            let enc = Self { session, output, w, h, started: Instant::now() };
            let set = |k: CFStringRef, v: CFTypeRef| VTSessionSetProperty(session, k, v);
            set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
            set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
            // Constrained Baseline (macOS 12+) is what the viewers declare (avc1.42E0xx).
            let cbp = cfstr(c"H264_ConstrainedBaseline_AutoLevel");
            if set(kVTCompressionPropertyKey_ProfileLevel, cbp.0) != 0 && set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_AutoLevel) != 0 {
                bail!("VideoToolbox: baseline profile not supported");
            }
            let rate = num_i32(bitrate(w, h, fps, quality) as i32);
            set(kVTCompressionPropertyKey_AverageBitRate, rate.0);
            let fr = num_f64(fps.clamp(1, 60) as f64);
            set(kVTCompressionPropertyKey_ExpectedFrameRate, fr.0);
            let gop = num_i32((fps.max(1) * 10) as i32);
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, gop.0);
            set(kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_601_4);
            let st = VTCompressionSessionPrepareToEncodeFrames(session);
            if st != 0 {
                bail!("VideoToolbox: prepare failed ({st})");
            }
            let mut hw: CFTypeRef = std::ptr::null();
            if VTSessionCopyProperty(session, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, std::ptr::null(), &mut hw) == 0 && !hw.is_null() {
                let on = CFBooleanGetValue(hw) != 0;
                CFRelease(hw);
                if !on {
                    bail!("VideoToolbox: encoder is not hardware accelerated");
                }
            }
            Ok(enc)
        }
    }
}

impl VideoEncoder for VtEncoder {
    fn encode(&mut self, rgb: &[u8], keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(self.session);
            if pool.is_null() {
                bail!("VideoToolbox: no pixel buffer pool");
            }
            let mut pb: CFTypeRef = std::ptr::null();
            if CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb) != 0 || pb.is_null() {
                bail!("VideoToolbox: no pixel buffer");
            }
            let pb = Cf(pb);
            CVPixelBufferLockBaseAddress(pb.0, 0);
            let base = CVPixelBufferGetBaseAddress(pb.0);
            let stride = CVPixelBufferGetBytesPerRow(pb.0);
            if base.is_null() || stride < self.w * 4 {
                CVPixelBufferUnlockBaseAddress(pb.0, 0);
                bail!("VideoToolbox: pixel buffer not writable");
            }
            for y in 0..self.h {
                let dst = std::slice::from_raw_parts_mut(base.add(y * stride), self.w * 4);
                for (d, s) in dst.chunks_exact_mut(4).zip(rgb[y * self.w * 3..(y + 1) * self.w * 3].chunks_exact(3)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            CVPixelBufferUnlockBaseAddress(pb.0, 0);
            CVBufferSetAttachment(pb.0, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4, ATTACH_SHOULD_PROPAGATE);

            let pts = CMTime { value: self.started.elapsed().as_micros() as i64, timescale: 1_000_000, flags: 1, epoch: 0 };
            let props = if keyframe { dict(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue)]) } else { Cf(std::ptr::null()) };
            let st = VTCompressionSessionEncodeFrame(self.session, pb.0, pts, CM_TIME_INVALID, props.0, std::ptr::null_mut(), std::ptr::null_mut());
            if st != 0 {
                bail!("VideoToolbox: encode failed ({st})");
            }
            // Real-time, no reordering: flushing returns this frame's output right away.
            let st = VTCompressionSessionCompleteFrames(self.session, CM_TIME_INVALID);
            if st != 0 {
                bail!("VideoToolbox: flush failed ({st})");
            }
        }
        let mut done = std::mem::take(&mut *self.output.lock().unwrap());
        match done.pop() {
            None => Ok(None),
            Some(Err(st)) => Err(anyhow!("VideoToolbox: frame failed ({st})")),
            Some(Ok((data, key))) if !done.is_empty() => {
                // Should not happen; keep earlier output rather than lose it.
                let mut all: Vec<u8> = done.into_iter().filter_map(|r| r.ok()).flat_map(|(d, _)| d).collect();
                all.extend_from_slice(&data);
                Ok(Some((all, key)))
            }
            Some(Ok(frame)) => Ok(Some(frame)),
        }
    }

    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn name(&self) -> &'static str {
        "videotoolbox"
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        unsafe {
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session);
        }
    }
}

/// Hardware VideoToolbox H.264 session for `w`×`h` (even) frames; fails where there is none.
pub fn new(w: usize, h: usize, fps: u32, quality: u8) -> Result<VtEncoder> {
    VtEncoder::new(w, h, fps, quality)
}
