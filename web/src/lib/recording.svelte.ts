// Playback of Aegiron session recordings (.aerec, docs/PROTOCOL.md §7) in the browser.
//
// A recording is the plaintext stream the controlled machine sent during the session, with
// timestamps: H.264 frames are decoded with WebCodecs (the same decoder setup as the live
// viewer), JPEG rectangles are drawn as images, and pointer positions are replayed.

const MAGIC = 'AEGREC1\n';
const MSG_JSON = 0x01;
const MSG_FRAME = 0x02;
const MSG_VIDEO = 0x04;
const VIDEO_H264 = 1;
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
	records: Rec[];
	durationMs: number;
}

export function parseRecording(buf: ArrayBuffer): ParsedRecording {
	const bytes = new Uint8Array(buf);
	const view = new DataView(buf);
	if (bytes.length < 12 || new TextDecoder().decode(bytes.subarray(0, 8)) !== MAGIC) throw new Error('ไม่ใช่ไฟล์บันทึกของ Aegiron');
	const hlen = view.getUint32(8);
	const header = JSON.parse(new TextDecoder().decode(bytes.subarray(12, 12 + hlen))) as RecordingHeader;
	const records: Rec[] = [];
	let at = 12 + hlen;
	while (at + 8 <= bytes.length) {
		const t = view.getUint32(at);
		const len = view.getUint32(at + 4);
		if (at + 8 + len > bytes.length) break;
		records.push({ t, data: bytes.subarray(at + 8, at + 8 + len) });
		at += 8 + len;
	}
	return { header, records, durationMs: records.length ? records[records.length - 1].t : 0 };
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

	private ctx: CanvasRenderingContext2D;
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
		this.seek(0);
	}

	get duration() {
		return this.rec.durationMs;
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
	}

	play() {
		if (this.playing) return;
		if (this.time >= this.duration) this.seek(0);
		this.playing = true;
		this.base = this.time;
		this.wall = performance.now();
		const tick = () => {
			if (!this.playing) return;
			const now = this.base + (performance.now() - this.wall) * this.speed;
			const recs = this.rec.records;
			while (this.next < recs.length && recs[this.next].t <= now) this.apply(recs[this.next++]);
			this.time = Math.min(now, this.duration);
			if (this.time >= this.duration) {
				this.playing = false;
				return;
			}
			this.raf = requestAnimationFrame(tick);
		};
		this.raf = requestAnimationFrame(tick);
	}

	pause() {
		this.playing = false;
		cancelAnimationFrame(this.raf);
	}

	setSpeed(s: number) {
		this.base = this.time;
		this.wall = performance.now();
		this.speed = s;
	}

	destroy() {
		this.pause();
		this.generation++;
		this.resetDecoder();
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
