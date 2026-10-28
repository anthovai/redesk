<script lang="ts">
	import { api, type TrustedDevice, type User } from '#lib/api.js';
	import { ago, dateTime } from '#lib/format.js';
	import { auth } from '#lib/session.svelte.js';

	let users = $state<User[]>([]);
	let error = $state('');
	let creating = $state<{ email: string; name: string; role: 'admin' | 'user'; password: string } | null>(null);
	let editing = $state<{ u: User; name: string; role: 'admin' | 'user'; disabled: boolean; password: string } | null>(null);
	let trusted = $state<TrustedDevice[]>([]);

	async function loadTrusted(u: User) {
		trusted = [];
		try {
			trusted = (await api.get<{ devices: TrustedDevice[] }>(`/api/users/${u.id}/trusted-devices`)).devices;
		} catch (e) {
			error = (e as Error).message;
		}
	}

	function edit(u: User) {
		editing = { u, name: u.name, role: u.role, disabled: u.disabled, password: '' };
		loadTrusted(u);
	}

	async function revokeTrusted(u: User, d?: TrustedDevice) {
		const what = d ? `"${d.label}" ของ ${u.email}` : `ทุกอุปกรณ์ของ ${u.email}`;
		if (!confirm(`เลิกจดจำ${what}? อุปกรณ์จะต้องใส่รหัส 2FA ในการเข้าสู่ระบบครั้งต่อไป`)) return;
		await run(() => api.del(`/api/users/${u.id}/trusted-devices${d ? `/${d.id}` : ''}`));
		await loadTrusted(u);
	}

	async function load() {
		try {
			users = (await api.get<{ users: User[] }>('/api/users')).users;
		} catch (e) {
			error = (e as Error).message;
		}
	}
	$effect(() => {
		load();
	});

	async function run(fn: () => Promise<unknown>) {
		error = '';
		try {
			await fn();
			await load();
			return true;
		} catch (e) {
			error = (e as Error).message;
			return false;
		}
	}

	async function create(ev: SubmitEvent) {
		ev.preventDefault();
		if (await run(() => api.post('/api/users', creating))) creating = null;
	}

	async function update(ev: SubmitEvent) {
		ev.preventDefault();
		const e = editing!;
		const ok = await run(async () => {
			await api.patch(`/api/users/${e.u.id}`, { name: e.name, role: e.role, disabled: e.disabled });
			if (e.password) await api.post(`/api/users/${e.u.id}/password`, { password: e.password });
		});
		if (ok) editing = null;
	}

	async function resetTotp(u: User) {
		if (confirm(`ปิด 2FA ของ ${u.email}? ผู้ใช้จะต้องตั้งค่าใหม่`)) {
			await run(() => api.del(`/api/users/${u.id}/totp`));
			editing = null;
		}
	}

	function randomPassword() {
		const a = new Uint8Array(12);
		crypto.getRandomValues(a);
		return btoa(String.fromCharCode(...a)).replace(/[+/=]/g, '').slice(0, 14);
	}
</script>

<svelte:head><title>ผู้ใช้ · Aegiron</title></svelte:head>

