// End-to-end media test in a real headless Chrome against a running server and agent
// (docs/TESTING.md):
//
//   1. live browser viewer: asks for AV1 (remembered choice), must get H.264 from an agent
//      without an AV1 encoder, decode frames with WebCodecs, show H264 and AV1 as unavailable,
//      decode the remote audio (WebCodecs AudioDecoder) and stop/resume it with mute
//   2. the session is recorded (policy "all") and uploaded with its audio
//   3. the recording plays in the console with video and audio
//   4. synthetic recordings (examples/make_recording.rs): AV1 + Opus, and AV1 switching to
//      H.264 halfway; checks what is on screen and in the speakers at each moment, that audio
//      and video stay in sync, and that seeking (forward, back, across the codec switch, while
//      paused) shows the right frame and restarts audio in sync
//   With AEGIRON_EXPECT_CM=1 (agent run with AEGIRON_TEST_CM=auto), part 1 also chats and makes
//   a voice call with Chrome's fake microphone: the agent echoes the message and reports the
//   voice packets it received.
//
//   AEGIRON_URL=http://127.0.0.1:21200 AEGIRON_PASSWORD=<admin password> \
//   AEGIRON_AGENT_CONFIG=<agent.toml> AEGIRON_E2E_DB=<postgres URL of the test server> \
//   [AEGIRON_MAKE_RECORDING=../agent/target/release/examples/make_recording] \
//   bun scripts/e2e-media.ts <device-id> <device-password>
//
// The agent should run with AEGIRON_TEST_AUDIO=tone (or play something) so the session has
// sound. Part 4 needs AEGIRON_MAKE_RECORDING, AEGIRON_AGENT_CONFIG and AEGIRON_E2E_DB: the
// pending recording row an upload needs is inserted directly, since the token for a real
// session only ever reaches the agent.

import { createHash, randomBytes } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Browser, Page } from 'puppeteer-core';
import { adminEmail, adminPassword, Api, base, check, done, launchChrome, sleep, waitFor } from './e2e-lib';

const [deviceId, devicePassword] = process.argv.slice(2);
if (!deviceId || !devicePassword) throw new Error('usage: e2e-media.ts <device-id> <device-password>');
const admin = await Api.login(adminEmail, adminPassword());

// Instrumentation injected into every page: what the decoders produce and when audio is due.
declare global {
	interface Window {
		__e2e: {
			videoConfigs: string[];
			videoFrames: number;
			draws: { at: number; idx: number }[];
			audioOut: { ts: number; rms: number; ch: number }[];
			starts: { ts: number | null; rms: number; heard: number; rate: number }[];
		};
	}
}
function instrument() {
	const E = (window.__e2e = { videoConfigs: [], videoFrames: 0, draws: [], audioOut: [], starts: [] });
	const VD = window.VideoDecoder;
	window.VideoDecoder = class extends VD {
		constructor(init: VideoDecoderInit) {
			super({ error: init.error, output: (f) => (E.videoFrames++, init.output(f)) });
		}
		configure(c: VideoDecoderConfig) {
			E.videoConfigs.push(c.codec);
			return super.configure(c);
		}
	} as typeof VideoDecoder;
	let cur: { ts: number; rms: number; ch: number } | null = null;
	const AD = window.AudioDecoder;
	window.AudioDecoder = class extends AD {
		constructor(init: AudioDecoderInit) {
			super({
				error: init.error,
				output: (d) => {
					try {
						const p = new Float32Array(d.numberOfFrames);
						d.copyTo(p, { planeIndex: 0, format: 'f32-planar' });
						let s = 0;
						for (const v of p) s += v * v;
						cur = { ts: d.timestamp, rms: Math.sqrt(s / Math.max(1, p.length)), ch: d.numberOfChannels };
						E.audioOut.push(cur);
					} catch {
						cur = null;
					}
					init.output(d);
					cur = null;
				}
			});
		}
	} as typeof AudioDecoder;
	const start = AudioBufferSourceNode.prototype.start;
	AudioBufferSourceNode.prototype.start = function (this: AudioBufferSourceNode, when = 0, offset = 0, ...rest: number[]) {
		const ctx = this.context as AudioContext;
		// When this block is heard, in performance.now() time.
		const heard = performance.now() + (Math.max(when, ctx.currentTime) - ctx.currentTime + (ctx.outputLatency || 0)) * 1000;
		const rate = this.playbackRate.value;
		E.starts.push({ ts: cur ? cur.ts / 1000 + (offset * 1000) / 1 : null, rms: cur?.rms ?? -1, heard, rate });
		return start.call(this, when, offset, ...rest);
	};
	// Recording frames (320×240, examples/make_recording.rs) carry their number in base 4.
	const draw = CanvasRenderingContext2D.prototype.drawImage as (...a: unknown[]) => void;
	CanvasRenderingContext2D.prototype.drawImage = function (this: CanvasRenderingContext2D, ...a: unknown[]) {
		draw.apply(this, a);
		if (typeof VideoFrame !== 'undefined' && a[0] instanceof VideoFrame && this.canvas.width === 320 && this.canvas.height === 240) {
			const levels = [20, 90, 160, 230];
			let idx = 0;
			[
				[80, 60],
				[240, 60],
				[80, 180],
				[240, 180]
			].forEach(([x, y], q) => {
				const v = this.getImageData(x, y, 1, 1).data[1];
				const d = levels.reduce((best, l, i) => (Math.abs(l - v) < Math.abs(levels[best] - v) ? i : best), 0);
				idx += d << (2 * q);
			});
			E.draws.push({ at: performance.now(), idx });
		}
	} as typeof CanvasRenderingContext2D.prototype.drawImage;
}

