// Session controls the viewer can trigger on the remote machine (docs/PROTOCOL.md, Session
// controls): view-only, block input, privacy mode, lock now / after the session, restart.

/** Controls an agent can offer in `info.features` (absent on older agents). */
export type ControlFeature = 'view_only' | 'block_input' | 'privacy' | 'lock' | 'restart';

/** What the agent confirmed with `ctl` replies; reset with every new session. */
export interface ControlState {
	block_input: boolean;
	privacy: boolean;
	lock_after: boolean;
}

export const noControls = (): ControlState => ({ block_input: false, privacy: false, lock_after: false });

/** Keepalive while the machine is blocked (the agent ends the session after 30 s of silence). */
export const KEEPALIVE_MS = 10_000;

/** How long a restarted machine has to come back (the agent's reconnect token lasts 15 minutes). */
export const RECONNECT_FOR_MS = 12 * 60_000;

/** Delay before reconnect attempt `n` (1-based) after a restart: 3 s, doubling up to 30 s. */
export function reconnectDelay(n: number): number {
	const k = Math.min(Math.max(n - 1, 0), 4);
	return Math.min(3000 * 2 ** k, 30_000);
}

export function hasControl(features: string[] | undefined, f: ControlFeature): boolean {
	return !!features?.includes(f);
}

const what: Record<string, string> = {
	block_input: 'บล็อกเมาส์และคีย์บอร์ด',
	privacy: 'โหมดความเป็นส่วนตัว',
	lock: 'ล็อกหน้าจอ',
	lock_after: 'ล็อกหน้าจอ',
	restart: 'รีสตาร์ท'
};

export interface CtlReply {
	op: string;
	ok: boolean;
	on?: boolean | null;
	resume?: string;
	message?: string;
}

/**
 * Applies an agent's `ctl` reply. Returns the new state, the message to show (if any) and, for an
 * accepted restart, the token for reconnecting (`null` = forget any pending one).
 */
export function applyCtl(state: ControlState, m: CtlReply): { state: ControlState; notice: string | null; resume?: string | null } {
	if (!m.ok) {
		return {
			state,
			notice: `เครื่องปลายทางทำ${what[m.op] ?? 'คำสั่ง'}ไม่สำเร็จ: ${m.message ?? ''}`,
			...(m.op === 'restart' ? { resume: null } : {})
		};
	}
	const on = !!m.on;
	switch (m.op) {
		case 'block_input':
		case 'privacy':
		case 'lock_after':
			return { state: { ...state, [m.op]: on }, notice: null };
		case 'lock':
			return { state, notice: 'ล็อกหน้าจอเครื่องปลายทางแล้ว' };
		case 'restart':
			return { state, notice: 'เครื่องปลายทางกำลังรีสตาร์ท จะเชื่อมต่อใหม่ให้อัตโนมัติ', resume: m.resume ?? null };
	}
	return { state, notice: null };
}
