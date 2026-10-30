//! Remote audio: the agent captures what the machine is playing (macOS ScreenCaptureKit,
//! Windows WASAPI loopback, Linux PulseAudio/PipeWire monitor source), encodes it as Opus (48 kHz stereo, 20 ms frames) and sends it as
//! `0x05` packets; viewers decode and play it.
//!
//! Opus comes from `unsafe-libopus` (libopus transpiled to Rust), so no C toolchain or cmake
//! is needed for the universal macOS build or the Windows cross build.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::sync::mpsc;

use crate::proto;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 20 ms frame.
pub const FRAME: usize = 960;
const BITRATE: i32 = 96_000;

// --- Opus ---------------------------------------------------------------------------------

pub struct OpusEncoder(*mut unsafe_libopus::OpusEncoder);
// The state is only touched through &mut self.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new() -> Result<Self> {
        let mut err = 0;
        let st = unsafe { unsafe_libopus::opus_encoder_create(SAMPLE_RATE as i32, CHANNELS as i32, unsafe_libopus::OPUS_APPLICATION_AUDIO, &mut err) };
        if st.is_null() || err != unsafe_libopus::OPUS_OK {
            bail!("opus encoder: error {err}");
        }
        unsafe {
            unsafe_libopus::opus_encoder_ctl_impl(st, unsafe_libopus::OPUS_SET_BITRATE_REQUEST, unsafe_libopus::varargs!(BITRATE));
            unsafe_libopus::opus_encoder_ctl_impl(st, unsafe_libopus::OPUS_SET_COMPLEXITY_REQUEST, unsafe_libopus::varargs!(5i32));
        }
        Ok(Self(st))
    }

    /// Encodes one 20 ms interleaved stereo frame (FRAME * CHANNELS samples).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        assert_eq!(pcm.len(), FRAME * CHANNELS);
        let mut out = vec![0u8; 1500];
        let n = unsafe { unsafe_libopus::opus_encode_float(self.0, pcm.as_ptr(), FRAME as i32, out.as_mut_ptr(), out.len() as i32) };
        if n < 0 {
            bail!("opus encode: error {n}");
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        unsafe { unsafe_libopus::opus_encoder_destroy(self.0) }
    }
}

pub struct OpusDecoder {
    st: *mut unsafe_libopus::OpusDecoder,
    pcm: Vec<f32>,
}
unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    pub fn new() -> Result<Self> {
        let mut err = 0;
        let st = unsafe { unsafe_libopus::opus_decoder_create(SAMPLE_RATE as i32, CHANNELS as i32, &mut err) };
        if st.is_null() || err != unsafe_libopus::OPUS_OK {
            bail!("opus decoder: error {err}");
        }
        // Room for the longest Opus packet (120 ms).
        Ok(Self { st, pcm: vec![0.0; 5760 * CHANNELS] })
    }

    /// Decodes one packet into interleaved stereo samples at 48 kHz.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32]> {
        let n = unsafe {
            unsafe_libopus::opus_decode_float(self.st, packet.as_ptr(), packet.len() as i32, self.pcm.as_mut_ptr(), (self.pcm.len() / CHANNELS) as i32, 0)
        };
        if n < 0 {
            bail!("opus decode: error {n}");
        }
        Ok(&self.pcm[..n as usize * CHANNELS])
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { unsafe_libopus::opus_decoder_destroy(self.st) }
    }
}

/// Root-mean-square level of a block of samples (0 = silence, ~0.7 = full-scale sine).
pub fn rms(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt()
}

// --- Format conversion ----------------------------------------------------------------------