<div class="mb-6 flex items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">ผู้ใช้</h1>
		<p class="mt-1 text-sm text-muted">ผู้ใช้ทั่วไปเชื่อมต่อได้เมื่อรู้ ID และรหัสผ่านของเครื่อง ผู้ดูแลเห็นอุปกรณ์ เซสชัน และ audit ทั้งหมด</p>
	</div>
	<button class="btn btn-primary" onclick={() => (creating = { email: '', name: '', role: 'user', password: randomPassword() })}>เพิ่มผู้ใช้</button>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<div class="card overflow-x-auto">
	<table class="table">
		<thead><tr><th>ผู้ใช้</th><th>บทบาท</th><th>2FA</th><th>เข้าระบบล่าสุด</th><th>สถานะ</th><th></th></tr></thead>
		<tbody>
			{#each users as u (u.id)}
				<tr class={u.disabled ? 'opacity-60' : ''}>
					<td>
						<p class="font-medium">{u.name || '—'}</p>
						<p class="text-xs text-muted">{u.email}</p>
					</td>
					<td>{u.role === 'admin' ? 'ผู้ดูแลระบบ' : 'ผู้ใช้'}</td>
					<td>{#if u.totp_enabled}<span class="text-ok">เปิด</span>{:else}<span class="text-muted">ปิด</span>{/if}</td>
					<td class="text-muted">{ago(u.last_login_at)}</td>
					<td>{u.disabled ? 'ระงับ' : 'ใช้งาน'}</td>
					<td class="text-right">
						<button class="btn" onclick={() => edit(u)}>แก้ไข</button>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

{#if creating}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (creating = null)}>
		<form class="card w-full max-w-md space-y-4 p-6 shadow-xl" onsubmit={create}>
			<h3 class="text-lg font-semibold">เพิ่มผู้ใช้</h3>
			<div><label class="label" for="c-email">อีเมล</label><input id="c-email" class="input" type="email" required bind:value={creating.email} /></div>
			<div><label class="label" for="c-name">ชื่อ</label><input id="c-name" class="input" bind:value={creating.name} /></div>
			<div>
				<label class="label" for="c-role">บทบาท</label>
				<select id="c-role" class="input" bind:value={creating.role}><option value="user">ผู้ใช้</option><option value="admin">ผู้ดูแลระบบ</option></select>
			</div>
			<div>
				<label class="label" for="c-pw">รหัสผ่านเริ่มต้น (อย่างน้อย 10 ตัว)</label>
				<input id="c-pw" class="input mono" minlength="10" required bind:value={creating.password} />
				<p class="mt-1 text-xs text-muted">ส่งให้ผู้ใช้ทางช่องทางที่ปลอดภัย แล้วให้เปลี่ยนเองที่หน้าบัญชี</p>
			</div>
			<div class="flex justify-end gap-2"><button type="button" class="btn" onclick={() => (creating = null)}>ยกเลิก</button><button class="btn btn-primary">สร้าง</button></div>
		</form>
	</div>
{/if}

{#if editing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (editing = null)}>
		<form class="card w-full max-w-md space-y-4 p-6 shadow-xl" onsubmit={update}>
			<h3 class="text-lg font-semibold">{editing.u.email}</h3>
			<div><label class="label" for="e-name">ชื่อ</label><input id="e-name" class="input" bind:value={editing.name} /></div>
			<div>
				<label class="label" for="e-role">บทบาท</label>
				<select id="e-role" class="input" bind:value={editing.role} disabled={editing.u.id === auth.user?.id}>
					<option value="user">ผู้ใช้</option><option value="admin">ผู้ดูแลระบบ</option>
				</select>
			</div>
			<div>
				<label class="label" for="e-pw">ตั้งรหัสผ่านใหม่ (เว้นว่างถ้าไม่เปลี่ยน)</label>
				<input id="e-pw" class="input mono" minlength="10" bind:value={editing.password} />
			</div>
			<label class="flex items-center gap-2 text-sm">
				<input type="checkbox" bind:checked={editing.disabled} disabled={editing.u.id === auth.user?.id} /> ระงับบัญชี (ออกจากระบบทุกอุปกรณ์)
			</label>
			<div class="space-y-2 border-t border-line pt-4">
				<div class="flex items-center justify-between gap-2">
					<p class="text-sm font-medium">อุปกรณ์ที่จดจำไว้ (ข้ามรหัส 2FA)</p>
					{#if trusted.length > 0}<button type="button" class="btn" onclick={() => revokeTrusted(editing!.u)}>เลิกจดจำทั้งหมด</button>{/if}
				</div>
				{#if trusted.length === 0}
					<p class="text-xs text-muted">ไม่มี</p>
				{:else}
					<ul class="space-y-1 text-sm">
						{#each trusted as d (d.id)}
							<li class="flex items-center justify-between gap-2">
								<span class="min-w-0">
									<span class="block truncate">{d.label || 'ไม่ทราบชื่อ'}</span>
									<span class="block text-xs text-muted">ใช้ล่าสุด {ago(d.last_used_at)} · หมดอายุ {dateTime(d.expires_at)}</span>
								</span>
								<button type="button" class="btn shrink-0" onclick={() => revokeTrusted(editing!.u, d)}>เลิกจดจำ</button>
							</li>
						{/each}
					</ul>
				{/if}
			</div>
			<div class="flex flex-wrap justify-between gap-2 border-t border-line pt-4">
				<button type="button" class="btn" disabled={!editing.u.totp_enabled} onclick={() => resetTotp(editing!.u)}>รีเซ็ต 2FA</button>
				<div class="flex gap-2"><button type="button" class="btn" onclick={() => (editing = null)}>ยกเลิก</button><button class="btn btn-primary">บันทึก</button></div>
			</div>
		</form>
	</div>
{/if}
