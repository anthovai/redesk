// Terminal sessions (docs/PROTOCOL.md §11): message framing shared by the live terminal and the
// recording player. Kept free of Svelte and DOM so it runs under `bun test`.

export const MSG_JSON = 0x01;
/** Terminal data: PTY output from the agent, keystrokes from the viewer. */
export const MSG_TERM = 0x11;

const enc = new TextEncoder();
const dec = new TextDecoder();

export function jsonMessage(obj: object): Uint8Array<ArrayBuffer> {
	const body = enc.encode(JSON.stringify(obj));
	const plain = new Uint8Array(body.length + 1);
	plain[0] = MSG_JSON;
	plain.set(body, 1);
	return plain;
}

/** Keystrokes (text from xterm.js, UTF-8 encoded) as a `0x11` message. */
export function termInput(data: string | Uint8Array): Uint8Array<ArrayBuffer> {
	const bytes = typeof data === 'string' ? enc.encode(data) : data;
	const plain = new Uint8Array(bytes.length + 1);
	plain[0] = MSG_TERM;
	plain.set(bytes, 1);
	return plain;
}

export function parseJson(plain: Uint8Array): any {
	return plain[0] === MSG_JSON ? JSON.parse(dec.decode(plain.subarray(1))) : null;
}

/** Why the agent refused a tunnel or terminal session (`auth_result.reason`). */
export function explainRefusal(reason: string, target = ''): string {
	switch (reason) {
		case 'terminal_disabled':
			return 'เครื่องปลายทางไม่อนุญาตเทอร์มินัล (เปิดได้ที่ ตั้งค่า → ความปลอดภัย ของเครื่องนั้น)';
		case 'tunnel_disabled':
			return 'เครื่องปลายทางไม่อนุญาต TCP tunnel (เปิดได้ที่ ตั้งค่า → ความปลอดภัย ของเครื่องนั้น)';
		case 'target_not_allowed':
			return `เครื่องปลายทางไม่อนุญาตให้ต่อไปที่ ${target}`;
		case 'recording_required':
			return 'องค์กรกำหนดให้บันทึกเซสชัน แต่ TCP tunnel บันทึกไม่ได้';
		case 'session_mismatch':
			return 'เซิร์ฟเวอร์หรือเครื่องปลายทางยังไม่รองรับเซสชันประเภทนี้';
		case 'denied':
			return 'ผู้ใช้ที่เครื่องปลายทางปฏิเสธการเชื่อมต่อ';
		case 'recording_failed':
			return 'องค์กรกำหนดให้บันทึกเซสชันนี้ แต่เครื่องปลายทางเริ่มบันทึกไม่ได้';
		default:
			return `เครื่องปลายทางปฏิเสธ: ${reason}`;
	}
}

export interface TermRecord {
	t: number;
	data: Uint8Array;
}

/** What a terminal recording shows: output to write, or a size change. */
export type TermEvent = { t: number; size: { cols: number; rows: number } } | { t: number; out: Uint8Array };

/** Terminal output and size records of a recording, in order; everything else is skipped. */
export function termEvents(records: TermRecord[]): TermEvent[] {
	const out: TermEvent[] = [];
	for (const r of records) {
		if (r.data[0] === MSG_TERM) out.push({ t: r.t, out: r.data.subarray(1) });
		else if (r.data[0] === MSG_JSON) {
			try {
				const m = parseJson(r.data);
				if (m?.t === 'term_size' && m.cols > 0 && m.rows > 0) out.push({ t: r.t, size: { cols: m.cols, rows: m.rows } });
			} catch {
				// not JSON we know
			}
		}
	}
	return out;
}

/** Index of the first event after `ms` (events before it are already on screen at `ms`). */
export function eventsUntil(events: TermEvent[], ms: number, from = 0): number {
	let i = from;
	while (i < events.length && events[i].t <= ms) i++;
	return i;
}
