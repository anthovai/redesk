// Remote audio in the browser: Opus packets (0x05, see docs/PROTOCOL.md) decoded with
// WebCodecs and played through Web Audio with a small jitter buffer.

export const OPUS_CONFIG: AudioDecoderConfig = { codec: 'opus', sampleRate: 48000, numberOfChannels: 2 };
/** Voice calls (0x07): mono Opus, 20 ms frames, in both directions. */
export const VOICE_CONFIG: AudioDecoderConfig = { codec: 'opus', sampleRate: 48000, numberOfChannels: 1 };
const VOICE_ENCODER: AudioEncoderConfig = { codec: 'opus', sampleRate: 48000, numberOfChannels: 1, bitrate: 32000 };

/** Audio codecs to offer the agent; empty when this browser cannot decode Opus with WebCodecs. */
export async function supportedAudioCodecs(): Promise<string[]> {
	try {
		if (typeof AudioDecoder === 'undefined' || typeof AudioContext === 'undefined') return [];
		return (await AudioDecoder.isConfigSupported(OPUS_CONFIG)).supported ? ['opus'] : [];
	} catch {
		return [];
	}
}

function opusDecoder(output: (data: AudioData) => void, failed: () => void, config = OPUS_CONFIG): AudioDecoder {
	const decoder = new AudioDecoder({
		output,
		error: (e) => {
			console.warn('opus decoder error', e);
			failed();
		}
	});
	decoder.configure(config);
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

	/** `config`: system audio (stereo, the default) or a voice call (mono). */
	constructor(private config: AudioDecoderConfig = OPUS_CONFIG) {}

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
				() => (this.decoder = null),
				this.config
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

/** Whether this browser can take part in a voice call (microphone + Opus encode and decode). */
export async function voiceSupported(): Promise<boolean> {
	try {
		if (typeof AudioEncoder === 'undefined' || typeof AudioDecoder === 'undefined' || !navigator.mediaDevices?.getUserMedia) return false;
		const [e, d] = await Promise.all([AudioEncoder.isConfigSupported(VOICE_ENCODER), AudioDecoder.isConfigSupported(VOICE_CONFIG)]);
		return !!e.supported && !!d.supported;
	} catch {
		return false;
	}
}

/** Samples per 20 ms frame at 48 kHz. */
const VOICE_FRAME = 960;

/**
 * The microphone during a voice call, encoded as 20 ms mono Opus packets. `open` asks for the
 * microphone (call it from the click that starts the call); nothing is sent before `start`.
 * The browser's echo cancellation and noise suppression are on.
 */
export class Microphone {
	private stream: MediaStream | null = null;
	private ctx: AudioContext | null = null;
	private node: ScriptProcessorNode | null = null;
	private encoder: AudioEncoder | null = null;
	private pending = new Float32Array(VOICE_FRAME);
	private filled = 0;
	/** Resampling to 48 kHz: read position into the next input block, and the last sample. */
	private pos = 0;
	private last = 0;
	private frames = 0;
	private seq = 0;
	muted = false;

	async open() {
		this.stream = await navigator.mediaDevices.getUserMedia({
			audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }
		});
	}

	/** Starts sending: `packet(seq, opus)` for every 20 ms of sound (nothing while muted). */
	start(packet: (seq: number, data: Uint8Array<ArrayBuffer>) => void) {
		if (!this.stream || this.ctx) return;
		// The device's own rate: Chrome cannot connect a microphone to a context at another rate.
		// `feed` resamples to 48 kHz.
		const ctx = new AudioContext({ latencyHint: 'interactive' });
		this.ctx = ctx;
		const encoder = new AudioEncoder({
			output: (chunk) => {
				const data = new Uint8Array(chunk.byteLength);
				chunk.copyTo(data);
				this.seq = (this.seq + 1) >>> 0;
				packet(this.seq, data);
			},
			error: (e) => console.warn('voice encoder error', e)
		});
		encoder.configure(VOICE_ENCODER);
		this.encoder = encoder;
		const src = ctx.createMediaStreamSource(this.stream);
		// ScriptProcessorNode is deprecated but runs everywhere without a separate worklet file.
		const node = ctx.createScriptProcessor(2048, 1, 1);
		node.onaudioprocess = (ev) => this.feed(ev.inputBuffer.getChannelData(0));
		src.connect(node);
		node.connect(ctx.destination); // it only runs when connected; its output stays silent
		this.node = node;
		if (ctx.state === 'suspended') ctx.resume().catch(() => {});
	}

	private feed(raw: Float32Array) {
		const enc = this.encoder;
		if (!enc || enc.state !== 'configured' || !this.ctx) return;
		const input = this.resample(raw, this.ctx.sampleRate);
		let i = 0;
		while (i < input.length) {
			const n = Math.min(VOICE_FRAME - this.filled, input.length - i);
			this.pending.set(input.subarray(i, i + n), this.filled);
			this.filled += n;
			i += n;
			if (this.filled < VOICE_FRAME) break;
			this.filled = 0;
			const ts = this.frames++ * 20_000;
			if (this.muted) continue;
			const data = new AudioData({ format: 'f32-planar', sampleRate: 48000, numberOfFrames: VOICE_FRAME, numberOfChannels: 1, timestamp: ts, data: this.pending.slice() });
			enc.encode(data);
			data.close();
		}
	}

	/** Linear resampling of one block from `rate` to 48 kHz, continuing from the previous block. */
	private resample(input: Float32Array, rate: number): Float32Array {
		if (rate === 48000) return input;
		const step = rate / 48000;
		const out: number[] = [];
		let p = this.pos;
		while (p < input.length - 1 + 1e-9) {
			const k = Math.floor(p);
			const t = p - k;
			const a = k < 0 ? this.last : input[k];
			const b = input[Math.max(0, k + 1)];
			out.push(a + (b - a) * t);
			p += step;
		}
		this.pos = p - input.length;
		if (input.length) this.last = input[input.length - 1];
		return Float32Array.from(out);
	}

	close() {
		this.node?.disconnect();
		this.node = null;
		if (this.encoder && this.encoder.state !== 'closed') this.encoder.close();
		this.encoder = null;
		this.ctx?.close().catch(() => {});
		this.ctx = null;
		for (const t of this.stream?.getTracks() ?? []) t.stop();
		this.stream = null;
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
	/** Recording time of the last packet given to the decoder. */
	private lastT = -1;
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
			// Chrome's AudioDecoder ignores the timestamps of later chunks and counts on from the
			// first one, so after a gap (muted or dropped audio) its output would be stamped too
			// early and dropped as already past. Each contiguous run gets its own decoder.
			if (this.decoder && p.t !== this.lastT + 20) this.retireDecoder();
			this.lastT = p.t;
			if (!this.decoder || this.decoder.state === 'closed') {
				const gen = this.generation;
				const dec: AudioDecoder = opusDecoder(
					(data) => this.schedule(data, gen),
					() => {
						if (this.decoder === dec) this.decoder = null;
					}
				);
				this.decoder = dec;
			}
			try {
				this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: p.t * 1000, data: p.data }));
			} catch (e) {
				console.warn('recording: opus decode failed', e);
			}
		}
	}

	/** Lets the current decoder finish what it was given, then closes it. */
	private retireDecoder() {
		const dec = this.decoder;
		this.decoder = null;
		if (!dec || dec.state === 'closed') return;
		dec.flush()
			.catch(() => {})
			.finally(() => dec.state !== 'closed' && dec.close());
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
