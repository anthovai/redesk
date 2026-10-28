//! Hardware H.264 and AV1 encoding with a Media Foundation transform (Windows: Intel Quick
//! Sync, NVIDIA NVENC, AMD AMF expose their encoders as hardware MFTs; AV1 needs NVIDIA RTX 40,
//! Intel Arc / Core Ultra or AMD RDNA 3 and newer).
//!
//! UNTESTED on real Windows hardware: this compiles for x86_64-pc-windows-gnu but has only been
//! exercised on macOS through the shared fallback logic. Any failure here (no hardware MFT,
//! a type the MFT rejects, a timeout) makes the caller fall back to OpenH264 (AV1: to H.264).
//!
//! Configured as H.264 Baseline profile (MF_MT_MPEG2_PROFILE = 66) or AV1 Main 4:2:0 8-bit,
//! low-latency mode, no B-frames, BT.601 limited range NV12 input. Hardware MFTs are
//! asynchronous: they ask for input and announce output through events, which this pumps
//! synchronously per frame.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VARIANT;
use windows_core::Interface;

use super::{Codec, VideoEncoder, av1_is_keyframe, av1_sequence_header, bitrate};

const TIMEOUT: Duration = Duration::from_millis(500);

pub struct MfEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    provides_samples: bool,
    out_size: u32,
    need_input: u32,
    w: usize,
    h: usize,
    nv12: Vec<u8>,
    frame: i64,
    fps: u32,
    codec: Codec,
    /// Last AV1 sequence header seen, for keyframes that come without one.
    seq_header: Option<Vec<u8>>,
}

pub fn new(w: usize, h: usize, fps: u32, quality: u8) -> Result<MfEncoder> {
    unsafe { MfEncoder::new(Codec::H264, w, h, fps, quality) }
}

