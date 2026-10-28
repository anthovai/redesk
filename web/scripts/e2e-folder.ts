// Headless-browser test of folder transfer through the real web console (Chromium + Playwright):
// logs in, connects to a device, uploads a local folder into a remote directory with the
// folder picker, then downloads a remote folder as .zip.
//
//   bun scripts/e2e-folder.ts <device-id> <device-password> <local-folder> <remote-dir> <remote-folder> <zip-out>
//
// <remote-folder> is the name of a folder inside <remote-dir> (it may be the one just uploaded).
// Env: AEGIRON_URL (default http://localhost:21114), AEGIRON_EMAIL, AEGIRON_PASSWORD,
//      PLAYWRIGHT_CORE (path to an installed playwright-core; default: resolve "playwright-core"),
//      CHROMIUM (browser binary, default: the cached headless shell of Playwright).
// playwright-core is not a dependency of the web app; install it anywhere, e.g.
//   mkdir /tmp/pw && cd /tmp/pw && bun add playwright-core

import { homedir } from 'node:os';

const base = process.env.AEGIRON_URL ?? 'http://localhost:21114';
const [deviceId, devicePassword, localFolder, remoteDir, remoteFolder, zipOut] = process.argv.slice(2);
if (!zipOut) throw new Error('usage: e2e-folder.ts <device-id> <device-password> <local-folder> <remote-dir> <remote-folder> <zip-out>');

const pwPath = process.env.PLAYWRIGHT_CORE ?? 'playwright-core';
const { chromium } = await import(pwPath);
const executablePath =
	process.env.CHROMIUM ?? `${homedir()}/Library/Caches/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-mac-arm64/chrome-headless-shell`;

const browser = await chromium.launch({ executablePath, headless: true });
const step = (s: string) => console.log(`[e2e] ${s}`);
try {
	const page = await browser.newPage({ acceptDownloads: true, viewport: { width: 1400, height: 900 } });
	page.on('console', (m: any) => m.type() === 'error' && console.log('[browser]', m.text()));
	page.on('pageerror', (e: Error) => console.log('[pageerror]', e.message));

	step('login');
	await page.goto(`${base}/login`);
	await page.fill('#email', process.env.AEGIRON_EMAIL ?? 'admin@aegiron.local');
	await page.fill('#password', process.env.AEGIRON_PASSWORD ?? '');
	await page.press('#password', 'Enter');
	await page.waitForURL((u: URL) => !u.pathname.startsWith('/login'), { timeout: 15_000 });

	step(`connect to ${deviceId}`);
	await page.goto(`${base}/connect/${deviceId}`);
	await page.fill('input[placeholder="รหัสผ่าน"]', devicePassword);
	await page.press('input[placeholder="รหัสผ่าน"]', 'Enter');
	const commands = page.locator('button[title="คำสั่ง"]');
	await commands.waitFor({ timeout: 30_000 });

	step('open file panel');
	await commands.click();
	await page.getByRole('menuitemcheckbox', { name: /โอนไฟล์/ }).click();
	const panel = page.getByTestId('file-panel');
	await panel.waitFor();
	await panel.locator('[data-testid=folder-input]').waitFor({ state: 'attached', timeout: 10_000 });

	step(`go to ${remoteDir}`);
	page.once('dialog', (d: any) => d.accept(remoteDir));
	await panel.getByRole('button', { name: 'ไปยังตำแหน่ง' }).click();
	await page.waitForFunction(
		(dir: string) => document.querySelector('[data-testid=file-path]')?.getAttribute('data-path') === dir,
		remoteDir.replace(/\/$/, ''),
		{ timeout: 10_000 }
	);

	step(`upload folder ${localFolder}`);
	await panel.locator('[data-testid=folder-input]').setInputFiles(localFolder);
	const up = panel.locator('[data-testid=transfer][data-direction=up]').last();
	await up.waitFor();
	await page.waitForFunction(
		() => {
			const els = document.querySelectorAll('[data-testid=transfer][data-direction=up]');
			const s = els[els.length - 1]?.getAttribute('data-status');
			return s === 'done' || s === 'error';
		},
		undefined,
		{ timeout: 120_000 }
	);
	const upStatus = await up.getAttribute('data-status');
	step(`upload ${upStatus}: ${(await up.innerText()).replace(/\s+/g, ' ')}`);
	if (upStatus !== 'done') throw new Error('folder upload failed');

	step(`download folder ${remoteFolder}`);
	const row = panel.locator(`[data-testid=file-row][data-name="${remoteFolder}"]`);
	await row.waitFor({ timeout: 10_000 });
	await row.hover();
	const [download] = await Promise.all([page.waitForEvent('download', { timeout: 120_000 }), row.getByRole('button', { name: 'ดาวน์โหลด' }).click()]);
	await download.saveAs(zipOut);
	const down = panel.locator('[data-testid=transfer][data-direction=down]').last();
	step(`download saved to ${zipOut} (${download.suggestedFilename()}): ${(await down.innerText()).replace(/\s+/g, ' ')}`);

	step('new folder, rename, delete');
	const rowNamed = (name: string) => panel.locator(`[data-testid=file-row][data-name="${name}"]`);
	page.once('dialog', (d: any) => d.accept('e2e-folder'));
	await panel.getByRole('button', { name: 'สร้างโฟลเดอร์ใหม่' }).click();
	await rowNamed('e2e-folder').waitFor({ timeout: 10_000 });
	await rowNamed('e2e-folder').hover();
	page.once('dialog', (d: any) => d.accept('e2e-renamed'));
	await rowNamed('e2e-folder').getByRole('button', { name: 'เปลี่ยนชื่อ' }).click();
	await rowNamed('e2e-renamed').waitFor({ timeout: 10_000 });
	await rowNamed('e2e-renamed').hover();
	page.once('dialog', (d: any) => d.accept());
	await rowNamed('e2e-renamed').getByRole('button', { name: 'ลบ' }).click();
	await rowNamed('e2e-renamed').waitFor({ state: 'detached', timeout: 10_000 });
	step('file operations ok');

	await page.screenshot({ path: zipOut.replace(/\.zip$/, '') + '.png' });
	step('ok');
} finally {
	await browser.close();
}
