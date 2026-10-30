// Browser side of a remote-control session (see docs/PROTOCOL.md).

import { api, ApiError, type SessionTicket } from './api';
import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel, type ViewerKeys } from './crypto';
import { fingerprint } from './format';
import { sanitizeRel, ZIP_LIMIT, ZipWriter, type LocalTree } from './filetree';
import { RemoteAudio, supportedAudioCodecs } from './audio';
import { decodableCodecs, decoderConfig, type VideoCodec } from './videocodec';

export type Phase = 'requesting' | 'waiting' | 'handshake' | 'auth' | 'approval' | 'connected' | 'closed';

export interface DisplayInfo {
	index: number;
	name: string;
	width: number;
	height: number;
	primary: boolean;
	/** Position in the same units as width/height (agents from 0.6). */
	x?: number;
	y?: number;
}

/** A display mode for `res_list` / `res_set` (feature "resolution"). */
export interface DisplayMode {
	w: number;
	h: number;
	/** Refresh rate, 0 when unknown. */
	hz: number;
}

export interface ModeList {
	display: number;
	current: DisplayMode;
	original: DisplayMode;
	modes: DisplayMode[];
}

export type KeyboardMode = 'translate' | 'map';

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
	/** Codecs this session can switch to with `cfg {codec}` (agents from 0.6). */
	codecs?: string[];
	/** The controlled machine records this session (organisation policy). */
	recording?: boolean;
	/** The controlled machine shows annotations drawn by the viewer. */
	whiteboard?: boolean;
	/** Optional capabilities (agents from 0.6), e.g. "fs2" for folders and file management. */
	features?: string[];
	/** Video encoder on the agent: videotoolbox | mediafoundation | openh264 (jpeg for JPEG). */
	encoder?: string;
	/** The agent streams its audio to this viewer (0x05 Opus packets). */
	audio?: boolean;
	audio_codec?: string | null;
	/** The remote resolution can be changed from here (feature "resolution", host allows it). */
	resolution?: boolean;
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
	/** Folder transfers: file counts and the file being sent right now. */
	folder?: boolean;
	files?: number;
	filesDone?: number;
	filesFailed?: number;
	current?: string;
	curSize?: number;
	curDone?: number;
}

interface FolderDownload {
	zip: ZipWriter;
	root: string;
	/** The file being received has a usable path (otherwise its chunks are dropped). */
	valid: boolean;
	/** A file is in progress (counted as done when the next one starts). */
	started: boolean;
}

const CHUNK = 64 * 1024;
const MAX_BUFFERED = 4 * 1024 * 1024;

const MSG_JSON = 0x01;
const MSG_FRAME = 0x02;
const MSG_FILE = 0x03;
const MSG_VIDEO = 0x04;
const MSG_AUDIO = 0x05;
const AUDIO_OPUS = 1;
const CODEC_KEY = 'aegiron.viewer.codec';