async function newPage(browser: Browser, prefs: Record<string, string> = {}): Promise<Page> {
	const ctx = await browser.createBrowserContext();
	const page = await ctx.newPage();
	page.on('dialog', (d) => d.accept());
	page.on('console', (m) => (m.type() === 'warn' || m.type() === 'error') && console.log(`  [browser ${m.type()}] ${m.text()}`));
	await page.evaluateOnNewDocument(instrument);
	await page.evaluateOnNewDocument((p: Record<string, string>) => Object.entries(p).forEach(([k, v]) => localStorage.setItem(k, v)), prefs);
	// The admin's API session (sign-ins are rate limited, so not one per page).
	const [name, value] = admin.cookie.split('=');
	await page.setCookie({ name, value, url: base, httpOnly: true, sameSite: 'Strict' });
	return page;
}

const e2e = (page: Page) => page.evaluate(() => JSON.parse(JSON.stringify(window.__e2e)) as Window['__e2e']);
const median = (v: number[]) => (v.length ? [...v].sort((a, b) => a - b)[Math.floor(v.length / 2)] : NaN);

const settingsBefore = (await admin.get<{ settings: any }>('/api/recordings/settings')).settings;
await admin.put('/api/recordings/settings', { mode: 'all', group_ids: [], retention_days: 0 });
const browser = await launchChrome();
try {
	// --- 1. Live viewer --------------------------------------------------------------------------
	const startedAt = Date.now();
	const page = await newPage(browser, { 'aegiron.viewer.codec': 'av1', 'aegiron.viewer.monitor': '1' });
	await page.goto(`${base}/connect/${deviceId}`, { waitUntil: 'networkidle0' });
	await page.locator('input[type=password]').fill(devicePassword);
	await page.locator('form button.btn-primary').click();
	const live = await waitFor(async () => (await e2e(page)).videoFrames > 5, 20_000);
	check(!!live, 'live: video frames decoded with WebCodecs', (await e2e(page)).videoFrames);
	await sleep(4000);
	let m = await e2e(page);
	check(m.videoConfigs.length > 0 && m.videoConfigs.every((c) => c.startsWith('avc1.')), 'live: asked for AV1, decoder configured for H.264 (agent has no AV1 encoder)', m.videoConfigs);
	const monitor = await page.evaluate(() => document.querySelector('dl')?.textContent ?? '');
	check(/H264/.test(monitor) && !/AV1/.test(monitor), 'live: quality monitor shows H264', monitor.replace(/\s+/g, ' ').slice(0, 160));
	check(/Opus/.test(monitor), 'live: quality monitor shows Opus audio');
	const out = m.audioOut.length;
	const loud = m.audioOut.filter((a) => a.rms > 0.05).length;
	check(out > 150 && loud > 50, 'live: remote audio decoded by AudioDecoder', { decoded: out, loud, maxRms: Math.max(...m.audioOut.map((a) => a.rms)).toFixed(3) });

	await page.locator('button[title="การแสดงผล"]').click();
	const menu = await page.evaluate(() =>
		[...document.querySelectorAll('[role=menuitemradio]')].map((b) => ({ text: b.textContent?.replace(/\s+/g, ' ').trim(), disabled: (b as HTMLButtonElement).disabled, checked: b.getAttribute('aria-checked') }))
	);
	const av1 = menu.find((b) => b.text?.startsWith('AV1'));
	const h264 = menu.find((b) => b.text?.startsWith('H.264'));
	check(!!av1?.disabled && /ปลายทางไม่รองรับ/.test(av1.text ?? ''), 'live: AV1 menu item disabled ("ปลายทางไม่รองรับ")', av1);
	check(h264?.checked === 'true' && av1?.checked === 'false', 'live: the codec in use (H.264) is the one marked', menu);
	const footer = await page.evaluate(() => document.body.innerText.match(/H264 · WebCodecs/)?.[0] ?? '');
	check(!!footer, 'live: menu shows "H264 · WebCodecs"');
	await page.keyboard.press('Escape');
	await page.locator('button[title="การแสดงผล"]').click(); // close the menu

	await page.locator('button[title="ปิดเสียง"]').click();
	await sleep(1000);
	const mutedAt = (await e2e(page)).audioOut.length;
	await sleep(2000);
	const whileMuted = (await e2e(page)).audioOut.length - mutedAt;
	check(whileMuted <= 2, 'live: mute stops the audio stream', `${whileMuted} packets in 2 s`);
	await page.locator('button[title="เปิดเสียง"]').click();
	await sleep(2000);
	const afterUnmute = (await e2e(page)).audioOut.length - mutedAt - whileMuted;
	check(afterUnmute > 50, 'live: unmute resumes it', `${afterUnmute} packets in 2 s`);

	if (process.env.AEGIRON_EXPECT_CM === '1') {
		// Chrome's fake microphone, without a permission prompt in this browser context.
		await page.browserContext().overridePermissions(base, ['microphone']);
		await page.locator('button[title="แชท"]').click();
		await page.locator('input[aria-label="ข้อความ"]').fill('hello from Chrome');
		await page.keyboard.press('Enter');
		const echoed = await waitFor(() => page.evaluate(() => document.querySelector('[role=log]')?.textContent?.includes('echo: hello from Chrome')));
		check(!!echoed, 'live: chat panel sends a message and shows the reply');

		await page.locator('button[title="โทรคุยด้วยเสียง"]').click();
		const inCall = await waitFor(() => page.$('button[title="วางสาย"]'), 20_000);
		check(!!inCall, 'live: voice call accepted (hang-up button shown)');
		const monoBefore = (await e2e(page)).audioOut.filter((a) => a.ch === 1).length;
		await sleep(3000);
		const mono = (await e2e(page)).audioOut.filter((a) => a.ch === 1);
		check(mono.length - monoBefore > 100 && mono.some((a) => a.rms > 0.05), "live: the agent's voice decodes (mono Opus)", {
			packets: mono.length - monoBefore,
			maxRms: Math.max(0, ...mono.map((a) => a.rms)).toFixed(3)
		});
		await page.locator('button[title="วางสาย"]').click();
		const report = await waitFor(() => page.evaluate(() => document.querySelector('[role=log]')?.textContent?.match(/received (\d+) voice packets, peak rms ([\d.]+)/)?.slice(1)));
		check(!!report && Number(report[0]) > 100 && Number(report[1]) > 0.01, "live: the agent heard the browser's microphone", report);
		check(!!(await page.$('button[title="โทรคุยด้วยเสียง"]')), 'live: back to idle after hanging up');
	}
	await page.browserContext().close();

	// --- 2. Recording of that session -----------------------------------------------------------
	const recorded = await waitFor(async () => {
		const list = (await admin.get<{ recordings: any[] }>(`/api/recordings?device=${deviceId}`)).recordings;
		return list.find((r) => r.status === 'ready' && Date.parse(r.created_at) >= startedAt - 2000);
	}, 30_000, 1000);
	check(!!recorded, 'record: live session recording uploaded', recorded && { id: recorded.id, codec: recorded.codec, audio: recorded.audio, ms: recorded.duration_ms });
	check(recorded?.codec === 'h264' && recorded?.audio === 'opus', 'record: codec h264, audio opus', recorded && [recorded.codec, recorded.audio]);
	const uploaded = (await admin.audit('recording.uploaded')).find((e) => e.detail?.recording_id === recorded?.id);
	check(uploaded?.detail?.audio === 'opus', 'record: recording.uploaded audit has audio', uploaded?.detail);

	// --- 3. Play it ----------------------------------------------------------------------------
	if (recorded) {
		const p = await newPage(browser);
		await p.goto(`${base}/recordings/${recorded.id}`, { waitUntil: 'networkidle0' });
		await sleep(4000);
		m = await e2e(p);
		check(m.videoConfigs[0]?.startsWith('avc1.') && m.videoFrames > 0, 'play: live recording video decodes (H.264)', { configs: m.videoConfigs, frames: m.videoFrames });
		const started = m.starts.filter((s) => s.rms >= 0);
		check(m.audioOut.length > 50 && started.length > 50, 'play: its audio decodes and is scheduled', { decoded: m.audioOut.length, scheduled: started.length });
		check(started.some((s) => s.rms > 0.05), 'play: recorded audio is not silent', Math.max(...started.map((s) => s.rms)).toFixed(3));
		const info = await p.evaluate(() => document.body.innerText.match(/H264 \+ Opus/)?.[0] ?? '');
		check(!!info, 'play: player shows "H264 + Opus"');
		await p.browserContext().close();
	}

	// --- 4. Synthetic AV1 recordings -----------------------------------------------------------
	const gen = process.env.AEGIRON_MAKE_RECORDING;
	const config = process.env.AEGIRON_AGENT_CONFIG;
	const db = process.env.AEGIRON_E2E_DB;
	if (!gen || !config || !db) {
		console.log('skip  synthetic AV1 recordings (set AEGIRON_MAKE_RECORDING, AEGIRON_AGENT_CONFIG and AEGIRON_E2E_DB)');
	} else {
		const dir = mkdtempSync(join(tmpdir(), 'aegiron-e2e-rec-'));
		try {
			for (const codec of ['av1', 'switch']) await synthetic(browser, codec, dir, gen, config, db);
		} finally {
			rmSync(dir, { recursive: true, force: true });
		}
	}
} finally {
	await browser.close();
	await admin.put('/api/recordings/settings', settingsBefore);
}
done();

