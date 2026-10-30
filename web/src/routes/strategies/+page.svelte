<script lang="ts">
	import { api, type Device, type Group, type Strategy, type StrategySettings } from '#lib/api.js';
	import { formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';

	type BoolKey = Exclude<keyof StrategySettings, 'approve_mode'>;
	const boolRows: { key: BoolKey; label: string; hint: string }[] = [
		{ key: 'allow_permanent_password', label: 'อนุญาตรหัสผ่านถาวร', hint: 'ปิด = ใช้รหัสผ่านครั้งเดียวเท่านั้น และลบรหัสถาวรที่ตั้งไว้' },
		{ key: 'allow_input', label: 'ควบคุมเมาส์และคีย์บอร์ด', hint: 'ปิด = ดูหน้าจอได้อย่างเดียว' },
		{ key: 'allow_clipboard', label: 'คลิปบอร์ด', hint: '' },
		{ key: 'allow_file_transfer', label: 'รับ-ส่งไฟล์', hint: '' },
		{ key: 'allow_audio', label: 'ส่งเสียง', hint: '' },
		{ key: 'allow_whiteboard', label: 'วาดบนหน้าจอ', hint: '' },
		{ key: 'allow_resolution', label: 'เปลี่ยนความละเอียดหน้าจอ', hint: '' },
		{ key: 'allow_tunnel', label: 'TCP tunnel', hint: '' },
		{ key: 'allow_tunnel_lan', label: 'tunnel ไปเครื่องอื่นใน LAN', hint: 'ปิด = tunnel ไปได้เฉพาะ localhost ของเครื่องนั้น' },
		{ key: 'allow_terminal', label: 'เทอร์มินัล', hint: '' },
		{ key: 'direct_enabled', label: 'เชื่อมต่อโดยตรง (LAN / P2P)', hint: 'ปิด = ทุกเซสชันผ่าน relay ของเซิร์ฟเวอร์' },
		{ key: 'lan_discovery', label: 'ให้เครื่องในวงแลนค้นพบ', hint: '' },
		{ key: 'lock_after_session', label: 'ล็อกหน้าจอเมื่อจบเซสชัน', hint: '' },
		{ key: 'record_sessions', label: 'บันทึกทุกเซสชัน', hint: 'บังคับที่เซิร์ฟเวอร์ ไม่ว่าตั้งค่าบันทึกเซสชันไว้อย่างไร' }
	];

	let strategies = $state<Strategy[]>([]);
	let groups = $state<Group[]>([]);
	let devices = $state<Device[]>([]);
	let error = $state('');
	let editing = $state<{ id: number; name: string; description: string; settings: StrategySettings; groups: Set<number> } | null>(null);

	async function load() {
		try {
			const [s, g, d] = await Promise.all([
				api.get<{ strategies: Strategy[] }>('/api/strategies'),
				api.get<{ groups: Group[] }>('/api/groups'),
				api.get<{ devices: Device[] }>('/api/devices')
			]);
			strategies = s.strategies;
			groups = g.groups;
			devices = d.devices;
			error = '';
		} catch (e) {
			error = (e as Error).message;
		}
	}
	$effect(() => {
		load();
	});

	const groupName = (id: number) => groups.find((g) => g.id === id)?.name ?? `#${id}`;
	/** Devices the strategy applies to: assigned directly, or through a group without an own one. */
	const coverage = (s: Strategy) =>
		devices.filter((d) => d.strategy_id === s.id || ((d.strategy_id ?? null) === null && d.group_id !== null && s.group_ids.includes(d.group_id))).length;

	function summary(s: StrategySettings): string[] {
		const out: string[] = [];
		if (s.approve_mode) out.push({ password: 'ใช้รหัสผ่าน', click: 'ต้องกดอนุญาต', both: 'รหัสผ่าน + กดอนุญาต' }[s.approve_mode]);
		for (const r of boolRows) {
			const v = s[r.key];
			if (v !== undefined) out.push(`${r.label}: ${v ? 'เปิด' : 'ปิด'}`);
		}
		return out;
	}

	function edit(s?: Strategy) {
		editing = s
			? { id: s.id, name: s.name, description: s.description, settings: { ...s.settings }, groups: new Set(s.group_ids) }
			: { id: 0, name: '', description: '', settings: {}, groups: new Set() };
	}

	function setBool(key: BoolKey, v: string) {
		if (!editing) return;
		const s = { ...editing.settings };
		if (v === '') delete s[key];
		else s[key] = v === 'on';
		editing.settings = s;
	}

	function toggleGroup(id: number) {
		if (!editing) return;
		const g = new Set(editing.groups);
		if (g.has(id)) g.delete(id);
		else g.add(id);
		editing.groups = g;
	}

	async function save(ev: SubmitEvent) {
		ev.preventDefault();
		const e = editing!;
		const body = { name: e.name, description: e.description, settings: e.settings, group_ids: [...e.groups] };
		try {
			if (e.id) await api.put(`/api/strategies/${e.id}`, body);
			else await api.post('/api/strategies', body);
			editing = null;
			await load();
		} catch (err) {
			error = (err as Error).message;
		}
	}

	async function remove(s: Strategy) {
		if (!confirm(`ลบนโยบาย "${s.name}"? เครื่องที่ใช้นโยบายนี้จะกลับไปใช้การตั้งค่าของเครื่องเอง (ค่าเดิมยังอยู่ แต่ผู้ใช้แก้ไขได้)`)) return;
		try {
			await api.del(`/api/strategies/${s.id}`);
			editing = null;
			await load();
		} catch (err) {
			error = (err as Error).message;
		}
	}
</script>

<svelte:head><title>นโยบายอุปกรณ์ · Aegiron</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">นโยบายอุปกรณ์</h1>
		<p class="mt-1 max-w-2xl text-sm text-muted">
			ชุดการตั้งค่าที่บังคับใช้กับเครื่องในกลุ่ม หรือกับเครื่องเดียว (ตั้งที่หน้าอุปกรณ์ และมีผลก่อนนโยบายของกลุ่ม) ค่าที่ไม่ได้กำหนดผู้ใช้ปรับเองได้ ค่าที่กำหนดจะแสดงในแอปว่า "จัดการโดยองค์กรของคุณ" และแก้ไขที่เครื่องไม่ได้
		</p>
	</div>
	<button class="btn btn-primary" onclick={() => edit()}><Icon name="plus" /> สร้างนโยบาย</button>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

{#if strategies.length === 0}
	<div class="card grid place-items-center gap-2 px-6 py-14 text-center">
		<Icon name="shield" size={28} class="text-muted" />
		<p class="font-medium">ยังไม่มีนโยบาย</p>
		<p class="max-w-sm text-sm text-muted">เช่น "เครื่อง kiosk": ต้องกดอนุญาตทุกครั้ง, ห้ามรับ-ส่งไฟล์, ใช้ relay เท่านั้น และบันทึกทุกเซสชัน</p>
	</div>
{:else}
	<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
		{#each strategies as s (s.id)}
			<article class="card flex flex-col p-4">
				<div class="flex items-start justify-between gap-2">
					<div class="min-w-0">
						<h2 class="truncate font-medium">{s.name}</h2>
						{#if s.description}<p class="text-sm text-muted">{s.description}</p>{/if}
					</div>
					<span class="badge shrink-0" title="จำนวนเครื่องที่ใช้นโยบายนี้"><Icon name="monitor" size={12} /> {coverage(s)}</span>
				</div>
				<ul class="mt-3 flex-1 space-y-0.5 text-sm">
					{#each summary(s.settings) as line (line)}<li>{line}</li>{:else}<li class="text-muted">ไม่ได้กำหนดค่าใด</li>{/each}
				</ul>
				<p class="mt-3 text-xs text-muted">
					กลุ่ม: {s.group_ids.map(groupName).join(', ') || '—'}
					{#if s.device_ids.length}<br />เครื่อง: {s.device_ids.map(formatId).join(', ')}{/if}
				</p>
				<div class="mt-4 flex gap-2">
					<button class="btn flex-1" onclick={() => edit(s)}><Icon name="edit" /> แก้ไข</button>
				</div>
			</article>
		{/each}
	</div>
{/if}

{#if editing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (editing = null)}>
		<form class="card flex max-h-[92vh] w-full max-w-xl flex-col gap-4 overflow-y-auto p-6 shadow-xl" onsubmit={save}>
			<h3 class="text-lg font-semibold">{editing.id ? 'แก้ไขนโยบาย' : 'สร้างนโยบาย'}</h3>
			<div><label class="label" for="s-name">ชื่อ</label><input id="s-name" class="input" required maxlength="64" bind:value={editing.name} /></div>
			<div><label class="label" for="s-desc">คำอธิบาย</label><input id="s-desc" class="input" bind:value={editing.description} /></div>
			<div class="divide-y divide-line rounded-lg border border-line">
				<div class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
					<label for="s-approve">การยอมรับการเชื่อมต่อ</label>
					<select
						id="s-approve"
						class="input w-44"
						value={editing.settings.approve_mode ?? ''}
						onchange={(ev) => {
							const v = (ev.currentTarget as HTMLSelectElement).value;
							const s = { ...editing!.settings };
							if (v) s.approve_mode = v as StrategySettings['approve_mode'];
							else delete s.approve_mode;
							editing!.settings = s;
						}}
					>
						<option value="">ไม่กำหนด</option>
						<option value="password">ใช้รหัสผ่าน</option>
						<option value="click">กดอนุญาต</option>
						<option value="both">ทั้งสองอย่าง</option>
					</select>
				</div>
				{#each boolRows as r (r.key)}
					<div class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
						<label for="s-{r.key}" class="min-w-0">
							{r.label}
							{#if r.hint}<span class="block text-xs text-muted">{r.hint}</span>{/if}
						</label>
						<select
							id="s-{r.key}"
							class="input w-44 shrink-0"
							value={editing.settings[r.key] === undefined ? '' : editing.settings[r.key] ? 'on' : 'off'}
							onchange={(ev) => setBool(r.key, (ev.currentTarget as HTMLSelectElement).value)}
						>
							<option value="">ไม่กำหนด</option>
							<option value="on">เปิด</option>
							{#if r.key !== 'record_sessions'}<option value="off">ปิด</option>{/if}
						</select>
					</div>
				{/each}
			</div>
			<div>
				<p class="label">ใช้กับกลุ่ม ({editing.groups.size})</p>
				<ul class="max-h-48 divide-y divide-line overflow-y-auto rounded-lg border border-line">
					{#each groups as g (g.id)}
						<li>
							<label class="flex items-center gap-3 px-3 py-2 text-sm hover:bg-raised">
								<input type="checkbox" checked={editing.groups.has(g.id)} onchange={() => toggleGroup(g.id)} />
								<span class="flex-1">{g.name}</span>
								{#if g.strategy_id && g.strategy_id !== editing.id}<span class="text-xs text-muted">ใช้นโยบายอื่นอยู่ (จะถูกแทนที่)</span>{/if}
							</label>
						</li>
					{:else}
						<li class="px-3 py-2 text-sm text-muted">ยังไม่มีกลุ่ม</li>
					{/each}
				</ul>
				<p class="mt-1 text-xs text-muted">กลุ่มหนึ่งใช้ได้หนึ่งนโยบาย ตั้งนโยบายให้เครื่องเดียวได้ที่หน้าอุปกรณ์ทั้งหมด</p>
			</div>
			<div class="flex justify-between gap-2 border-t border-line pt-4">
				{#if editing.id}
					<button type="button" class="btn btn-danger" onclick={() => remove(strategies.find((s) => s.id === editing!.id)!)}><Icon name="trash" /> ลบ</button>
				{:else}<span></span>{/if}
				<div class="flex gap-2">
					<button type="button" class="btn" onclick={() => (editing = null)}>ยกเลิก</button>
					<button class="btn btn-primary">บันทึก</button>
				</div>
			</div>
		</form>
	</div>
{/if}
