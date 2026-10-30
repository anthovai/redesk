// Helpers shared by the end-to-end scripts (docs/TESTING.md): API calls with a session cookie,
// TOTP codes, and a headless Chrome driven by puppeteer-core.

import puppeteer, { type Browser } from 'puppeteer-core';

export const base = process.env.AEGIRON_URL ?? 'http://127.0.0.1:21114';
export const adminEmail = process.env.AEGIRON_EMAIL ?? 'admin@aegiron.local';

export function adminPassword(): string {
	const pw = process.env.AEGIRON_PASSWORD;
	if (!pw) throw new Error('set AEGIRON_PASSWORD (the admin password of the test server)');
	return pw;
}

let failures = 0;

/** Prints a check result; failures are counted and make `done()` exit non-zero. */
export function check(ok: boolean, what: string, detail?: unknown) {
	if (!ok) failures++;
	console.log(`${ok ? 'PASS' : 'FAIL'} ${what}${detail === undefined ? '' : ` — ${typeof detail === 'string' ? detail : JSON.stringify(detail)}`}`);
}

export function done(): never {
	console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
	process.exit(failures ? 1 : 0);
}

/** A logged-in API client (cookie session, CSRF header). */
export class Api {
	constructor(public cookie: string) {}

	static async login(email: string, password: string, totp = ''): Promise<Api> {
		const r = await fetch(`${base}/api/auth/login`, {
			method: 'POST',
			headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json' },
			body: JSON.stringify({ email, password, totp })
		});
		const body = await r.json();
		if (!r.ok || !body.user) throw new Error(`login ${email}: ${r.status} ${JSON.stringify(body)}`);
		const cookie = r.headers
			.getSetCookie()
			.map((c) => c.split(';')[0])
			.find((c) => c.startsWith('aegiron_session='));
		if (!cookie) throw new Error('login: no session cookie');
		return new Api(cookie);
	}

	async req<T = any>(method: string, path: string, body?: unknown): Promise<T> {
		const r = await fetch(`${base}${path}`, {
			method,
			headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json', Cookie: this.cookie },
			body: body === undefined ? undefined : JSON.stringify(body)
		});
		const text = await r.text();
		if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text}`);
		return (text ? JSON.parse(text) : null) as T;
	}

	get = <T = any>(path: string) => this.req<T>('GET', path);
	post = <T = any>(path: string, body?: unknown) => this.req<T>('POST', path, body ?? {});
	put = <T = any>(path: string, body?: unknown) => this.req<T>('PUT', path, body ?? {});
	del = <T = any>(path: string) => this.req<T>('DELETE', path);

	/** Audit entries with this action, newest first. */
	async audit(action: string): Promise<any[]> {
		const r = await this.get<{ entries?: any[]; items?: any[] }>(`/api/audit?action=${encodeURIComponent(action)}&limit=200`);
		const list = (r.entries ?? r.items ?? (Array.isArray(r) ? r : [])) as any[];
		return list.filter((e) => e.action === action);
	}
}

function base32(s: string): Uint8Array {
	const alpha = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
	let bits = 0;
	let value = 0;
	const out: number[] = [];
	for (const c of s.replace(/=+$/, '').toUpperCase()) {
		value = (value << 5) | alpha.indexOf(c);
		bits += 5;
		if (bits >= 8) {
			out.push((value >>> (bits - 8)) & 0xff);
			bits -= 8;
		}
	}
	return new Uint8Array(out);
}

/** RFC 6238 code (SHA-1, 30 s, 6 digits) for `secret` at `at` (ms). */
export async function totp(secret: string, at = Date.now()): Promise<string> {
	const counter = new Uint8Array(8);
	new DataView(counter.buffer).setBigUint64(0, BigInt(Math.floor(at / 30_000)));
	const key = await crypto.subtle.importKey('raw', base32(secret), { name: 'HMAC', hash: 'SHA-1' }, false, ['sign']);
	const h = new Uint8Array(await crypto.subtle.sign('HMAC', key, counter));
	const o = h[h.length - 1] & 0xf;
	const n = ((h[o] & 0x7f) << 24) | (h[o + 1] << 16) | (h[o + 2] << 8) | h[o + 3];
	return String(n % 1_000_000).padStart(6, '0');
}

/** Creates (or reuses) a user with 2FA enabled; returns its TOTP secret. */
export async function userWithTotp(admin: Api, email: string, password: string): Promise<string> {
	const users = (await admin.get<{ users: any[] }>('/api/users')).users;
	let u = users.find((x) => x.email === email);
	if (!u) u = (await admin.post<{ user: any }>('/api/users', { email, name: email.split('@')[0], role: 'user', password })).user ?? null;
	if (u?.id && u.totp_enabled) await admin.del(`/api/users/${u.id}/totp`);
	const me = await Api.login(email, password);
	const { secret } = await me.post<{ secret: string }>('/api/auth/totp/setup');
	await me.post('/api/auth/totp/enable', { code: await totp(secret) });
	return secret;
}

export function chromePath(): string {
	return process.env.CHROME_PATH ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
}

/**
 * Headless Chrome with WebCodecs and autoplay allowed (no user gesture in automation), and a
 * fake microphone (a beep) granted without a prompt for voice calls.
 */
export async function launchChrome(): Promise<Browser> {
	return puppeteer.launch({
		executablePath: chromePath(),
		headless: process.env.HEADFUL ? false : true,
		args: [
			'--autoplay-policy=no-user-gesture-required',
			'--no-first-run',
			'--no-default-browser-check',
			'--mute-audio',
			'--use-fake-device-for-media-stream',
			'--use-fake-ui-for-media-stream'
		]
	});
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Polls `fn` until it returns a truthy value or `ms` passes. */
export async function waitFor<T>(fn: () => Promise<T> | T, ms = 10_000, step = 200): Promise<T | undefined> {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		const v = await fn();
		if (v) return v;
		await sleep(step);
	}
	return undefined;
}
