<script lang="ts">
	import { api, type Endpoint, type Group, type LaunchInfo, type Provider } from '#lib/api.js';
	import { ago } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';
	import { defaultPorts, providers } from '#lib/providers.js';
	import { auth } from '#lib/session.svelte.js';

	type Form = {
		id: number;
		name: string;
		provider: Provider;
		address: string;
		port: string;
		username: string;
		password: string;
		clearPassword: boolean;
		hasSecret: boolean;
		security: string;
		ignoreCert: boolean;
		domain: string;
		sshAuth: 'password' | 'key';
		group: string;
		tags: string;
		note: string;
		synced: boolean;
	};

	let endpoints = $state<Endpoint[]>([]);
	let groups = $state<Group[]>([]);
	let gateway = $state(false);
	let loading = $state(true);
	let error = $state('');
	let filter = $state('');
	let providerFilter = $state<Provider | ''>('');
	let form = $state<Form | null>(null);
	let importing = $state<{ csv: string; group: string; result: { created: number; errors: string[] } | null } | null>(null);
	let secretDialog = $state<{ name: string; secret: string; shown: boolean } | null>(null);
	let toast = $state('');

	const isAdmin = $derived(auth.user?.role === 'admin');

	async function load() {
		try {
			const r = await api.get<{ endpoints: Endpoint[]; gateway: boolean }>('/api/endpoints');
			endpoints = r.endpoints;
			gateway = r.gateway;
			if (isAdmin) groups = (await api.get<{ groups: Group[] }>('/api/groups')).groups;
			error = '';
		} catch (e) {
			error = (e as Error).message;
		} finally {
			loading = false;
		}
	}
	$effect(() => {
		load();
	});

	const counts = $derived(
		endpoints.reduce<Record<string, number>>((m, e) => ((m[e.provider] = (m[e.provider] ?? 0) + 1), m), {})
	);
	const shown = $derived(
		endpoints.filter((e) => {
			if (providerFilter && e.provider !== providerFilter) return false;
			const q = filter.trim().toLowerCase();
			return !q || [e.name, e.address, e.username, e.note, ...e.tags].some((v) => v.toLowerCase().includes(q));
		})
	);
	const groupName = (id: number | null) => groups.find((g) => g.id === id)?.name ?? '';
	const canEdit = (e: Endpoint) => isAdmin || (e.created_by === auth.user?.id && e.source !== 'teamviewer_api');

	function flash(msg: string) {
		toast = msg;
		setTimeout(() => (toast = ''), 3500);
	}

	function newForm(): Form {
		return {
			id: 0, name: '', provider: 'anydesk', address: '', port: '', username: '', password: '', clearPassword: false,
			hasSecret: false, security: 'any', ignoreCert: false, domain: '', sshAuth: 'password', group: '', tags: '', note: '', synced: false
		};
	}

	function edit(e: Endpoint) {
		form = {
			id: e.id, name: e.name, provider: e.provider, address: e.address, port: e.port ? String(e.port) : '',
			username: e.username, password: '', clearPassword: false, hasSecret: e.has_secret,
			security: e.options.security || 'any', ignoreCert: !!e.options.ignore_cert, domain: e.options.domain ?? '',
			sshAuth: e.options.auth === 'key' ? 'key' : 'password',
			group: e.group_id ? String(e.group_id) : '', tags: e.tags.join(', '), note: e.note, synced: e.source === 'teamviewer_api'
		};
	}

	async function save(ev: SubmitEvent) {
		ev.preventDefault();
		const f = form!;
		const body = {
			name: f.name,
			provider: f.provider,
			address: f.address,
			port: f.port ? +f.port : null,
			username: f.username,
			password: f.clearPassword ? '' : f.password ? f.password : f.id ? null : '',
			options:
				f.provider === 'rdp'
					? { security: f.security, ignore_cert: f.ignoreCert, domain: f.domain }
					: f.provider === 'ssh'
						? { auth: f.sshAuth }
						: null,
			group_id: f.group ? +f.group : null,
			tags: f.tags.split(',').map((t) => t.trim()).filter(Boolean),
			note: f.note
		};
		try {
			if (f.id) await api.put(`/api/endpoints/${f.id}`, body);
			else await api.post('/api/endpoints', body);
			form = null;
			await load();
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function remove(e: Endpoint) {
		if (!confirm(`ลบ "${e.name}"?`)) return;
		await api.del(`/api/endpoints/${e.id}`);
		form = null;
		await load();
	}

	// Opens the vendor's own app. The password is copied to the clipboard (never put in the URL)
	// and wiped again after 30 seconds if this tab still has focus.
	async function launchNative(e: Endpoint) {
		try {
			const r = await api.post<LaunchInfo>(`/api/endpoints/${e.id}/launch`);
			if (r.secret) {
				try {
					await navigator.clipboard.writeText(r.secret);
					flash(`คัดลอกรหัสผ่านแล้ว วางในหน้าต่าง ${providers[e.provider].label} (จะล้างใน 30 วินาที)`);
					setTimeout(() => document.hasFocus() && navigator.clipboard.writeText('').catch(() => {}), 30_000);
				} catch {
					secretDialog = { name: e.name, secret: r.secret, shown: false };
				}
			}
			if (r.file) {
				const url = URL.createObjectURL(new Blob([r.file.content], { type: 'application/x-rdp' }));
				const a = document.createElement('a');
				a.href = url;
				a.download = r.file.name;
				a.click();
				setTimeout(() => URL.revokeObjectURL(url), 5000);
			}
			if (r.url) location.href = r.url;
		} catch (err) {
			error = (err as Error).message;
		}
	}

	async function runImport(ev: SubmitEvent) {
		ev.preventDefault();
		const i = importing!;
		try {
			i.result = await api.post<{ created: number; errors: string[] }>('/api/endpoints/import', {
				csv: i.csv,
				group_id: i.group ? +i.group : null
			});
			await load();
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function pickCsv(ev: Event) {
		const file = (ev.currentTarget as HTMLInputElement).files?.[0];
		if (file && importing) importing.csv = await file.text();
	}

	const csvTemplate =
		'name,provider,address,port,username,password,tags,note\n' +
		'เครื่องบัญชี 1,anydesk,123456789,,,,finance;office,\n' +
		'Server DC,rdp,dc01.corp.local,3389,administrator,,servers,\n' +
		'สาขาเชียงใหม่,teamviewer,987654321,,,,branch,';
</script>

<svelte:head><title>การเชื่อมต่อ · Aegiron</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">การเชื่อมต่อ</h1>
		<p class="mt-1 max-w-2xl text-sm text-muted">
			รวมเครื่องที่ใช้ AnyDesk, TeamViewer, RustDesk และ RDP / VNC / SSH ไว้ที่เดียว รหัสผ่านเก็บเข้ารหัสในเซิร์ฟเวอร์ และทุกการเปิดถูกบันทึกใน audit log
		</p>
	</div>
	<div class="flex gap-2">
		{#if isAdmin}
			<button class="btn" onclick={() => (importing = { csv: '', group: '', result: null })}><Icon name="upload" /> นำเข้า CSV</button>
		{/if}
		<button class="btn btn-primary" onclick={() => (form = newForm())}><Icon name="plus" /> เพิ่ม</button>
	</div>
</div>

<div class="mb-4 flex flex-wrap items-center gap-2">
	<button class="btn {providerFilter === '' ? 'border-accent bg-accent-soft text-accent' : ''}" onclick={() => (providerFilter = '')}>
		ทั้งหมด <span class="text-muted">{endpoints.length}</span>
	</button>
	{#each Object.entries(providers) as [key, p] (key)}
		{#if counts[key]}
			<button
				class="btn {providerFilter === key ? 'border-accent bg-accent-soft text-accent' : ''}"
				onclick={() => (providerFilter = key as Provider)}
			>
				{p.label} <span class="text-muted">{counts[key]}</span>
			</button>
		{/if}
	{/each}
	<div class="relative ml-auto">
		<Icon name="search" class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
		<input class="input w-64 pl-9" placeholder="ค้นหาชื่อ, ID, โฮสต์, แท็ก" bind:value={filter} />
	</div>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

{#if loading}
	<p class="text-sm text-muted">กำลังโหลด…</p>
{:else if endpoints.length === 0}
	<div class="card grid place-items-center gap-2 px-6 py-14 text-center">
		<Icon name="link" size={28} class="text-muted" />
		<p class="font-medium">ยังไม่มีการเชื่อมต่อ</p>
		<p class="max-w-md text-sm text-muted">
			เพิ่มเครื่องที่ใช้ AnyDesk / TeamViewer / RustDesk หรือเซิร์ฟเวอร์ RDP / VNC / SSH
			{#if isAdmin}หรือเชื่อม TeamViewer API ที่หน้า <a href="/settings" class="text-accent underline">ตั้งค่าเซิร์ฟเวอร์</a>{/if}
		</p>
	</div>
{:else}
	<div class="card overflow-x-auto">
		<table class="table">
			<thead>
				<tr><th>ประเภท</th><th>ชื่อ</th><th>ปลายทาง</th><th>กลุ่ม / แท็ก</th><th class="text-right">เปิด</th></tr>
			</thead>
			<tbody>
				{#each shown as e (e.id)}
					{@const p = providers[e.provider]}
					<tr>
						<td><span class="badge font-medium {p.color}">{p.label}</span></td>
						<td>
							<p class="flex items-center gap-1.5 font-medium">
								{#if e.online !== null}
									<span class="size-2 rounded-full {e.online ? 'bg-ok' : 'bg-line'}" title={e.online ? 'ออนไลน์' : 'ออฟไลน์'}></span>
								{/if}
								{e.name}
								{#if e.has_secret}<span title="มีรหัสผ่านใน vault"><Icon name="lock" size={12} class="text-muted" /></span>{/if}
							</p>
							{#if e.source === 'teamviewer_api'}
								<p class="text-xs text-muted">ซิงก์จาก TeamViewer API · {ago(e.synced_at)}</p>
							{:else if e.note}
								<p class="max-w-xs truncate text-xs text-muted">{e.note}</p>
							{/if}
						</td>
						<td class="mono text-sm whitespace-nowrap">
							{#if e.username}<span class="text-muted">{e.username}@</span>{/if}{e.address}{#if e.port && e.port !== defaultPorts[e.provider]}<span class="text-muted">:{e.port}</span>{/if}
						</td>
						<td>
							<div class="flex flex-wrap gap-1">
								{#if e.group_id}<span class="badge"><Icon name="layers" size={11} /> {groupName(e.group_id) || 'กลุ่ม'}</span>{/if}
								{#each e.tags as t (t)}<span class="badge">{t}</span>{/each}
							</div>
						</td>
						<td>
							<div class="flex justify-end gap-1.5">
								{#if p.gateway && gateway}
									<a class="btn btn-primary" href="/gateway/{e.id}" title="เปิดในเบราว์เซอร์ผ่าน Aegiron gateway">เปิดในเบราว์เซอร์</a>
									<button class="btn" title="เปิดด้วยโปรแกรมในเครื่อง" onclick={() => launchNative(e)}><Icon name="link" /></button>
								{:else}
									<button class="btn btn-primary" onclick={() => launchNative(e)}>
										{p.kind === 'vendor' ? `เปิดใน ${p.label}` : 'เปิดด้วยโปรแกรม'}
									</button>
								{/if}
								{#if canEdit(e)}
									<button class="btn" title="แก้ไข" onclick={() => edit(e)}><Icon name="edit" /></button>
								{/if}
							</div>
						</td>
					</tr>
				{:else}
					<tr><td colspan="5" class="py-10 text-center text-muted">ไม่พบรายการที่ตรงกับตัวกรอง</td></tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}

{#if toast}
	<div class="fixed bottom-5 left-1/2 z-30 -translate-x-1/2 rounded-lg border border-line bg-surface px-4 py-2 text-sm shadow-lg">{toast}</div>
{/if}

{#if form}
	{@const p = providers[form.provider]}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (form = null)}>
		<form class="card max-h-[92vh] w-full max-w-lg space-y-4 overflow-y-auto p-6 shadow-xl" onsubmit={save}>
			<h3 class="text-lg font-semibold">{form.id ? 'แก้ไขการเชื่อมต่อ' : 'เพิ่มการเชื่อมต่อ'}</h3>
			<div>
				<span class="label">ประเภท</span>
				<div class="grid grid-cols-3 gap-1.5">
					{#each Object.entries(providers) as [key, info] (key)}
						<label
							class="flex cursor-pointer items-center justify-center rounded-lg border px-2 py-2 text-sm {form.provider === key
								? 'border-accent bg-accent-soft font-medium text-accent'
								: 'border-line hover:bg-raised'} {form.synced ? 'pointer-events-none opacity-60' : ''}"
						>
							<input type="radio" class="sr-only" value={key} bind:group={form.provider} disabled={form.synced} />
							{info.label}
						</label>
					{/each}
				</div>
			</div>
			<div><label class="label" for="ep-name">ชื่อ</label><input id="ep-name" class="input" required bind:value={form.name} /></div>
			<div class="grid grid-cols-[1fr_auto] gap-2">
				<div>
					<label class="label" for="ep-addr">{p.addressLabel}</label>
					<input id="ep-addr" class="input mono" required placeholder={p.addressHint} disabled={form.synced} bind:value={form.address} />
				</div>
				{#if p.kind === 'protocol'}
					<div>
						<label class="label" for="ep-port">พอร์ต</label>
						<input id="ep-port" class="input mono w-24" inputmode="numeric" placeholder={String(defaultPorts[form.provider])} bind:value={form.port} />
					</div>
				{/if}
			</div>
			{#if p.kind === 'protocol'}
				<div><label class="label" for="ep-user">ชื่อผู้ใช้</label><input id="ep-user" class="input" autocomplete="off" bind:value={form.username} /></div>
			{/if}
			{#if form.provider === 'ssh'}
				<div>
					<label class="label" for="ep-sshauth">ยืนยันตัวตนด้วย</label>
					<select id="ep-sshauth" class="input" bind:value={form.sshAuth}>
						<option value="password">รหัสผ่าน</option><option value="key">กุญแจ SSH (private key)</option>
					</select>
				</div>
			{/if}
			<div>
				{#if form.provider === 'ssh' && form.sshAuth === 'key'}
					<label class="label" for="ep-pw">Private key (OpenSSH หรือ PEM ที่ไม่มี passphrase)</label>
					<textarea
						id="ep-pw"
						class="input mono text-xs"
						rows="5"
						autocomplete="off"
						spellcheck="false"
						placeholder={form.hasSecret ? 'มีกุญแจเก็บไว้แล้ว — เว้นว่างถ้าไม่เปลี่ยน' : '-----BEGIN OPENSSH PRIVATE KEY-----'}
						disabled={form.clearPassword}
						bind:value={form.password}
					></textarea>
				{:else}
					<label class="label" for="ep-pw">
						รหัสผ่าน {p.kind === 'vendor' ? '(รหัส unattended ของเครื่อง)' : ''}
					</label>
					<input
						id="ep-pw"
						class="input"
						type="password"
						autocomplete="new-password"
						placeholder={form.hasSecret ? 'มีรหัสผ่านเก็บไว้แล้ว — เว้นว่างถ้าไม่เปลี่ยน' : 'ไม่บังคับ'}
						disabled={form.clearPassword}
						bind:value={form.password}
					/>
				{/if}
				{#if form.hasSecret}
					<label class="mt-1 flex items-center gap-2 text-xs text-muted"><input type="checkbox" bind:checked={form.clearPassword} /> ลบรหัสผ่านที่เก็บไว้</label>
				{/if}
				<p class="mt-1 text-xs text-muted">
					{form.provider === 'ssh' && form.sshAuth === 'key'
						? 'ใช้ผ่าน gateway เท่านั้น ไม่ถูกส่งไปที่เบราว์เซอร์และไม่ถูกคัดลอกเมื่อเปิดด้วยโปรแกรมในเครื่อง'
						: p.gateway
							? 'ผ่าน gateway เซิร์ฟเวอร์ใส่รหัสให้เอง ไม่ถูกส่งไปที่เบราว์เซอร์ ส่วนการเปิดด้วยโปรแกรมในเครื่องจะคัดลอกรหัสให้เฉพาะผู้ดูแลและผู้สร้างรายการ'
							: 'เมื่อกดเปิด จะคัดลอกไปที่คลิปบอร์ดให้วางในโปรแกรม (ไม่ใส่ในลิงก์)'}
				</p>
			</div>
			{#if form.provider === 'rdp'}
				<fieldset class="grid grid-cols-2 gap-3 rounded-lg bg-raised p-3">
					<div>
						<label class="label" for="ep-sec">ความปลอดภัย</label>
						<select id="ep-sec" class="input" bind:value={form.security}>
							<option value="any">อัตโนมัติ</option><option value="nla">NLA</option><option value="tls">TLS</option><option value="rdp">RDP</option>
						</select>
					</div>
					<div><label class="label" for="ep-dom">Domain</label><input id="ep-dom" class="input" bind:value={form.domain} /></div>
					<label class="col-span-2 flex items-center gap-2 text-sm">
						<input type="checkbox" bind:checked={form.ignoreCert} /> ไม่ตรวจใบรับรองของเซิร์ฟเวอร์ (เฉพาะเครื่องในเครือข่ายภายในที่ใช้ self-signed)
					</label>
				</fieldset>
			{/if}
			{#if isAdmin}
				<div>
					<label class="label" for="ep-group">แชร์กับกลุ่ม</label>
					<select id="ep-group" class="input" bind:value={form.group}>
						<option value="">ไม่แชร์ (เห็นเฉพาะผู้สร้างและผู้ดูแล)</option>
						{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
					</select>
				</div>
			{/if}
			<div><label class="label" for="ep-tags">แท็ก (คั่นด้วย ,)</label><input id="ep-tags" class="input" bind:value={form.tags} /></div>
			<div><label class="label" for="ep-note">หมายเหตุ</label><textarea id="ep-note" class="input" rows="2" bind:value={form.note}></textarea></div>
			<div class="flex justify-between gap-2 border-t border-line pt-4">
				{#if form.id}
					<button type="button" class="btn btn-danger" onclick={() => remove(endpoints.find((e) => e.id === form!.id)!)}><Icon name="trash" /> ลบ</button>
				{:else}<span></span>{/if}
				<div class="flex gap-2">
					<button type="button" class="btn" onclick={() => (form = null)}>ยกเลิก</button>
					<button class="btn btn-primary">บันทึก</button>
				</div>
			</div>
		</form>
	</div>
{/if}

{#if importing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (importing = null)}>
		<form class="card max-h-[92vh] w-full max-w-2xl space-y-4 overflow-y-auto p-6 shadow-xl" onsubmit={runImport}>
			<h3 class="text-lg font-semibold">นำเข้าจาก CSV</h3>
			<p class="text-sm text-muted">
				ใช้ย้ายรายชื่อเครื่องจาก AnyDesk / TeamViewer / RustDesk หรือรายชื่อเซิร์ฟเวอร์ คอลัมน์ที่ต้องมี: <code class="mono">name, provider, address</code>
				ส่วน <code class="mono">port, username, password, tags (คั่นด้วย ;), note</code> ใส่หรือไม่ก็ได้
			</p>
			<pre class="mono overflow-x-auto rounded-lg bg-raised p-3 text-xs">{csvTemplate}</pre>
			<input type="file" accept=".csv,text/csv" class="text-sm" onchange={pickCsv} />
			<textarea class="input mono text-xs" rows="8" placeholder="หรือวาง CSV ที่นี่" bind:value={importing.csv}></textarea>
			<div>
				<label class="label" for="imp-group">ใส่เข้ากลุ่ม</label>
				<select id="imp-group" class="input" bind:value={importing.group}>
					<option value="">ไม่กำหนด</option>
					{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
				</select>
			</div>
			{#if importing.result}
				<div class="rounded-lg bg-raised p-3 text-sm">
					<p class="font-medium text-ok">นำเข้าแล้ว {importing.result.created} รายการ</p>
					{#each importing.result.errors as err (err)}<p class="text-xs text-danger">{err}</p>{/each}
				</div>
			{/if}
			<div class="flex justify-end gap-2">
				<button type="button" class="btn" onclick={() => (importing = null)}>ปิด</button>
				<button class="btn btn-primary" disabled={!importing.csv.trim()}>นำเข้า</button>
			</div>
		</form>
	</div>
{/if}

{#if secretDialog}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation">
		<div class="card w-full max-w-sm space-y-4 p-6 shadow-xl">
			<h3 class="font-semibold">รหัสผ่านของ {secretDialog.name}</h3>
			<p class="text-sm text-muted">เบราว์เซอร์ไม่อนุญาตให้คัดลอกอัตโนมัติ กดคัดลอกแล้ววางในหน้าต่างโปรแกรม</p>
			<code class="mono block rounded-lg bg-raised p-3 text-center">{secretDialog.shown ? secretDialog.secret : '••••••••'}</code>
			<div class="flex justify-end gap-2">
				<button class="btn" onclick={() => (secretDialog!.shown = !secretDialog!.shown)}>{secretDialog.shown ? 'ซ่อน' : 'แสดง'}</button>
				<button
					class="btn btn-primary"
					onclick={async () => {
						await navigator.clipboard.writeText(secretDialog!.secret).catch(() => {});
						secretDialog = null;
						flash('คัดลอกแล้ว');
					}}>คัดลอกและปิด</button
				>
			</div>
		</div>
	</div>
{/if}
