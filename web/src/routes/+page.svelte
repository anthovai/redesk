<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, cleanDeviceId, validDeviceId, wakeDevice, type AddressEntry, type Device } from '#lib/api.js';
	import { ago, formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';
	import PeerCard, { type PeerAction } from '#lib/PeerCard.svelte';
	import { saveView, savedView } from '#lib/peer.js';
	import ViewToggle from '#lib/ViewToggle.svelte';

	let entries = $state<AddressEntry[]>([]);
	let shared = $state<Device[]>([]);
	let loading = $state(true);
	let error = $state('');
	let connectId = $state('');
	let filter = $state('');

	// Add / edit dialog
	let editing = $state<{ id: string; alias: string; tags: string; favorite: boolean; isNew: boolean } | null>(null);

	async function load() {
		try {
			const [ab, mine] = await Promise.all([
				api.get<{ entries: AddressEntry[] }>('/api/address-book'),
				api.get<{ devices: Device[] }>('/api/my-devices')
			]);
			entries = ab.entries;
			shared = mine.devices;
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

	const cleanId = cleanDeviceId;
	const shown = $derived(
		entries.filter((e) => {
			const q = filter.trim().toLowerCase();
			if (!q) return true;
			return [e.device_id, e.alias, e.device?.hostname ?? '', ...e.tags].some((v) => v.toLowerCase().includes(q));
		})
	);

	function quickConnect(e: SubmitEvent) {
		e.preventDefault();
		const id = cleanId(connectId);
		if (validDeviceId(id)) goto(`/connect/${id}`);
	}

	async function save() {
		if (!editing) return;
		const id = cleanId(editing.id);
		try {
			await api.put(`/api/address-book/${id}`, {
				alias: editing.alias,
				tags: editing.tags.split(',').map((t) => t.trim()).filter(Boolean),
				favorite: editing.favorite
			});
			editing = null;
			await load();
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function remove(id: string) {
		if (!confirm(`ลบ ${formatId(id)} ออกจากสมุดที่อยู่?`)) return;
		await api.del(`/api/address-book/${id}`);
		await load();
	}

	let toast = $state<{ ok: boolean; message: string } | null>(null);
	let waking = $state('');
	async function wake(id: string) {
		waking = id;
		toast = await wakeDevice(id);
		waking = '';
		setTimeout(() => (toast = null), 4000);
	}

	let view = $state(savedView('aegiron.home.view'));
	const setView = (v: 'grid' | 'list') => saveView('aegiron.home.view', v);

	const editEntry = (e: AddressEntry) => (editing = { id: e.device_id, alias: e.alias, tags: e.tags.join(', '), favorite: e.favorite, isNew: false });
	const addShared = (d: Device) => api.put(`/api/address-book/${d.id}`, { alias: '', tags: [], favorite: false }).then(load);

	function entryActions(e: AddressEntry): PeerAction[] {
		const d = e.device;
		return [
			{ label: 'เชื่อมต่อ', icon: 'link', href: `/connect/${e.device_id}`, disabled: !d?.online },
			...(d?.can_wake ? [{ label: 'ปลุกเครื่อง', icon: 'power', onclick: () => wake(e.device_id), disabled: waking === d.id } satisfies PeerAction] : []),
			{ label: e.favorite ? 'นำออกจากรายการโปรด' : 'เพิ่มในรายการโปรด', icon: 'star', onclick: () => toggleFavorite(e), divider: true },
			{ label: 'แก้ไข', icon: 'edit', onclick: () => editEntry(e) },
			{ label: 'ลบ', icon: 'trash', onclick: () => remove(e.device_id), danger: true, divider: true }
		];
	}

	function sharedActions(d: Device): PeerAction[] {
		return [
			{ label: 'เชื่อมต่อ', icon: 'link', href: `/connect/${d.id}`, disabled: !d.online },
			...(d.can_wake ? [{ label: 'ปลุกเครื่อง', icon: 'power', onclick: () => wake(d.id), disabled: waking === d.id } satisfies PeerAction] : []),
			...(!entries.some((e) => e.device_id === d.id) ? [{ label: 'เพิ่มในสมุดที่อยู่', icon: 'book', onclick: () => addShared(d) } satisfies PeerAction] : [])
		];
	}

	async function toggleFavorite(e: AddressEntry) {
		await api.put(`/api/address-book/${e.device_id}`, { alias: e.alias, tags: e.tags, favorite: !e.favorite });
		await load();
	}
</script>

<svelte:head><title>สมุดที่อยู่ · Aegiron</title></svelte:head>

<section class="card mb-8 overflow-hidden">
	<div class="grid gap-6 p-6 md:grid-cols-[1fr_auto] md:items-end md:p-8">
		<div>
			<h1 class="text-2xl font-semibold tracking-tight">เชื่อมต่อเครื่องปลายทาง</h1>
			<p class="mt-1 text-sm text-muted">ใส่ ID ที่แสดงในโปรแกรม Aegiron ของเครื่องที่ต้องการควบคุม (ตัวเลข 9 หลัก หรือ ID ที่ผู้ดูแลตั้งให้)</p>
		</div>
		<form class="flex w-full gap-2 md:w-[26rem]" onsubmit={quickConnect}>
			<input
				class="input mono text-lg tracking-wider"
				placeholder="123 456 789"
				aria-label="Device ID"
				bind:value={connectId}
			/>
			<button class="btn btn-primary shrink-0 px-5" disabled={!validDeviceId(cleanId(connectId))}>
				เชื่อมต่อ <Icon name="link" />
			</button>
		</form>
	</div>
</section>

{#if shared.length}
	<section class="mb-8">
		<div class="mb-4 flex flex-wrap items-end justify-between gap-3">
			<div>
				<h2 class="mb-1 text-lg font-semibold">อุปกรณ์ที่แชร์กับฉัน</h2>
				<p class="text-sm text-muted">จากกลุ่มอุปกรณ์ที่ผู้ดูแลระบบเพิ่มคุณเป็นสมาชิก</p>
			</div>
			<ViewToggle bind:value={view} onchange={setView} />
		</div>
		{#if view === 'grid'}
			<div class="grid grid-cols-[repeat(auto-fill,minmax(190px,1fr))] gap-4">
				{#each shared as d (d.id)}
					<PeerCard
						id={d.id}
						name={d.alias || d.hostname}
						hostname={d.hostname}
						os={d.os}
						online={d.online}
						canConnect={d.online}
						detail={d.online ? d.os : `เห็นล่าสุด ${ago(d.last_seen_at)}`}
						actions={sharedActions(d)}
					/>
				{/each}
			</div>
		{:else}
		<div class="card divide-y divide-line">
			{#each shared as d (d.id)}
				<div class="flex items-center gap-3 px-4 py-3">
					<span class="size-2 shrink-0 rounded-full {d.online ? 'bg-ok' : 'bg-line'}" title={d.online ? 'ออนไลน์' : 'ออฟไลน์'}></span>
					<div class="min-w-0 flex-1">
						<p class="truncate font-medium">{d.alias || d.hostname}</p>
						<p class="text-xs text-muted"><span class="mono">{formatId(d.id)}</span> · {d.online ? d.os : `เห็นล่าสุด ${ago(d.last_seen_at)}`}</p>
					</div>
					{#if !entries.some((e) => e.device_id === d.id)}
						<button class="btn" title="เพิ่มในสมุดที่อยู่" onclick={() => addShared(d)}>
							<Icon name="book" />
						</button>
					{/if}
					{#if d.can_wake}
						<button class="btn" disabled={waking === d.id} onclick={() => wake(d.id)}>ปลุกเครื่อง</button>
					{/if}
					<a class="btn btn-primary {d.online ? '' : 'pointer-events-none opacity-50'}" href="/connect/{d.id}">เชื่อมต่อ</a>
				</div>
			{/each}
		</div>
		{/if}
	</section>
{/if}

<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
	<h2 class="text-lg font-semibold">สมุดที่อยู่ของฉัน</h2>
	<div class="flex gap-2">
		<div class="relative">
			<Icon name="search" class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
			<input class="input w-56 pl-9" placeholder="ค้นหา ID, ชื่อ, แท็ก" bind:value={filter} />
		</div>
		<button class="btn" onclick={() => (editing = { id: '', alias: '', tags: '', favorite: false, isNew: true })}>
			<Icon name="plus" /> เพิ่ม
		</button>
		<ViewToggle bind:value={view} onchange={setView} />
	</div>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

{#if loading}
	<p class="text-sm text-muted">กำลังโหลด…</p>
{:else if entries.length === 0}
	<div class="card grid place-items-center gap-2 px-6 py-14 text-center">
		<Icon name="book" size={28} class="text-muted" />
		<p class="font-medium">ยังไม่มีเครื่องในสมุดที่อยู่</p>
		<p class="max-w-sm text-sm text-muted">เพิ่ม ID ของเครื่องที่ใช้บ่อย เพื่อดูสถานะออนไลน์และเชื่อมต่อได้ในคลิกเดียว</p>
	</div>
{:else if view === 'grid'}
	<div class="grid grid-cols-[repeat(auto-fill,minmax(190px,1fr))] gap-4">
		{#each shown as e (e.device_id)}
			{@const d = e.device}
			<PeerCard
				id={e.device_id}
				name={e.alias || d?.alias || d?.hostname || 'ไม่มีชื่อ'}
				hostname={d?.hostname ?? ''}
				os={d?.os ?? ''}
				online={!!d?.online}
				canConnect={!!d?.online}
				favorite={e.favorite}
				status={d ? '' : 'ยังไม่เคยลงทะเบียน'}
				detail={[!d ? '' : d.online ? `ออนไลน์ · ${d.os}` : `เห็นล่าสุด ${ago(d.last_seen_at)}`, e.tags.length ? `แท็ก: ${e.tags.join(', ')}` : ''].filter(Boolean).join('\n')}
				actions={entryActions(e)}
			/>
		{/each}
	</div>
{:else}
	<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
		{#each shown as e (e.device_id)}
			{@const d = e.device}
			<article class="card flex flex-col p-4">
				<div class="flex items-start gap-3">
					<div class="relative grid size-10 shrink-0 place-items-center rounded-lg bg-raised text-muted">
						<Icon name="monitor" size={20} />
						<span
							class="absolute -right-0.5 -bottom-0.5 size-3 rounded-full border-2 border-surface {d?.online ? 'bg-ok' : 'bg-line'}"
							title={d?.online ? 'ออนไลน์' : 'ออฟไลน์'}
						></span>
					</div>
					<div class="min-w-0 flex-1">
						<p class="truncate font-medium">{e.alias || d?.alias || d?.hostname || 'ไม่มีชื่อ'}</p>
						<p class="mono text-sm text-muted">{formatId(e.device_id)}</p>
					</div>
					<button class="btn btn-ghost p-1.5 {e.favorite ? 'text-accent' : 'text-muted'}" title="รายการโปรด" onclick={() => toggleFavorite(e)}>
						<Icon name="star" />
					</button>
				</div>
				<p class="mt-3 text-xs text-muted">
					{#if !d}ยังไม่เคยลงทะเบียน{:else if d.online}ออนไลน์ · {d.os}{:else}เห็นล่าสุด {ago(d.last_seen_at)}{/if}
				</p>
				{#if e.tags.length}
					<div class="mt-2 flex flex-wrap gap-1">
						{#each e.tags as t (t)}<span class="badge">{t}</span>{/each}
					</div>
				{/if}
				<div class="mt-4 flex gap-2 pt-1">
					{#if d?.can_wake}
						<button class="btn flex-1" title="ส่ง Wake-on-LAN ผ่านเครื่องที่ออนไลน์ในเครือข่ายเดียวกัน" disabled={waking === d.id} onclick={() => wake(e.device_id)}>
							ปลุกเครื่อง
						</button>
					{:else}
						<a class="btn btn-primary flex-1 {d?.online ? '' : 'pointer-events-none opacity-50'}" href="/connect/{e.device_id}">เชื่อมต่อ</a>
					{/if}
					<button
						class="btn"
						title="แก้ไข"
						onclick={() => editEntry(e)}
					>
						<Icon name="edit" />
					</button>
					<button class="btn" title="ลบ" onclick={() => remove(e.device_id)}><Icon name="trash" /></button>
				</div>
			</article>
		{/each}
	</div>
{/if}

{#if toast}
	<div class="fixed bottom-5 left-1/2 z-30 -translate-x-1/2 rounded-lg border border-line bg-surface px-4 py-2 text-sm shadow-lg {toast.ok ? '' : 'text-danger'}" role="status">
		{toast.message}
	</div>
{/if}

{#if editing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (editing = null)}>
		<form
			class="card w-full max-w-md space-y-4 p-6 shadow-xl"
			onsubmit={(ev) => {
				ev.preventDefault();
				save();
			}}
		>
			<h3 class="text-lg font-semibold">{editing.isNew ? 'เพิ่มเครื่อง' : 'แก้ไขเครื่อง'}</h3>
			<div>
				<label class="label" for="ab-id">Device ID</label>
				<input id="ab-id" class="input mono" disabled={!editing.isNew} required bind:value={editing.id} />
			</div>
			<div>
				<label class="label" for="ab-alias">ชื่อเรียก</label>
				<input id="ab-alias" class="input" placeholder="เช่น เครื่องบัญชี ชั้น 3" bind:value={editing.alias} />
			</div>
			<div>
				<label class="label" for="ab-tags">แท็ก (คั่นด้วย ,)</label>
				<input id="ab-tags" class="input" placeholder="office, finance" bind:value={editing.tags} />
			</div>
			<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={editing.favorite} /> รายการโปรด</label>
			<div class="flex justify-end gap-2 pt-2">
				<button type="button" class="btn" onclick={() => (editing = null)}>ยกเลิก</button>
				<button class="btn btn-primary" disabled={!validDeviceId(cleanId(editing.id))}>บันทึก</button>
			</div>
		</form>
	</div>
{/if}
