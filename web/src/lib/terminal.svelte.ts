// Browser side of a remote terminal session (docs/PROTOCOL.md §11): the same relay, handshake
// and authentication as the remote-control viewer, then PTY output and keystrokes as `0x11`
// messages. Rendering is xterm.js (the page owns it); this class only speaks the protocol.

import { api, ApiError, type SessionTicket } from './api';
import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel, type ViewerKeys } from './crypto';
import { fingerprint } from './format';
import { explainRefusal, jsonMessage, MSG_JSON, MSG_TERM, parseJson, termInput } from './termproto';

export type TermPhase = 'requesting' | 'waiting' | 'handshake' | 'auth' | 'approval' | 'starting' | 'connected' | 'closed';

const PING_MS = 30_000;

export class RemoteTerminal {
	phase = $state<TermPhase>('requesting');
	error = $state<string | null>(null);
	authBusy = $state(false);
	device = $state<SessionTicket['device'] | null>(null);
	keyFingerprint = $state('');
	hostname = $state('');
	shell = $state('');
	/** The controlled machine records this session (organisation policy). */
	recording = $state(false);
	/** Exit status of the remote shell once it ended (null: unknown or still running). */
	exitCode = $state<number | null>(null);

	/** PTY output; the page writes it into xterm.js. */
	onOutput: (data: Uint8Array) => void = () => {};
	/** Terminal size to start the shell with. */
	size: () => { cols: number; rows: number } = () => ({ cols: 80, rows: 24 });

	private ws: WebSocket | null = null;
	private ch: Channel | null = null;
	private keys: ViewerKeys | null = null;
	private sid = '';
	private sendChain: Promise<void> = Promise.resolve();
	private recvChain: Promise<void> = Promise.resolve();
	private ping: ReturnType<typeof setInterval> | null = null;

	constructor(public deviceId: string) {}

	async start() {
		try {
			const t = await api.post<SessionTicket>('/api/sessions', { device_id: this.deviceId, type: 'terminal' });
			// A server that predates terminals ignores `type` and would start a screen session.
			if (t.ticket.type !== 'terminal') throw new Error('เซิร์ฟเวอร์ยังไม่รองรับเทอร์มินัล (ต้องอัปเดตเซิร์ฟเวอร์ Aegiron)');
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
				this.recvChain = this.recvChain.then(() => this.onMessage(ev.data)).catch((e) => this.fail(e));
			};
			ws.onclose = (ev) => {
				if (this.phase !== 'closed') {
					this.phase = 'closed';
					if (!this.error && this.exitCode === null && ev.reason && ev.reason !== 'viewer_closed') this.error = `การเชื่อมต่อสิ้นสุด (${ev.reason})`;
				}
				this.stopPing();
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
		this.stopPing();
	}

	private stopPing() {
		if (this.ping) clearInterval(this.ping);
		this.ping = null;
	}

	private sendPlain(plain: Uint8Array<ArrayBuffer>) {
		this.sendChain = this.sendChain.then(async () => {
			if (!this.ch || this.ws?.readyState !== WebSocket.OPEN) return;
			this.ws.send(await this.ch.seal(plain));
		});
	}

	private send(obj: object) {
		this.sendPlain(jsonMessage(obj));
	}

	submitPassword(password: string) {
		this.authBusy = true;
		this.error = null;
		this.send({ t: 'auth', password, codecs: [], features: [], audio: [], session: 'terminal', target: null });
	}

	/** Keystrokes and pasted text from xterm.js. */
	input(data: string) {
		if (this.phase === 'connected') this.sendPlain(termInput(data));
	}

	resize(cols: number, rows: number) {
		if (this.phase === 'connected') this.send({ t: 'term_resize', cols, rows });
	}

	close() {
		if (this.ch) this.send({ t: 'bye' });
		this.sendChain.then(() => this.ws?.close());
		this.phase = 'closed';
		this.stopPing();
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
		if (plain[0] === MSG_TERM) this.onOutput(plain.subarray(1));
		else if (plain[0] === MSG_JSON) this.onControl(parseJson(plain));
	}

	private onControl(m: any) {
		switch (m.t) {
			case 'auth_required':
				if (m.password) {
					this.phase = 'auth';
				} else {
					this.phase = 'approval';
					this.send({ t: 'auth', password: null, codecs: [], features: [], audio: [], session: 'terminal', target: null });
				}
				break;
			case 'auth_pending':
				this.phase = 'approval';
				break;
			case 'auth_result':
				this.authBusy = false;
				if (m.ok) {
					this.phase = 'starting';
					this.error = null;
				} else if (m.reason === 'wrong_password') {
					this.error = m.attempts_left > 0 ? `รหัสผ่านไม่ถูกต้อง (เหลือ ${m.attempts_left} ครั้ง)` : 'รหัสผ่านไม่ถูกต้อง';
					if (m.attempts_left > 0) this.phase = 'auth';
				} else if (m.reason === 'locked') {
					this.error = `ล็อกชั่วคราวเพราะใส่รหัสผิดบ่อย ลองใหม่ใน ${m.retry_after} วินาที`;
				} else {
					this.error = explainRefusal(m.reason);
				}
				break;
			case 'info':
				if (m.session !== 'terminal') {
					this.fail(new Error('เครื่องปลายทางยังไม่รองรับเทอร์มินัล (ต้องอัปเดต Aegiron บนเครื่องนั้น)'));
					return;
				}
				this.hostname = m.hostname ?? '';
				this.recording = !!m.recording;
				this.phase = 'connected';
				this.send({ t: 'term_open', ...this.size() });
				this.ping = setInterval(() => this.send({ t: 'ping' }), PING_MS);
				break;
			case 'term_started':
				this.shell = m.shell ?? '';
				break;
			case 'term_exit':
				if (m.message) this.error = m.message;
				this.exitCode = typeof m.code === 'number' ? m.code : null;
				this.close();
				break;
		}
	}
}
