// End-to-end test of "remember this device for 30 days" (docs/PROTOCOL.md §5, Trusted devices)
// in a real headless Chrome against a running server, plus the desktop/CLI sign-in when an agent
// binary is given:
//
//   1. a user with 2FA signs in (password, then TOTP with "จดจำอุปกรณ์นี้ 30 วัน" ticked)
//   2. after signing out, the same browser signs in with the password only
//   3. an admin revokes that browser from the Users page
//   4. the browser is asked for a TOTP code again (and the server tells it to drop its cookie)
//   5. `aegiron-agent login --remember-device` (AEGIRON_AGENT_BIN), sign-in again without a code,
//      admin revoke, then the code is required again
//   6. the audit log has every step
//
//   AEGIRON_URL=http://127.0.0.1:21200 AEGIRON_PASSWORD=<admin password> \
//   [AEGIRON_AGENT_BIN=../agent/target/release/aegiron-agent] bun scripts/e2e-auth.ts
//
// Creates the users e2e-web@aegiron.test and e2e-app@aegiron.test (2FA is reset and re-enabled
// on every run). Only run it against a test server.

import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Page } from 'puppeteer-core';
import { adminEmail, adminPassword, Api, base, check, done, launchChrome, sleep, totp, userWithTotp, waitFor } from './e2e-lib';

const userPassword = 'e2e-user-password-1';
const admin = await Api.login(adminEmail, adminPassword());

/** A TOTP code that stays valid for the next few seconds (never reuses one already sent). */
let lastCode = '';
async function freshCode(secret: string): Promise<string> {
	for (;;) {
		const left = 30_000 - (Date.now() % 30_000);
		const code = await totp(secret);
		if (code !== lastCode && left > 4000) return (lastCode = code);
		await sleep(left + 200);
	}
}

// --- Web console -----------------------------------------------------------------------------

const webEmail = 'e2e-web@aegiron.test';
const webSecret = await userWithTotp(admin, webEmail, userPassword);
const webUser = (await admin.get<{ users: any[] }>('/api/users')).users.find((u) => u.email === webEmail);

const browser = await launchChrome();
try {
	const ctx = await browser.createBrowserContext();
	const page = await ctx.newPage();
	page.on('dialog', (d) => d.accept());

	async function signIn(p: Page, email: string, password: string) {
		await p.goto(`${base}/login`, { waitUntil: 'networkidle0' });
		await p.locator('#email').fill(email);
		await p.locator('#password').fill(password);
		await p.locator('form button.btn-primary').click();
		// Either the TOTP step or the console.
		return waitFor(async () => ((await p.$('#totp')) ? 'totp' : !p.url().includes('/login') ? 'in' : ''), 10_000);
	}
	async function signOut(p: Page) {
		await p.evaluate(() => fetch('/api/auth/logout', { method: 'POST', headers: { 'X-Aegiron': '1' } }));
	}
	const trustCookie = async () => (await ctx.cookies()).find((c) => c.name === 'aegiron_trust');

	let step = await signIn(page, webEmail, userPassword);
	check(step === 'totp', 'web: password alone asks for the TOTP code', step);
	await page.locator('#totp').fill(await freshCode(webSecret));
	await page.locator('form input[type=checkbox]').click();
	await page.locator('form button.btn-primary').click();
	const inConsole = await waitFor(() => !page.url().includes('/login'), 10_000);
	check(!!inConsole, 'web: TOTP + remember signs in', page.url());
	const cookie = await trustCookie();
	check(!!cookie && cookie.httpOnly && cookie.path === '/api/auth' && cookie.sameSite === 'Strict', 'web: trust cookie is HttpOnly, SameSite=Strict, path /api/auth', cookie && { path: cookie.path, httpOnly: cookie.httpOnly, sameSite: cookie.sameSite });
	const days = cookie ? (cookie.expires * 1000 - Date.now()) / 86_400_000 : 0;
	check(days > 29.9 && days <= 30, 'web: trust cookie lasts 30 days', days.toFixed(2));

	// The account page lists this browser as the current one.
	await page.goto(`${base}/account`, { waitUntil: 'networkidle0' });
	const mine = await page.evaluate(() => fetch('/api/auth/trusted-devices').then((r) => r.json()));
	check(mine.devices?.length === 1 && mine.devices[0].current === true && mine.devices[0].label === 'Chrome บน macOS', 'web: account lists this browser as current', mine.devices?.map((d: any) => [d.label, d.current]));
	const shown = await page.evaluate(() => document.body.innerText.includes('Chrome บน macOS'));
	check(shown, 'web: account page shows the remembered browser');

	await signOut(page);
	step = await signIn(page, webEmail, userPassword);
	check(step === 'in', 'web: remembered browser signs in with the password only', step);
	await signOut(page);

	// A wrong password with a valid trust cookie still fails.
	const wrong = await page.evaluate(
		(email) =>
			fetch('/api/auth/login', { method: 'POST', headers: { 'X-Aegiron': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ email, password: 'not-the-password' }) }).then((r) => r.status),
		webEmail
	);
	check(wrong === 401, 'web: trust cookie never replaces the password', wrong);

	// Admin revokes it from the Users page.
	const adminCtx = await browser.createBrowserContext();
	const ap = await adminCtx.newPage();
	ap.on('dialog', (d) => d.accept());
	step = await signIn(ap, adminEmail, adminPassword());
	check(step === 'in', 'web: admin signs in', step);
	await ap.goto(`${base}/users`, { waitUntil: 'networkidle0' });
	const row = await waitFor(() => ap.$(`xpath/.//tr[.//p[text()="${webEmail}"]]//button`), 5000);
	await row!.click();
	const listed = await waitFor(() => ap.evaluate(() => document.body.innerText.includes('Chrome บน macOS')), 5000);
	check(!!listed, 'web: admin sees the remembered browser in the user dialog');
	const revokeBtn = await ap.$('xpath/.//li[.//span[text()="Chrome บน macOS"]]//button');
	await revokeBtn!.click();
	const gone = await waitFor(() => ap.evaluate(() => !document.body.innerText.includes('Chrome บน macOS')), 5000);
	check(!!gone, 'web: admin revoke removes it from the list');
	const left = await admin.get<{ devices: any[] }>(`/api/users/${webUser.id}/trusted-devices`);
	check(left.devices.length === 0, 'web: no remembered device left on the server', left.devices.length);

	step = await signIn(page, webEmail, userPassword);
	check(step === 'totp', 'web: revoked browser must enter the TOTP code again', step);
	check(!(await trustCookie()), 'web: server cleared the revoked trust cookie');
	await page.locator('#totp').fill(await freshCode(webSecret));
	await page.locator('form button.btn-primary').click();
	check(!!(await waitFor(() => !page.url().includes('/login'), 10_000)), 'web: TOTP without remember signs in');
	check(!(await trustCookie()), 'web: no trust cookie without the checkbox');
	await ctx.close();
	await adminCtx.close();
} finally {
	await browser.close();
}

