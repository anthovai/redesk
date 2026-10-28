// Playback of Aegiron session recordings (.aerec, docs/PROTOCOL.md §7) in the browser.
//
// A recording is the plaintext stream the controlled machine sent during the session, with
// timestamps: H.264 frames are decoded with WebCodecs (the same decoder setup as the live
// viewer), JPEG rectangles are drawn as images, pointer positions are replayed, and Opus audio
// (when the session streamed it) plays through Web Audio on the same clock.

import { RecordedAudio, type AudioPacket } from './audio.js';

const MAGIC = 'AEGREC1\n';
const MSG_JSON = 0x01;
const MSG_FRAME = 0x02;
const MSG_VIDEO = 0x04;
const MSG_AUDIO = 0x05;
const VIDEO_H264 = 1;
const AUDIO_OPUS = 1;
/** Audio plays up to this speed (faster is unintelligible); above it playback is silent. */
const MAX_AUDIO_SPEED = 2;
const H264_CONFIG: VideoDecoderConfig = { codec: 'avc1.42E033', optimizeForLatency: true, hardwareAcceleration: 'no-preference' };

export interface RecordingHeader {
	v: number;
	sid: string;
	device_id: string;
	hostname?: string;
	os?: string;
	started_ms: number;
}

interface Rec {
	t: number;
	data: Uint8Array<ArrayBuffer>;
}

export interface ParsedRecording {
	header: RecordingHeader;
	/** Video, JPEG and JSON records. */
	records: Rec[];
	/** Opus packets, kept apart from the video records; empty for silent or older recordings. */
	audio: AudioPacket[];
	durationMs: number;
}

/**
 * Record times are when the agent sent a packet, which jitters with the network. Consecutive
 * packets (by sequence number) are placed 20 ms apart instead, as long as that stays close to
 * the send time; a gap (dropped or muted audio) or drift starts a new run at the send time.
 */
function audioPacket(r: Rec, prev: { t: number; seq: number } | null): { t: number; seq: number } | null {
	const d = r.data;
	if (d.length < 7 || d[1] !== AUDIO_OPUS) return null;
	const seq = new DataView(d.buffer, d.byteOffset, d.byteLength).getUint32(2);
	if (prev) {
		const n = (seq - prev.seq) >>> 0;
		const expected = prev.t + n * 20;
		if (n >= 1 && n <= 50 && Math.abs(r.t - expected) <= 100) return { t: expected, seq };
	}
	// Never before the end of the previous packet: positions stay in order.
	return { t: Math.max(r.t, prev ? prev.t + 20 : 0), seq };
}

export function parseRecording(buf: ArrayBuffer): ParsedRecording {
	const bytes = new Uint8Array(buf);
	const view = new DataView(buf);
	if (bytes.length < 12 || new TextDecoder().decode(bytes.subarray(0, 8)) !== MAGIC) throw new Error('ไม่ใช่ไฟล์บันทึกของ Aegiron');
	const hlen = view.getUint32(8);
	const header = JSON.parse(new TextDecoder().decode(bytes.subarray(12, 12 + hlen))) as RecordingHeader;
	const records: Rec[] = [];
	const audio: AudioPacket[] = [];
	let prev: { t: number; seq: number } | null = null;
	let durationMs = 0;
	let at = 12 + hlen;
	while (at + 8 <= bytes.length) {
		const t = view.getUint32(at);
		const len = view.getUint32(at + 4);
		if (at + 8 + len > bytes.length) break;
		const r = { t, data: bytes.subarray(at + 8, at + 8 + len) };
		if (r.data[0] === MSG_AUDIO) {
			prev = audioPacket(r, prev);
			if (prev) audio.push({ t: prev.t, data: r.data.subarray(6) });
		} else records.push(r);
		durationMs = Math.max(durationMs, t);
		at += 8 + len;
	}
	return { header, records, audio, durationMs };
}

