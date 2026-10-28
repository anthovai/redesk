//! Video codecs for the screen stream.
//!
//! H.264 (OpenH264, constrained baseline, screen-content tuning) is preferred; JPEG
//! rectangles remain as the fallback for viewers that cannot decode H.264. The viewer lists
//! what it can decode in its `auth` message and the agent picks the first match.

use anyhow::{Result, anyhow};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer, YUVSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Jpeg,
    H264,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Jpeg => "jpeg",
            Codec::H264 => "h264",
        }
    }

    /// Wire id in the MSG_VIDEO header.
    pub fn id(self) -> u8 {
        match self {
            Codec::Jpeg => 0,
            Codec::H264 => 1,
        }
    }

    /// Best codec both sides support; viewers that send nothing get JPEG.
    pub fn negotiate(viewer: &[String]) -> Codec {
        if viewer.iter().any(|c| c == "h264") { Codec::H264 } else { Codec::Jpeg }
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

pub struct H264Encoder {
    enc: Encoder,
    pub w: usize,
    pub h: usize,
    yuv: YUVBuffer,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_prefers_h264() {
        assert_eq!(Codec::negotiate(&["jpeg".into(), "h264".into()]), Codec::H264);
        assert_eq!(Codec::negotiate(&["jpeg".into()]), Codec::Jpeg);
        assert_eq!(Codec::negotiate(&[]), Codec::Jpeg);
    }

    #[test]
    fn h264_round_trip() {
        let (w, h) = (320usize, 240usize);
        // A gradient frame, then the same frame with a block changed.
        let mut rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, 128]).collect();
        let mut enc = H264Encoder::new(w, h, 15, 70).unwrap();
        let mut dec = H264Decoder::new().unwrap();

        let (key, is_key) = enc.encode(&rgb, true).unwrap().expect("first frame is encoded");
        assert!(is_key, "first frame must be a keyframe");
        assert_eq!(&key[..4], &[0, 0, 0, 1], "Annex-B start code for WebCodecs");
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
        assert!(delta.len() < key.len(), "P-frame ({}) should be smaller than the keyframe ({})", delta.len(), key.len());
        let (_, _, px) = dec.decode(&delta).unwrap().expect("delta decodes");
        let p = &px[(70 * w + 70) * 4..(70 * w + 70) * 4 + 3];
        assert!(p[0] > 200 && p[1] < 60, "changed block should be red, got {p:?}");
    }
}
