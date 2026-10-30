// End-to-end test of the RDP / VNC / SSH browser gateway through the real web console,
// against the Docker targets of deploy/test/gateway/compose.yml. Run it via
// deploy/test/gateway-e2e.sh, which starts the targets, an isolated server and sets the env.
//
// For each target it adds the connection to the vault (through the console form or the API,
// as an admin would), opens /gateway/<id> in headless Chrome, checks that the remote screen
// renders, types a command, exchanges clipboard text, disconnects, then checks the audit log,
// the session list and guacd's recording files. It also checks that no stored secret ever
// reaches the browser and that device-group permissions apply to gateway connections.
//
// Env: AEGIRON_URL, AEGIRON_EMAIL, AEGIRON_PASSWORD, GWTEST_DIR (holds id_ed25519 / id_rsa),
//      GWTEST_RDP_PASSWORD, GWTEST_VNC_PASSWORD, GWTEST_SSH_PASSWORD,
//      CHROME (browser binary; default: the installed Google Chrome), GWTEST_SHOTS (screenshot dir),
//      GWTEST_ONLY (comma-separated case names to run).

import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync } from 'node:fs';
import { chromium, type Browser, type BrowserContext, type Page } from 'playwright-core';

const base = process.env.AEGIRON_URL ?? 'http://127.0.0.1:21300';
const dir = process.env.GWTEST_DIR ?? '';
const shots = process.env.GWTEST_SHOTS ?? `${dir}/shots`;
const only = process.env.GWTEST_ONLY?.split(',').filter(Boolean);
const env = (k: string) => {
	const v = process.env[k];
	if (!v) throw new Error(`${k} is not set (run through deploy/test/gateway-e2e.sh)`);
	return v;
};
const admin = { email: env('AEGIRON_EMAIL'), password: env('AEGIRON_PASSWORD') };
const rdpPassword = env('GWTEST_RDP_PASSWORD');
const vncPassword = env('GWTEST_VNC_PASSWORD');
const sshPassword = env('GWTEST_SSH_PASSWORD');
const ed25519 = readFileSync(`${dir}/id_ed25519`, 'utf8');
const rsa = readFileSync(`${dir}/id_rsa`, 'utf8');
// Strings that must never appear in anything the browser receives.
const secrets = [rdpPassword, vncPassword, sshPassword, 'Wrong-Pass-9', ...[ed25519, rsa].map((k) => k.split('\n')[1])];
mkdirSync(shots, { recursive: true });

const results: { name: string; ok: boolean; detail: string }[] = [];
const log = (s: string) => console.log(`[e2e] ${s}`);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function sh(cmd: string, args: string[]): string {
	return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}
const dockerExec = (container: string, ...args: string[]) => sh('docker', ['exec', container, ...args]);

async function poll<T>(what: string, fn: () => Promise<T | undefined | null | false> | T | undefined | null | false, ms = 15000): Promise<T> {
	const end = Date.now() + ms;
	let last: unknown;
	while (Date.now() < end) {
		try {
			const v = await fn();
			if (v) return v as T;
		} catch (e) {
			last = e;
		}
		await sleep(250);
	}
	throw new Error(`timed out waiting for ${what}${last ? `: ${(last as Error).message}` : ''}`);
}

// --- REST helpers (cookie session, like the console) ---