const webAudit = async (action: string) => (await admin.audit(action)).filter((e) => e.target === webEmail || e.actor_email === webEmail);
check((await webAudit('auth.trusted_device_added')).length >= 1, 'audit: auth.trusted_device_added');
const logins = await webAudit('auth.login');
check(logins.some((e) => e.detail?.trusted_device_added) && logins.some((e) => e.detail?.trusted_device), 'audit: auth.login with trusted_device_added and with trusted_device', logins.map((e) => e.detail));
check((await webAudit('auth.trusted_device_rejected')).length >= 1, 'audit: auth.trusted_device_rejected after revoke');
const revoked = (await admin.audit('user.trusted_device_revoked')).find((e) => e.target === webEmail);
check(revoked?.detail?.by === 'admin' && revoked?.detail?.label === 'Chrome บน macOS', 'audit: user.trusted_device_revoked by admin', revoked?.detail);

// --- Desktop app / CLI -----------------------------------------------------------------------

const bin = process.env.AEGIRON_AGENT_BIN;
if (bin) {
	const appEmail = 'e2e-app@aegiron.test';
	const appSecret = await userWithTotp(admin, appEmail, userPassword);
	const appUser = (await admin.get<{ users: any[] }>('/api/users')).users.find((u) => u.email === appEmail);
	const dir = mkdtempSync(join(tmpdir(), 'aegiron-e2e-auth-'));
	const config = join(dir, 'agent.toml');
	const login = async (extra: string[], code = '') => {
		const p = Bun.spawn([bin, '--config', config, 'login', '--server', base, '--email', appEmail, ...extra], {
			env: { ...process.env, AEGIRON_LOGIN_PASSWORD: userPassword, AEGIRON_LOGIN_TOTP: code || 'no-code-given' },
			stdin: 'ignore',
			stdout: 'pipe',
			stderr: 'pipe'
		});
		const [out, err] = [await new Response(p.stdout).text(), await new Response(p.stderr).text()];
		return { code: await p.exited, out: out + err };
	};
	try {
		let r = await login(['--remember-device'], await freshCode(appSecret));
		check(r.code === 0 && /Signed in as/.test(r.out) && !/2FA skipped/.test(r.out), 'app: login --remember-device with TOTP', r.out.trim());
		check(readFileSync(config, 'utf8').includes('[[trusted_logins]]'), 'app: trust token saved in the config');
		r = await login([]);
		check(r.code === 0 && /2FA skipped: remembered device/.test(r.out), 'app: second login skips 2FA', r.out.trim());
		const devs = (await admin.get<{ devices: any[] }>(`/api/users/${appUser.id}/trusted-devices`)).devices;
		check(devs.length === 1 && devs[0].label.startsWith('แอป Aegiron: '), 'app: listed for the admin as the desktop app', devs.map((d) => d.label));
		await admin.del(`/api/users/${appUser.id}/trusted-devices`);
		r = await login([]);
		check(r.code !== 0 && /2FA|authentication code/.test(r.out), 'app: after admin revoke a code is required again', r.out.trim());
		check(!readFileSync(config, 'utf8').includes('[[trusted_logins]]'), 'app: rejected trust token forgotten');
		r = await login([], await freshCode(appSecret));
		check(r.code === 0 && !/2FA skipped/.test(r.out), 'app: login with a code works after revoke', r.out.trim());
		const appLogins = (await admin.audit('auth.agent_login')).filter((e) => e.target === appEmail || e.actor_email === appEmail);
		check(appLogins.some((e) => e.detail?.trusted_device_added) && appLogins.some((e) => e.detail?.trusted_device), 'audit: auth.agent_login with trusted_device_added and trusted_device', appLogins.map((e) => e.detail));
		const all = (await admin.audit('user.trusted_devices_revoked')).find((e) => e.target === appEmail);
		check(all?.detail?.reason === 'admin' && all?.detail?.count === 1, 'audit: user.trusted_devices_revoked by admin', all?.detail);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
} else {
	console.log('skip  app sign-in (set AEGIRON_AGENT_BIN)');
}

done();