pub fn new_av1(w: usize, h: usize, fps: u32, quality: u8) -> Result<MfEncoder> {
    unsafe { MfEncoder::new(Codec::Av1, w, h, fps, quality) }
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl MfEncoder {
    unsafe fn new(codec: Codec, w: usize, h: usize, fps: u32, quality: u8) -> Result<Self> {
        unsafe {
            // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is usable on this thread either way.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if !*STARTED.get_or_init(|| MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok()) {
                bail!("Media Foundation unavailable");
            }

            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let (subtype, label) = match codec {
                Codec::Av1 => (MFVideoFormat_AV1, "AV1"),
                _ => (MFVideoFormat_H264, "H.264"),
            };
            let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut list,
                &mut count,
            )
            .context("MFTEnumEx")?;
            if list.is_null() || count == 0 {
                bail!("no hardware {label} encoder (Media Foundation)");
            }
            let activates: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| (*list.add(i)).take()).collect();
            windows::Win32::System::Com::CoTaskMemFree(Some(list as *const _));

            let mut last = anyhow!("no usable hardware {label} encoder");
            for a in activates.into_iter().flatten() {
                match Self::open(&a, codec, w, h, fps, quality) {
                    Ok(e) => return Ok(e),
                    Err(e) => {
                        let _ = a.ShutdownObject();
                        last = e;
                    }
                }
            }
            Err(last)
        }
    }

    unsafe fn open(a: &IMFActivate, codec: Codec, w: usize, h: usize, fps: u32, quality: u8) -> Result<Self> {
        unsafe {
            let transform: IMFTransform = a.ActivateObject().context("activate MFT")?;
            let attrs = transform.GetAttributes().context("MFT attributes")?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).context("unlock async MFT")?;
            }
            let events: IMFMediaEventGenerator = transform.cast().context("MFT has no event generator (not async)")?;
            let fps = fps.clamp(1, 60);

            // Output first (encoders need it before the input type).
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            let (subtype, profile) = match codec {
                Codec::Av1 => (MFVideoFormat_AV1, eAVEncAV1VProfile_Main_420_8.0 as u32),
                _ => (MFVideoFormat_H264, eAVEncH264VProfile_Base.0 as u32),
            };
            out.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate(w, h, fps, quality))?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
            transform.SetOutputType(0, &out, 0).with_context(|| format!("{} output type", codec.name()))?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            inp.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32)?;
            inp.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            transform.SetInputType(0, &inp, 0).context("NV12 input type")?;

            let codec_api: Option<ICodecAPI> = transform.cast().ok();
            if let Some(api) = &codec_api {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate(w, h, fps, quality)));
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps * 10));
            }

            let info = transform.GetOutputStreamInfo(0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let (w2, h2) = (w, h);
            Ok(Self {
                transform,
                events,
                codec_api,
                provides_samples: info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0,
                out_size: info.cbSize.max((w * h) as u32),
                need_input: 0,
                w: w2,
                h: h2,
                nv12: vec![0; w * h * 3 / 2],
                frame: 0,
                fps,
                codec,
                seq_header: None,
            })
        }
    }

    /// Handles queued events until `done` says stop or the deadline passes.
    unsafe fn pump(&mut self, deadline: Instant, out: &mut Vec<(Vec<u8>, bool)>, done: impl Fn(&Self, usize) -> bool) -> Result<()> {
        unsafe {
            while !done(self, out.len()) {
                match self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                    Ok(ev) => {
                        let t = ev.GetType()?;
                        if t == METransformNeedInput.0 as u32 {
                            self.need_input += 1;
                        } else if t == METransformHaveOutput.0 as u32 {
                            if let Some(o) = self.output()? {
                                out.push(o);
                            }
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                        if Instant::now() >= deadline {
                            bail!("Media Foundation encoder timed out");
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => return Err(e).context("MFT event"),
                }
            }
            Ok(())
        }
    }

    unsafe fn output(&mut self) -> Result<Option<(Vec<u8>, bool)>> {
        unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(sample), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let res = self.transform.ProcessOutput(0, &mut buf, &mut status);
            let sample = std::mem::ManuallyDrop::take(&mut buf[0].pSample);
            drop(std::mem::ManuallyDrop::take(&mut buf[0].pEvents));
            match res {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => bail!("encoder changed its output type"),
                Err(e) => return Err(e).context("ProcessOutput"),
            }
            let Some(sample) = sample else { return Ok(None) };
            let mb = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            mb.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            mb.Unlock()?;
            // Not every MFT sets CleanPoint; an IDR slice (NAL type 5) or an AV1 key frame
            // header is the ground truth.
            let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0
                || match self.codec {
                    Codec::Av1 => av1_is_keyframe(&data),
                    _ => data.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5),
                };
            Ok(Some((data, key)))
        }
    }

    /// RGB -> NV12 (BT.601 limited range, like OpenH264's own conversion).
    fn to_nv12(&mut self, rgb: &[u8]) {
        let (w, h) = (self.w, self.h);
        let (y_plane, uv) = self.nv12.split_at_mut(w * h);
        for y in 0..h {
            for x in 0..w {
                let p = &rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
                let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
                y_plane[y * w + x] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
            }
        }
        for y in (0..h).step_by(2) {
            for x in (0..w).step_by(2) {
                let (mut r, mut g, mut b) = (0, 0, 0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = &rgb[((y + dy) * w + x + dx) * 3..];
                    r += p[0] as i32;
                    g += p[1] as i32;
                    b += p[2] as i32;
                }
                let (r, g, b) = (r / 4, g / 4, b / 4);
                let o = (y / 2) * w + x;
                uv[o] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                uv[o + 1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, rgb: &[u8], keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        self.to_nv12(rgb);
        unsafe {
            let mut out = Vec::new();
            let deadline = Instant::now() + TIMEOUT;
            self.pump(deadline, &mut out, |s, _| s.need_input > 0)?;
            if keyframe {
                if let Some(api) = &self.codec_api {
                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
                }
            }
            let mb = MFCreateMemoryBuffer(self.nv12.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            mb.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), ptr, self.nv12.len());
            mb.Unlock()?;
            mb.SetCurrentLength(self.nv12.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&mb)?;
            let dur = 10_000_000 / self.fps as i64;
            sample.SetSampleTime(self.frame * dur)?;
            sample.SetSampleDuration(dur)?;
            self.frame += 1;
            self.transform.ProcessInput(0, &sample, 0).context("ProcessInput")?;
            self.need_input -= 1;
            // Low-latency mode returns this frame's output before asking for the next input.
            let before = out.len();
            self.pump(deadline, &mut out, |_, n| n > before)?;
            let key = out.iter().any(|(_, k)| *k);
            let mut data: Vec<u8> = out.into_iter().flat_map(|(d, _)| d).collect();
            if self.codec == Codec::Av1 && !data.is_empty() {
                // Viewers configure their decoder from the keyframe, so every keyframe needs the
                // sequence header in band.
                match av1_sequence_header(&data) {
                    Some(sh) => self.seq_header = Some(sh.to_vec()),
                    None if key => {
                        let sh = self.seq_header.as_deref().context("AV1 keyframe without a sequence header")?;
                        data.splice(0..0, sh.iter().copied());
                    }
                    None => {}
                }
            }
            Ok((!data.is_empty()).then_some((data, key)))
        }
    }

    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn name(&self) -> &'static str {
        "mediafoundation"
    }

    fn codec(&self) -> Codec {
        self.codec
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}
