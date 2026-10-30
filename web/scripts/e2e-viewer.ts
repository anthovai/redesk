// End-to-end protocol test: logs in, opens a session to a running agent through the relay,
// performs the same E2E handshake as the browser viewer, authenticates (a wrong password first),
// then checks video codec negotiation, codec switching (`cfg {codec}`), remote audio (0x05) and
// mute, with the browser's own crypto code.
//
//   bun scripts/e2e-viewer.ts <device-id> <device-password> [remote-dir-for-file-test]
// Env: AEGIRON_URL (default http://127.0.0.1:21114), AEGIRON_EMAIL, AEGIRON_PASSWORD,
//      AEGIRON_CODECS (what the viewer offers, best first; default "av1,h264,jpeg"),
//      AEGIRON_EXPECT_CODEC (default h264: what an agent without an AV1 encoder must pick),
//      AEGIRON_EXPECT_AUDIO=0 when the agent has no audio (allow_audio off, Linux),
//      AEGIRON_EXPECT_CM=1 when the agent runs with AEGIRON_TEST_CM=auto (chat and voice calls).
// With a remote dir, also lists it, uploads a random file there, downloads it back and compares.

import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel } from '../src/lib/crypto';
import { adminEmail, adminPassword, Api, base, check, done, sleep } from './e2e-lib';

const [deviceId, devicePassword, fileDir] = process.argv.slice(2);
if (!deviceId || !devicePassword) throw new Error('usage: e2e-viewer.ts <device-id> <device-password>');
const offered = (process.env.AEGIRON_CODECS ?? 'av1,h264,jpeg').split(',');
const expectCodec = process.env.AEGIRON_EXPECT_CODEC ?? 'h264';
const expectAudio = process.env.AEGIRON_EXPECT_AUDIO !== '0';
const expectCm = process.env.AEGIRON_EXPECT_CM === '1';
const features = expectCm ? ['chat', 'voice'] : undefined;

const api = await Api.login(adminEmail, adminPassword());
const t = await api.post<any>('/api/sessions', { device_id: deviceId });
console.log('ticket', t.ticket.sid, 'device', t.device.hostname);

const wsUrl = `${base.replace(/^http/, 'ws')}${t.ticket.relay_path}?sid=${encodeURIComponent(t.ticket.sid)}&token=${encodeURIComponent(t.ticket.token)}`;
// Bun's WebSocket accepts custom headers; browsers send the cookie automatically.
const ws = new WebSocket(wsUrl, { headers: { Cookie: api.cookie } } as any);
ws.binaryType = 'arraybuffer';

const keys = await newViewerKeys();
let ch: Channel | null = null;
let chain = Promise.resolve();
const enc = new TextEncoder();
const payload = crypto.getRandomValues(new Uint8Array(200_000));
const downloaded: Uint8Array[] = [];

// What arrived, by kind; `mark()` starts a new counting window.
let stats = { jpeg: 0, h264: 0, av1: 0, keyframes: 0, firstVideoKey: null as boolean | null, audio: 0, audioGaps: 0, bytes: 0 };
let lastSeq = -1;
// Voice call (0x06) from the agent: packets, sequence gaps, and the packets themselves, which
// are sent straight back as this viewer's microphone (bun has no Opus encoder).
let voice = { packets: 0, gaps: 0, lastSeq: -1, echo: false };
let voiceSeq = 0;
const chats: string[] = [];
const voiceMsgs: any[] = [];
const mark = () => {
	const s = stats;
	stats = { jpeg: 0, h264: 0, av1: 0, keyframes: 0, firstVideoKey: null, audio: 0, audioGaps: 0, bytes: 0 };
	return s;
};
let info: any = null;
const encoderMsgs: any[] = [];
let authed: () => void;
const authOk = new Promise<void>((r) => (authed = r));
let fileDone: () => void;
const filesFinished = new Promise<void>((r) => (fileDone = r));

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