async function run(cmd: string[]) {
	const p = Bun.spawn(cmd, { stdout: 'pipe', stderr: 'pipe' });
	const [out, err] = [await new Response(p.stdout).text(), await new Response(p.stderr).text()];
	if ((await p.exited) !== 0) throw new Error(`${cmd[0]} failed: ${out}${err}`);
	return out;
}

async function synthetic(browser: Browser, codec: string, dir: string, gen: string, config: string, db: string) {
	const sid = `e2e${codec}${randomBytes(6).toString('hex')}`;
	const token = randomBytes(24).toString('base64url');
	const hash = createHash('sha256').update(token).digest('hex');
	const admin_id = (await admin.get<{ user: { id: number } }>('/api/auth/me')).user.id;
	await run([process.env.PSQL ?? 'psql', db, '-v', 'ON_ERROR_STOP=1', '-qc',
		`INSERT INTO session_recordings(sid, device_id, user_id, viewer_ip, token_hash, recording_started) VALUES ('${sid}', '${deviceId}', ${admin_id}, '127.0.0.1', decode('${hash}', 'hex'), true)`]);
	await run([gen, '--out', join(dir, `${sid}.aerec`), '--sid', sid, '--device-id', deviceId, '--codec', codec, '--seconds', '20', '--upload', base, '--config', config, '--token', token]);
	const rec = (await admin.get<{ recordings: any[] }>(`/api/recordings?device=${deviceId}`)).recordings.find((r) => r.sid === sid);
	const wantCodec = codec === 'av1' ? 'av1' : 'h264+av1';
	check(rec?.status === 'ready' && rec.codec === wantCodec && rec.audio === 'opus' && Math.abs(rec.duration_ms - 20_000) < 50, `${codec}: uploaded (${wantCodec} + opus, 20 s)`, rec && [rec.status, rec.codec, rec.audio, rec.duration_ms]);
	if (!rec) return;

	const page = await newPage(browser);
	await page.goto(`${base}/recordings/${rec.id}`, { waitUntil: 'networkidle0' });
	const label = codec === 'av1' ? 'AV1 + Opus' : 'H264+AV1 + Opus';
	check(!!(await waitFor(() => page.evaluate((l) => document.body.innerText.includes(l), label), 10_000)), `${codec}: player shows "${label}"`);

	const now = () => page.evaluate(() => performance.now());
	/**
	 * Plays for `ms` after a seek to `from` (made at page time `since`) and checks what was drawn
	 * and heard: a seek first decodes from the keyframe before `from` up to `from` (drawn in a
	 * burst), then frames follow the clock.
	 */
	async function window(name: string, ms: number, from: number, since: number, seeked = true) {
		await sleep(ms);
		const after = await e2e(page);
		const draws = after.draws.filter((d) => d.at >= since);
		const settled = draws.filter((d) => d.at >= since + 150);
		const starts = after.starts.filter((s) => s.heard >= since && s.ts !== null && s.rate === 1);
		// Frame k is due at k × 100 ms (+ up to 15 ms of send jitter).
		const videoOff = median(settled.map((d) => d.at - d.idx * 100));
		const audioOff = median(starts.map((s) => s.heard - s.ts!));
		const sync = audioOff - videoOff;
		const inOrder = draws.every((d, i) => i === 0 || d.idx >= draws[i - 1].idx);
		const atSeek = draws.filter((d) => d.at < since + 150).at(-1)?.idx;
		const want = Math.floor(from / 100);
		if (seeked) check(atSeek !== undefined && Math.abs(atSeek - want) <= 2, `${codec} ${name}: frame ${want} shown right after the seek`, atSeek);
		check(inOrder && settled.length >= (ms / 100) * 0.7, `${codec} ${name}: frames then follow in order`, { drawn: settled.length, last: draws.at(-1)?.idx });
		check(starts.length >= expectedBlocks(from, ms) * 0.7, `${codec} ${name}: audio scheduled`, `${starts.length} blocks, ~${expectedBlocks(from, ms)} expected`);
		check(Math.abs(sync) <= 60, `${codec} ${name}: audio within 60 ms of video`, `${sync.toFixed(1)} ms (audio − video)`);
		// The beep is on in the first half of every second; nothing between 8 and 9 s (muted).
		let wrong = 0;
		for (const s of starts) {
			const t = s.ts!;
			const inSecond = t % 1000;
			if (inSecond < 30 || (inSecond > 470 && inSecond < 530) || inSecond > 970) continue;
			const want = t >= 8000 && t < 9000 ? 'none' : inSecond < 500 ? 'beep' : 'silence';
			const got = s.rms > 0.1 ? 'beep' : s.rms < 0.02 ? 'silence' : 'between';
			if (want !== got) wrong++;
		}
		check(wrong === 0, `${codec} ${name}: what is heard matches the recording time (beep / silence / muted gap)`, `${wrong} of ${starts.length} blocks wrong`);
	}
	/** 20 ms blocks with sound between `from` and `from + ms` (+ 400 ms decoded ahead), minus the muted second. */
	const expectedBlocks = (from: number, ms: number) => {
		const end = Math.min(from + ms + 400, 20_000);
		return Math.round((end - from - Math.max(0, Math.min(end, 9000) - Math.max(from, 8000))) / 20);
	};
	const seekTo = async (ms: number) => {
		const t = await now();
		await page.evaluate((v) => {
			const r = document.querySelector('input[type=range]') as HTMLInputElement;
			r.value = String(v);
			r.dispatchEvent(new Event('input', { bubbles: true }));
		}, ms);
		return t;
	};

	await window('from the start', 3000, 0, await seekTo(0));
	await window('after seek to 12 s', 3000, 12_000, await seekTo(12_000));
	await window('after seek back to 3 s', 3000, 3_000, await seekTo(3_000));
	await window('across the muted gap (7–10 s)', 3000, 7_000, await seekTo(7_000));

	// Paused seek: the frame at that time is drawn and no audio is scheduled.
	await page.locator('button.btn-primary.w-24').click();
	await sleep(300);
	const paused = await e2e(page);
	await seekTo(15_050);
	await sleep(800);
	const afterPausedSeek = await e2e(page);
	const shown = afterPausedSeek.draws.at(-1)?.idx;
	check(shown === 150, `${codec}: paused seek to 15.05 s shows frame 150`, shown);
	check(afterPausedSeek.starts.length === paused.starts.length, `${codec}: no audio while paused`, afterPausedSeek.starts.length - paused.starts.length);
	const resumed = await now();
	await page.locator('button.btn-primary.w-24').click();
	await window('resume after paused seek', 2000, 15_050, resumed, false);
	if (codec === 'switch') {
		const cfgs = (await e2e(page)).videoConfigs;
		check(cfgs.some((c) => c.startsWith('av01.')) && cfgs.some((c) => c.startsWith('avc1.')), 'switch: decoder reconfigured between AV1 and H.264', [...new Set(cfgs)]);
	}

	// 2×: audio still plays (pitch not corrected); 4×: silent.
	await seekTo(2_000);
	await page.locator('xpath/.//button[normalize-space()="2×"]').click();
	let s0 = (await e2e(page)).starts.length;
	await sleep(1500);
	const at2 = (await e2e(page)).starts.slice(s0);
	check(at2.length > 30 && at2.every((s) => s.rate === 2), `${codec}: audio plays at 2×`, at2.length);
	await page.locator('xpath/.//button[normalize-space()="4×"]').click();
	await sleep(300);
	s0 = (await e2e(page)).starts.length;
	await sleep(1000);
	check((await e2e(page)).starts.length === s0, `${codec}: silent at 4×`);
	await page.browserContext().close();
}
