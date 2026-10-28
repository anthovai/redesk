// Browser side of a remote-control session (see docs/PROTOCOL.md).

import { api, ApiError, type SessionTicket } from './api';
import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel, type ViewerKeys } from './crypto';
import { fingerprint } from './format';

export type Phase = 'requesting' | 'waiting' | 'handshake' | 'auth' | 'approval' | 'connected' | 'closed';

export interface DisplayInfo {
	index: number;
	name: string;
	width: number;
	height: number;
	primary: boolean;
}

export interface AgentInfo {
	hostname: string;
	os: string;
	displays: DisplayInfo[];
	display: number;
	input: boolean;
	clipboard: boolean;
	files?: boolean;
	max_fps: number;
	codec?: string;
	/** The controlled machine records this session (organisation policy). */
	recording?: boolean;
	/** The controlled machine shows annotations drawn by the viewer. */
	whiteboard?: boolean;
}

/** A whiteboard stroke, normalised to the remote screen. */
export interface Stroke {
	color: string;
	pts: [number, number][];
}

/** Pen width as a fraction of the remote screen height (same as the desktop app). */
export const PEN_WIDTH = 0.004;

export interface RemoteEntry {
	name: string;
	dir: boolean;
	size: number;
	mtime: number | null;
}

export interface Transfer {
	id: number;
	name: string;
	direction: 'up' | 'down';
	size: number;
	done: number;
	status: 'active' | 'done' | 'error';
	message?: string;
}

const CHUNK = 64 * 1024;
const MAX_BUFFERED = 4 * 1024 * 1024;

const MSG_JSON = 0x01;
const MSG_FRAME = 0x02;
const MSG_FILE = 0x03;
const MSG_VIDEO = 0x04;
const VIDEO_H264 = 1;

// Constrained Baseline (what OpenH264 emits); Annex B, so no avcC description is needed.
const H264_CONFIG: VideoDecoderConfig = { codec: 'avc1.42E033', optimizeForLatency: true, hardwareAcceleration: 'no-preference' };

/** Codecs to offer the agent, best first. JPEG always works. */
async function supportedCodecs(): Promise<string[]> {
	try {
		if (typeof VideoDecoder !== 'undefined' && (await VideoDecoder.isConfigSupported(H264_CONFIG)).supported) return ['h264', 'jpeg'];
	} catch {
		// fall through to JPEG
	}
	return ['jpeg'];
}
const enc = new TextEncoder();
const dec = new TextDecoder();

const closeReasons: Record<string, string> = {
	pair_timeout: 'เครื่องปลายทางไม่ตอบสนอง',
	agent_busy: 'เครื่องปลายทางไม่ว่าง',
	agent_disconnected: 'เครื่องปลายทางตัดการเชื่อมต่อ',
	agent_closed: 'เครื่องปลายทางปิดการเชื่อมต่อ',
	viewer_closed: 'ปิดการเชื่อมต่อแล้ว',
	server_restart: 'เซิร์ฟเวอร์รีสตาร์ท'
};

function explainClose(reason: string): string {
	if (reason.startsWith('agent_rejected:')) {
		const r = reason.slice('agent_rejected:'.length).trim();
		if (r.includes('denied')) return 'ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ';
		if (r.includes('too many')) return 'ใส่รหัสผ่านผิดหลายครั้งเกินไป';
		if (r.includes('locked')) return 'เครื่องปลายทางล็อกการยืนยันตัวตนชั่วคราว';
		return `เครื่องปลายทางปฏิเสธ: ${r}`;
	}
	return closeReasons[reason] ?? (reason ? `การเชื่อมต่อสิ้นสุด (${reason})` : 'การเชื่อมต่อสิ้นสุด');
}

