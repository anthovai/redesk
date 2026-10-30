// End-to-end test of tunnel and terminal sessions (docs/PROTOCOL.md §11) against a running
// server and agent. The agent must allow both (`aegiron-agent set --allow-tunnel true
// --allow-terminal true`, before it starts) and must not allow tunnels to private networks.
//
//   1. `aegiron-agent tunnel` to a local HTTP server: one page, 8 parallel 3 MiB downloads (flow
//      control), a tunnel to a TCP server that answers only after the client half-closed, a
//      target outside the policy refused before any password is asked; audit entries with type
//      and target.
//   2. `aegiron-agent terminal --command`: output with Thai text and the command's exit status.
//   3. The protocol with the browser's crypto: resize, Ctrl-C interrupts `sleep`, exit status.
//   4. The web console's terminal (xterm.js) in headless Chrome: typing, output, shell exit.
//   5. Recording policy "all": tunnels refused, a terminal session is recorded (output only) and
//      the console plays it back.
//
//   bun scripts/e2e-remote.ts <device-id>
// Env: AEGIRON_URL, AEGIRON_PASSWORD (admin), AEGIRON_AGENT_BIN (aegiron-agent), AEGIRON_AGENT_CONFIG
//      (the device's agent.toml: its one-time password is read with `show-password`), CHROME_PATH.

import { createHash } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { connect, createServer, type AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { b64decode, b64encode, completeHandshake, newViewerKeys, type Channel } from '../src/lib/crypto';
import { jsonMessage, MSG_TERM, parseJson, termInput } from '../src/lib/termproto';
import { adminEmail, adminPassword, Api, base, check, done, launchChrome, sleep, waitFor } from './e2e-lib';

const [deviceId] = process.argv.slice(2);
const bin = process.env.AEGIRON_AGENT_BIN;
const deviceConfig = process.env.AEGIRON_AGENT_CONFIG;
if (!deviceId || !bin || !deviceConfig) throw new Error('usage: AEGIRON_AGENT_BIN=… AEGIRON_AGENT_CONFIG=… e2e-remote.ts <device-id>');

const admin = await Api.login(adminEmail, adminPassword());
const work = mkdtempSync(join(tmpdir(), 'aegiron-e2e-remote-'));
const viewerConfig = join(work, 'viewer.toml');

async function run(args: string[], env: Record<string, string> = {}, timeoutMs = 60_000) {
	const p = Bun.spawn([bin!, '--config', viewerConfig, ...args], { env: { ...process.env, ...env }, stdout: 'pipe', stderr: 'pipe' });
	const timer = setTimeout(() => p.kill(), timeoutMs);
	const [out, err] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text()]);
	clearTimeout(timer);
	return { code: await p.exited, out, err };
}

/** The device's current one-time password (it rotates after every successful session). */
async function devicePassword(): Promise<string> {
	const p = Bun.spawn([bin!, '--config', deviceConfig!, 'show-password'], { stdout: 'pipe' });
	return (await new Response(p.stdout).text()).trim();
}

async function freePort(): Promise<number> {
	const s = createServer();
	await new Promise<void>((r) => s.listen(0, '127.0.0.1', r));
	const port = (s.address() as AddressInfo).port;
	await new Promise((r) => s.close(r));
	return port;
}

const sha = (b: Uint8Array) => createHash('sha256').update(b).digest('hex');

// --- targets on "the device" (this machine) ---
const big = new Uint8Array(3 << 20).map((_, i) => (i * 7 + (i >> 11)) & 0xff);
const http = Bun.serve({
	port: 0,
	hostname: '127.0.0.1',
	fetch: (req) => (new URL(req.url).pathname === '/big' ? new Response(big) : new Response('hello through the tunnel'))
});
// Answers only after the client finished sending (half-close), then closes.
const halfClose = createServer({ allowHalfOpen: true }, (s) => {
	const h = createHash('sha256');
	let n = 0;
	s.on('data', (d) => {
		n += d.length;
		h.update(d);
	});
	s.on('end', () => s.end(`got ${n} ${h.digest('hex')}`));
});
await new Promise<void>((r) => halfClose.listen(0, '127.0.0.1', r));
const halfPort = (halfClose.address() as AddressInfo).port;

// --- the viewer: a second config signed in as the admin (like the desktop app) ---
let r = await run(['login', '--server', base, '--email', adminEmail], { AEGIRON_LOGIN_PASSWORD: adminPassword() });
check(r.code === 0, 'viewer: aegiron-agent login', r.out.trim() || r.err.trim());
await run(['set', '--direct', 'false']);

