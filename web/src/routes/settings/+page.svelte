<script lang="ts">
	import { api, type EnrollToken, type Group, type Integration, type ServerInfo, type Settings } from '#lib/api.js';
	import { ago } from '#lib/format.js';
	import { dateTime, fingerprint } from '#lib/format.js';
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

	async function load() {
		try {
			info = await api.get<ServerInfo>('/api/server-info');
			fp = await fingerprint(info.public_key);
			settings = (await api.get<{ settings: Settings }>('/api/settings')).settings;
			tokens = (await api.get<{ tokens: EnrollToken[] }>('/api/enroll-tokens')).tokens;
			groups = (await api.get<{ groups: Group[] }>('/api/groups')).groups;
			tv = await api.get('/api/integrations/teamviewer');
			if (tv?.integration) tvForm = { token: '', enabled: tv.integration.enabled, group: tv.integration.group_id ? String(tv.integration.group_id) : '' };
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
</div>