function storedCodec(): VideoCodec {
	try {
		return localStorage.getItem(CODEC_KEY) === 'av1' ? 'av1' : 'h264';
	} catch {
		return 'h264';
	}
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
	/** Speaker muted by the user (the agent is asked to stop sending audio). */
	muted = $state(false);
	/** Loudest decoded audio block of the last second (RMS 0–1); null before any audio. */
	audioLevel = $state<number | null>(null);
	/** Audio is decoded but the browser has not allowed playback yet (needs a click). */
	audioBlocked = $state(false);
	/** Video codec the user asked for (H.264 unless they picked AV1); the agent may not have it. */
	wantCodec = $state<VideoCodec>(storedCodec());
	/** Codecs this browser decodes with WebCodecs. */
	decodable = $state<VideoCodec[]>([]);
	/** Streaming every display as one picture (feature "alldisp"). */
	displayAll = $state(false);
	/** Display modes of the display asked for with `listModes` (feature "resolution"). */
	modes = $state<ModeList | null>(null);
	/** How keys reach the remote machine (feature "keymode"); this session only. */
	keyboard = $state<KeyboardMode>('translate');
	/** A message for the page to show once (resolution changes); `n` changes every time. */
	notice = $state<{ text: string; n: number } | null>(null);

	private ws: WebSocket | null = null;
	private ch: Channel | null = null;
	private keys: ViewerKeys | null = null;
	private sid = '';
	private sendChain: Promise<void> = Promise.resolve();
	private recvChain: Promise<void> = Promise.resolve();
	private canvas: HTMLCanvasElement | null = null;
	private ctx: CanvasRenderingContext2D | null = null;
	private pendingMove: [number, number] | null = null;
	private pendingRel: [number, number] = [0, 0];
	private relScheduled = false;
	private moveScheduled = false;
	private frames = 0;
	private bytesIn = 0;
	private statsTimer: ReturnType<typeof setInterval> | null = null;
	private nextTransfer = 1;
	private codecs: Promise<VideoCodec[]> = decodableCodecs().then((c) => (this.decodable = c));
	private audioCodecs: Promise<string[]> = supportedAudioCodecs();
	private audio = new RemoteAudio();
	private decoder: VideoDecoder | null = null;
	/** WebCodecs codec string the decoder is configured for (codec, and level for AV1). */
	private decoderCodec = '';
	private needKeyframe = true;
	private videoTs = 0;
	private downloads = new Map<number, Uint8Array<ArrayBuffer>[]>();
	private folderDownloads = new Map<number, FolderDownload>();
	/** Replies (`fs_ok` / `fs_error`) awaited by id. */
	private replies = new Map<number, (m: any) => void>();
	/** Per-file upload id -> its folder transfer id. */
	private subOf = new Map<number, number>();
	/** Folder uploads that have sent everything and wait for the last `put_done`. */
	private folderSent = new Set<number>();
	/** Ids whose replies are ignored (empty sub-folders). */
	private quiet = new Set<number>();
	/** File operations started from the panel (rm / mv / mkdir). */
	private ops = new Set<number>();

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
		else if (plain[0] === MSG_AUDIO && plain.length > 6 && plain[1] === AUDIO_OPUS) {
			this.audio.push(new DataView(plain.buffer, plain.byteOffset).getUint32(2), plain.subarray(6));
		}
	}

	private onControl(m: any) {
		switch (m.t) {
			case 'auth_required':
				this.needPassword = m.password;
				if (m.password) {
					this.phase = 'auth';
				} else {
					this.phase = 'approval';
					this.authMessage(null).then((m) => this.send(m));
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
			case 'encoder':
				// The agent switched encoders mid-session (e.g. hardware -> OpenH264 -> JPEG).
				if (this.info) this.info = { ...this.info, encoder: m.encoder, codec: m.codec ?? this.info.codec };
				break;
			case 'ls':
				this.files = { path: m.path, parent: m.parent, sep: m.sep, entries: m.entries, error: '', loading: false };
				break;
			case 'fs_ok':
				if (this.takeReply(m) || this.quiet.delete(m.id)) break;
				this.ops.delete(m.id);
				if (this.files.path) this.listDir(this.files.path);
				break;
			case 'fs_error':
				if (this.takeReply(m) || this.quiet.delete(m.id)) break;
				if (m.id != null && this.subOf.has(m.id)) {
					this.subFinished(m.id, false);
				} else if (m.id != null && this.transfers.some((t) => t.id === m.id)) {
					this.updateTransfer(m.id, { status: 'error', message: m.message });
					this.folderDownloads.delete(m.id);
				} else {
					this.ops.delete(m.id);
					this.files = { ...this.files, error: m.message, loading: false };
				}
				break;
			case 'dir_start':
				this.onDirStart(m);
				break;
			case 'dir_file':
				this.onDirFile(m);
				break;
			case 'dir_skip':
				this.onDirSkip(m);
				break;
			case 'put_done':
				if (this.subOf.has(m.id)) {
					this.subFinished(m.id, true);
					break;
				}
				this.updateTransfer(m.id, { status: 'done' });
				if (this.files.path) this.listDir(this.files.path);
				break;
			case 'get_start':
				this.downloads.set(m.id, []);
				this.updateTransfer(m.id, { size: m.size, name: m.name });
				break;
			case 'get_end':
				if (this.folderDownloads.has(m.id)) this.finishFolderDownload(m.id);
				else this.finishDownload(m.id);
				break;
			case 'res_list':
				this.modes = { display: m.display, current: m.current, original: m.original, modes: m.modes ?? [] };
				break;
			case 'res_ok':
				if (this.modes && this.modes.display === m.display) this.modes = { ...this.modes, current: { w: m.w, h: m.h, hz: m.hz ?? 0 } };
				this.say(`เปลี่ยนความละเอียดเป็น ${m.w}×${m.h} แล้ว`);
				break;
			case 'res_error':
				this.say(`เปลี่ยนความละเอียดไม่สำเร็จ: ${m.message ?? ''}`);
				break;
			case 'displays':
				if (this.info && Array.isArray(m.displays)) this.info = { ...this.info, displays: m.displays };
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

	/**
	 * H.264 or AV1 via WebCodecs. Deltas before the first keyframe are dropped; errors and codec
	 * switches restart from one (the agent sends a keyframe with every switch).
	 */
	private decodeVideo(p: Uint8Array<ArrayBuffer>) {
		if (!this.canvas || !this.ctx || p.length < 7) return;
		const config = decoderConfig(p[1], (p[3] << 8) | p[4], (p[5] << 8) | p[6]);
		if (!config) return;
		const key = (p[2] & 1) === 1;
		if (config.codec !== this.decoderCodec) {
			if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
			this.decoder = null;
			this.decoderCodec = config.codec;
			this.needKeyframe = true;
		}
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
					console.warn('video decoder error, requesting keyframe', e);
					this.decoder = null;
					this.needKeyframe = true;
					this.send({ t: 'keyframe' });
				}
			});
			this.decoder.configure(config);
		}
		this.needKeyframe = false;
		this.videoTs += 33_333;
		this.decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: this.videoTs, data: p.subarray(7) }));
	}

	private startStats() {
		if (this.statsTimer) clearInterval(this.statsTimer);
		this.statsTimer = setInterval(() => {
			this.stats = { fps: this.frames, kbps: Math.round((this.bytesIn * 8) / 1000) };
			this.audioLevel = this.audio.takeLevel();
			this.audioBlocked = this.audioLevel !== null && !this.muted && !this.audio.playing;
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
		this.audio.close();
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

	cancelTransfer(id: number, message = 'ยกเลิกแล้ว') {
		this.updateTransfer(id, { status: 'error', message });
		this.downloads.delete(id);
		this.folderDownloads.delete(id);
		this.folderSent.delete(id);
		this.send({ t: 'cancel', id });
	}

	// --- folders and file management (agent feature "fs2") ---

	/** Folder transfer, delete, rename and new folder are available. */
	get fs2(): boolean {
		return !!this.info?.files && !!this.info?.features?.includes('fs2');
	}

	private takeReply(m: any): boolean {
		const r = m.id != null ? this.replies.get(m.id) : undefined;
		if (!r) return false;
		this.replies.delete(m.id);
		r(m);
		return true;
	}

	/** Sends a request and resolves with its `fs_ok` / `fs_error`. */
	private request(id: number, msg: object): Promise<any> {
		return new Promise((resolve) => {
			this.replies.set(id, resolve);
			this.send(msg);
		});
	}

	private op(msg: Record<string, unknown>) {
		const id = this.nextTransfer++;
		this.ops.add(id);
		this.files = { ...this.files, error: '' };
		this.send({ ...msg, id });
	}

	makeDir(dir: string, name: string) {
		this.op({ t: 'mkdir', dir, name });
	}

	rename(path: string, name: string) {
		this.op({ t: 'mv', path, name });
	}

	remove(path: string) {
		this.op({ t: 'rm', path });
	}

	private subFinished(sub: number, ok: boolean) {
		const id = this.subOf.get(sub)!;
		this.subOf.delete(sub);
		const t = this.transfers.find((x) => x.id === id);
		if (!t) return;
		this.updateTransfer(id, ok ? { filesDone: (t.filesDone ?? 0) + 1 } : { filesFailed: (t.filesFailed ?? 0) + 1 });
		this.checkFolderUpload(id);
	}

	private checkFolderUpload(id: number) {
		const t = this.transfers.find((x) => x.id === id);
		if (!t || !this.folderSent.has(id) || t.status !== 'active') return;
		if ((t.filesDone ?? 0) + (t.filesFailed ?? 0) < (t.files ?? 0)) return;
		this.folderSent.delete(id);
		const failed = t.filesFailed ?? 0;
		this.updateTransfer(id, failed ? { status: 'error', message: `ส่งไม่สำเร็จ ${failed} ไฟล์` } : { status: 'done', current: undefined });
		if (this.files.path) this.listDir(this.files.path);
	}

	/** Uploads a whole folder into `dir` (`""` = ~/Downloads/Aegiron) keeping its structure. */
	async uploadTree(tree: LocalTree, dir: string) {
		const id = this.nextTransfer++;
		const size = tree.files.reduce((n, f) => n + f.file.size, 0);
		const active = () => this.phase === 'connected' && this.transfers.find((t) => t.id === id)?.status === 'active';
		this.transfers = [
			...this.transfers,
			{ id, name: tree.name, direction: 'up', size, done: 0, status: 'active', folder: true, files: tree.files.length, filesDone: 0, filesFailed: 0 }
		];
		// A new folder (`name (1)` if taken), so nothing existing is merged into or overwritten.
		const r = await this.request(id, { t: 'mkdir', id, dir, name: tree.name });
		if (r.t !== 'fs_ok') {
			this.updateTransfer(id, { status: 'error', message: r.message });
			return;
		}
		const root: string = r.path;
		for (const rel of tree.emptyDirs) {
			const sub = this.nextTransfer++;
			this.quiet.add(sub);
			await this.send({ t: 'mkdir', id: sub, dir: root, name: rel });
		}
		let done = 0;
		for (const { rel, file } of tree.files) {
			if (!active()) return;
			const sub = this.nextTransfer++;
			this.subOf.set(sub, id);
			this.updateTransfer(id, { current: rel, curSize: file.size, curDone: 0 });
			await this.send({ t: 'put', id: sub, dir: root, name: file.name, size: file.size, rel });
			for (let off = 0; off < file.size; off += CHUNK) {
				if (!active()) {
					this.send({ t: 'cancel', id: sub });
					this.subOf.delete(sub);
					return;
				}
				const data = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer());
				await this.enqueue(this.chunk(sub, data));
				while (this.ws && this.ws.bufferedAmount > MAX_BUFFERED) await new Promise((r) => setTimeout(r, 20));
				done += data.length;
				this.updateTransfer(id, { done, curDone: Math.min(file.size, off + CHUNK) });
			}
			await this.send({ t: 'put_end', id: sub });
		}
		this.folderSent.add(id);
		this.checkFolderUpload(id);
	}

	/** Downloads a remote folder; the browser saves it as `<name>.zip`. */
	downloadDir(path: string, name: string) {
		const id = this.nextTransfer++;
		this.transfers = [
			...this.transfers,
			{ id, name, direction: 'down', size: 0, done: 0, status: 'active', folder: true, files: 0, filesDone: 0, filesFailed: 0 }
		];
		this.folderDownloads.set(id, { zip: new ZipWriter(), root: sanitizeRel(name)?.[0] ?? 'folder', valid: false, started: false });
		this.send({ t: 'get_dir', id, path });
	}

	private onDirStart(m: any) {
		const fd = this.folderDownloads.get(m.id);
		if (!fd) return;
		if (m.size >= ZIP_LIMIT) {
			this.cancelTransfer(m.id, 'โฟลเดอร์ใหญ่เกิน 4 GB สำหรับไฟล์ ZIP');
			return;
		}
		fd.root = sanitizeRel(String(m.name ?? ''))?.[0] ?? fd.root;
		fd.zip.addDir(fd.root);
		for (const d of m.dirs ?? []) {
			const parts = sanitizeRel(String(d));
			if (parts) fd.zip.addDir(`${fd.root}/${parts.join('/')}`);
		}
		this.updateTransfer(m.id, { name: fd.root, size: m.size, files: m.files });
	}

	private onDirFile(m: any) {
		const fd = this.folderDownloads.get(m.id);
		const t = this.transfers.find((x) => x.id === m.id);
		if (!fd || !t) return;
		const parts = sanitizeRel(String(m.rel ?? ''));
		const patch: Partial<Transfer> = { current: String(m.rel ?? ''), curSize: m.size, curDone: 0 };
		if (fd.started) patch.filesDone = (t.filesDone ?? 0) + 1;
		if (parts) {
			fd.zip.beginFile(`${fd.root}/${parts.join('/')}`);
			fd.valid = true;
			fd.started = true;
		} else {
			// Unsafe path from the remote side: drop this file.
			fd.zip.endFile();
			fd.valid = false;
			fd.started = false;
			patch.filesFailed = (t.filesFailed ?? 0) + 1;
		}
		this.updateTransfer(m.id, patch);
	}

	private onDirSkip(m: any) {
		const fd = this.folderDownloads.get(m.id);
		const t = this.transfers.find((x) => x.id === m.id);
		if (!fd || !t) return;
		fd.zip.endFile();
		this.updateTransfer(m.id, {
			filesDone: (t.filesDone ?? 0) + (fd.started ? 1 : 0),
			filesFailed: (t.filesFailed ?? 0) + 1
		});
		fd.started = false;
		fd.valid = false;
	}

	private finishFolderDownload(id: number) {
		const fd = this.folderDownloads.get(id);
		this.folderDownloads.delete(id);
		const t = this.transfers.find((x) => x.id === id);
		if (!fd || !t || t.status !== 'active') return;
		const url = URL.createObjectURL(fd.zip.finish());
		const a = document.createElement('a');
		a.href = url;
		a.download = `${fd.root}.zip`;
		a.click();
		setTimeout(() => URL.revokeObjectURL(url), 60_000);
		const failed = t.filesFailed ?? 0;
		this.updateTransfer(id, {
			status: 'done',
			filesDone: (t.filesDone ?? 0) + (fd.started ? 1 : 0),
			current: undefined,
			message: failed ? `ข้ามไป ${failed} ไฟล์` : undefined
		});
	}

	private chunk(id: number, data: Uint8Array): Uint8Array<ArrayBuffer> {
		const plain = new Uint8Array(5 + data.length);
		plain[0] = MSG_FILE;
		new DataView(plain.buffer).setUint32(1, id);
		plain.set(data, 5);
		return plain;
	}

	clearFinishedTransfers() {
		this.transfers = this.transfers.filter((t) => t.status === 'active');
	}

	private onFileChunk(p: Uint8Array<ArrayBuffer>) {
		const id = new DataView(p.buffer, p.byteOffset).getUint32(1);
		const fd = this.folderDownloads.get(id);
		if (fd) {
			const data = p.slice(5);
			if (fd.valid) fd.zip.write(data);
			const t = this.transfers.find((x) => x.id === id);
			if (t) this.updateTransfer(id, { done: t.done + data.length, curDone: (t.curDone ?? 0) + data.length });
			if (fd.zip.size >= ZIP_LIMIT) this.cancelTransfer(id, 'โฟลเดอร์ใหญ่เกิน 4 GB สำหรับไฟล์ ZIP');
			return;
		}
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
		this.audio.unlock(); // inside the click/submit gesture, so the browser lets it play
		this.authMessage(password).then((m) => this.send(m));
	}

	private async authMessage(password: string | null) {
		// Best first: the user's choice, then the other codec this browser decodes, then JPEG. AV1
		// is listed even when not chosen so the agent reports whether a switch is possible.
		const order: VideoCodec[] = this.wantCodec === 'av1' ? ['av1', 'h264'] : ['h264', 'av1'];
		const decodable = await this.codecs;
		const codecs = [...order.filter((c) => decodable.includes(c)), 'jpeg'];
		return { t: 'auth', password, codecs, audio: await this.audioCodecs };
	}

	/** Lets audio play after a user gesture (browsers block autoplay until one). */
	unlockAudio() {
		if (!this.muted) this.audio.unlock();
	}

	setMuted(muted: boolean) {
		this.muted = muted;
		this.audio.muted = muted;
		if (!muted) this.audio.unlock();
		this.send({ t: 'audio', on: !muted });
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

	/** Relative pointer motion in the remote's input units (feature "relmouse"), one message per frame. */
	relativeMove(dx: number, dy: number) {
		this.pendingRel[0] += dx;
		this.pendingRel[1] += dy;
		if (this.relScheduled) return;
		this.relScheduled = true;
		requestAnimationFrame(() => {
			this.relScheduled = false;
			const clamp = (v: number) => Math.max(-2000, Math.min(2000, Math.round(v)));
			const [dx, dy] = [clamp(this.pendingRel[0]), clamp(this.pendingRel[1])];
			// Keep the fraction that did not make a whole unit for the next frame.
			this.pendingRel = this.pendingRel.map((v) => v - Math.round(v)) as [number, number];
			if (dx || dy) this.send({ t: 'mr', dx, dy });
		});
	}

	/** Button press/release where the remote pointer is (relative mode). */
	relativeButton(b: number, down: boolean) {
		this.send({ t: 'mb', b, down });
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
		if (cfg.display !== undefined) {
			this.displayAll = false;
			if (this.info) this.info.display = cfg.display;
		}
	}

	/** Optional capability of the agent (`info.features`). */
	has(feature: string): boolean {
		return !!this.info?.features?.includes(feature);
	}

	/** Every display as one picture (feature "alldisp"); `configure({display})` goes back to one. */
	showAllDisplays() {
		if (!this.has('alldisp')) return;
		this.displayAll = true;
		this.send({ t: 'cfg', display_all: true });
	}

	/** Whether the remote resolution can be changed (feature "resolution", allowed by the host). */
	get canResize(): boolean {
		return this.has('resolution') && !!this.info?.resolution;
	}

	listModes(display: number) {
		if (!this.canResize) return;
		if (this.modes?.display !== display) this.modes = null;
		this.send({ t: 'res_list', display });
	}

	setResolution(display: number, mode: DisplayMode) {
		if (!this.canResize) return;
		this.send({ t: 'res_set', display, w: mode.w, h: mode.h, ...(mode.hz ? { hz: mode.hz } : {}) });
	}

	restoreResolution(display: number) {
		if (!this.canResize) return;
		this.send({ t: 'res_restore', display });
	}

	/** Map (physical key positions) or translate (characters); feature "keymode". */
	setKeyboard(mode: KeyboardMode) {
		if (!this.has('keymode')) return;
		this.keyboard = mode;
		this.send({ t: 'cfg', keyboard: mode });
	}

	private say(text: string) {
		this.notice = { text, n: (this.notice?.n ?? 0) + 1 };
	}

	refresh() {
		this.send({ t: 'refresh' });
	}

	/** Switches the video codec (remembered for later sessions). Agents without it keep the current one. */
	setCodec(codec: VideoCodec) {
		this.wantCodec = codec;
		try {
			localStorage.setItem(CODEC_KEY, codec);
		} catch {
			// private mode: this session only
		}
		this.send({ t: 'cfg', codec });
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
