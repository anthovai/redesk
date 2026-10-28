<script lang="ts">
	import { page } from '$app/state';
	import { api, type Device, type Group } from '#lib/api.js';
	import { ago, fingerprint, formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';

	let devices = $state<Device[]>([]);
	let loading = $state(true);
	let error = $state('');
	let filter = $state('');
	let onlyOnline = $state(false);
	let groups = $state<Group[]>([]);
	let groupFilter = $state(page.url.searchParams.get('group') ?? '');
	let editing = $state<{ d: Device; alias: string; note: string; disabled: boolean; group: string; fp: string } | null>(null);
	const groupName = (id: number | null) => groups.find((g) => g.id === id)?.name ?? '';

	async function load() {
		try {
			const [d, g] = await Promise.all([api.get<{ devices: Device[] }>('/api/devices'), api.get<{ groups: Group[] }>('/api/groups')]);
			devices = d.devices;
			groups = g.groups;
			error = '';
		} catch (e) {
			error = (e as Error).message;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load();
		const t = setInterval(load, 10_000);
		return () => clearInterval(t);
	});

	const shown = $derived(
		devices.filter((d) => {
			if (onlyOnline && !d.online) return false;
			if (groupFilter === 'none' ? d.group_id !== null : groupFilter && String(d.group_id) !== groupFilter) return false;
			const q = filter.trim().toLowerCase();
			return !q || [d.id, d.alias, d.hostname, d.os, d.last_ip].some((v) => v.toLowerCase().includes(q));
		})
	);
	const onlineCount = $derived(devices.filter((d) => d.online).length);

	async function open(d: Device) {
		editing = { d, alias: d.alias, note: d.note, disabled: d.disabled, group: d.group_id ? String(d.group_id) : '', fp: d.public_key ? await fingerprint(d.public_key) : '' };
	}

	async function save() {
		if (!editing) return;
		await act(() => api.patch(`/api/devices/${editing!.d.id}`, { alias: editing!.alias, note: editing!.note, disabled: editing!.disabled, group_id: editing!.group ? +editing!.group : null }));
		editing = null;
	}

	async function act(fn: () => Promise<unknown>) {
		try {
			await fn();
			await load();
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function resetKey(d: Device) {
		if (!confirm(`รีเซ็ตกุญแจของ ${formatId(d.id)}?\n\nเครื่องจะถูกตัดการเชื่อมต่อ และการลงทะเบียนครั้งถัดไปจะผูกกุญแจใหม่ ใช้เมื่อติดตั้ง agent ใหม่บนเครื่องเดิม`)) return;
		await act(() => api.post(`/api/devices/${d.id}/reset-key`));
		editing = null;
	}

	async function remove(d: Device) {
		if (!confirm(`ลบอุปกรณ์ ${formatId(d.id)} ออกจากระบบ?`)) return;
		await act(() => api.del(`/api/devices/${d.id}`));
		editing = null;
	}

	async function addToBook(d: Device) {
		await act(() => api.put(`/api/address-book/${d.id}`, { alias: d.alias || d.hostname, tags: [], favorite: false }));
	}
</script>

<svelte:head><title>อุปกรณ์ทั้งหมด · ReDesk</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">อุปกรณ์ทั้งหมด</h1>
		<p class="mt-1 text-sm text-muted">{devices.length} เครื่อง · ออนไลน์ {onlineCount}</p>
	</div>
	<div class="flex flex-wrap items-center gap-3">
		<label class="flex items-center gap-2 text-sm text-muted"><input type="checkbox" bind:checked={onlyOnline} /> เฉพาะออนไลน์</label>
		<select class="input w-auto" bind:value={groupFilter} aria-label="กลุ่ม">
			<option value="">ทุกกลุ่ม</option>
			<option value="none">ไม่มีกลุ่ม</option>
			{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
		</select>
		<div class="relative">
			<Icon name="search" class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
			<input class="input w-64 pl-9" placeholder="ค้นหา ID, ชื่อเครื่อง, IP" bind:value={filter} />
		</div>
	</div>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<div class="card overflow-x-auto">
	<table class="table">
		<thead>
			<tr>
				<th>สถานะ</th>
				<th>ID</th>
				<th>ชื่อ</th>
				<th>กลุ่ม</th>
				<th>ระบบ</th>
				<th>IP ล่าสุด</th>
				<th>เห็นล่าสุด</th>
				<th class="text-right">จัดการ</th>
			</tr>
		</thead>
		<tbody>
			{#if loading}
				<tr><td colspan="8" class="text-muted">กำลังโหลด…</td></tr>
			{:else if shown.length === 0}
				<tr><td colspan="8" class="py-10 text-center text-muted">ไม่พบอุปกรณ์ — ติดตั้ง agent ตามคำแนะนำในหน้า "ตั้งค่าเซิร์ฟเวอร์"</td></tr>
			{/if}
			{#each shown as d (d.id)}
				<tr class={d.disabled ? 'opacity-60' : ''}>
					<td>
						{#if d.disabled}<span class="badge">ปิดใช้งาน</span>
						{:else if !d.public_key}<span class="badge">รอลงทะเบียนใหม่</span>
						{:else}
							<span class="inline-flex items-center gap-1.5 text-xs {d.online ? 'text-ok' : 'text-muted'}">
								<span class="size-2 rounded-full {d.online ? 'bg-ok' : 'bg-line'}"></span>{d.online ? 'ออนไลน์' : 'ออฟไลน์'}
							</span>
						{/if}
					</td>
					<td class="mono whitespace-nowrap">{formatId(d.id)}</td>
					<td>
						<p class="font-medium">{d.alias || d.hostname}</p>
						{#if d.alias}<p class="text-xs text-muted">{d.hostname}</p>{/if}
					</td>
					<td>{#if d.group_id}<span class="badge">{groupName(d.group_id)}</span>{:else}<span class="text-muted">—</span>{/if}</td>
					<td class="text-muted">{d.os} <span class="text-xs">v{d.agent_version}</span></td>
					<td class="mono text-muted">{d.last_ip}</td>
					<td class="whitespace-nowrap text-muted">{d.online ? 'ตอนนี้' : ago(d.last_seen_at)}</td>
					<td>
						<div class="flex justify-end gap-1.5">
							<a class="btn btn-primary {d.online && !d.disabled ? '' : 'pointer-events-none opacity-40'}" href="/connect/{d.id}">เชื่อมต่อ</a>
							<button class="btn" title="เพิ่มในสมุดที่อยู่" onclick={() => addToBook(d)}><Icon name="book" /></button>
							<button class="btn" title="รายละเอียด" onclick={() => open(d)}><Icon name="edit" /></button>
						</div>
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

{#if editing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (editing = null)}>
		<form
			class="card w-full max-w-lg space-y-4 p-6 shadow-xl"
			onsubmit={(ev) => {
				ev.preventDefault();
				save();
			}}
		>
			<div>
				<h3 class="text-lg font-semibold">อุปกรณ์ {formatId(editing.d.id)}</h3>
				<p class="text-sm text-muted">{editing.d.hostname} · {editing.d.os} · ลงทะเบียน {ago(editing.d.created_at)}</p>
			</div>
			<dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-lg bg-raised p-3 text-sm">
				<dt class="text-muted">ลายนิ้วมือกุญแจ</dt>
				<dd class="mono">{editing.fp || '— (ถูกรีเซ็ต)'}</dd>
				<dt class="text-muted">IP ล่าสุด</dt>
				<dd class="mono">{editing.d.last_ip || '—'}</dd>
			</dl>
			<div>
				<label class="label" for="d-alias">ชื่อเรียก (ทุกคนเห็น)</label>
				<input id="d-alias" class="input" bind:value={editing.alias} />
			</div>
			<div>
				<label class="label" for="d-group">กลุ่ม (กำหนดว่าใครเชื่อมต่อได้)</label>
				<select id="d-group" class="input" bind:value={editing.group}>
					<option value="">ไม่มีกลุ่ม</option>
					{#each groups as g (g.id)}<option value={String(g.id)}>{g.name}</option>{/each}
				</select>
			</div>
			<div>
				<label class="label" for="d-note">หมายเหตุ</label>
				<textarea id="d-note" class="input" rows="3" bind:value={editing.note}></textarea>
			</div>
			<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={editing.disabled} /> ปิดใช้งาน (ตัดการเชื่อมต่อและห้ามลงทะเบียน)</label>
			<div class="flex flex-wrap justify-between gap-2 border-t border-line pt-4">
				<div class="flex gap-2">
					<button type="button" class="btn" onclick={() => resetKey(editing!.d)}><Icon name="key" /> รีเซ็ตกุญแจ</button>
					<button type="button" class="btn btn-danger" onclick={() => remove(editing!.d)}><Icon name="trash" /> ลบ</button>
				</div>
				<div class="flex gap-2">
					<button type="button" class="btn" onclick={() => (editing = null)}>ยกเลิก</button>
					<button class="btn btn-primary">บันทึก</button>
				</div>
			</div>
		</form>
	</div>
{/if}
