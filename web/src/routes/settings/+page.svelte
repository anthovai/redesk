<script lang="ts">
	import { api, type AgentRelease, type EnrollToken, type Group, type Integration, type OIDCSettings, type ServerInfo, type Settings } from '#lib/api.js';
	import { ago } from '#lib/format.js';
	import { bytes, dateTime, fingerprint } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';

	let info = $state<ServerInfo | null>(null);
	let fp = $state('');
	let settings = $state<Settings | null>(null);
	let tokens = $state<EnrollToken[]>([]);
	let newSecret = $state('');
	let groups = $state<Group[]>([]);
	let tokenForm = $state({ note: '', valid_days: 7, max_uses: 1, group: '' });
	let saved = $state(false);
	let tv = $state<{ integration: Integration | null; has_token?: boolean } | null>(null);
	let tvForm = $state({ token: '', enabled: true, group: '' });
	let tvMsg = $state<[string, boolean] | null>(null);
	let tvBusy = $state(false);
	let error = $state('');
	let oidc = $state<OIDCSettings | null>(null);
	let oidcRedirect = $state('');
	let oidcForm = $state({ name: '', issuer: '', client_id: '', client_secret: '', domains: '', auto_create: false, enabled: false });
	let oidcMsg = $state<[string, boolean] | null>(null);
	let oidcBusy = $state(false);
	let releases = $state<AgentRelease[]>([]);
	let relForm = $state({ version: '', os: 'windows', arch: 'x86_64', notes: '' });
	let relFile = $state<FileList | null>(null);
	let relMsg = $state<[string, boolean] | null>(null);
	let relBusy = $state(false);

	async function load() {
		try {
			info = await api.get<ServerInfo>('/api/server-info');
			fp = await fingerprint(info.public_key);
			settings = (await api.get<{ settings: Settings }>('/api/settings')).settings;
			tokens = (await api.get<{ tokens: EnrollToken[] }>('/api/enroll-tokens')).tokens;
			groups = (await api.get<{ groups: Group[] }>('/api/groups')).groups;
			tv = await api.get('/api/integrations/teamviewer');
			if (tv?.integration) tvForm = { token: '', enabled: tv.integration.enabled, group: tv.integration.group_id ? String(tv.integration.group_id) : '' };
			const o = await api.get<{ settings: OIDCSettings; redirect_uri: string }>('/api/oidc/settings');
			oidc = o.settings;
			oidcRedirect = o.redirect_uri;
			oidcForm = {
				name: o.settings.name,
				issuer: o.settings.issuer,
				client_id: o.settings.client_id,
				client_secret: '',
				domains: (o.settings.allowed_domains ?? []).join(', '),
				auto_create: o.settings.auto_create,
				enabled: o.settings.enabled
			};
			releases = (await api.get<{ releases: AgentRelease[] }>('/api/agent/releases')).releases;
		} catch (e) {
			error = (e as Error).message;
		}
	}
	$effect(() => {
		load();
	});

	const serverUrl = $derived(typeof location !== 'undefined' ? location.origin : '');
	const initCmd = $derived(
		info
			? `redesk-agent init --server ${serverUrl} --key ${info.public_key}${settings?.require_enroll ? ' --enroll-token <TOKEN>' : ''}${serverUrl.startsWith('http://') && !/localhost|127\.0\.0\.1/.test(serverUrl) ? ' --insecure' : ''}`
			: ''
	);

	async function saveSettings(ev: SubmitEvent) {
		ev.preventDefault();
		try {
			await api.put('/api/settings', settings);
			saved = true;
			setTimeout(() => (saved = false), 2000);
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function createToken(ev: SubmitEvent) {
		ev.preventDefault();
		const { group, ...rest } = tokenForm;
		const r = await api.post<{ secret: string }>('/api/enroll-tokens', { ...rest, group_id: group ? +group : null });
		newSecret = r.secret;
		tokenForm = { note: '', valid_days: 7, max_uses: 1, group: '' };
		await load();
	}

	async function deleteToken(t: EnrollToken) {
		if (!confirm('ลบ token นี้?')) return;
		await api.del(`/api/enroll-tokens/${t.id}`);
		await load();
	}

	const copy = (s: string) => navigator.clipboard.writeText(s);

	async function saveTv(ev: SubmitEvent) {
		ev.preventDefault();
		tvMsg = null;
		try {
			await api.put('/api/integrations/teamviewer', {
				token: tvForm.token ? tvForm.token : null,
				enabled: tvForm.enabled,
				group_id: tvForm.group ? +tvForm.group : null
			});
			tvForm.token = '';
			tvMsg = ['บันทึกแล้ว', true];
			await load();
		} catch (e) {
			tvMsg = [(e as Error).message, false];
		}
	}

	async function syncTv() {
		tvBusy = true;
		tvMsg = null;
		try {
			const r = await api.post<{ result: { devices: number; new_devices: number; sessions: number } }>('/api/integrations/teamviewer/sync');
			tvMsg = [`ซิงก์แล้ว: อุปกรณ์ ${r.result.devices} (ใหม่ ${r.result.new_devices}), ประวัติการเชื่อมต่อ ${r.result.sessions} รายการ`, true];
			await load();
		} catch (e) {
			tvMsg = [(e as Error).message, false];
		} finally {
			tvBusy = false;
		}
	}

	async function saveOidc(ev: SubmitEvent) {
		ev.preventDefault();
		oidcMsg = null;
		oidcBusy = true;
		try {
			await api.put('/api/oidc/settings', {
				name: oidcForm.name,
				issuer: oidcForm.issuer,
				client_id: oidcForm.client_id,
				client_secret: oidcForm.client_secret ? oidcForm.client_secret : null,
				allowed_domains: oidcForm.domains
					.split(/[\s,]+/)
					.map((d) => d.trim())
					.filter(Boolean),
				auto_create: oidcForm.auto_create,
				enabled: oidcForm.enabled
			});
			oidcMsg = ['บันทึกแล้ว', true];
			await load();
		} catch (e) {
			oidcMsg = [(e as Error).message, false];
		} finally {
			oidcBusy = false;
		}
	}

	async function uploadRelease(ev: SubmitEvent) {
		ev.preventDefault();
		const file = relFile?.[0];
		if (!file) return;
		relMsg = null;
		relBusy = true;
		try {
			const fd = new FormData();
			fd.set('version', relForm.version.trim());
			fd.set('os', relForm.os);
			fd.set('arch', relForm.arch);
			fd.set('notes', relForm.notes);
			fd.set('file', file);
			await api.upload('/api/agent/releases', fd);
			relForm = { version: '', os: relForm.os, arch: relForm.arch, notes: '' };
			relFile = null;
			(ev.target as HTMLFormElement).reset();
			relMsg = ['อัปโหลดแล้ว — ตรวจสอบ SHA-256 แล้วกด "เปิดให้ดาวน์โหลด" เมื่อพร้อม', true];
			await load();
		} catch (e) {
			relMsg = [(e as Error).message, false];
		} finally {
			relBusy = false;
		}
	}

	async function toggleRelease(r: AgentRelease) {
		try {
			await api.patch(`/api/agent/releases/${r.id}`, { enabled: !r.enabled });
			await load();
		} catch (e) {
			relMsg = [(e as Error).message, false];
		}
	}

	async function deleteRelease(r: AgentRelease) {
		if (!confirm(`ลบเวอร์ชัน ${r.version} (${r.os}/${r.arch})?`)) return;
		try {
			await api.del(`/api/agent/releases/${r.id}`);
			await load();
		} catch (e) {
			relMsg = [(e as Error).message, false];
		}
	}

	const osLabel: Record<string, string> = { windows: 'Windows', macos: 'macOS', linux: 'Linux' };

	async function removeTv() {
		if (!confirm('ยกเลิกการเชื่อม TeamViewer API? (อุปกรณ์ที่ซิงก์มาแล้วจะยังอยู่)')) return;
		await api.del('/api/integrations/teamviewer');
		await load();
	}
</script>

<svelte:head><title>ตั้งค่าเซิร์ฟเวอร์ · ReDesk</title></svelte:head>

<h1 class="mb-6 text-2xl font-semibold tracking-tight">ตั้งค่าเซิร์ฟเวอร์</h1>
{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<div class="grid gap-6 lg:grid-cols-2">
	<section class="card p-6 lg:col-span-2">
		<h2 class="font-semibold">ติดตั้ง agent บนเครื่องที่จะถูกควบคุม</h2>
		<p class="mt-1 text-sm text-muted">
			agent จะตรึง (pin) กุญแจสาธารณะของเซิร์ฟเวอร์นี้ ถ้ามีคนปลอมเป็นเซิร์ฟเวอร์ agent จะปฏิเสธการเชื่อมต่อทันที
		</p>
		{#if info}
			<dl class="mt-4 grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_1fr]">
				<dt class="text-muted">Server public key</dt>
				<dd class="mono break-all">{info.public_key}</dd>
				<dt class="text-muted">Fingerprint</dt>
				<dd class="mono">{fp}</dd>
				<dt class="text-muted">เวอร์ชัน</dt>
				<dd>{info.version}</dd>
			</dl>
			<div class="mt-4 flex items-start gap-2 rounded-lg bg-raised p-3">
				<code class="mono flex-1 text-xs break-all">{initCmd}<br />redesk-agent run</code>
				<button class="btn shrink-0" onclick={() => copy(initCmd)} title="คัดลอก"><Icon name="copy" /></button>
			</div>
		{/if}
	</section>

	{#if settings}
		<form class="card space-y-4 p-6" onsubmit={saveSettings}>
			<h2 class="font-semibold">นโยบาย</h2>
			<label class="flex items-start gap-3 text-sm">
				<input type="checkbox" class="mt-1" bind:checked={settings.require_enroll} />
				<span>
					<span class="font-medium">บังคับใช้ enrollment token</span>
					<span class="block text-muted">เครื่องใหม่ต้องมี token ที่ผู้ดูแลออกให้ จึงจะลงทะเบียนกับเซิร์ฟเวอร์ได้</span>
				</span>
			</label>
			<fieldset class="space-y-2 text-sm">
				<legend class="mb-1 font-medium">การเข้าถึงอุปกรณ์ของผู้ใช้ทั่วไป</legend>
				<label class="flex items-start gap-3">
					<input type="radio" class="mt-1" value="open" bind:group={settings.access_policy} />
					<span>เปิด <span class="block text-muted">เชื่อมต่อได้ทุกเครื่องถ้ารู้ ID และรหัสผ่าน (เหมือน RustDesk)</span></span>
				</label>
				<label class="flex items-start gap-3">
					<input type="radio" class="mt-1" value="groups" bind:group={settings.access_policy} />
					<span>ตามกลุ่ม <span class="block text-muted">เชื่อมต่อได้เฉพาะเครื่องใน<a href="/groups" class="text-accent underline">กลุ่ม</a>ที่ตนเป็นสมาชิก</span></span>
				</label>
			</fieldset>
			<div>
				<label class="label" for="hours">อายุการล็อกอินคอนโซล (ชั่วโมง)</label>
				<input id="hours" class="input w-32" type="number" min="1" max="720" bind:value={settings.session_hours} />
			</div>
			<button class="btn btn-primary">{saved ? 'บันทึกแล้ว ✓' : 'บันทึก'}</button>
		</form>
	{/if}

	<section class="card p-6">
		<h2 class="font-semibold">Enrollment tokens</h2>
		<form class="mt-4 grid grid-cols-2 items-end gap-2 sm:grid-cols-[1fr_auto_auto]" onsubmit={createToken}>
			<div class="col-span-2 sm:col-span-1"><label class="label" for="t-note">หมายเหตุ</label><input id="t-note" class="input" placeholder="เช่น แผนกบัญชี" bind:value={tokenForm.note} /></div>
			<div><label class="label" for="t-days">อายุ (วัน)</label><input id="t-days" class="input w-20" type="number" min="0" bind:value={tokenForm.valid_days} /></div>
			<div><label class="label" for="t-uses">ใช้ได้ (ครั้ง)</label><input id="t-uses" class="input w-20" type="number" min="0" bind:value={tokenForm.max_uses} /></div>
			<div class="col-span-2 sm:col-span-2">
				<label class="label" for="t-group">เครื่องที่ลงทะเบียนเข้ากลุ่ม</label>
				<select id="t-group" class="input" bind:value={tokenForm.group}>
					<option value="">ไม่กำหนด</option>
					{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
				</select>
			</div>
			<button class="btn btn-primary">สร้าง</button>
		</form>
		{#if newSecret}
			<div class="mt-3 rounded-lg border border-accent bg-accent-soft p-3 text-sm">
				<p class="font-medium text-accent">คัดลอก token นี้ตอนนี้ — จะไม่แสดงอีก</p>
				<div class="mt-2 flex items-center gap-2">
					<code class="mono flex-1 break-all">{newSecret}</code>
					<button class="btn" onclick={() => copy(newSecret)}><Icon name="copy" /></button>
				</div>
			</div>
		{/if}
		<ul class="mt-4 divide-y divide-line text-sm">
			{#each tokens as t (t.id)}
				<li class="flex items-center gap-3 py-2">
					<div class="min-w-0 flex-1">
						<p class="truncate">{t.note || `Token #${t.id}`}</p>
						<p class="text-xs text-muted">
							{#if t.group_id}กลุ่ม {groups.find((g) => g.id === t.group_id)?.name ?? '?'} · {/if}ใช้แล้ว {t.uses}{t.max_uses ? `/${t.max_uses}` : ''} · {t.expires_at ? `หมดอายุ ${dateTime(t.expires_at)}` : 'ไม่หมดอายุ'}
						</p>
					</div>
					<button class="btn btn-ghost p-1.5" title="ลบ" onclick={() => deleteToken(t)}><Icon name="trash" /></button>
				</li>
			{:else}
				<li class="py-2 text-muted">ยังไม่มี token</li>
			{/each}
		</ul>
	</section>
	<section class="card space-y-4 p-6 lg:col-span-2">
		<div>
			<h2 class="font-semibold">TeamViewer API</h2>
			<p class="mt-1 text-sm text-muted">
				ดึงรายชื่ออุปกรณ์ (เข้าหน้า "การเชื่อมต่อ") และประวัติการเชื่อมต่อของ TeamViewer มาไว้ใน ReDesk ทุก 15 นาที
				ใช้ <b>script token</b> ที่สร้างใน TeamViewer Management Console → Profile → Apps ให้สิทธิ์เฉพาะ Computers &amp; Contacts: read และ Connection reporting: read
			</p>
		</div>
		{#if tv?.integration}
			<p class="text-sm">
				ซิงก์ล่าสุด: {tv.integration.last_sync_at ? ago(tv.integration.last_sync_at) : 'ยังไม่เคย'}
				{#if tv.integration.last_error}<span class="block text-danger">ข้อผิดพลาดล่าสุด: {tv.integration.last_error}</span>{/if}
			</p>
		{/if}
		<form class="grid gap-3 sm:grid-cols-[1fr_auto]" onsubmit={saveTv}>
			<div>
				<label class="label" for="tv-token">Script token</label>
				<input id="tv-token" class="input mono" type="password" autocomplete="off" placeholder={tv?.has_token ? 'เก็บไว้แล้ว (เข้ารหัส) — เว้นว่างถ้าไม่เปลี่ยน' : '12345678-AbCdEf...'} bind:value={tvForm.token} />
			</div>
			<div>
				<label class="label" for="tv-group">อุปกรณ์ใหม่เข้ากลุ่ม</label>
				<select id="tv-group" class="input" bind:value={tvForm.group}>
					<option value="">ไม่กำหนด</option>
					{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
				</select>
			</div>
			<label class="flex items-center gap-2 text-sm sm:col-span-2"><input type="checkbox" bind:checked={tvForm.enabled} /> ซิงก์อัตโนมัติ</label>
			<div class="flex flex-wrap gap-2 sm:col-span-2">
				<button class="btn btn-primary" disabled={!tv?.integration && !tvForm.token}>บันทึก</button>
				{#if tv?.integration}
					<button type="button" class="btn" disabled={tvBusy} onclick={syncTv}><Icon name="refresh" /> {tvBusy ? 'กำลังซิงก์…' : 'ซิงก์ตอนนี้'}</button>
					<button type="button" class="btn btn-danger" onclick={removeTv}>ยกเลิกการเชื่อม</button>
				{/if}
			</div>
		</form>
		{#if tvMsg}<p class="text-sm {tvMsg[1] ? 'text-ok' : 'text-danger'}">{tvMsg[0]}</p>{/if}
		<p class="border-t border-line pt-3 text-xs text-muted">
			AnyDesk: REST API สาธารณะของ AnyDesk ยังไม่เปิดข้อมูลอุปกรณ์/เซสชัน ให้ใช้ "นำเข้า CSV" ที่หน้าการเชื่อมต่อแทน
		</p>
	</section>

	<section class="card space-y-4 p-6 lg:col-span-2">
		<div>
			<h2 class="font-semibold">เข้าสู่ระบบด้วย SSO (OpenID Connect)</h2>
			<p class="mt-1 text-sm text-muted">
				ให้ผู้ใช้เข้าสู่ระบบด้วยบัญชีองค์กร เช่น Microsoft 365 (Entra ID), Google Workspace หรือ Keycloak ทั้งในคอนโซลเว็บและโปรแกรม ReDesk
				โดยลงทะเบียนแอปที่ผู้ให้บริการแล้วใส่ Redirect URI ด้านล่าง
			</p>
		</div>
		{#if oidcRedirect}
			<div class="flex items-center gap-2 rounded-lg bg-raised p-3 text-sm">
				<span class="text-muted">Redirect URI</span>
				<code class="mono flex-1 break-all">{oidcRedirect}</code>
				<button class="btn shrink-0" onclick={() => copy(oidcRedirect)} title="คัดลอก"><Icon name="copy" /></button>
			</div>
		{/if}
		<form class="grid gap-3 sm:grid-cols-2" onsubmit={saveOidc}>
			<div>
				<label class="label" for="oidc-name">ชื่อที่แสดงบนปุ่ม</label>
				<input id="oidc-name" class="input" placeholder="Microsoft 365" maxlength="64" bind:value={oidcForm.name} />
			</div>
			<div>
				<label class="label" for="oidc-issuer">Issuer URL</label>
				<input id="oidc-issuer" class="input mono" placeholder="https://login.microsoftonline.com/<tenant-id>/v2.0" bind:value={oidcForm.issuer} />
			</div>
			<div>
				<label class="label" for="oidc-client">Client ID</label>
				<input id="oidc-client" class="input mono" autocomplete="off" bind:value={oidcForm.client_id} />
			</div>
			<div>
				<label class="label" for="oidc-secret">Client secret</label>
				<input
					id="oidc-secret"
					class="input mono"
					type="password"
					autocomplete="off"
					placeholder={oidc?.has_secret ? 'เก็บไว้แล้ว (เข้ารหัส) — เว้นว่างถ้าไม่เปลี่ยน' : ''}
					bind:value={oidcForm.client_secret}
				/>
			</div>
			<div class="sm:col-span-2">
				<label class="label" for="oidc-domains">โดเมนอีเมลที่อนุญาต (ไม่บังคับ)</label>
				<input id="oidc-domains" class="input mono" placeholder="example.co.th, example.com" bind:value={oidcForm.domains} />
				<p class="mt-1 text-xs text-muted">เว้นว่าง = รับทุกโดเมนที่ผู้ให้บริการยืนยันแล้ว</p>
			</div>
			<label class="flex items-start gap-3 text-sm sm:col-span-2">
				<input type="checkbox" class="mt-1" bind:checked={oidcForm.auto_create} />
				<span>
					<span class="font-medium">สร้างบัญชีให้อัตโนมัติ</span>
					<span class="block text-muted">ผู้ที่เข้าสู่ระบบครั้งแรกจะได้บัญชีสิทธิ์ "ผู้ใช้" ถ้าปิดไว้ ผู้ดูแลต้องสร้างบัญชีที่อีเมลตรงกันไว้ก่อน</span>
				</span>
			</label>
			<label class="flex items-center gap-2 text-sm sm:col-span-2"><input type="checkbox" bind:checked={oidcForm.enabled} /> เปิดใช้งาน</label>
			<div class="sm:col-span-2">
				<button class="btn btn-primary" disabled={oidcBusy}>{oidcBusy ? 'กำลังตรวจสอบ…' : 'บันทึก'}</button>
			</div>
		</form>
		{#if oidcMsg}<p class="text-sm {oidcMsg[1] ? 'text-ok' : 'text-danger'}">{oidcMsg[0]}</p>{/if}
	</section>

	<section class="card space-y-4 p-6 lg:col-span-2">
		<div>
			<h2 class="font-semibold">เวอร์ชันของ agent</h2>
			<p class="mt-1 text-sm text-muted">
				อัปโหลดไฟล์ติดตั้งที่ตรวจสอบแล้ว โปรแกรม ReDesk จะแจ้งผู้ใช้เมื่อมีเวอร์ชันใหม่ที่เปิดให้ดาวน์โหลด และตรวจขนาดกับ SHA-256 ก่อนใช้ไฟล์ทุกครั้ง
				— ไม่มีการติดตั้งอัตโนมัติ
			</p>
		</div>
		<form class="grid gap-3 sm:grid-cols-[1fr_auto_auto]" onsubmit={uploadRelease}>
			<div>
				<label class="label" for="rel-version">เวอร์ชัน</label>
				<input id="rel-version" class="input mono" placeholder="0.4.0" required maxlength="64" bind:value={relForm.version} />
			</div>
			<div>
				<label class="label" for="rel-os">ระบบปฏิบัติการ</label>
				<select id="rel-os" class="input" bind:value={relForm.os}>
					<option value="windows">Windows</option>
					<option value="macos">macOS</option>
					<option value="linux">Linux</option>
				</select>
			</div>
			<div>
				<label class="label" for="rel-arch">สถาปัตยกรรม</label>
				<select id="rel-arch" class="input" bind:value={relForm.arch}>
					<option value="x86_64">x86_64</option>
					<option value="aarch64">aarch64 (ARM64)</option>
					<option value="universal">universal</option>
				</select>
			</div>
			<div class="sm:col-span-3">
				<label class="label" for="rel-notes">รายละเอียดการเปลี่ยนแปลง</label>
				<textarea id="rel-notes" class="input" rows="2" maxlength="4000" bind:value={relForm.notes}></textarea>
			</div>
			<div class="sm:col-span-2">
				<label class="label" for="rel-file">ไฟล์ (สูงสุด 300 MB)</label>
				<input id="rel-file" class="input" type="file" required bind:files={relFile} />
			</div>
			<div class="flex items-end">
				<button class="btn btn-primary" disabled={relBusy || !relFile?.length}>{relBusy ? 'กำลังอัปโหลด…' : 'อัปโหลด'}</button>
			</div>
		</form>
		{#if relMsg}<p class="text-sm {relMsg[1] ? 'text-ok' : 'text-danger'}">{relMsg[0]}</p>{/if}
		<ul class="divide-y divide-line text-sm">
			{#each releases as r (r.id)}
				<li class="flex flex-wrap items-center gap-3 py-3">
					<div class="min-w-0 flex-1">
						<p>
							<span class="font-medium">{r.version}</span>
							<span class="text-muted">· {osLabel[r.os] ?? r.os} {r.arch} · {bytes(r.size)} · {dateTime(r.created_at)}</span>
							{#if r.enabled}<span class="ml-1 rounded bg-accent-soft px-1.5 py-0.5 text-xs text-accent">เปิดให้ดาวน์โหลด</span>{/if}
						</p>
						<p class="mono text-xs break-all text-muted">{r.filename} · SHA-256 {r.sha256}</p>
						{#if r.notes}<p class="mt-1 text-xs whitespace-pre-line text-muted">{r.notes}</p>{/if}
					</div>
					<a class="btn btn-ghost p-1.5" title="ดาวน์โหลด" href={`/api/agent/releases/${r.id}/download`} data-sveltekit-reload><Icon name="download" /></a>
					<button class="btn" onclick={() => toggleRelease(r)}>{r.enabled ? 'ปิดการดาวน์โหลด' : 'เปิดให้ดาวน์โหลด'}</button>
					<button class="btn btn-ghost p-1.5" title="ลบ" onclick={() => deleteRelease(r)}><Icon name="trash" /></button>
				</li>
			{:else}
				<li class="py-2 text-muted">ยังไม่มีเวอร์ชันที่อัปโหลด</li>
			{/each}
		</ul>
	</section>
</div>
