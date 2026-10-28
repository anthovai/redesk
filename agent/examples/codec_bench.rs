//! Encoder latency on synthetic screen content: the H.264 encoders sessions use, against
//! rav1e (software AV1) set up for the lowest latency it offers. This is the measurement behind
//! keeping rav1e out of live sessions (docs/PROTOCOL.md, Video codecs).
//!
//!   cargo run --release --example codec_bench -- [width height] [frames] [rav1e speed]
//!
//! Two scenes: "typing" (a line of text grows each frame) and "scrolling" (the whole page moves
//! 6 px per frame). Sessions only send a frame when the screen changed, so what counts is the
//! time from handing a frame to the encoder until its own packet comes out. rav1e holds frames
//! back for lookahead even in low-latency mode, so the frame is re-sent (unchanged) until its
//! packet appears, which is what a live session would have to do. Each packet is also decoded
//! (OpenH264 / rav1d) to time the viewer side.

use std::time::Instant;

use aegiron_agent::codec::{self, Av1Decoder, H264Decoder, H264Encoder, VideoEncoder};
use rav1e::prelude::*;

/// A light page with lines of "text"; `scroll` moves it up by that many pixels and `typed`
/// characters are drawn on one line.
fn screen(w: usize, h: usize, scroll: usize, typed: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        let page_y = y + scroll;
        let (line, row) = (page_y / 18, page_y % 18);
        for x in 0..w {
            let ink = if x < 40 || x > w - 280 || y < 40 || row >= 12 {
                false
            } else {
                let (ch, seed) = (x / 9, (line * 131 + (x / 9) * 17) % 23);
                let on_line = if line % 7 == 6 { ch < 20 } else { seed > 4 };
                on_line && (x * 7 + row * 13 + seed) % 5 < 2
            };
            let typing = (500..512).contains(&y) && x >= 60 && x < 60 + typed * 9 && (x + y) % 3 == 0;
            let c = if ink || typing { [32, 34, 40] } else if x > w - 260 { [236, 239, 244] } else { [250, 250, 250] };
            rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
        }
    }
    rgb
}

struct Stats {
    name: String,
    enc_ms: Vec<f64>,
    dec_ms: Vec<f64>,
    bytes: usize,
    held: usize,
}

impl Stats {
    fn new(name: &str) -> Self {
        Self { name: name.into(), enc_ms: Vec::new(), dec_ms: Vec::new(), bytes: 0, held: 0 }
    }

    fn print(mut self, frames: usize) {
        let pct = |v: &mut Vec<f64>, p: usize| {
            v.sort_by(|a, b| a.total_cmp(b));
            v.get((v.len().saturating_sub(1)) * p / 100).copied().unwrap_or(0.0)
        };
        let key = self.enc_ms.first().copied().unwrap_or(0.0);
        let mut deltas = self.enc_ms.split_off(1.min(self.enc_ms.len()));
        let (m, p95) = (pct(&mut deltas, 50), pct(&mut deltas, 95));
        let dec = pct(&mut self.dec_ms, 50);
        println!(
            "  {:<28} key {:>6.1} ms | change median {:>6.1} ms, p95 {:>6.1} ms | decode median {:>5.1} ms/packet | {:>6.0} kbit/change{}",
            self.name,
            key,
            m,
            p95,
            dec,
            self.bytes as f64 * 8.0 / frames as f64 / 1000.0,
            if self.held > 0 { format!(" | {} extra encodes to flush", self.held) } else { String::new() },
        );
    }
}