/** Starts `aegiron-agent tunnel` and waits until it listens; returns the process. */
async function openTunnel(spec: string) {
	const p = Bun.spawn([bin!, '--config', viewerConfig, 'tunnel', deviceId, spec], {
		env: { ...process.env, AEGIRON_REMOTE_PASSWORD: await devicePassword() },
		stdout: 'pipe',
		stderr: 'pipe'
	});
	const reader = p.stdout.getReader();
	let out = '';
	const open = await Promise.race([
		(async () => {
			while (!out.includes('tunnel open')) {
				const { value, done } = await reader.read();
				if (done) return false;
				out += new TextDecoder().decode(value);
			}
			return true;
		})(),
		sleep(30_000).then(() => false)
	]);
	reader.releaseLock();
	return { p, open, out };
}

async function stopTunnel(p: Bun.Subprocess<'ignore', 'pipe', 'pipe'>) {
	p.kill('SIGINT');
	const code = await p.exited;
	return { code, err: await new Response(p.stderr).text() };
}

// 1. Tunnels
{
	const lport = await freePort();
	const t = await openTunnel(`${lport}:127.0.0.1:${http.port}`);
	check(t.open, 'tunnel: listening', t.out.trim());
	const page = await fetch(`http://127.0.0.1:${lport}/`).then((x) => x.text()).catch((e) => String(e));
	check(page === 'hello through the tunnel', 'tunnel: HTTP page through the tunnel', page);
	const started = Date.now();
	const bodies = await Promise.all(
		Array.from({ length: 8 }, () =>
			fetch(`http://127.0.0.1:${lport}/big`, { headers: { Connection: 'close' } })
				.then((x) => x.arrayBuffer())
				.catch(() => new ArrayBuffer(0))
		)
	);
	const ok = bodies.every((b) => sha(new Uint8Array(b)) === sha(big));
	check(ok, `tunnel: 8 parallel 3 MiB downloads intact (${Date.now() - started} ms)`, bodies.map((b) => b.byteLength));
	const s = await stopTunnel(t.p);
	check(s.code === 0 && /tunnel closed: \d+ connections/.test(s.err), 'tunnel: closed with Ctrl-C, connections counted', s.err.trim());
}
{
	const lport = await freePort();
	const t = await openTunnel(`${lport}:localhost:${halfPort}`);
	check(t.open, 'tunnel (half-close): listening', t.out.trim());
	const payload = new Uint8Array(1 << 20).map((_, i) => (i * 13) & 0xff);
	const reply = await new Promise<string>((resolve) => {
		const c = connect({ port: lport, host: '127.0.0.1', allowHalfOpen: true }, () => {
			c.write(payload);
			c.end(); // half-close: the server answers only after this, and we can still read
		});
		let got = '';
		c.on('data', (d) => (got += d.toString()));
		c.on('end', () => resolve(got));
		c.on('error', (e) => resolve(String(e)));
	});
	check(reply === `got ${payload.length} ${sha(payload)}`, 'tunnel: half-close reaches the target, its answer comes back', reply);
	await stopTunnel(t.p);
}
{
	const lport = await freePort();
	const before = await devicePassword();
	r = await run(['tunnel', deviceId, `${lport}:10.255.255.1:22`], { AEGIRON_REMOTE_PASSWORD: before }, 30_000);
	check(r.code !== 0 && r.err.includes('ไม่อนุญาตให้ต่อไปที่ 10.255.255.1:22'), 'tunnel: private address refused (allow_tunnel_lan off)', r.err.trim());
	check((await devicePassword()) === before, 'tunnel: refused before the password was used');
}
{
	const tunnelReq = (await admin.audit('session.request')).find((e) => e.detail?.type === 'tunnel' && e.detail?.target === `127.0.0.1:${http.port}`);
	check(!!tunnelReq, 'audit: session.request with type and target', tunnelReq?.detail);
	const open = (await admin.audit('agent.tunnel_open')).find((e) => e.detail?.target === `127.0.0.1:${http.port}`);
	check(!!open, 'audit: agent.tunnel_open with target', open?.detail);
	const closed = await waitFor(async () => (await admin.audit('agent.tunnel_closed')).find((e) => e.detail?.target === `127.0.0.1:${http.port}`));
	check(!!closed && closed.detail.connections >= 8 && closed.detail.bytes_down >= 8 * big.length, 'audit: agent.tunnel_closed with connections and bytes', closed?.detail);
	const end = await waitFor(async () => (await admin.audit('session.end')).find((e) => e.detail?.type === 'tunnel'));
	check(!!end && !!end.detail.target, 'audit: session.end with type tunnel', end?.detail);
	const denied = (await admin.audit('agent.denied')).find((e) => e.detail?.reason === 'target_not_allowed');
	check(!!denied && denied.detail.target === '10.255.255.1:22', 'audit: agent.denied target_not_allowed', denied?.detail);
	const sessions = (await admin.get<{ sessions: any[] }>('/api/sessions')).sessions;
	check(sessions.some((s) => s.type === 'tunnel' && s.target === `127.0.0.1:${http.port}`), 'sessions list: type and target');
}