/** Width and height from a JPEG's SOF marker (to recognise full-screen rectangles). */
function jpegSize(d: Uint8Array): [number, number] | null {
	let i = 2;
	while (i + 9 < d.length) {
		if (d[i] !== 0xff) return null;
		const marker = d[i + 1];
		const len = (d[i + 2] << 8) | d[i + 3];
		if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
			return [(d[i + 7] << 8) | d[i + 8], (d[i + 5] << 8) | d[i + 6]];
		}
		i += 2 + len;
	}
	return null;
}

/** A record playback can start from without anything before it. */
function isKey(r: Rec): boolean {
	const d = r.data;
	if (d[0] === MSG_VIDEO) return d[1] === VIDEO_H264 && (d[2] & 1) === 1;
	if (d[0] === MSG_FRAME && d.length > 9) {
		const v = new DataView(d.buffer, d.byteOffset, d.byteLength);
		if (v.getUint16(1) !== 0 || v.getUint16(3) !== 0) return false;
		const size = jpegSize(d.subarray(9));
		return !!size && size[0] === v.getUint16(5) && size[1] === v.getUint16(7);
	}
	return false;
}

export class RecordingPlayer {
	playing = $state(false);
	/** Playback position in ms. */
	time = $state(0);
	speed = $state(1);
	frame = $state({ w: 0, h: 0 });
	cursor = $state<{ x: number; y: number } | null>(null);
	error = $state('');
	muted = $state(false);
	/** The recording has sound but the browser has not allowed playback yet (needs a click). */
	audioBlocked = $state(false);

	private ctx: CanvasRenderingContext2D;
	private audio: RecordedAudio | null = null;
	private decoder: VideoDecoder | null = null;
	private videoTs = 0;
	private next = 0;
	private wall = 0;
	private base = 0;
	private raf = 0;
	private drawChain: Promise<void> = Promise.resolve();
	private generation = 0;

	constructor(
		public rec: ParsedRecording,
		private canvas: HTMLCanvasElement
	) {
		this.ctx = canvas.getContext('2d', { alpha: false })!;
		if (rec.audio.length && RecordedAudio.supported) this.audio = new RecordedAudio(rec.audio);
		this.seek(0);
	}

	get duration() {
		return this.rec.durationMs;
	}

	/** Whether the recording has sound this browser can play. */
	get hasAudio() {
		return !!this.audio;
	}

	/** Whether the current speed plays sound at all. */
	get audioAtSpeed() {
		return this.speed <= MAX_AUDIO_SPEED;
	}

	/** The playback position right now (between animation frames too). */
	private now = () => (this.playing ? this.base + (performance.now() - this.wall) * this.speed : this.time);

	/** Restarts the audio from the current position after a seek, pause, play or speed change. */
	private syncAudio() {
		this.audio?.reset(this.time, this.now, this.playing && this.audioAtSpeed ? this.speed : 0);
	}

	toggleMute() {
		if (!this.audio) return;
		// The click is a user gesture: the browser now allows playback.
		this.audio.unlock();
		if (this.audioBlocked) {
			this.audioBlocked = false;
			this.muted = false;
		} else this.muted = !this.muted;
		this.audio.muted = this.muted;
	}

	private resize(w: number, h: number) {
		if (this.canvas.width !== w || this.canvas.height !== h) {
			this.canvas.width = w;
			this.canvas.height = h;
			this.frame = { w, h };
		}
	}

	private resetDecoder() {
		if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
		this.decoder = null;
	}