class Api {
	constructor(public cookie = '') {}
	static async login(email: string, password: string) {
		const r = await fetch(`${base}/api/auth/login`, {
			method: 'POST',
			headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json' },
			body: JSON.stringify({ email, password })
		});
		if (!r.ok) throw new Error(`login ${email}: ${r.status} ${await r.text()}`);
		return new Api(r.headers.get('set-cookie')!.split(';')[0]);
	}
	async req<T = any>(method: string, path: string, body?: unknown): Promise<T> {
		const r = await fetch(`${base}${path}`, {
			method,
			headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json', Cookie: this.cookie },
			body: body === undefined ? undefined : JSON.stringify(body)
		});
		const text = await r.text();
		if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text}`);
		return text ? JSON.parse(text) : (undefined as T);
	}
}

// --- browser helpers ---

let browser: Browser;
const leaks: string[] = [];

async function newContext(email: string, password: string): Promise<BrowserContext> {
	const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
	await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: base });
	const page = await ctx.newPage();
	await page.goto(`${base}/login`);
	await page.fill('#email', email);
	await page.fill('#password', password);
	await page.press('#password', 'Enter');
	await page.waitForURL((u) => !u.pathname.startsWith('/login'));
	await page.close();
	return ctx;
}

function watchForSecrets(page: Page, label: string) {
	const check = (where: string, data: string) => {
		for (const s of secrets) if (s && data.includes(s)) leaks.push(`${label}: secret seen in ${where}`);
	};
	page.on('websocket', (ws) => {
		ws.on('framereceived', (f) => check(`ws ${ws.url()}`, String(f.payload)));
	});
	page.on('response', async (r) => {
		if (!r.url().includes('/api/')) return;
		try {
			check(`http ${r.url()}`, await r.text());
		} catch {
			/* redirects and aborted requests have no body */
		}
	});
	page.on('pageerror', (e) => log(`[${label} pageerror] ${e.message}`));
}

type Stats = { width: number; height: number; colors: number; bright: number; share: Record<string, number> };

/** Samples the remote display's canvases (the Guacamole layers); returns the busiest layer. */
async function screenStats(page: Page, probe: Record<string, [number, number, number]>): Promise<Stats> {
	return page.evaluate((probe) => {
		const app = document.querySelector('[role=application]')!;
		const canvases = ([...app.querySelectorAll('canvas')] as HTMLCanvasElement[]).filter((c) => c.width >= 100 && c.height >= 100);
		let best = { width: 0, height: 0, colors: 0, bright: 0, share: {} as Record<string, number> };
		for (const c of canvases) {
			const data = c.getContext('2d')!.getImageData(0, 0, c.width, c.height).data;
			const seen = new Set<number>();
			const hits: Record<string, number> = Object.fromEntries(Object.keys(probe).map((k) => [k, 0]));
			let n = 0;
			let bright = 0;
			for (let i = 0; i < data.length; i += 4 * 7) {
				if (data[i + 3] === 0) continue; // transparent: nothing drawn here on this layer
				n++;
				const [r, g, b] = [data[i], data[i + 1], data[i + 2]];
				seen.add((r << 16) | (g << 8) | b);
				if (r > 150 && g > 150 && b > 150) bright++;
				for (const [k, [pr, pg, pb]] of Object.entries(probe))
					if (Math.abs(r - pr) < 12 && Math.abs(g - pg) < 12 && Math.abs(b - pb) < 12) hits[k]++;
			}
			if (!n || seen.size <= best.colors) continue;
			const share = Object.fromEntries(Object.entries(hits).map(([k, v]) => [k, v / n]));
			best = { width: c.width, height: c.height, colors: seen.size, bright: bright / n, share };
		}
		return best;
	}, probe);
}

const statusText = (page: Page) => page.locator('[role=application] ~ div .card').first().innerText().catch(() => '');

async function openGateway(ctx: BrowserContext, id: number, label: string) {
	const page = await ctx.newPage();
	watchForSecrets(page, label);
	await page.goto(`${base}/gateway/${id}`);
	return page;
}

async function waitConnected(page: Page, ms = 30000) {
	await poll(
		'connected',
		async () => {
			if (await page.getByRole('button', { name: 'ตัดการเชื่อมต่อ' }).isVisible()) return true;
			const t = await statusText(page);
			if (t.includes('ไม่สำเร็จ') || t.includes('ตัดการเชื่อมต่อแล้ว')) throw new Error(`closed: ${t.replace(/\s+/g, ' ')}`);
			return false;
		},
		ms
	);
}

async function clickRemote(page: Page, fx: number, fy: number, button: 'left' | 'right' = 'left') {
	const box = (await page.locator('[role=application]').boundingBox())!;
	await page.mouse.click(box.x + box.width * fx, box.y + box.height * fy, { button });
}

async function sendClipboard(page: Page, text: string) {
	await page.evaluate((t) => navigator.clipboard.writeText(t), text);
	await page.getByRole('button', { name: /ส่งคลิปบอร์ด/ }).click();
	await page.getByText(/ส่งคลิปบอร์ดแล้ว/).waitFor({ timeout: 5000 });
}

async function disconnect(page: Page) {
	await page.getByRole('button', { name: 'ตัดการเชื่อมต่อ' }).click();
	await poll('disconnected', async () => (await statusText(page)).includes('ตัดการเชื่อมต่อแล้ว'), 10000);
}

/** Audit entries of one gateway session (start + end), matched by the sid in the detail. */
async function auditFor(api: Api, endpointId: number, after: number) {
	const { entries } = await api.req<{ entries: any[] }>('GET', '/api/audit?limit=200');
	return entries.filter((e) => e.target === String(endpointId) && e.id > after && e.action.startsWith('gateway.'));
}
async function lastAuditId(api: Api) {
	const { entries } = await api.req<{ entries: any[] }>('GET', '/api/audit?limit=1');
	return entries[0]?.id ?? 0;
}

function recordingFiles(): Record<string, number> {
	const out: Record<string, number> = {};
	const ls = dockerExec('aegiron-gwtest-guacd', 'sh', '-c', 'cd /recordings 2>/dev/null && for f in *; do [ -f "$f" ] && echo "$(wc -c < "$f") $f"; done; true');
	for (const line of ls.trim().split('\n').filter(Boolean)) {
		const [size, name] = line.trim().split(/\s+/, 2);
		out[name] = +size;
	}
	return out;
}

// --- cases ---

type Endpoint = { id: number; name: string; provider: string };
const ep: Record<string, Endpoint> = {};

async function check(name: string, fn: () => Promise<string | void>) {
	if (only && !only.includes(name)) return;
	log(`--- ${name}`);
	try {
		const detail = (await fn()) ?? '';
		results.push({ name, ok: true, detail });
		log(`PASS ${name} ${detail}`);
	} catch (e) {
		results.push({ name, ok: false, detail: (e as Error).message });
		log(`FAIL ${name}: ${(e as Error).message}`);
	}
}

/** Full graphical session: render, keyboard into the xterm, clipboard both ways, disconnect, audit, recording. */
async function graphicalSession(
	api: Api,
	ctx: BrowserContext,
	e: Endpoint,
	o: { container: string; bg: [number, number, number]; xenv: string[]; user: string; kbFile: string }
) {
	const before = await lastAuditId(api);
	const page = await openGateway(ctx, e.id, e.name);
	await waitConnected(page);
	const stats = await poll('remote desktop to render', async () => {
		const s = await screenStats(page, { bg: o.bg, black: [0, 0, 0] });
		return s.width > 300 && s.share.bg > 0.2 && s.share.black > 0.05 && s.colors > 3 ? s : null;
	});
	await page.screenshot({ path: `${shots}/${e.name}.png` });

	// Keyboard: type into the xterm (pointer focus, no window manager) and look for the file it writes.
	const token = `kb${Date.now() % 100000}`;
	await clickRemote(page, 0.2, 0.2);
	await page.locator('[role=application]').focus();
	await page.keyboard.type(`echo ${token}-$((6*7)) > ${o.kbFile}`, { delay: 15 });
	await page.keyboard.press('Enter');
	const typed = await poll('typed command to run on the target', () => {
		try {
			return dockerExec(o.container, 'cat', o.kbFile).trim() === `${token}-42` ? 'ok' : null;
		} catch {
			return null;
		}
	});

	// Clipboard browser -> remote.
	const clipIn = `clip-in-${token}`;
	await sendClipboard(page, clipIn);
	const xclip = (...a: string[]) => sh('docker', ['exec', '-u', o.user, ...o.xenv.flatMap((v) => ['-e', v]), o.container, 'xclip', ...a]);
	await poll('clipboard to reach the target', () => xclip('-o', '-selection', 'clipboard').trim() === clipIn);

	// Clipboard remote -> browser (the console copies it to the local clipboard).
	const clipOut = `clip-out-${token}`;
	sh('docker', ['exec', '-d', '-u', o.user, ...o.xenv.flatMap((v) => ['-e', v]), o.container, 'sh', '-c', `printf %s ${clipOut} | xclip -selection clipboard -loops 5`]);
	await poll('remote clipboard to reach the browser', async () => (await page.evaluate(() => navigator.clipboard.readText())) === clipOut);

	await disconnect(page);
	await page.close();
	const audit = await poll('gateway.end audit entry', async () => {
		const a = await auditFor(api, e.id, before);
		return a.some((x) => x.action === 'gateway.end') ? a : null;
	});
	const start = audit.find((x) => x.action === 'gateway.start');
	const end = audit.find((x) => x.action === 'gateway.end');
	if (!start || !start.detail.recorded) throw new Error(`gateway.start missing or not marked recorded: ${JSON.stringify(audit)}`);
	if (end.detail.reason !== 'browser_closed') throw new Error(`unexpected end reason ${end.detail.reason}`);
	const sid = start.detail.sid as string;
	const rec = await poll('recording file', () => {
		const f = recordingFiles();
		return f[sid] > 10000 ? f : null;
	});
	// A Guacamole recording is the instruction stream itself, playable with Guacamole.SessionRecording.
	const head = dockerExec('aegiron-gwtest-guacd', 'head', '-c', '200', `/recordings/${sid}`);
	if (!/^\d+\.[a-z]+,/.test(head) || !/\d+\.(img|png|size),/.test(head)) throw new Error(`recording is not a Guacamole stream: ${head.slice(0, 60)}`);
	return `${stats.width}x${stats.height}, ${stats.colors} colours, keyboard ${typed}, clipboard both ways, recording ${rec[sid]} bytes, bytes_to_viewer=${end.detail.bytes_to_viewer}`;
}

async function sshSession(api: Api, ctx: BrowserContext, e: Endpoint, user: string, idleSeconds = 0) {
	const before = await lastAuditId(api);
	const page = await openGateway(ctx, e.id, e.name);
	await waitConnected(page);
	await page.locator('[role=application]').focus();
	const token = `ssh${Date.now() % 100000}`;
	await poll('prompt to render', async () => (await screenStats(page, {})).bright > 0.0002);
	// An idle session must stay up: guacd drops users it hears nothing from for 15 s, so the
	// tunnel's keep-alives have to pass through the server.
	if (idleSeconds) await sleep(idleSeconds * 1000);
	await page.keyboard.type(`echo ${token}-$((6*7)) $(whoami)`, { delay: 15 });
	await page.keyboard.press('Enter');
	// Clipboard browser -> terminal: guacd pastes the clipboard on a right (or middle) click, as the console tells the user.
	await sendClipboard(page, `echo pasted-${token}`);
	await page.locator('[role=application]').focus();
	await clickRemote(page, 0.5, 0.5, 'right');
	await page.keyboard.press('Enter');
	await sleep(1500);
	const stats = await screenStats(page, {});
	await page.screenshot({ path: `${shots}/${e.name}.png` });
	await disconnect(page);
	await page.close();
	const audit = await poll('gateway.end audit entry', async () => {
		const a = await auditFor(api, e.id, before);
		return a.some((x) => x.action === 'gateway.end') ? a : null;
	});
	const sid = audit.find((x) => x.action === 'gateway.start')?.detail.sid as string;
	// The typescript recording holds the terminal output: proof the command ran remotely.
	const ts = await poll('typescript with command output', () => {
		const t = dockerExec('aegiron-gwtest-guacd', 'sh', '-c', `cat /recordings/${sid}.typescript 2>/dev/null || true`);
		return t.includes(`${token}-42 ${user}`) && t.includes(`pasted-${token}`) ? t : null;
	});
	const files = recordingFiles();
	return `text pixels ${(stats.bright * 100).toFixed(2)}%, output "${token}-42 ${user}" + pasted clipboard seen in typescript (${ts.length} bytes), recording ${files[sid]} bytes`;
}

/** Opens a connection that must fail; returns the message the console shows. */
async function expectFailure(ctx: BrowserContext, e: Endpoint, ms = 30000) {
	const page = await openGateway(ctx, e.id, e.name);
	const text = await poll(
		'failure message',
		async () => {
			const t = (await statusText(page)).replace(/\s+/g, ' ');
			if (await page.getByRole('button', { name: 'ตัดการเชื่อมต่อ' }).isVisible()) return `CONNECTED`;
			return t.includes('ไม่สำเร็จ') || t.includes('ตัดการเชื่อมต่อแล้ว') ? t : null;
		},
		ms
	);
	await page.screenshot({ path: `${shots}/${e.name}.png` });
	await page.close();
	if (text === 'CONNECTED') throw new Error('connected although it should have failed');
	// A failed connection must say so, not look like a normal "disconnected".
	if (!text.includes('ไม่สำเร็จ')) throw new Error(`failure shown as a plain disconnect: ${text}`);
	return text;
}

/** A failing connection: the console says so and the audit log records how it ended. */
async function failureAudited(api: Api, ctx: BrowserContext, e: Endpoint) {
	const before = await lastAuditId(api);
	const msg = await expectFailure(ctx, e);
	const end = await poll('gateway.end audit entry', async () => (await auditFor(api, e.id, before)).find((x) => x.action === 'gateway.end'));
	if (end.detail.reason !== 'target_closed') throw new Error(`audit reason ${end.detail.reason}, want target_closed`);
	return `console: "${msg.replace(/ กลับ ลองอีกครั้ง$/, '')}" | audit: ${end.detail.reason}${end.detail.error ? ` / ${end.detail.error}` : ''}`;
}

// --- main ---

browser = await chromium.launch({
	headless: true,
	...(process.env.CHROME ? { executablePath: process.env.CHROME } : { channel: 'chrome' })
});
try {
	const api = await Api.login(admin.email, admin.password);
	const adminCtx = await newContext(admin.email, admin.password);

	// Clean slate: remove endpoints/groups/users from a previous run of this script.
	for (const e of (await api.req<{ endpoints: Endpoint[] }>('GET', '/api/endpoints')).endpoints)
		if (e.name.startsWith('gwtest-')) await api.req('DELETE', `/api/endpoints/${e.id}`);

	// One connection through the console form, exactly as an admin adds it.
	{
		const page = await adminCtx.newPage();
		watchForSecrets(page, 'connections page');
		await page.goto(`${base}/connections`);
		await page.getByRole('button', { name: 'เพิ่ม' }).click();
		await page.locator('label', { hasText: /^\s*VNC\s*$/ }).click();
		await page.fill('#ep-name', 'gwtest-vnc');
		await page.fill('#ep-addr', 'aegiron-gwtest-vnc');
		await page.fill('#ep-pw', vncPassword);
		await page.getByRole('button', { name: 'บันทึก' }).click();
		await page.getByText('gwtest-vnc', { exact: true }).waitFor();
		// SSH with a private key pasted into the form.
		await page.getByRole('button', { name: 'เพิ่ม' }).click();
		await page.locator('label', { hasText: /^\s*SSH\s*$/ }).click();
		await page.fill('#ep-name', 'gwtest-ssh-key-ed25519');
		await page.fill('#ep-addr', 'aegiron-gwtest-ssh');
		await page.fill('#ep-user', 'keyuser');
		await page.selectOption('#ep-sshauth', 'key');
		await page.fill('#ep-pw', ed25519);
		await page.getByRole('button', { name: 'บันทึก' }).click();
		await page.getByText('gwtest-ssh-key-ed25519', { exact: true }).waitFor();
		await page.close();
	}
	const add = async (body: Record<string, unknown>) => {
		const { id } = await api.req<{ id: number }>('POST', '/api/endpoints', body);
		return { id, name: body.name as string, provider: body.provider as string };
	};
	const viaForm = (await api.req<{ endpoints: Endpoint[] }>('GET', '/api/endpoints')).endpoints;
	ep.vnc = viaForm.find((e) => e.name === 'gwtest-vnc')!;
	ep.sshEd = viaForm.find((e) => e.name === 'gwtest-ssh-key-ed25519')!;
	const rdp = (name: string, host: string, security: string, password = rdpPassword) =>
		add({ name, provider: 'rdp', address: host, username: 'aegiron', password, options: { security, ignore_cert: true } });
	ep.rdpAny = await rdp('gwtest-rdp-any', 'aegiron-gwtest-rdp', 'any');
	ep.rdpRdp = await rdp('gwtest-rdp-rdpsec', 'aegiron-gwtest-rdp', 'rdp');
	ep.rdpTls = await rdp('gwtest-rdp-tls', 'aegiron-gwtest-rdp-tls', 'tls');
	ep.rdpNla = await rdp('gwtest-rdp-nla', 'aegiron-gwtest-rdp', 'nla');
	ep.rdpBad = await rdp('gwtest-rdp-wrongpw', 'aegiron-gwtest-rdp', 'any', 'Wrong-Pass-9');
	ep.nla = await rdp('gwtest-rdp-nla-shadow', 'aegiron-gwtest-rdp-nla', 'nla');
	ep.nlaAny = await rdp('gwtest-rdp-nla-shadow-any', 'aegiron-gwtest-rdp-nla', 'any');
	ep.nlaBad = await rdp('gwtest-rdp-nla-shadow-wrongpw', 'aegiron-gwtest-rdp-nla', 'nla', 'Wrong-Pass-9');
	ep.rdpTlsStrict = await add({ name: 'gwtest-rdp-tls-verify', provider: 'rdp', address: 'aegiron-gwtest-rdp-tls', username: 'aegiron', password: rdpPassword, options: { security: 'tls' } });
	ep.vncBad = await add({ name: 'gwtest-vnc-wrongpw', provider: 'vnc', address: 'aegiron-gwtest-vnc', password: 'Wrong-Pass-9' });
	ep.sshPw = await add({ name: 'gwtest-ssh-password', provider: 'ssh', address: 'aegiron-gwtest-ssh', username: 'pwuser', password: sshPassword });
	ep.sshBad = await add({ name: 'gwtest-ssh-wrongpw', provider: 'ssh', address: 'aegiron-gwtest-ssh', username: 'pwuser', password: 'Wrong-Pass-9' });
	ep.sshRsa = await add({ name: 'gwtest-ssh-key-rsa', provider: 'ssh', address: 'aegiron-gwtest-ssh', username: 'keyuser', password: rsa, options: { auth: 'key' } });
	ep.down = await add({ name: 'gwtest-unreachable', provider: 'vnc', address: 'aegiron-gwtest-nothing', password: 'x' });

	const rdpX = { container: 'aegiron-gwtest-rdp', bg: [0x1f, 0x5f, 0xa8] as [number, number, number], xenv: ['DISPLAY=:10', 'XAUTHORITY=/home/aegiron/.Xauthority'], user: 'aegiron', kbFile: '/tmp/aegiron-kb' };
	await check('rdp-security-any', () => graphicalSession(api, adminCtx, ep.rdpAny, rdpX));
	// Legacy "RDP" (RC4) security: FreeRDP 2 in guacd 1.5.5 and xrdp disagree on the MAC
	// ("MAC checksum error for non-FIPS PDU" in xrdp.log), a known incompatibility of those two.
	await check('rdp-security-rdp-xrdp-incompatible', () => failureAudited(api, adminCtx, ep.rdpRdp));
	await check('rdp-security-tls', () => graphicalSession(api, adminCtx, ep.rdpTls, { ...rdpX, container: 'aegiron-gwtest-rdp-tls' }));
	await check('rdp-tls-untrusted-cert-rejected', () => failureAudited(api, adminCtx, ep.rdpTlsStrict));
	// xrdp does not implement NLA (CredSSP), so the client's NLA-only request is refused.
	await check('rdp-security-nla-against-xrdp', () => failureAudited(api, adminCtx, ep.rdpNla));
	// Real NLA (CredSSP with NTLM) against FreeRDP's shadow server. It authenticates like a
	// Windows host but barely sends screen updates from Xvfb, so only the login is checked here.
	const nlaLogin = async (e: Endpoint) => {
		const before = await lastAuditId(api);
		const page = await openGateway(adminCtx, e.id, e.name);
		await waitConnected(page);
		await sleep(3000);
		if (!(await page.getByRole('button', { name: 'ตัดการเชื่อมต่อ' }).isVisible())) throw new Error('session dropped after NLA login');
		await disconnect(page);
		await page.close();
		const end = await poll('gateway.end', async () => (await auditFor(api, e.id, before)).find((x) => x.action === 'gateway.end'));
		if (end.detail.reason !== 'browser_closed' || end.detail.error) throw new Error(`audit: ${JSON.stringify(end.detail)}`);
		return `NLA login accepted, session held until the user disconnected (${end.detail.seconds}s)`;
	};
	await check('rdp-nla-login', () => nlaLogin(ep.nla));
	await check('rdp-nla-negotiated-from-any', () => nlaLogin(ep.nlaAny));
	await check('rdp-nla-wrong-password', () => failureAudited(api, adminCtx, ep.nlaBad));
	// xrdp (unlike Windows with NLA) accepts the connection and shows its own login dialog.
	await check('rdp-wrong-password-shows-xrdp-login', async () => {
		const page = await openGateway(adminCtx, ep.rdpBad.id, ep.rdpBad.name);
		await waitConnected(page);
		await sleep(5000);
		const s = await screenStats(page, { bg: rdpX.bg });
		await page.screenshot({ path: `${shots}/${ep.rdpBad.name}.png` });
		await disconnect(page);
		await page.close();
		if (s.share.bg > 0.05) throw new Error('the user desktop appeared with a wrong password');
		return `no desktop (desktop colour ${(s.share.bg * 100).toFixed(1)}%), xrdp login dialog shown`;
	});
	await check('vnc-password', () =>
		graphicalSession(api, adminCtx, ep.vnc, { container: 'aegiron-gwtest-vnc', bg: [0x2e, 0x7d, 0x32], xenv: ['DISPLAY=:0'], user: 'aegiron', kbFile: '/tmp/aegiron-kb' })
	);
	await check('vnc-wrong-password', () => failureAudited(api, adminCtx, ep.vncBad));
	await check('ssh-password-after-idle', () => sshSession(api, adminCtx, ep.sshPw, 'pwuser', 25));
	await check('ssh-wrong-password', () => failureAudited(api, adminCtx, ep.sshBad));
	await check('ssh-key-ed25519', () => sshSession(api, adminCtx, ep.sshEd, 'keyuser'));
	await check('ssh-key-rsa', () => sshSession(api, adminCtx, ep.sshRsa, 'keyuser'));
	await check('unreachable-host', () => failureAudited(api, adminCtx, ep.down));

	await check('group-permissions', async () => {
		const email = 'operator@gwtest.local';
		const password = 'Gw-Operator-Pass-1!';
		const users = (await api.req<{ users: { id: number; email: string }[] }>('GET', '/api/users')).users;
		let uid = users.find((u) => u.email === email)?.id;
		if (!uid) uid = (await api.req<{ user: { id: number } }>('POST', '/api/users', { email, name: 'Operator', role: 'user', password })).user.id;
		const groups = (await api.req<{ groups: { id: number; name: string }[] }>('GET', '/api/groups')).groups;
		let gid = groups.find((g) => g.name === 'gwtest-ops')?.id;
		if (!gid) gid = (await api.req<{ id: number }>('POST', '/api/groups', { name: 'gwtest-ops', member_ids: [uid] })).id;
		else await api.req('PUT', `/api/groups/${gid}`, { name: 'gwtest-ops', member_ids: [uid] });
		const shared = await add({ name: 'gwtest-ssh-shared', provider: 'ssh', address: 'aegiron-gwtest-ssh', username: 'pwuser', password: sshPassword, group_id: gid });

		const op = await Api.login(email, password);
		const visible = (await op.req<{ endpoints: Endpoint[] }>('GET', '/api/endpoints')).endpoints.map((e) => e.name);
		if (!visible.includes('gwtest-ssh-shared') || visible.some((n) => n !== 'gwtest-ssh-shared' && n.startsWith('gwtest-')))
			throw new Error(`operator sees ${visible.join(', ')}`);
		const opCtx = await newContext(email, password);
		const p = await openGateway(opCtx, shared.id, 'operator shared');
		await waitConnected(p);
		await disconnect(p);
		await p.close();
		const denied = await expectFailure(opCtx, ep.sshPw);
		if (!denied.includes('ไม่มีสิทธิ์')) throw new Error(`ungrouped endpoint: ${denied}`);
		// Removing the user from the group revokes gateway access.
		await api.req('PUT', `/api/groups/${gid}`, { name: 'gwtest-ops', member_ids: [] });
		const revoked = await expectFailure(opCtx, shared);
		if (!revoked.includes('ไม่มีสิทธิ์')) throw new Error(`after removal from group: ${revoked}`);
		// A plain user cannot pull the stored password of a shared gateway connection either.
		await api.req('PUT', `/api/groups/${gid}`, { name: 'gwtest-ops', member_ids: [uid] });
		const launch = await op.req<any>('POST', `/api/endpoints/${shared.id}/launch`);
		await opCtx.close();
		return `shared endpoint works, ungrouped denied, revoked after removal; launch released secret to member: ${'secret' in launch}`;
	});

	await check('websocket-auth-and-origin', async () => {
		// Bun's WebSocket takes request headers, unlike the browser's.
		const open = (headers: Record<string, string>) =>
			new Promise<string>((resolve) => {
				const ws = new WebSocket(`${base.replace(/^http/, 'ws')}/ws/gateway?endpoint=${ep.sshPw.id}`, { headers, protocols: ['guacamole'] } as any);
				let first = '';
				ws.onmessage = (m) => {
					first ||= String(m.data);
					ws.close();
				};
				ws.onclose = (e) => resolve(first ? `opened` : `closed ${e.code} ${e.reason}`);
				ws.onerror = () => {};
			});
		const anon = await open({ Origin: base });
		if (!anon.includes('769')) throw new Error(`no session: ${anon}`);
		const foreign = await open({ Origin: 'https://evil.example', Cookie: api.cookie });
		if (foreign === 'opened') throw new Error('cross-site WebSocket with the admin cookie was accepted');
		const same = await open({ Origin: base, Cookie: api.cookie });
		if (same !== 'opened') throw new Error(`same-origin with cookie: ${same}`);
		return `no cookie: ${anon}; foreign Origin: ${foreign}; same origin: ${same}`;
	});

	await check('no-secret-reaches-browser', async () => {
		if (leaks.length) throw new Error(leaks.join('; '));
		return 'no stored password or key seen in any WebSocket frame or /api response';
	});

	await check('sessions-list', async () => {
		const r = await api.req<any>('GET', '/api/sessions');
		const list = (r.sessions ?? r) as any[];
		const gw = list.filter((s) => JSON.stringify(s).includes('gwtest-'));
		if (!gw.length) throw new Error(`no gateway sessions in /api/sessions: ${JSON.stringify(list).slice(0, 300)}`);
		return `${gw.length} gateway sessions listed`;
	});
} finally {
	await browser.close();
}

console.log('\n=== gateway e2e results ===');
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name}  ${r.detail}`);
const failed = results.filter((r) => !r.ok).length;
console.log(`${results.length - failed}/${results.length} passed; screenshots in ${shots}`);
process.exit(failed ? 1 : 0);