// 2. Terminal from the CLI
{
	r = await run(['terminal', deviceId, '--command', 'echo "x$((6*7))y สวัสดี"; (exit 3)'], { AEGIRON_REMOTE_PASSWORD: await devicePassword() });
	check(r.out.includes('x42y สวัสดี'), 'terminal (CLI): command output with Thai text', r.out.slice(-200));
	check(r.code === 3, 'terminal (CLI): exits with the remote status', r.code);
	const opened = (await admin.audit('agent.terminal_open'))[0];
	check(!!opened?.detail?.shell, 'audit: agent.terminal_open with the shell', opened?.detail);
	const closed = await waitFor(async () => (await admin.audit('agent.terminal_closed')).find((e) => e.detail?.reason === 'exited'));
	check(closed?.detail?.code === 3, 'audit: agent.terminal_closed with the exit status', closed?.detail);
}

// 3. Terminal protocol with the browser's crypto
{
	// Everything awaited before the socket exists: `paired` must not arrive before onmessage is set.
	const keys = await newViewerKeys();
	const pw = await devicePassword();
	const t = await admin.post<any>('/api/sessions', { device_id: deviceId, type: 'terminal' });
	check(t.ticket.type === 'terminal', 'protocol: ticket says terminal');
	const ws = new WebSocket(`${base.replace(/^http/, 'ws')}${t.ticket.relay_path}?sid=${encodeURIComponent(t.ticket.sid)}&token=${encodeURIComponent(t.ticket.token)}`, {
		headers: { Cookie: admin.cookie }
	} as any);
	ws.binaryType = 'arraybuffer';
	let ch: Channel | null = null;
	let chain = Promise.resolve();
	let out = '';
	let info: any = null;
	let exit: any = null;
	const dec = new TextDecoder();
	const send = async (plain: Uint8Array<ArrayBuffer>) => ws.send(await ch!.seal(plain));
	ws.onmessage = (ev) => {
		chain = chain.then(async () => {
			if (typeof ev.data === 'string') {
				const m = JSON.parse(ev.data);
				if (m.t === 'paired') ws.send(JSON.stringify({ t: 'hello', v: 1, epk: b64encode(keys.epk) }));
				if (m.t === 'hello') ch = await completeHandshake(keys, t.ticket.sid, b64decode(m.epk), b64decode(m.sig), b64decode(t.device.public_key));
				return;
			}
			const plain = await ch!.open(ev.data as ArrayBuffer);
			if (plain[0] === MSG_TERM) return void (out += dec.decode(plain.subarray(1), { stream: true }));
			const m = parseJson(plain);
			if (m.t === 'auth_required') await send(jsonMessage({ t: 'auth', password: pw, session: 'terminal', target: null }));
			if (m.t === 'info') {
				info = m;
				await send(jsonMessage({ t: 'term_open', cols: 80, rows: 24 }));
			}
			if (m.t === 'term_exit') exit = m;
		});
	};
	const seen = (s: string, ms = 10_000) => waitFor(() => out.includes(s), ms, 50);
	check(!!(await waitFor(() => info, 20_000)), 'protocol: info for a terminal session', info);
	await sleep(1500); // the shell's prompt (whatever it looks like)
	await send(termInput('stty size\r'));
	check(!!(await seen('24 80')), 'protocol: the shell starts at the viewer size');
	await send(jsonMessage({ t: 'term_resize', cols: 100, rows: 30 }));
	await sleep(200);
	await send(termInput('stty size\r'));
	check(!!(await seen('30 100')), 'protocol: term_resize resizes the PTY');
	const startSleep = Date.now();
	await send(termInput('sleep 20; echo "slept-$((1+1))"\r'));
	await sleep(500);
	await send(termInput('\x03'));
	await send(termInput('echo "int-$((2+2))"\r'));
	check(!!(await seen('int-4', 8_000)) && !out.includes('slept-2') && Date.now() - startSleep < 15_000, 'protocol: Ctrl-C interrupts the running command');
	await send(termInput('exit 0\r'));
	check(!!(await waitFor(() => exit, 10_000)) && exit.code === 0, 'protocol: term_exit with status 0', exit);
	ws.close();
}