/// Turns captured audio of any rate and channel count into 20 ms frames of 48 kHz stereo.
/// Resampling is linear: plenty for remote-desktop audio and free of extra dependencies.
pub struct Framer {
    buf: Vec<f32>,
    /// Fractional read position into the next input block (in input frames), carried over.
    pos: f64,
    last: [f32; 2],
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(FRAME * CHANNELS * 4), pos: 0.0, last: [0.0; 2] }
    }

    /// `data` is interleaved with `channels` channels at `rate` Hz. Calls `frame` with each
    /// complete 20 ms stereo frame.
    pub fn push(&mut self, data: &[f32], channels: usize, rate: u32, mut frame: impl FnMut(&[f32])) {
        if channels == 0 || rate == 0 {
            return;
        }
        let n = data.len() / channels;
        let stereo = |i: usize| -> [f32; 2] {
            let f = &data[i * channels..i * channels + channels];
            if channels == 1 { [f[0], f[0]] } else { [f[0], f[1]] }
        };
        if rate == SAMPLE_RATE {
            for i in 0..n {
                self.buf.extend_from_slice(&stereo(i));
            }
            if n > 0 {
                self.last = stereo(n - 1);
            }
        } else {
            // Output sample k sits at input position pos + k * step; position -1 is `last`.
            let step = rate as f64 / SAMPLE_RATE as f64;
            let mut p = self.pos;
            while p < n as f64 - 1.0 + 1e-9 {
                let i = p.floor() as isize;
                let t = (p - i as f64) as f32;
                let a = if i < 0 { self.last } else { stereo(i as usize) };
                let b = stereo((i + 1).max(0) as usize);
                self.buf.extend_from_slice(&[a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                p += step;
            }
            self.pos = p - n as f64;
            if n > 0 {
                self.last = stereo(n - 1);
            }
        }
        let whole = self.buf.len() / (FRAME * CHANNELS) * FRAME * CHANNELS;
        for f in self.buf[..whole].chunks_exact(FRAME * CHANNELS) {
            frame(f);
        }
        self.buf.drain(..whole);
    }
}

// --- Agent side: capture + encode -----------------------------------------------------------

/// PCM callback from a capture backend: interleaved f32, channel count, sample rate.
pub type PcmSink = Box<dyn FnMut(&[f32], usize, u32) + Send>;

/// Whether this machine can capture its output audio (checked once).
pub fn capture_supported() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| test_tone() || imp::capture_supported())
}

/// `AEGIRON_TEST_AUDIO=tone` replaces system-audio capture with a generated beep (440 Hz,
/// 0.5 s on / 0.5 s off) so end-to-end tests exercise encoding, streaming, recording and
/// playback on machines where capture is unavailable (e.g. no Screen Recording permission).
/// Tests only: never set by the app or the installers.
fn test_tone() -> bool {
    std::env::var("AEGIRON_TEST_AUDIO").is_ok_and(|v| v == "tone")
}

