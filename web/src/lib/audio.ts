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

function opusDecoder(output: (data: AudioData) => void, failed: () => void): AudioDecoder {
	const decoder = new AudioDecoder({
		output,
		error: (e) => {
			console.warn('opus decoder error', e);
			failed();
		}
	});
	decoder.configure(OPUS_CONFIG);
	return decoder;
}

/** Decoded samples as planar float (at most two channels) plus their RMS level. */
function readPlanes(data: AudioData): { planes: Float32Array<ArrayBuffer>[]; rms: number } {
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
	return { planes, rms: Math.sqrt(sum / Math.max(1, frames * channels)) };
}

function toBuffer(ctx: BaseAudioContext, planes: Float32Array<ArrayBuffer>[], rate: number): AudioBuffer {
	const buf = ctx.createBuffer(planes.length, planes[0].length, rate);
	planes.forEach((p, c) => buf.copyToChannel(p, c));
	return buf;
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
			this.decoder = opusDecoder(
				(data) => this.play(data),
				() => (this.decoder = null)
			);
		}
		this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: seq * 20_000, data: packet }));
	}

	private play(data: AudioData) {
		try {
			const { planes, rms } = readPlanes(data);
			this.peak = Math.max(this.peak, rms);
			this.decoded = true;
			const ctx = this.ctx;
			if (this.muted || !ctx || ctx.state !== 'running') return;
			const buf = toBuffer(ctx, planes, data.sampleRate);
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

/** One recorded Opus packet (20 ms) at its position in a session recording, in ms. */
export interface AudioPacket {
	t: number;
	data: Uint8Array<ArrayBuffer>;
}

/** Decode this far (recording time) ahead of the playback position. */
const RECORDED_AHEAD_MS = 400;
/** A block that starts this close to the end of the previous one continues it seamlessly. */
const JOIN = 0.03;

/**
 * Audio of a session recording, played in step with the player's clock. Packets are decoded
 * shortly before they are due and each block is scheduled at the context time that matches its
 * position in the recording, so seeking, pausing and speed changes only need `reset`.
 */
export class RecordedAudio {
	private ctx: AudioContext | null = null;
	private gain: GainNode | null = null;
	private decoder: AudioDecoder | null = null;
	private sources = new Set<AudioBufferSourceNode>();
	private next = 0;
	private playhead = 0;
	private generation = 0;
	/** The player's position in ms, and its speed (0 = silent); both set by `reset`. */
	private clock: () => number = () => 0;
	private rate = 0;
	private mute = false;

	constructor(private packets: AudioPacket[]) {}

	/** Whether this browser can decode and play the recording's audio. */
	static get supported(): boolean {
		return typeof AudioDecoder !== 'undefined' && typeof AudioContext !== 'undefined';
	}

	/** Creates or resumes the AudioContext; call from a user gesture (autoplay policy). */
	unlock() {
		if (!RecordedAudio.supported) return;
		try {
			if (!this.ctx) {
				this.ctx = new AudioContext({ sampleRate: 48000 });
				this.gain = this.ctx.createGain();
				this.gain.gain.value = this.mute ? 0 : 1;
				this.gain.connect(this.ctx.destination);
			}
			if (this.ctx.state === 'suspended') this.ctx.resume().catch(() => {});
		} catch {
			// no audio output available
		}
	}

	/** Whether the browser actually lets us play (autoplay policy may hold it back). */
	get running(): boolean {
		return this.ctx?.state === 'running';
	}

	get muted() {
		return this.mute;
	}

	set muted(m: boolean) {
		this.mute = m;
		if (this.gain) this.gain.gain.value = m ? 0 : 1;
	}

	/**
	 * Drops everything decoded or scheduled and continues from recording time `ms`. `clock`
	 * returns the player's current position in ms; `rate` is its speed (0 while paused).
	 */
	reset(ms: number, clock: () => number, rate: number) {
		this.generation++;
		for (const s of this.sources) {
			try {
				s.stop();
			} catch {
				// never started
			}
		}
		this.sources.clear();
		if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
		this.decoder = null;
		this.playhead = 0;
		this.clock = clock;
		this.rate = rate;
		// First packet still audible at `ms`.
		let lo = 0;
		let hi = this.packets.length;
		while (lo < hi) {
			const mid = (lo + hi) >> 1;
			if (this.packets[mid].t + 20 <= ms) lo = mid + 1;
			else hi = mid;
		}
		this.next = lo;
	}

	/** Feeds the decoder up to a little ahead of the clock; call on every player tick. */
	tick() {
		if (!this.rate) return;
		const now = this.clock();
		// Never decode a backlog (e.g. while the browser held playback back).
		while (this.next < this.packets.length && this.packets[this.next].t + 20 <= now) this.next++;
		if (!this.running) return;
		const until = now + RECORDED_AHEAD_MS * this.rate;
		while (this.next < this.packets.length && this.packets[this.next].t <= until) {
			const p = this.packets[this.next++];
			if (!this.decoder || this.decoder.state === 'closed') {
				const gen = this.generation;
				this.decoder = opusDecoder(
					(data) => this.schedule(data, gen),
					() => (this.decoder = null)
				);
			}
			try {
				this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: p.t * 1000, data: p.data }));
			} catch (e) {
				console.warn('recording: opus decode failed', e);
			}
		}
	}

	private schedule(data: AudioData, gen: number) {
		try {
			const ctx = this.ctx;
			if (gen !== this.generation || !ctx || !this.gain || !this.rate) return;
			const buf = toBuffer(ctx, readPlanes(data).planes, data.sampleRate);
			const now = ctx.currentTime;
			// Context time at which this block is due on the player's clock (it is heard after
			// the output latency, so it starts that much earlier).
			let when = now + (data.timestamp / 1000 - this.clock()) / 1000 / this.rate - (ctx.outputLatency || 0);
			// Back-to-back packets join seamlessly instead of following clock jitter; a gap in the
			// recording (dropped or muted audio) or drift beyond JOIN re-anchors on the clock.
			if (Math.abs(when - this.playhead) < JOIN) when = this.playhead;
			const dur = buf.duration / this.rate;
			if (when + dur <= now) return; // already past
			const src = ctx.createBufferSource();
			src.buffer = buf;
			src.playbackRate.value = this.rate;
			src.connect(this.gain);
			src.onended = () => this.sources.delete(src);
			this.sources.add(src);
			if (when >= now) src.start(when);
			else src.start(now, (now - when) * this.rate);
			this.playhead = when + dur;
		} finally {
			data.close();
		}
	}

	close() {
		this.reset(0, () => 0, 0);
		this.ctx?.close().catch(() => {});
		this.ctx = null;
		this.gain = null;
	}
}