export class RemoteViewer {
	phase = $state<Phase>('requesting');
	error = $state<string | null>(null);
	needPassword = $state(false);
	authBusy = $state(false);
	attemptsLeft = $state<number | null>(null);
	info = $state<AgentInfo | null>(null);
	device = $state<SessionTicket['device'] | null>(null);
	keyFingerprint = $state('');
	frame = $state({ w: 0, h: 0 });
	stats = $state({ fps: 0, kbps: 0 });
	remoteClipboard = $state<string | null>(null);
	/** Remote pointer normalised to the screen; null while it is elsewhere. */
	cursor = $state<{ x: number; y: number } | null>(null);
	files = $state<{ path: string; parent: string | null; sep: string; entries: RemoteEntry[]; error: string; loading: boolean }>({
		path: '',
		parent: null,
		sep: '/',
		entries: [],
		error: '',
		loading: false
	});
	transfers = $state<Transfer[]>([]);

	private ws: WebSocket | null = null;
	private ch: Channel | null = null;
	private keys: ViewerKeys | null = null;
	private sid = '';
	private sendChain: Promise<void> = Promise.resolve();
	private recvChain: Promise<void> = Promise.resolve();
	private canvas: HTMLCanvasElement | null = null;
	private ctx: CanvasRenderingContext2D | null = null;
	private pendingMove: [number, number] | null = null;
	private moveScheduled = false;
	private frames = 0;
	private bytesIn = 0;
	private statsTimer: ReturnType<typeof setInterval> | null = null;
	private nextTransfer = 1;
	private codecs: Promise<string[]> = supportedCodecs();
	private decoder: VideoDecoder | null = null;
	private needKeyframe = true;
	private videoTs = 0;
	private downloads = new Map<number, Uint8Array<ArrayBuffer>[]>();

	constructor(public deviceId: string) {}

	attach(canvas: HTMLCanvasElement) {
		this.canvas = canvas;
		this.ctx = canvas.getContext('2d', { alpha: false });
	}

	async start() {
		try {
			const t = await api.post<SessionTicket>('/api/sessions', { device_id: this.deviceId });
			this.device = t.device;
			this.sid = t.ticket.sid;
			this.keyFingerprint = await fingerprint(t.device.public_key);
			const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
			const q = `sid=${encodeURIComponent(t.ticket.sid)}&token=${encodeURIComponent(t.ticket.token)}`;
			const ws = new WebSocket(`${proto}//${location.host}${t.ticket.relay_path}?${q}`);
			ws.binaryType = 'arraybuffer';
			this.ws = ws;
			this.phase = 'waiting';
			ws.onmessage = (ev) => {
				if (typeof ev.data !== 'string') this.bytesIn += (ev.data as ArrayBuffer).byteLength;
				this.recvChain = this.recvChain.then(() => this.onMessage(ev.data)).catch((e) => this.fail(e));
			};
			ws.onclose = (ev) => {
				if (this.phase !== 'closed') {
					this.phase = 'closed';
					this.error ??= explainClose(ev.reason);
				}
				this.stopStats();
			};
			ws.onerror = () => {
				this.error ??= 'เชื่อมต่อ relay ไม่สำเร็จ';
			};
		} catch (e) {
			this.fail(e);
		}
	}

	private fail(e: unknown) {
		this.error = e instanceof ApiError || e instanceof Error ? e.message : String(e);
		this.phase = 'closed';
		this.ws?.close();
		this.stopStats();
	}

	private async onMessage(data: string | ArrayBuffer) {
		if (typeof data === 'string') {
			const m = JSON.parse(data);
			if (m.t === 'paired') {
				this.phase = 'handshake';
				this.keys = await newViewerKeys();
				this.ws?.send(JSON.stringify({ t: 'hello', v: 1, epk: b64encode(this.keys.epk) }));
			} else if (m.t === 'hello' && this.keys && this.device) {
				this.ch = await completeHandshake(this.keys, this.sid, b64decode(m.epk), b64decode(m.sig), b64decode(this.device.public_key));
			}
			return;
		}
		if (!this.ch) return;
		const plain = await this.ch.open(data);
		if (plain[0] === MSG_JSON) this.onControl(JSON.parse(dec.decode(plain.subarray(1))));
		else if (plain[0] === MSG_VIDEO) this.decodeVideo(plain);
		else if (plain[0] === MSG_FRAME) await this.drawFrame(plain);
		else if (plain[0] === MSG_FILE) this.onFileChunk(plain);
	}