/// A running capture: the platform backend or the test tone. Dropping it stops capture.
enum Source {
    #[allow(dead_code)]
    System(imp::Capture),
    Tone(Arc<AtomicBool>),
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Source::Tone(stop) = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Feeds the test tone to `sink` in real time, 10 ms per block (48 kHz stereo).
fn start_tone(mut sink: PcmSink) -> Result<Source> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("aegiron-test-tone".into())
        .spawn(move || {
            let block = SAMPLE_RATE as usize / 100;
            let start = std::time::Instant::now();
            let mut n = 0u64;
            let mut buf = vec![0f32; block * CHANNELS];
            while !stop2.load(Ordering::Relaxed) {
                for (i, f) in buf.chunks_exact_mut(CHANNELS).enumerate() {
                    let k = n + i as u64;
                    let on = (k / (SAMPLE_RATE as u64 / 2)) % 2 == 0;
                    let s = if on { (k as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.3 } else { 0.0 };
                    f.fill(s);
                }
                sink(&buf, CHANNELS, SAMPLE_RATE);
                n += block as u64;
                let due = start + Duration::from_micros(n * 1_000_000 / SAMPLE_RATE as u64);
                std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
            }
        })
        .map_err(|e| anyhow!("test tone thread: {e}"))?;
    Ok(Source::Tone(stop))
}

/// Captures system audio and sends Opus `0x05` packets to `out` while `on` is set, until
/// `stop` is set or the receiver goes away. Packets are dropped (not queued) when the
/// network falls behind, so audio never builds up latency.
pub fn stream(out: mpsc::Sender<Vec<u8>>, on: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<(Vec<f32>, usize, u32)>(64);
    let sink: PcmSink = Box::new(move |data, ch, rate| {
        let _ = pcm_tx.try_send((data.to_vec(), ch, rate));
    });
    let started = match test_tone() {
        true => {
            tracing::warn!("audio: AEGIRON_TEST_AUDIO=tone, sending a generated test tone instead of system audio");
            start_tone(sink)
        }
        false => imp::start(sink).map(Source::System),
    };
    let capture = match started {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("audio capture unavailable: {e:#}");
            return;
        }
    };
    let mut enc = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("{e:#}");
            return;
        }
    };
    tracing::info!("audio: streaming system audio (Opus {} kbps)", BITRATE / 1000);
    let mut framer = Framer::new();
    let mut seq = 0u32;
    let mut sent_level = (0usize, 0f32, std::time::Instant::now());
    while !stop.load(Ordering::Relaxed) && !out.is_closed() {
        let Ok((data, ch, rate)) = pcm_rx.recv_timeout(Duration::from_millis(200)) else { continue };
        if !on.load(Ordering::Relaxed) {
            continue;
        }
        let mut gone = false;
        framer.push(&data, ch, rate, |frame| {
            let level = rms(frame);
            match enc.encode(frame) {
                Ok(pkt) => {
                    seq = seq.wrapping_add(1);
                    match out.try_send(proto::audio(proto::AUDIO_OPUS, seq, &pkt)) {
                        Err(mpsc::error::TrySendError::Closed(_)) => gone = true,
                        Err(mpsc::error::TrySendError::Full(_)) => tracing::debug!("audio: dropped a frame (network behind)"),
                        Ok(()) => {
                            sent_level.0 += 1;
                            sent_level.1 = sent_level.1.max(level);
                        }
                    }
                }
                Err(e) => tracing::debug!("{e:#}"),
            }
        });
        if gone {
            break;
        }
        if sent_level.2.elapsed() >= Duration::from_secs(5) {
            tracing::debug!("audio: sent {} frames in 5 s, peak rms {:.3}", sent_level.0, sent_level.1);
            sent_level = (0, 0.0, std::time::Instant::now());
        }
    }
    drop(capture);
    tracing::info!("audio: capture stopped");
}

// --- Viewer side: playback ------------------------------------------------------------------

/// Whether this build can play audio (the viewer only asks for audio if so).
pub fn playback_supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

/// Jitter buffer shared between the network task and the audio device callback.
#[derive(Default)]
struct Ring {
    q: VecDeque<f32>,
    /// Waiting to fill up before (re)starting, to ride out network jitter.
    priming: bool,
}

/// Start playing once this much is buffered; drop the backlog beyond the maximum.
const PRIME: usize = (SAMPLE_RATE as usize / 1000) * 80 * CHANNELS;
const MAX_BUFFERED: usize = (SAMPLE_RATE as usize / 1000) * 300 * CHANNELS;

/// Plays 48 kHz stereo PCM on the default output device. The device stream lives on its own
/// thread (cpal streams are not Send everywhere); this handle is.
pub struct Player {
    ring: Arc<Mutex<Ring>>,
    stop: Arc<AtomicBool>,
}

