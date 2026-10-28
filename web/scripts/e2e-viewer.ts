// End-to-end smoke test: logs in, opens a session to a running agent through the relay,
// performs the same E2E handshake as the browser viewer, authenticates, and counts frames.
//
//   bun scripts/e2e-viewer.ts <device-id> <device-password> [remote-dir-for-file-test]
// Env: REDESK_URL (default http://localhost:21114), REDESK_EMAIL, REDESK_PASSWORD
// With a remote dir, also lists it, uploads a random file there, downloads it back and compares.

import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel } from '../src/lib/crypto';

const base = process.env.REDESK_URL ?? 'http://localhost:21114';
const [deviceId, devicePassword, fileDir] = process.argv.slice(2);
if (!deviceId || !devicePassword) throw new Error('usage: e2e-viewer.ts <device-id> <device-password>');

const login = await fetch(`${base}/api/auth/login`, {
	method: 'POST',
	headers: { 'X-ReDesk': '1', 'Content-Type': 'application/json' },
	body: JSON.stringify({ email: process.env.REDESK_EMAIL ?? 'admin@redesk.local', password: process.env.REDESK_PASSWORD })
});
if (!login.ok) throw new Error(`login failed: ${await login.text()}`);
const cookie = login.headers.get('set-cookie')!.split(';')[0];

const res = await fetch(`${base}/api/sessions`, {
	method: 'POST',
	headers: { 'X-ReDesk': '1', 'Content-Type': 'application/json', Cookie: cookie },
	body: JSON.stringify({ device_id: deviceId })
});
const t = await res.json();
if (!res.ok) throw new Error(`session request failed: ${t.error}`);
console.log('ticket', t.ticket.sid, 'device', t.device.hostname);

const wsUrl = `${base.replace(/^http/, 'ws')}${t.ticket.relay_path}?sid=${encodeURIComponent(t.ticket.sid)}&token=${encodeURIComponent(t.ticket.token)}`;
// Bun's WebSocket accepts custom headers; browsers send the cookie automatically.
const ws = new WebSocket(wsUrl, { headers: { Cookie: cookie } } as any);
ws.binaryType = 'arraybuffer';

const keys = await newViewerKeys();
let ch: Channel | null = null;
let frames = 0;
let frameBytes = 0;
let chain = Promise.resolve();
const enc = new TextEncoder();
const payload = crypto.getRandomValues(new Uint8Array(200_000));
const downloaded: Uint8Array[] = [];

async function send(obj: object) {
	const body = enc.encode(JSON.stringify(obj));
	const plain = new Uint8Array(body.length + 1);
	plain[0] = 1;
	plain.set(body, 1);
	ws.send(await ch!.seal(plain));
}

async function sendChunk(id: number, data: Uint8Array) {
	const plain = new Uint8Array(5 + data.length);
	plain[0] = 3;
	new DataView(plain.buffer).setUint32(1, id);
	plain.set(data, 5);
	ws.send(await ch!.seal(plain));
}

async function finish() {
	console.log(`frames received: ${frames} (${Math.round(frameBytes / 1024)} KiB)`);
	await send({ t: 'bye' });
	ws.close(1000, 'viewer_closed');
}

const done = new Promise<void>((resolve, reject) => {
	const timer = setTimeout(() => reject(new Error('timeout')), 20_000);
	ws.onclose = (e) => {
		clearTimeout(timer);
		console.log('closed:', e.code, e.reason);
		resolve();
	};
	ws.onmessage = (ev) => {
		chain = chain.then(async () => {
			if (typeof ev.data === 'string') {
				const m = JSON.parse(ev.data);
				console.log('relay text:', m.t);
				if (m.t === 'paired') ws.send(JSON.stringify({ t: 'hello', v: 1, epk: b64encode(keys.epk) }));
				if (m.t === 'hello') {
					ch = await completeHandshake(keys, t.ticket.sid, b64decode(m.epk), b64decode(m.sig), b64decode(t.device.public_key));
					console.log('handshake OK (agent signature verified)');
				}
				return;
			}
			const plain = await ch!.open(ev.data as ArrayBuffer);
			if (plain[0] === 2) {
				frames++;
				frameBytes += plain.length;
				return;
			}
			if (plain[0] === 3) {
				downloaded.push(plain.slice(5));
				return;
			}
			const m = JSON.parse(new TextDecoder().decode(plain.subarray(1)));
			console.log('agent:', JSON.stringify(m).slice(0, 200));
			if (m.t === 'auth_required') await send({ t: 'auth', password: 'wrong-password' });
			if (m.t === 'auth_result' && m.reason === 'wrong_password') await send({ t: 'auth', password: devicePassword });
			if (m.t === 'auth_result' && m.ok) {
				await send({ t: 'mm', x: 0.5, y: 0.5 });
				if (fileDir) {
					await send({ t: 'ls', path: fileDir });
					await send({ t: 'put', id: 1, dir: fileDir, name: '../evil/e2e test.bin', size: payload.length });
					for (let o = 0; o < payload.length; o += 65536) await sendChunk(1, payload.subarray(o, o + 65536));
					await send({ t: 'put_end', id: 1 });
				} else {
					setTimeout(finish, 3000);
				}
			}
			if (m.t === 'put_done') await send({ t: 'get', id: 2, path: m.path });
			if (m.t === 'get_end') {
				const got = new Uint8Array(downloaded.reduce((n, d) => n + d.length, 0));
				let o = 0;
				for (const d of downloaded) (got.set(d, o), (o += d.length));
				const same = got.length === payload.length && got.every((b, i) => b === payload[i]);
				console.log(`file round trip: ${got.length} bytes, identical=${same}`);
				await finish();
			}
		}).catch((e) => {
			console.error('error:', e);
			ws.close();
		});
	};
});
await done;