	private apply(r: Rec) {
		const d = r.data;
		if (d[0] === MSG_VIDEO && d.length > 7 && d[1] === VIDEO_H264) {
			const key = (d[2] & 1) === 1;
			if (!this.decoder) {
				if (!key) return;
				if (typeof VideoDecoder === 'undefined') {
					this.error = 'เบราว์เซอร์นี้เล่นวิดีโอ H.264 ไม่ได้ (ต้องรองรับ WebCodecs)';
					return;
				}
				const gen = this.generation;
				this.decoder = new VideoDecoder({
					output: (frame) => {
						if (gen === this.generation) {
							this.resize(frame.displayWidth, frame.displayHeight);
							this.ctx.drawImage(frame, 0, 0);
						}
						frame.close();
					},
					error: (e) => {
						console.warn('recording: h264 decode error', e);
						this.decoder = null;
					}
				});
				this.decoder.configure(H264_CONFIG);
			}
			this.videoTs += 33_333;
			this.decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: this.videoTs, data: d.subarray(7) }));
		} else if (d[0] === MSG_FRAME && d.length > 9) {
			const v = new DataView(d.buffer, d.byteOffset, d.byteLength);
			const [x, y, fw, fh] = [v.getUint16(1), v.getUint16(3), v.getUint16(5), v.getUint16(7)];
			const blob = new Blob([d.subarray(9)], { type: 'image/jpeg' });
			const gen = this.generation;
			// Rectangles must land in order: chain the asynchronous decodes.
			this.drawChain = this.drawChain.then(async () => {
				const bmp = await createImageBitmap(blob).catch(() => null);
				if (!bmp) return;
				if (gen === this.generation) {
					this.resize(fw, fh);
					this.ctx.drawImage(bmp, x, y);
				}
				bmp.close();
			});
		} else if (d[0] === MSG_JSON) {
			try {
				const m = JSON.parse(new TextDecoder().decode(d.subarray(1)));
				if (m.t === 'cursor') this.cursor = m.visible ? { x: m.x, y: m.y } : null;
			} catch {
				// ignore
			}
		}
	}

	/** Jumps to `ms`: restarts from the closest keyframe before it and fast-forwards. */
	seek(ms: number) {
		const recs = this.rec.records;
		ms = Math.max(0, Math.min(ms, this.duration));
		this.generation++;
		this.resetDecoder();
		this.drawChain = Promise.resolve();
		this.cursor = null;
		let start = 0;
		for (let i = 0; i < recs.length && recs[i].t <= ms; i++) if (isKey(recs[i])) start = i;
		// The pointer position at `ms` may come from before the keyframe.
		for (let i = start - 1; i >= 0; i--) {
			const d = recs[i].data;
			if (d[0] === MSG_JSON) {
				this.apply(recs[i]);
				break;
			}
		}
		let i = start;
		for (; i < recs.length && recs[i].t <= ms; i++) this.apply(recs[i]);
		this.next = i;
		this.time = ms;
		this.base = ms;
		this.wall = performance.now();
		this.syncAudio();
	}

	play() {
		if (this.playing) return;
		if (this.time >= this.duration) this.seek(0);
		this.audio?.unlock();
		this.playing = true;
		this.base = this.time;
		this.wall = performance.now();
		this.syncAudio();
		const tick = () => {
			if (!this.playing) return;
			const now = this.now();
			const recs = this.rec.records;
			while (this.next < recs.length && recs[this.next].t <= now) this.apply(recs[this.next++]);
			this.time = Math.min(now, this.duration);
			if (this.audio) {
				this.audio.tick();
				const blocked = !this.audio.running && this.audioAtSpeed;
				if (blocked !== this.audioBlocked) this.audioBlocked = blocked;
			}
			if (this.time >= this.duration) {
				this.playing = false;
				this.syncAudio();
				return;
			}
			this.raf = requestAnimationFrame(tick);
		};
		this.raf = requestAnimationFrame(tick);
	}

	pause() {
		this.playing = false;
		cancelAnimationFrame(this.raf);
		this.syncAudio();
	}

	setSpeed(s: number) {
		this.base = this.time;
		this.wall = performance.now();
		this.speed = s;
		this.syncAudio();
	}

	destroy() {
		this.pause();
		this.generation++;
		this.resetDecoder();
		this.audio?.close();
		this.audio = null;
	}
}

export function clock(ms: number): string {
	const s = Math.floor(ms / 1000);
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	const r = s % 60;
	const mm = h ? String(m).padStart(2, '0') : String(m);
	return `${h ? `${h}:` : ''}${mm}:${String(r).padStart(2, '0')}`;
}
