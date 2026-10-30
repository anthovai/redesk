//! Two-way voice calls inside a session: each side sends its microphone as Opus `0x07` packets
//! (48 kHz mono, 20 ms) and plays the other side's. Used by the agent (host) and the desktop
//! viewer alike; the browser viewer has its own implementation in web/src/lib/audio.ts.
//!
//! There is no echo cancellation here: the browser's microphone has it, the desktop app's does
//! not, so a call between two desktop apps on loudspeakers can echo (headsets avoid it).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio::{self, Framer, OpusDecoder, OpusEncoder, PcmSink, Player};
use crate::proto;

/// One running call: the microphone streaming out and the other side's voice playing.
/// Dropping it hangs up locally (stops the microphone and the output).
pub struct Call {
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    dec: Option<OpusDecoder>,
    player: Option<Player>,
    /// AEGIRON_AUDIO_OUT=0: decode without playing (tests on one machine).
    silent: bool,
    /// Loudest decoded block from the other side since the last `take_level`.
    level: Option<f32>,
}

impl Call {
    /// Opens the microphone (blocking for up to a few seconds) and starts sending `0x07`
    /// packets to `out`; packets are dropped rather than queued when it is full.
    pub fn start(out: mpsc::Sender<Vec<u8>>) -> Result<Call> {
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<(Vec<f32>, usize, u32)>(64);
        let sink: PcmSink = Box::new(move |data, ch, rate| {
            let _ = pcm_tx.try_send((data.to_vec(), ch, rate));
        });
        let source = audio::start_mic(sink)?;
        let mut enc = OpusEncoder::voice()?;
        {
            let (stop, muted) = (stop.clone(), muted.clone());
            std::thread::Builder::new().name("aegiron-voice".into()).spawn(move || {
                let _source = source; // the microphone stops when this thread ends
                let mut framer = Framer::new();
                let mut mono = vec![0f32; audio::FRAME];
                let mut seq = 0u32;
                while !stop.load(Ordering::Relaxed) && !out.is_closed() {
                    let Ok((data, ch, rate)) = pcm_rx.recv_timeout(Duration::from_millis(200)) else { continue };
                    if muted.load(Ordering::Relaxed) {
                        continue;
                    }
                    framer.push(&data, ch, rate, |frame| {
                        for (m, s) in mono.iter_mut().zip(frame.chunks_exact(audio::CHANNELS)) {
                            *m = (s[0] + s[1]) * 0.5;
                        }
                        if let Ok(pkt) = enc.encode(&mono) {
                            seq = seq.wrapping_add(1);
                            let _ = out.try_send(proto::voice(seq, &pkt));
                        }
                    });
                }
                tracing::info!("voice: microphone stopped");
            })?;
        }
        let silent = std::env::var("AEGIRON_AUDIO_OUT").is_ok_and(|v| v == "0");
        tracing::info!("voice: call started");
        Ok(Call { stop, muted, dec: None, player: None, silent, level: None })
    }

    /// Plays one `0x07` message from the other side.
    pub fn receive(&mut self, msg: &[u8]) {
        let Some((proto::AUDIO_OPUS, _seq, packet)) = proto::parse_voice(msg) else { return };
        if self.dec.is_none() {
            match OpusDecoder::new() {
                Ok(d) => self.dec = Some(d),
                Err(e) => {
                    tracing::warn!("voice: {e:#}");
                    return;
                }
            }
        }
        let Ok(pcm) = self.dec.as_mut().unwrap().decode(packet) else { return };
        self.level = Some(self.level.unwrap_or(0.0).max(audio::rms(pcm)));
        if self.silent {
            return;
        }
        if self.player.is_none() {
            match Player::new() {
                Ok(p) => self.player = Some(p),
                Err(e) => {
                    tracing::warn!("voice: no audio output: {e:#}");
                    self.silent = true;
                    return;
                }
            }
        }
        if let Some(p) = &self.player {
            p.push(pcm);
        }
    }

    /// The next packet opens the output device (blocking briefly).
    pub fn needs_output(&self) -> bool {
        self.player.is_none() && !self.silent
    }

    /// Mutes or unmutes this side's microphone (nothing is sent while muted).
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Loudest block heard from the other side since the last call (None before any).
    pub fn take_level(&mut self) -> Option<f32> {
        self.level.take()
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        tracing::info!("voice: call ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two calls wired back to back with the test tone as microphone: each side hears the other.
    #[test]
    fn calls_carry_the_test_tone_both_ways() {
        // SAFETY: tests in this binary that read these variables expect the tone anyway.
        unsafe {
            std::env::set_var("AEGIRON_TEST_AUDIO", "tone");
            std::env::set_var("AEGIRON_AUDIO_OUT", "0");
        }
        let (a_tx, mut a_rx) = mpsc::channel(64);
        let (b_tx, mut b_rx) = mpsc::channel(64);
        let mut a = Call::start(a_tx).unwrap();
        let mut b = Call::start(b_tx).unwrap();
        let mut seqs = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while seqs.len() < 20 && std::time::Instant::now() < deadline {
            if let Ok(pkt) = a_rx.try_recv() {
                let (codec, seq, data) = proto::parse_voice(&pkt).unwrap();
                assert_eq!(codec, proto::AUDIO_OPUS);
                assert!(data.len() < 200, "32 kbit/s mono is ~80 bytes per 20 ms, got {}", data.len());
                seqs.push(seq);
                b.receive(&pkt);
            }
            if let Ok(pkt) = b_rx.try_recv() {
                a.receive(&pkt);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(seqs.len(), 20, "20 packets (400 ms) in 3 s");
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "no gaps: {seqs:?}");
        assert!(b.take_level().is_some_and(|l| l > 0.05), "b hears a");
        assert!(a.take_level().is_some(), "a hears b");

        // Muted: nothing more is sent.
        a.set_muted(true);
        std::thread::sleep(Duration::from_millis(100));
        while a_rx.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(200));
        assert!(a_rx.try_recv().is_err(), "muted microphone still sends");
    }
}