// 4. The console's terminal page (xterm.js) in headless Chrome
const browser = await launchChrome();
try {
	const page = await browser.newPage();
	const [name, value] = admin.cookie.split('=');
	await page.setCookie({ name, value, url: base, httpOnly: true, sameSite: 'Strict' });
	const pw = await devicePassword();
	await page.goto(`${base}/terminal/${deviceId}`, { waitUntil: 'networkidle0' });
	await page.waitForSelector('#term-password', { timeout: 20_000 });
	await page.type('#term-password', pw);
	await page.keyboard.press('Enter');
	const rows = () => page.evaluate(() => document.querySelector('.xterm-rows')?.textContent ?? '');
	check(!!(await waitFor(async () => (await rows()).trim().length > 0, 20_000)), 'web terminal: shell prompt shown');
	await sleep(1000);
	await page.click('[data-testid="terminal"]');
	await page.keyboard.type('echo "web-$((3*4))-ไทย"');
	await page.keyboard.press('Enter');
	check(!!(await waitFor(async () => (await rows()).includes('web-12-ไทย'), 10_000)), 'web terminal: typed command output (Thai) rendered by xterm.js', (await rows()).slice(-160));
	await page.keyboard.type('exit 5');
	await page.keyboard.press('Enter');
	const status = await waitFor(() => page.evaluate(() => document.querySelector('[data-testid="term-exit"]')?.textContent ?? ''), 10_000);
	check(!!status && status.includes('5'), 'web terminal: shell exit shown', status);
	await page.close();
} catch (e) {
	check(false, 'web terminal page', String(e));
}

// 5. Recording policy "all"
const before = (await admin.get<{ settings: any }>('/api/recordings/settings')).settings;
await admin.put('/api/recordings/settings', { mode: 'all', group_ids: [], retention_days: 0 });
try {
	const refused = await fetch(`${base}/api/sessions`, {
		method: 'POST',
		headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json', Cookie: admin.cookie },
		body: JSON.stringify({ device_id: deviceId, type: 'tunnel', target: 'localhost:22' })
	});
	check(refused.status === 422, 'recording: tunnels refused when sessions must be recorded', refused.status);
	r = await run(['terminal', deviceId, '--command', 'echo "recorded-$((2+2))"'], { AEGIRON_REMOTE_PASSWORD: await devicePassword() });
	check(r.code === 0 && r.out.includes('recorded-4'), 'recording: terminal session ran', r.err.slice(-200));
	const rec = await waitFor(
		async () => (await admin.get<{ recordings: any[] }>(`/api/recordings?device=${deviceId}`)).recordings.find((x) => x.codec === 'terminal' && x.status === 'ready'),
		30_000,
		500
	);
	check(!!rec, 'recording: uploaded with codec "terminal"', rec);
	if (rec) {
		const file = new Uint8Array(await (await fetch(`${base}/api/recordings/${rec.id}/file`, { headers: { Cookie: admin.cookie } })).arrayBuffer());
		const text = new TextDecoder().decode(file);
		check(text.includes('recorded-4') && text.includes('term_size'), 'recording: contains the output and the terminal size');
		const page = await browser.newPage();
		const [name, value] = admin.cookie.split('=');
		await page.setCookie({ name, value, url: base, httpOnly: true, sameSite: 'Strict' });
		await page.goto(`${base}/recordings/${rec.id}`, { waitUntil: 'networkidle0' });
		const shown = await waitFor(() => page.evaluate(() => document.querySelector('[data-testid="term-playback"] .xterm-rows')?.textContent ?? ''), 15_000);
		check(!!(await waitFor(async () => (await page.evaluate(() => document.querySelector('[data-testid="term-playback"] .xterm-rows')?.textContent ?? '')).includes('recorded-4'), 15_000)), 'recording: the console plays it back', shown);
		await page.close();
	}
} finally {
	await admin.put('/api/recordings/settings', before);
	await browser.close();
	http.stop(true);
	halfClose.close();
	rmSync(work, { recursive: true, force: true });
}
done();