fn bench_h264(name: &str, enc: &mut dyn VideoEncoder, frames: &[Vec<u8>]) -> Stats {
    let mut s = Stats::new(name);
    let mut dec = H264Decoder::new().unwrap();
    for (i, f) in frames.iter().enumerate() {
        let t = Instant::now();
        let out = enc.encode(f, i == 0).unwrap();
        s.enc_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        if let Some((data, _)) = out {
            s.bytes += data.len();
            let t = Instant::now();
            dec.decode(&data).unwrap();
            s.dec_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    s
}

fn bench_rav1e(w: usize, h: usize, fps: u32, speed: u8, frames: &[Vec<u8>]) -> Stats {
    let mut s = Stats::new(&format!("rav1e speed {speed} (AV1, sw)"));
    let mut cfg = EncoderConfig::with_speed_preset(speed);
    (cfg.width, cfg.height, cfg.low_latency) = (w, h, true);
    cfg.time_base = Rational::new(1, fps as u64);
    cfg.bitrate = codec::bitrate(w, h, fps, 70) as i32;
    (cfg.min_key_frame_interval, cfg.max_key_frame_interval) = (0, fps as u64 * 10);
    cfg.speed_settings.rdo_lookahead_frames = 1;
    // Tiles are what lets rav1e use several cores on one frame.
    cfg.tiles = 8;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut ctx: Context<u8> = Config::new().with_encoder_config(cfg).with_threads(threads).new_context().unwrap();
    let mut dec = Av1Decoder::new().unwrap();
    let mut next_input = 0u64;
    for (i, f) in frames.iter().enumerate() {
        let planes = codec::rgb_to_i420(f, w, h);
        let mut frame = ctx.new_frame();
        for (plane, data) in frame.planes.iter_mut().zip(&planes) {
            let stride = plane.cfg.width;
            plane.copy_from_raw_u8(data, stride, 1);
        }
        let t = Instant::now();
        let params = FrameParameters { frame_type_override: if i == 0 { FrameTypeOverride::Key } else { FrameTypeOverride::No }, ..Default::default() };
        ctx.send_frame((frame.clone(), params)).unwrap();
        let target = next_input;
        next_input += 1;
        let mut dec_ms = 0.0;
        'out: loop {
            loop {
                match ctx.receive_packet() {
                    Ok(p) => {
                        s.bytes += p.data.len();
                        let td = Instant::now();
                        dec.decode(&p.data).unwrap();
                        let ms = td.elapsed().as_secs_f64() * 1000.0;
                        dec_ms += ms;
                        s.dec_ms.push(ms);
                        if p.input_frameno == target {
                            break 'out;
                        }
                    }
                    Err(EncoderStatus::Encoded) => {}
                    Err(EncoderStatus::NeedMoreData) => break,
                    Err(e) => panic!("rav1e: {e:?}"),
                }
            }
            // Still held back: push the same picture again to move the pipeline along.
            ctx.send_frame(frame.clone()).unwrap();
            next_input += 1;
            s.held += 1;
        }
        s.enc_ms.push(t.elapsed().as_secs_f64() * 1000.0 - dec_ms);
    }
    s
}

fn main() {
    let args: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (w, h) = (args.first().copied().unwrap_or(1920) & !1, args.get(1).copied().unwrap_or(1080) & !1);
    let n = args.get(2).copied().unwrap_or(40).max(2);
    let speed = args.get(3).copied().unwrap_or(10).min(10) as u8;
    let fps = 30;
    println!("{w}x{h}, {n} frames per scene, {} threads", std::thread::available_parallelism().map_or(0, |n| n.get()));
    for (scene, frames) in [
        ("typing", (0..n).map(|i| screen(w, h, 0, i)).collect::<Vec<_>>()),
        ("scrolling", (0..n).map(|i| screen(w, h, i * 6, 0)).collect::<Vec<_>>()),
    ] {
        println!("{scene}:");
        if let Ok(mut hw) = codec::hardware_encoder(w, h, fps, 70) {
            let name = format!("{} (H.264, hw)", hw.name());
            bench_h264(&name, &mut *hw, &frames).print(n);
        }
        bench_h264("openh264 (H.264, sw)", &mut H264Encoder::new(w, h, fps, 70).unwrap(), &frames).print(n);
        bench_rav1e(w, h, fps, speed, &frames).print(n);
    }
}