impl Player {
    pub fn new() -> Result<Self> {
        let ring = Arc::new(Mutex::new(Ring { q: VecDeque::new(), priming: true }));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        {
            let (ring, stop) = (ring.clone(), stop.clone());
            std::thread::Builder::new()
                .name("aegiron-audio-out".into())
                .spawn(move || play_thread(ring, stop, ready_tx))
                .map_err(|e| anyhow!("audio thread: {e}"))?;
        }
        ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("audio output did not start"))??;
        Ok(Self { ring, stop })
    }

    pub fn push(&self, pcm: &[f32]) {
        let mut r = self.ring.lock().unwrap();
        r.q.extend(pcm.iter().copied());
        if r.q.len() > MAX_BUFFERED {
            let extra = r.q.len() - PRIME;
            r.q.drain(..extra - extra % CHANNELS);
        }
    }

    /// Forget buffered audio (on mute) so unmuting does not replay stale sound.
    pub fn clear(&self) {
        let mut r = self.ring.lock().unwrap();
        r.q.clear();
        r.priming = true;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn play_thread(ring: Arc<Mutex<Ring>>, stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<Result<()>>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let res = (|| -> Result<cpal::Stream> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| anyhow!("no audio output device"))?;
        let def = device.default_output_config().map_err(|e| anyhow!("audio output config: {e}"))?;
        let channels = def.channels() as usize;
        let rate = def.sample_rate();
        let config = cpal::StreamConfig { channels: def.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        // The device rarely runs at exactly 48 kHz stereo f32; convert in the callback.
        let step = SAMPLE_RATE as f64 / rate as f64;
        let mut pos = 0f64;
        let mut cur = [0f32; 2];
        let mut next = [0f32; 2];
        let fill = move |out: &mut [f32]| {
            let mut r = ring.lock().unwrap();
            if r.priming && r.q.len() >= PRIME {
                r.priming = false;
            }
            for frame in out.chunks_mut(channels) {
                let s = if r.priming {
                    [0.0, 0.0]
                } else {
                    while pos >= 1.0 {
                        cur = next;
                        if r.q.len() >= CHANNELS {
                            next = [r.q.pop_front().unwrap(), r.q.pop_front().unwrap()];
                        } else {
                            next = [0.0, 0.0];
                            r.priming = true; // underrun: wait for the buffer to refill
                        }
                        pos -= 1.0;
                    }
                    let t = pos as f32;
                    pos += step;
                    [cur[0] + (next[0] - cur[0]) * t, cur[1] + (next[1] - cur[1]) * t]
                };
                for (c, o) in frame.iter_mut().enumerate() {
                    *o = if channels == 1 { (s[0] + s[1]) * 0.5 } else if c < 2 { s[c] } else { 0.0 };
                }
            }
        };
        let err = |e| tracing::warn!("audio output: {e}");
        let stream = match def.sample_format() {
            cpal::SampleFormat::F32 => {
                let mut fill = fill;
                device.build_output_stream(config, move |o: &mut [f32], _| fill(o), err, None)
            }
            cpal::SampleFormat::I16 => {
                let mut tmp = Vec::new();
                let mut fill = fill;
                device.build_output_stream(
                    config,
                    move |o: &mut [i16], _| {
                        tmp.resize(o.len(), 0.0);
                        fill(&mut tmp);
                        for (d, s) in o.iter_mut().zip(&tmp) {
                            *d = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        }
                    },
                    err,
                    None,
                )
            }
            f => bail!("unsupported output sample format {f:?}"),
        }
        .map_err(|e| anyhow!("audio output: {e}"))?;
        stream.play().map_err(|e| anyhow!("audio output: {e}"))?;
        tracing::info!("audio: playing on {} ({rate} Hz, {channels} ch)", device.description().map(|d| d.name().to_string()).unwrap_or_default());
        Ok(stream)
    })();
    match res {
        Ok(stream) => {
            let _ = ready.send(Ok(()));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(stream);
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn play_thread(_ring: Arc<Mutex<Ring>>, _stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<Result<()>>) {
    let _ = ready.send(Err(anyhow!("audio playback is not supported on this platform")));
}

// --- Capture backends -----------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[path = "audio_mac.rs"]
mod imp;

#[cfg(windows)]
mod imp {
    //! WASAPI loopback through cpal: an input stream on the default *output* device records
    //! what the machine plays. CI runs it on Windows against a virtual sound card (Scream).
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    pub fn capture_supported() -> bool {
        cpal::default_host().default_output_device().is_some()
    }

    pub struct Capture {
        stop: Arc<AtomicBool>,
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    pub fn start(sink: PcmSink) -> Result<Capture> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            let res = (|| -> Result<cpal::Stream> {
                let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("no output device"))?;
                let def = device.default_output_config().map_err(|e| anyhow!("{e}"))?;
                let (ch, rate) = (def.channels() as usize, def.sample_rate());
                let config = cpal::StreamConfig { channels: def.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
                let mut sink = sink;
                let err = |e| tracing::warn!("audio loopback: {e}");
                let stream = match def.sample_format() {
                    cpal::SampleFormat::F32 => device.build_input_stream(config.clone(), move |d: &[f32], _| sink(d, ch, rate), err, None),
                    cpal::SampleFormat::I16 => {
                        let mut tmp = Vec::new();
                        device.build_input_stream(
                            config,
                            move |d: &[i16], _| {
                                tmp.clear();
                                tmp.extend(d.iter().map(|s| *s as f32 / 32768.0));
                                sink(&tmp, ch, rate)
                            },
                            err,
                            None,
                        )
                    }
                    f => bail!("unsupported loopback format {f:?}"),
                }
                .map_err(|e| anyhow!("loopback stream: {e}"))?;
                stream.play().map_err(|e| anyhow!("{e}"))?;
                Ok(stream)
            })();
            match res {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    while !stop2.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("loopback did not start"))??;
        Ok(Capture { stop })
    }
}

#[cfg(target_os = "linux")]
#[path = "audio_linux.rs"]
mod imp;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn capture_supported() -> bool {
        false
    }

    pub struct Capture;

    pub fn start(_sink: PcmSink) -> Result<Capture> {
        bail!("system audio capture is not supported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tone_is_realtime_stereo_and_audible() {
        let (tx, rx) = std::sync::mpsc::channel();
        let src = start_tone(Box::new(move |d: &[f32], ch, rate| {
            let _ = tx.send((d.to_vec(), ch, rate));
        }))
        .unwrap();
        let (d, ch, rate) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(src);
        // 10 ms of 48 kHz stereo, starting in the "on" half of the beep.
        assert_eq!((ch, rate, d.len()), (CHANNELS, SAMPLE_RATE, 960));
        assert!(rms(&d) > 0.1);
    }

    fn tone(n: usize, rate: u32, hz: f32) -> Vec<f32> {
        (0..n).flat_map(|i| {
            let s = (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5;
            [s, s]
        })
        .collect()
    }

    #[test]
    fn opus_round_trip_keeps_level() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        let pcm = tone(FRAME * 20, SAMPLE_RATE, 440.0);
        let mut out = Vec::new();
        for f in pcm.chunks_exact(FRAME * CHANNELS) {
            let pkt = enc.encode(f).unwrap();
            assert!(pkt.len() < 600, "20 ms at 96 kbps is ~240 bytes, got {}", pkt.len());
            out.extend_from_slice(dec.decode(&pkt).unwrap());
        }
        assert_eq!(out.len(), pcm.len());
        // Skip the codec's warm-up; a 0.5 sine has rms ~0.354.
        let level = rms(&out[out.len() / 2..]);
        assert!((level - 0.354).abs() < 0.05, "decoded rms {level}");
    }

    #[test]
    fn framer_resamples_and_downmixes() {
        let mut f = Framer::new();
        let mut frames = 0;
        // 1 s of 44.1 kHz mono, pushed in odd-sized blocks.
        let mono: Vec<f32> = (0..44_100).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5).collect();
        let mut levels = Vec::new();
        for block in mono.chunks(441 * 3 + 7) {
            f.push(block, 1, 44_100, |fr| {
                assert_eq!(fr.len(), FRAME * CHANNELS);
                frames += 1;
                levels.push(rms(fr));
            });
        }
        assert!((49..=50).contains(&frames), "1 s should give ~50 frames, got {frames}");
        assert!(levels.iter().all(|l| (l - 0.354).abs() < 0.02), "{levels:?}");
    }

    /// Whether the machine has an audio output to test with. Windows runners (and servers)
    /// usually have none, so the device tests skip there unless AEGIRON_REQUIRE_AUDIO=1 (set
    /// by CI after installing a virtual audio device) makes a missing device a failure.
    /// The returned guard keeps the device tests from running at the same time: each would hear
    /// the other's tone.
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    fn have_output() -> Option<std::sync::MutexGuard<'static, ()>> {
        use cpal::traits::HostTrait;
        static DEVICE: Mutex<()> = Mutex::new(());
        let guard = DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        if cpal::default_host().default_output_device().is_some() {
            return Some(guard);
        }
        assert!(std::env::var("AEGIRON_REQUIRE_AUDIO").as_deref() != Ok("1"), "AEGIRON_REQUIRE_AUDIO=1 but there is no audio output device");
        eprintln!("skipped: no audio output device");
        None
    }

    /// Records the loudest block captured from system audio.
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    fn capture_peak(peak: Arc<Mutex<(f32, usize)>>) -> imp::Capture {
        let sink: PcmSink = Box::new(move |d, _, _| {
            let mut p = peak.lock().unwrap();
            p.0 = p.0.max(rms(d));
            p.1 += d.len();
        });
        // macOS: include this process's own audio, which the agent normally leaves out.
        #[cfg(target_os = "macos")]
        return imp::start_with(sink, false).unwrap();
        #[cfg(not(target_os = "macos"))]
        return imp::start(sink).unwrap();
    }

    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    fn play_tone(secs: usize) {
        let player = Player::new().unwrap();
        for chunk in tone(SAMPLE_RATE as usize * secs, SAMPLE_RATE, 660.0).chunks(FRAME * CHANNELS) {
            player.push(chunk);
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Manual check on a machine with Screen Recording permission (macOS) or an audio output
    /// (Windows and Linux; CI installs a virtual one or runs PulseAudio with a null sink): `cargo test --lib audio -- --ignored`. Plays a
    /// sound and expects the capture to hear it.
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    #[test]
    #[ignore]
    fn capture_hears_system_audio() {
        let Some(_device) = have_output() else { return };
        assert!(capture_supported(), "no capture backend");
        let peak = Arc::new(Mutex::new((0f32, 0usize)));
        let cap = capture_peak(peak.clone());
        if cfg!(target_os = "macos") {
            for _ in 0..3 {
                std::process::Command::new("afplay").arg("/System/Library/Sounds/Ping.aiff").status().unwrap();
            }
        } else {
            // WASAPI loopback delivers nothing while nothing plays, and Windows has no
            // command-line player: play a tone through cpal's output path instead (on Linux
            // through ALSA, which the sound server's ALSA plugin routes to the default sink).
            play_tone(2);
        }
        drop(cap);
        let (level, samples) = *peak.lock().unwrap();
        eprintln!("captured {samples} samples, peak rms {level:.4}");
        assert!(level > 0.01, "captured audio is silent");
    }

    /// Manual check: what the viewer's Player plays is heard by the system-audio capture,
    /// i.e. playback through cpal really reaches the output device.
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    #[test]
    #[ignore]
    fn player_output_is_heard() {
        let Some(_device) = have_output() else { return };
        let peak = Arc::new(Mutex::new((0f32, 0usize)));
        let cap = capture_peak(peak.clone());
        std::thread::sleep(Duration::from_millis(300));
        let quiet = peak.lock().unwrap().0;
        play_tone(2);
        drop(cap);
        let level = peak.lock().unwrap().0;
        eprintln!("before playback rms {quiet:.4}, during playback {level:.4}");
        assert!(level > 0.05, "player output not heard");
    }

    #[test]
    fn audio_packet_layout() {
        let m = proto::audio(proto::AUDIO_OPUS, 0x01020304, &[9, 8]);
        assert_eq!(m, vec![0x05, 1, 1, 2, 3, 4, 9, 8]);
        assert_eq!(proto::parse_audio(&m), Some((1, 0x01020304, &[9u8, 8][..])));
    }
}