	private onControl(m: any) {
		switch (m.t) {
			case 'auth_required':
				this.needPassword = m.password;
				if (m.password) {
					this.phase = 'auth';
				} else {
					this.phase = 'approval';
					this.codecs.then((codecs) => this.send({ t: 'auth', password: null, codecs }));
				}
				break;
			case 'auth_pending':
				this.phase = 'approval';
				break;
			case 'auth_result':
				this.authBusy = false;
				if (m.ok) {
					this.phase = 'connected';
					this.error = null;
					this.startStats();
				} else if (m.reason === 'wrong_password') {
					this.attemptsLeft = m.attempts_left;
					this.error = m.attempts_left > 0 ? `รหัสผ่านไม่ถูกต้อง (เหลือ ${m.attempts_left} ครั้ง)` : 'รหัสผ่านไม่ถูกต้อง';
					if (m.attempts_left > 0) this.phase = 'auth';
				} else if (m.reason === 'locked') {
					this.error = `ล็อกชั่วคราวเพราะใส่รหัสผิดบ่อย ลองใหม่ใน ${m.retry_after} วินาที`;
				} else if (m.reason === 'denied') {
					this.error = 'ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ';
				} else if (m.reason === 'recording_failed') {
					this.error = 'องค์กรกำหนดให้บันทึกเซสชันนี้ แต่เครื่องปลายทางเริ่มบันทึกไม่ได้';
				}
				break;
			case 'info':
				this.info = m;
				break;
			case 'ls':
				this.files = { path: m.path, parent: m.parent, sep: m.sep, entries: m.entries, error: '', loading: false };
				break;
			case 'fs_error':
				if (m.id != null) this.updateTransfer(m.id, { status: 'error', message: m.message });
				else this.files = { ...this.files, error: m.message, loading: false };
				break;
			case 'put_done':
				this.updateTransfer(m.id, { status: 'done' });
				if (this.files.path) this.listDir(this.files.path);
				break;
			case 'get_start':
				this.downloads.set(m.id, []);
				this.updateTransfer(m.id, { size: m.size, name: m.name });
				break;
			case 'get_end':
				this.finishDownload(m.id);
				break;
			case 'cursor':
				this.cursor = m.visible ? { x: m.x, y: m.y } : null;
				break;
			case 'clip':
				this.remoteClipboard = m.s;
				navigator.clipboard?.writeText(m.s).catch(() => {});
				break;
		}
	}

	private async drawFrame(p: Uint8Array<ArrayBuffer>) {
		const v = new DataView(p.buffer, p.byteOffset, p.byteLength);
		const x = v.getUint16(1);
		const y = v.getUint16(3);
		const fw = v.getUint16(5);
		const fh = v.getUint16(7);
		if (!this.canvas || !this.ctx) return;
		this.resize(fw, fh);
		const bmp = await createImageBitmap(new Blob([p.subarray(9)], { type: 'image/jpeg' }));
		this.ctx.drawImage(bmp, x, y);
		bmp.close();
		this.frames++;
	}

	private resize(fw: number, fh: number) {
		const canvas = this.canvas!;
		if (canvas.width !== fw || canvas.height !== fh) {
			canvas.width = fw;
			canvas.height = fh;
			this.frame = { w: fw, h: fh };
		}
	}

