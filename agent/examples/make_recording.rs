//! Writes a synthetic session recording (.aerec, docs/PROTOCOL.md §7) for end-to-end tests of
//! the console's player, and optionally uploads it the way an agent does. A Mac has no AV1
//! encoder for live sessions, so this is how AV1 recordings are produced for testing (rav1e,
//! a dev-dependency).
//!
//!   cargo run --release --example make_recording -- --out rec.aerec --sid <sid> --device-id <id> \
//!       [--codec av1|h264|switch] [--seconds 20] [--upload <server> --config <agent.toml> --token <t>]
//!
//! Content, so a test can check what plays when (web/scripts/e2e-media.ts):
//! - video: 320×240 at 10 fps; frame k (at k × 100 ms) shows k in base 4 as the gray levels of
//!   its four quadrants (top-left = lowest digit). A keyframe every 2 s. `switch` is AV1 for the
//!   first half and H.264 after, like a session whose AV1 encoder failed.
//! - audio: Opus packets every 20 ms; a 440 Hz beep during the first half of every second of
//!   recording time and silence in the second half. Record times jitter by up to 15 ms (as sent
//!   over a network), and the audio stops from 8.0 to 9.0 s without a sequence gap, as when the
//!   viewer mutes (the agent only counts frames it sends).
//! - a pointer position every 500 ms and the end marker.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use aegiron_agent::audio::{CHANNELS, FRAME, OpusEncoder, SAMPLE_RATE};
use aegiron_agent::codec::{self, Codec, H264Encoder};
use aegiron_agent::{config::Config, identity, proto, recording};

const W: usize = 320;
const H: usize = 240;
const FPS: usize = 10;
const LEVELS: [u8; 4] = [20, 90, 160, 230];

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// Frame `k` as packed RGB: its index in base 4, one digit per quadrant.
fn frame(k: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let q = (y >= H / 2) as usize * 2 + (x >= W / 2) as usize;
            let v = LEVELS[(k >> (2 * q)) & 3];
            rgb[(y * W + x) * 3..(y * W + x) * 3 + 3].fill(v);
        }
    }
    rgb
}

/// AV1 temporal units for all frames, in order (rav1e, low latency: no reordering).
fn av1_frames(frames: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    use rav1e::prelude::*;
    let mut cfg = EncoderConfig::with_speed_preset(10);
    (cfg.width, cfg.height, cfg.low_latency) = (W, H, true);
    cfg.min_key_frame_interval = 20;
    cfg.max_key_frame_interval = 20;
    let mut ctx: Context<u8> = Config::new().with_encoder_config(cfg).with_threads(4).new_context()?;
    for rgb in frames {
        let mut f = ctx.new_frame();
        for (plane, data) in f.planes.iter_mut().zip(codec::rgb_to_i420(rgb, W, H)) {
            let stride = plane.cfg.width;
            plane.copy_from_raw_u8(&data, stride, 1);
        }
        ctx.send_frame(f)?;
    }
    ctx.flush();
    let mut out = Vec::new();
    loop {
        match ctx.receive_packet() {
            Ok(p) => out.push(p.data),
            Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => bail!("rav1e: {e:?}"),
        }
    }
    if out.len() != frames.len() {
        bail!("rav1e returned {} packets for {} frames", out.len(), frames.len());
    }
    Ok(out)
}

/// Small deterministic jitter (0..=15 ms), like network send times.
fn jitter(i: usize) -> u32 {
    ((i * 2_654_435_761) >> 7) as u32 % 16
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(arg(&args, "--out").context("--out")?);
    let sid = arg(&args, "--sid").context("--sid")?;
    let device_id = arg(&args, "--device-id").context("--device-id")?;
    let codec = arg(&args, "--codec").unwrap_or_else(|| "av1".into());
    let seconds: usize = arg(&args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(20);
    if seconds * FPS > 256 {
        bail!("at most 25 seconds (frame numbers are 4 base-4 digits)");
    }

    let n = seconds * FPS;
    let frames: Vec<Vec<u8>> = (0..n).map(frame).collect();
    let split = match codec.as_str() {
        "av1" => n,
        "h264" => 0,
        "switch" => n / 2,
        c => bail!("unknown codec {c}"),
    };
    let mut video: Vec<(u32, Vec<u8>)> = Vec::new();
    if split > 0 {
        for (k, data) in av1_frames(&frames[..split])?.into_iter().enumerate() {
            let key = codec::av1_is_keyframe(&data);
            video.push(((k * 100) as u32 + jitter(k), proto::video(Codec::Av1.id(), key, W as u16, H as u16, &data)));
        }
    }
    if split < n {
        let mut enc = H264Encoder::new(W, H, FPS as u32, 70)?;
        for k in split..n {
            let Some((data, key)) = enc.encode(&frames[k], k == split || k % 20 == 0)? else { bail!("openh264 dropped frame {k}") };
            video.push(((k * 100) as u32 + jitter(k), proto::video(Codec::H264.id(), key, W as u16, H as u16, &data)));
        }
    }

    let mut opus = OpusEncoder::new()?;
    let mut audio: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut seq = 0u32;
    let mut pcm = vec![0f32; FRAME * CHANNELS];
    for i in 0..seconds * 50 {
        let t = i * 20;
        if (8000..9000).contains(&t) {
            continue; // muted: nothing captured, sequence numbers continue afterwards
        }
        for (j, s) in pcm.chunks_exact_mut(CHANNELS).enumerate() {
            let pos = t * (SAMPLE_RATE as usize / 1000) + j;
            let on = (pos / (SAMPLE_RATE as usize / 2)) % 2 == 0;
            s.fill(if on { (pos as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.3 } else { 0.0 });
        }
        seq += 1;
        audio.push((t as u32 + jitter(i + 7), proto::audio(proto::AUDIO_OPUS, seq, &opus.encode(&pcm)?)));
    }

    let mut records: Vec<(u32, Vec<u8>)> = video;
    records.extend(audio);
    for i in 0..seconds * 2 {
        let p = i as f64 / (seconds * 2) as f64;
        records.push(((i * 500) as u32, proto::json(&serde_json::json!({"t": "cursor", "x": p, "y": 1.0 - p, "visible": true}))));
    }
    // Stable sort: records with the same time keep their order (video before audio before cursor).
    records.sort_by_key(|r| r.0);
    records.push(((seconds * 1000) as u32, proto::json(&serde_json::json!({"t": "end"}))));

    let header = serde_json::json!({
        "v": 1, "sid": sid, "device_id": device_id, "hostname": "e2e-synthetic", "os": "macos",
        "started_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64,
    })
    .to_string();
    let mut f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    f.write_all(recording::MAGIC)?;
    f.write_all(&(header.len() as u32).to_be_bytes())?;
    f.write_all(header.as_bytes())?;
    for (t, data) in &records {
        f.write_all(&t.to_be_bytes())?;
        f.write_all(&(data.len() as u32).to_be_bytes())?;
        f.write_all(data)?;
    }
    f.flush()?;
    drop(f);
    println!("wrote {} ({} records, {} s, {codec})", out.display(), records.len(), seconds);

    if let Some(server) = arg(&args, "--upload") {
        let config = Config::load(&PathBuf::from(arg(&args, "--config").context("--config")?))?;
        if config.id != device_id {
            bail!("--device-id {device_id} is not the device in --config ({})", config.id);
        }
        let token = arg(&args, "--token").context("--token")?;
        recording::upload(&server, &config.id, &identity(&config)?, &sid, &token, &out)?;
        println!("uploaded {sid}");
    }
    Ok(())
}
