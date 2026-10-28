// Remote audio in the browser: Opus packets (0x05, see docs/PROTOCOL.md) decoded with
// WebCodecs and played through Web Audio with a small jitter buffer.

export const OPUS_CONFIG: AudioDecoderConfig = { codec: 'opus', sampleRate: 48000, numberOfChannels: 2 };

/** Audio codecs to offer the agent; empty when this browser cannot decode Opus with WebCodecs. */
export async function supportedAudioCodecs(): Promise<string[]> {
	try {
		if (typeof AudioDecoder === 'undefined' || typeof AudioContext === 'undefined') return [];
		return (await AudioDecoder.isConfigSupported(OPUS_CONFIG)).supported ? ['opus'] : [];
	} catch {
		return [];
	}
}

/** Start this far ahead of "now" so network jitter does not cause gaps. */
const LEAD = 0.08;
/** If scheduled audio runs further ahead than this, skip forward to keep latency low. */
const MAX_AHEAD = 0.35;

export class RemoteAudio {
	private ctx: AudioContext | null = null;
	private decoder: AudioDecoder | null = null;
	private playhead = 0;
	private peak = 0;
	private decoded = false;
	muted = false;

	/** Creates or resumes the AudioContext; call from a user gesture (autoplay policy). */
	unlock() {
		try {
			this.ctx ??= new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
			if (this.ctx.state === 'suspended') this.ctx.resume().catch(() => {});
		} catch {
			// no audio output available
		}
	}

	/** One Opus packet; `seq` counts 20 ms frames. */
	push(seq: number, packet: Uint8Array<ArrayBuffer>) {
		if (!this.decoder || this.decoder.state === 'closed') {
			this.decoder = new AudioDecoder({
				output: (data) => this.play(data),
				error: (e) => {
					console.warn('opus decoder error', e);
					this.decoder = null;
				}
			});
			this.decoder.configure(OPUS_CONFIG);
		}
		this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: seq * 20_000, data: packet }));
	}

	private play(data: AudioData) {
		try {
			const frames = data.numberOfFrames;
			const channels = Math.min(2, data.numberOfChannels);
			const planes: Float32Array<ArrayBuffer>[] = [];
			let sum = 0;
			for (let c = 0; c < channels; c++) {
				const plane = new Float32Array(frames);
				data.copyTo(plane, { planeIndex: c, format: 'f32-planar' });
				for (let i = 0; i < frames; i++) sum += plane[i] * plane[i];
				planes.push(plane);
			}
			this.peak = Math.max(this.peak, Math.sqrt(sum / Math.max(1, frames * channels)));
			this.decoded = true;
			const ctx = this.ctx;
			if (this.muted || !ctx || ctx.state !== 'running') return;
			const buf = ctx.createBuffer(channels, frames, data.sampleRate);
			planes.forEach((p, c) => buf.copyToChannel(p, c));
			const now = ctx.currentTime;
			if (this.playhead < now + 0.01 || this.playhead > now + MAX_AHEAD) this.playhead = now + LEAD;
			const src = ctx.createBufferSource();
			src.buffer = buf;
			src.connect(ctx.destination);
			src.start(this.playhead);
			this.playhead += buf.duration;
		} finally {
			data.close();
		}
	}

	/** Loudest decoded block (RMS) since the last call; null before any audio arrived. */
	takeLevel(): number | null {
		if (!this.decoded) return null;
		const p = this.peak;
		this.peak = 0;
		return p;
	}

	/** Whether sound is actually audible (decoded, unmuted, and the browser allowed playback). */
	get playing(): boolean {
		return !this.muted && this.ctx?.state === 'running';
	}

	close() {
		if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
		this.decoder = null;
		this.ctx?.close().catch(() => {});
		this.ctx = null;
	}
}