	/** H.264 via WebCodecs. Deltas before the first keyframe are dropped; errors restart from one. */
	private decodeVideo(p: Uint8Array<ArrayBuffer>) {
		if (p[1] !== VIDEO_H264 || !this.canvas || !this.ctx) return;
		const key = (p[2] & 1) === 1;
		if (this.needKeyframe && !key) return;
		if (!this.decoder || this.decoder.state === 'closed') {
			const ctx = this.ctx;
			this.decoder = new VideoDecoder({
				output: (frame) => {
					this.resize(frame.displayWidth, frame.displayHeight);
					ctx.drawImage(frame, 0, 0);
					frame.close();
					this.frames++;
				},
				error: (e) => {
					console.warn('h264 decoder error, requesting keyframe', e);
					this.decoder = null;
					this.needKeyframe = true;
					this.send({ t: 'keyframe' });
				}
			});
			this.decoder.configure(H264_CONFIG);
		}
		this.needKeyframe = false;
		this.videoTs += 33_333;
		this.decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: this.videoTs, data: p.subarray(7) }));
	}

	private startStats() {
		this.stopStats();
		this.statsTimer = setInterval(() => {
			this.stats = { fps: this.frames, kbps: Math.round((this.bytesIn * 8) / 1000) };
			this.frames = 0;
			this.bytesIn = 0;
		}, 1000);
	}

	private stopStats() {
		if (this.statsTimer) clearInterval(this.statsTimer);
		this.statsTimer = null;
		// Stats stop exactly when the session ends; release the hardware decoder with them.
		if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
		this.decoder = null;
	}

	/** Encrypts and sends in call order (the nonce counter depends on it). */
	private enqueue(plain: Uint8Array<ArrayBuffer>): Promise<void> {
		const ch = this.ch;
		const ws = this.ws;
		if (!ch || !ws) return Promise.resolve();
		this.sendChain = this.sendChain
			.then(async () => {
				const ct = await ch.seal(plain);
				if (ws.readyState === WebSocket.OPEN) ws.send(ct);
			})
			.catch((e) => this.fail(e));
		return this.sendChain;
	}

	send(msg: object): Promise<void> {
		const body = enc.encode(JSON.stringify(msg));
		const plain = new Uint8Array(body.length + 1);
		plain[0] = MSG_JSON;
		plain.set(body, 1);
		return this.enqueue(plain);
	}

	// --- file transfer ---

	private updateTransfer(id: number, patch: Partial<Transfer>) {
		this.transfers = this.transfers.map((t) => (t.id === id ? { ...t, ...patch } : t));
	}

	listDir(path: string) {
		this.files = { ...this.files, loading: true, error: '' };
		this.send({ t: 'ls', path });
	}

	async upload(file: File, dir: string) {
		const id = this.nextTransfer++;
		this.transfers = [...this.transfers, { id, name: file.name, direction: 'up', size: file.size, done: 0, status: 'active' }];
		await this.send({ t: 'put', id, dir, name: file.name, size: file.size });
		for (let off = 0; off < file.size; off += CHUNK) {
			if (this.phase !== 'connected' || this.transfers.find((t) => t.id === id)?.status !== 'active') return;
			const data = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer());
			const plain = new Uint8Array(5 + data.length);
			plain[0] = MSG_FILE;
			new DataView(plain.buffer).setUint32(1, id);
			plain.set(data, 5);
			await this.enqueue(plain);
			// Let the socket drain so large files don't sit in browser memory.
			while (this.ws && this.ws.bufferedAmount > MAX_BUFFERED) await new Promise((r) => setTimeout(r, 20));
			this.updateTransfer(id, { done: Math.min(file.size, off + CHUNK) });
		}
		await this.send({ t: 'put_end', id });
	}

	download(path: string, name: string) {
		const id = this.nextTransfer++;
		this.transfers = [...this.transfers, { id, name, direction: 'down', size: 0, done: 0, status: 'active' }];
		this.send({ t: 'get', id, path });
	}

	cancelTransfer(id: number) {
		this.updateTransfer(id, { status: 'error', message: 'ยกเลิกแล้ว' });
		this.downloads.delete(id);
		this.send({ t: 'cancel', id });
	}

	clearFinishedTransfers() {
		this.transfers = this.transfers.filter((t) => t.status === 'active');
	}

	private onFileChunk(p: Uint8Array<ArrayBuffer>) {
		const id = new DataView(p.buffer, p.byteOffset).getUint32(1);
		const parts = this.downloads.get(id);
		if (!parts) return;
		parts.push(p.slice(5));
		const t = this.transfers.find((x) => x.id === id);
		if (t) this.updateTransfer(id, { done: t.done + p.length - 5 });
	}

	private finishDownload(id: number) {
		const parts = this.downloads.get(id);
		this.downloads.delete(id);
		const t = this.transfers.find((x) => x.id === id);
		if (!parts || !t) return;
		const url = URL.createObjectURL(new Blob(parts));
		const a = document.createElement('a');
		a.href = url;
		a.download = t.name;
		a.click();
		setTimeout(() => URL.revokeObjectURL(url), 10_000);
		this.updateTransfer(id, { status: 'done' });
	}

	submitPassword(password: string) {
		this.authBusy = true;
		this.error = null;
		this.codecs.then((codecs) => this.send({ t: 'auth', password, codecs }));
	}

	// --- input (coordinates normalised to the displayed frame) ---

	mouseMove(x: number, y: number) {
		this.pendingMove = [x, y];
		if (this.moveScheduled) return;
		this.moveScheduled = true;
		requestAnimationFrame(() => {
			this.moveScheduled = false;
			if (this.pendingMove) this.send({ t: 'mm', x: this.pendingMove[0], y: this.pendingMove[1] });
			this.pendingMove = null;
		});
	}

	mouseButton(b: number, down: boolean, x: number, y: number) {
		this.pendingMove = null;
		this.send({ t: 'mb', b, down, x, y });
	}

	wheel(dx: number, dy: number) {
		this.send({ t: 'wh', dx, dy });
	}

	key(code: string, key: string, down: boolean) {
		this.send({ t: 'key', code, key, down });
	}

	typeText(s: string) {
		this.send({ t: 'text', s });
	}

	pushClipboard(s: string) {
		this.send({ t: 'clip', s });
	}

	configure(cfg: { fps?: number; quality?: number; max_width?: number; display?: number }) {
		this.send({ t: 'cfg', ...cfg });
		if (cfg.display !== undefined && this.info) this.info.display = cfg.display;
	}

	refresh() {
		this.send({ t: 'refresh' });
	}

	// --- whiteboard (only when info.whiteboard) ---

	strokes = $state<Stroke[]>([]);
	private strokeId = 0;
	private unsent: number[] = [];
	private flushScheduled = false;

	beginStroke(color: string, x: number, y: number) {
		this.flushStroke();
		this.strokeId++;
		this.strokes.push({ color, pts: [[x, y]] });
		this.queuePoint(x, y);
	}

	extendStroke(x: number, y: number) {
		const s = this.strokes[this.strokes.length - 1];
		if (!s) return;
		const [lx, ly] = s.pts[s.pts.length - 1];
		if (Math.hypot(x - lx, y - ly) < 0.0015) return;
		s.pts.push([x, y]);
		this.queuePoint(x, y);
	}

	/** Sends the points of the current stroke drawn since the last message. */
	flushStroke() {
		if (!this.unsent.length) return;
		const color = this.strokes[this.strokes.length - 1]?.color ?? '#ff3b30';
		this.send({ t: 'wb', op: 'line', id: this.strokeId, c: color, w: PEN_WIDTH, p: this.unsent.splice(0) });
	}

	/** Points go out at most once per animation frame. */
	private queuePoint(x: number, y: number) {
		this.unsent.push(x, y);
		if (this.flushScheduled) return;
		this.flushScheduled = true;
		requestAnimationFrame(() => {
			this.flushScheduled = false;
			this.flushStroke();
		});
	}

	clearStrokes() {
		this.strokes = [];
		this.send({ t: 'wb', op: 'clear' });
	}

	close() {
		if (this.phase === 'closed') return;
		this.send({ t: 'bye' });
		this.sendChain.then(() => this.ws?.close(1000, 'viewer_closed'));
		this.phase = 'closed';
		this.error = null;
		this.stopStats();
	}
}