const closed = new Promise<void>((resolve) => {
	ws.onclose = (e) => {
		console.log('closed:', e.code, e.reason);
		resolve();
	};
});
ws.onmessage = (ev) => {
	chain = chain
		.then(async () => {
			if (typeof ev.data === 'string') {
				const m = JSON.parse(ev.data);
				if (m.t === 'paired') ws.send(JSON.stringify({ t: 'hello', v: 1, epk: b64encode(keys.epk) }));
				if (m.t === 'hello') {
					ch = await completeHandshake(keys, t.ticket.sid, b64decode(m.epk), b64decode(m.sig), b64decode(t.device.public_key));
					check(true, 'handshake (agent signature verified)');
				}
				return;
			}
			const plain = await ch!.open(ev.data as ArrayBuffer);
			stats.bytes += plain.length;
			if (plain[0] === 2) return void stats.jpeg++;
			if (plain[0] === 4) {
				const key = (plain[2] & 1) === 1;
				if (plain[1] === 1) stats.h264++;
				else if (plain[1] === 2) stats.av1++;
				if (key) stats.keyframes++;
				stats.firstVideoKey ??= key;
				return;
			}
			if (plain[0] === 5) {
				const seq = new DataView(plain.buffer, plain.byteOffset).getUint32(2);
				if (plain[1] === 1 && plain.length > 6) stats.audio++;
				if (lastSeq >= 0 && seq !== lastSeq + 1) stats.audioGaps++;
				lastSeq = seq;
				return;
			}
			if (plain[0] === 6) {
				const seq = new DataView(plain.buffer, plain.byteOffset).getUint32(2);
				if (plain[1] === 1 && plain.length > 6) voice.packets++;
				if (voice.lastSeq >= 0 && seq !== voice.lastSeq + 1) voice.gaps++;
				voice.lastSeq = seq;
				if (voice.echo) {
					const out = plain.slice();
					new DataView(out.buffer).setUint32(2, ++voiceSeq);
					ws.send(await ch!.seal(out));
				}
				return;
			}
			if (plain[0] === 3) return void downloaded.push(plain.slice(5));
			const m = JSON.parse(new TextDecoder().decode(plain.subarray(1)));
			if (m.t !== 'cursor') console.log('agent:', JSON.stringify(m).slice(0, 200));
			if (m.t === 'auth_required') await send({ t: 'auth', password: 'wrong-password', codecs: offered, audio: ['opus'], features });
			if (m.t === 'auth_result' && !m.ok) {
				check(m.reason === 'wrong_password', 'wrong password rejected', m);
				await send({ t: 'auth', password: devicePassword, codecs: offered, audio: ['opus'], features });
			}
			if (m.t === 'chat') chats.push(m.text);
			if (m.t === 'voice') voiceMsgs.push(m);
			if (m.t === 'info') info = m;
			if (m.t === 'encoder') encoderMsgs.push(m);
			if (m.t === 'auth_result' && m.ok) authed();
			if (m.t === 'put_done') await send({ t: 'get', id: 2, path: m.path });
			if (m.t === 'get_end') {
				const got = new Uint8Array(downloaded.reduce((n, d) => n + d.length, 0));
				let o = 0;
				for (const d of downloaded) (got.set(d, o), (o += d.length));
				check(got.length === payload.length && got.every((b, i) => b === payload[i]), 'file round trip', `${got.length} bytes`);
				fileDone();
			}
		})
		.catch((e) => {
			console.error('error:', e);
			ws.close();
		});
};

const timer = setTimeout(() => {
	check(false, 'session finished within 60 s');
	done();
}, 60_000);
await authOk;
await sleep(500);
check(info?.codec === expectCodec, `info.codec is ${expectCodec} (offered ${offered.join(',')})`, info?.codec);
check(!info?.codecs?.includes('av1') || expectCodec === 'av1', 'info.codecs lists AV1 only with an AV1 encoder', info?.codecs);
check(!!info?.audio === expectAudio && (info?.audio_codec ?? null) === (expectAudio ? 'opus' : null), `info.audio ${expectAudio}`, [info?.audio, info?.audio_codec]);

