// End-to-end test of the session controls (docs/PROTOCOL.md, Session controls) against an agent
// started with AEGIRON_TEST_CONTROLS=<log>, which logs instead of locking or restarting:
// negotiation in info.features, view-only, refusals, lock now and after the session, restart with
// a reconnect token that replaces the password once, and the audit entries.
//
//   bun scripts/e2e-controls.ts <device-id> <device-password>
// Env: AEGIRON_URL, AEGIRON_PASSWORD (admin), AEGIRON_CONTROLS_LOG (the agent's
//      AEGIRON_TEST_CONTROLS file). The agent must have allow_input off (as in `make e2e`), so
//      blocking input and privacy mode must not be offered.

import { readFileSync } from 'node:fs';
import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel } from '../src/lib/crypto';
import { Api, base, check, done, sleep } from './e2e-lib';

const [deviceId, devicePassword] = process.argv.slice(2);
const logPath = process.env.AEGIRON_CONTROLS_LOG;
if (!deviceId || !devicePassword || !logPath) throw new Error('usage: AEGIRON_CONTROLS_LOG=<file> e2e-controls.ts <device-id> <device-password>');
const log = () => {
	try {
		return readFileSync(logPath, 'utf8').trim().split('\n').filter(Boolean);
	} catch {
		return [];
	}
};

const api = await Api.admin();
const enc = new TextEncoder();

/** One session through the relay; `auth` is what the viewer answers to auth_required. */
async function session(auth: object) {
	const t = await api.post<any>('/api/sessions', { device_id: deviceId });
	const url = `${base.replace(/^http/, 'ws')}${t.ticket.relay_path}?sid=${encodeURIComponent(t.ticket.sid)}&token=${encodeURIComponent(t.ticket.token)}`;
	const ws = new WebSocket(url, { headers: { Cookie: api.cookie } } as any);
	ws.binaryType = 'arraybuffer';
	const keys = await newViewerKeys();
	let ch: Channel | null = null;
	let chain = Promise.resolve();
	const msgs: any[] = [];
	const closed = new Promise<void>((r) => (ws.onclose = () => r()));
	const send = async (obj: object) => {
		const body = enc.encode(JSON.stringify(obj));
		const plain = new Uint8Array(body.length + 1);
		plain[0] = 1;
		plain.set(body, 1);
		ws.send(await ch!.seal(plain));
	};
	ws.onmessage = (ev) => {
		chain = chain.then(async () => {
			if (typeof ev.data === 'string') {
				const m = JSON.parse(ev.data);
				if (m.t === 'paired') ws.send(JSON.stringify({ t: 'hello', v: 1, epk: b64encode(keys.epk) }));
				if (m.t === 'hello') ch = await completeHandshake(keys, t.ticket.sid, b64decode(m.epk), b64decode(m.sig), b64decode(t.device.public_key));
				return;
			}
			const plain = await ch!.open(ev.data as ArrayBuffer);
			if (plain[0] !== 1) return;
			const m = JSON.parse(new TextDecoder().decode(plain.subarray(1)));
			if (m.t === 'cursor') return;
			msgs.push(m);
			if (m.t === 'auth_required') await send({ codecs: ['jpeg'], ...auth, t: 'auth' });
		});
	};
	/** The next message matching `pred` (waits up to 10 s). */
	const next = async (pred: (m: any) => boolean, what: string): Promise<any> => {
		for (let i = 0; i < 100; i++) {
			const k = msgs.findIndex(pred);
			if (k >= 0) return msgs.splice(k, 1)[0];
			await sleep(100);
		}
		throw new Error(`timed out waiting for ${what}`);
	};
	const ctl = async (msg: object, op: string) => {
		await send(msg);
		return next((m) => m.t === 'ctl' && m.op === op, `ctl ${op}`);
	};
	const close = async () => {
		await send({ t: 'bye' });
		ws.close(1000, 'viewer_closed');
		await closed;
	};
	return { send, next, ctl, close };
}

const timer = setTimeout(() => {
	check(false, 'finished within 90 s');
	done();
}, 90_000);

// 1. A password session: what is offered, and each control.
const s1 = await session({ password: devicePassword });
check((await s1.next((m) => m.t === 'auth_result', 'auth_result')).ok === true, 'password session');
const info = await s1.next((m) => m.t === 'info', 'info');
const f: string[] = info.features ?? [];
check(['view_only', 'lock', 'restart'].every((x) => f.includes(x)), 'info.features lists view_only, lock, restart', f);
check(!f.includes('block_input') && !f.includes('privacy'), 'no block_input / privacy without the input permission', f);

let r = await s1.ctl({ t: 'view_only', on: true }, 'view_only');
check(r.ok && r.on === true, 'view_only on', r);
r = await s1.ctl({ t: 'block_input', on: true }, 'block_input');
check(r.ok === false && /not allowed/.test(r.message), 'block_input refused when not offered', r);
r = await s1.ctl({ t: 'privacy', on: true }, 'privacy');
check(r.ok === false, 'privacy refused when not offered', r);
r = await s1.ctl({ t: 'lock' }, 'lock');
check(r.ok && log().includes('lock'), 'lock (test log)', log());
r = await s1.ctl({ t: 'lock_after', on: true }, 'lock_after');
check(r.ok && r.on === true, 'lock_after on', r);
r = await s1.ctl({ t: 'restart' }, 'restart');
check(r.ok && typeof r.resume === 'string' && r.resume.length >= 40, 'restart answers with a reconnect token', { ok: r.ok });
const token: string = r.resume;
await sleep(2000);
check(log().some((l) => l.startsWith('restart: ')), 'restart command run (test log)', log());
await s1.close();
await sleep(1000);
check(log().filter((l) => l === 'lock').length === 2, 'locked again when the session ended (lock_after)', log());

// 2. Back "after the restart": the token replaces the password, once.
const s2 = await session({ password: null, resume: token });
const ok2 = await s2.next((m) => m.t === 'auth_result', 'auth_result');
check(ok2.ok === true, 'reconnect with the token, no password', ok2);
await s2.next((m) => m.t === 'info', 'info');
await s2.close();
const s3 = await session({ password: null, resume: token });
const no3 = await s3.next((m) => m.t === 'auth_result', 'auth_result');
check(no3.ok === false, 'the same token is refused the second time', no3);
await s3.close().catch(() => {});

// 3. Audit log.
await sleep(1000);
for (const [action, n] of [
	['agent.view_only', 1],
	['agent.lock_screen', 2],
	['agent.lock_after_session', 1],
	['agent.restart', 1]
] as const) {
	const entries = await api.audit(action);
	check(entries.length >= n, `audit ${action}`, entries.length);
}
const resumed = (await api.audit('agent.auth_ok')).filter((e) => JSON.stringify(e.detail ?? e).includes('resume'));
check(resumed.length >= 1, 'audit agent.auth_ok with method resume', resumed.length);

clearTimeout(timer);
done();