const refreshFor = async (ms: number) => {
	for (let i = 0; i < ms / 500; i++) {
		await send({ t: 'refresh' }); // a static screen sends nothing otherwise
		await sleep(500);
	}
};
mark();
await refreshFor(3000);
let s = mark();
const main = expectCodec === 'jpeg' ? s.jpeg : s[expectCodec as 'h264' | 'av1'];
check(main > 3 && s.firstVideoKey !== false, `${expectCodec} frames arrive, starting with a keyframe`, s);
if (expectAudio) check(s.audio >= 120 && s.audioGaps === 0, 'Opus packets every 20 ms without sequence gaps', { packets: s.audio, gaps: s.audioGaps });
else check(s.audio === 0, 'no audio packets', s.audio);

// Asking for a codec the agent cannot encode changes nothing.
if (!info?.codecs?.includes('av1')) {
	await send({ t: 'cfg', codec: 'av1' });
	await refreshFor(2000);
	s = mark();
	check(s.av1 === 0 && s[expectCodec as 'h264'] > 0 && encoderMsgs.length === 0, 'cfg {codec: av1} ignored without an AV1 encoder', s);
}
// Switch to JPEG and back, as the console's codec menu does.
if (info?.codecs?.includes('jpeg') && expectCodec !== 'jpeg') {
	await send({ t: 'cfg', codec: 'jpeg' });
	await refreshFor(2000);
	mark();
	await refreshFor(1500);
	s = mark();
	check(s.jpeg > 0 && s.h264 + s.av1 === 0 && encoderMsgs.at(-1)?.codec === 'jpeg', 'cfg {codec: jpeg} switches to JPEG and announces it', { s, encoder: encoderMsgs.at(-1) });
	await send({ t: 'cfg', codec: expectCodec });
	await refreshFor(2000);
	s = mark();
	check(s[expectCodec as 'h264'] > 0 && encoderMsgs.at(-1)?.codec === expectCodec, `back to ${expectCodec}`, { s, encoder: encoderMsgs.at(-1) });
}
if (expectAudio) {
	await send({ t: 'audio', on: false });
	await sleep(500);
	mark();
	await sleep(1500);
	s = mark();
	check(s.audio === 0, 'audio {on: false} stops 0x05 packets', s.audio);
	await send({ t: 'audio', on: true });
	await sleep(500);
	mark();
	await sleep(1500);
	s = mark();
	check(s.audio >= 60, 'audio {on: true} resumes them', s.audio);
}

if (expectCm) {
	const f: string[] = info?.features ?? [];
	check(f.includes('chat') && f.includes('voice') && !f.includes('switch'), 'info.features: chat and voice (no switch without the desktop app)', f);
	await send({ t: 'chat', text: 'สวัสดี e2e 👋' });
	await sleep(800);
	check(chats.includes('echo: สวัสดี e2e 👋'), 'chat reaches the agent and comes back', chats);

	await send({ t: 'voice', op: 'call' });
	await sleep(1000);
	check(voiceMsgs.some((m) => m.op === 'accept'), 'voice call accepted', voiceMsgs);
	voice = { packets: 0, gaps: 0, lastSeq: voice.lastSeq, echo: true };
	await sleep(2000);
	check(voice.packets >= 80 && voice.gaps === 0, 'voice: 0x06 Opus packets every 20 ms without gaps', voice);
	await send({ t: 'voice', op: 'mute', muted: true });
	await send({ t: 'voice', op: 'hangup' });
	await sleep(800);
	const report = chats.find((c) => c.startsWith('test: received'));
	const [, n, rms] = report?.match(/received (\d+) voice packets, peak rms ([\d.]+)/) ?? [];
	check(Number(n) >= 80 && Number(rms) > 0.05, "the agent heard this viewer's voice", report);
	voice.echo = false;
	const before = voice.packets;
	await sleep(500);
	check(voice.packets - before <= 2, 'no voice packets after hanging up', voice.packets - before);
}

if (fileDir) {
	await send({ t: 'ls', path: fileDir });
	await send({ t: 'put', id: 1, dir: fileDir, name: '../evil/e2e test.bin', size: payload.length });
	for (let o = 0; o < payload.length; o += 65536) await sendChunk(1, payload.subarray(o, o + 65536));
	await send({ t: 'put_end', id: 1 });
	await filesFinished;
}
await send({ t: 'bye' });
ws.close(1000, 'viewer_closed');
await closed;
clearTimeout(timer);
done();
