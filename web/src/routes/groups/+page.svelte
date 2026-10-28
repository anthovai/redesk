<script lang="ts">
	import { api, type Group, type Settings, type User } from '#lib/api.js';
	import Icon from '#lib/Icon.svelte';

	let groups = $state<Group[]>([]);
	let users = $state<User[]>([]);
	let settings = $state<Settings | null>(null);
	let error = $state('');
	let editing = $state<{ id: number; name: string; description: string; members: Set<number> } | null>(null);

	async function load() {
		try {
			const [g, u, s] = await Promise.all([
				api.get<{ groups: Group[] }>('/api/groups'),
				api.get<{ users: User[] }>('/api/users'),
				api.get<{ settings: Settings }>('/api/settings')
			]);
			groups = g.groups;
			users = u.users.filter((x) => !x.disabled);
			settings = s.settings;
		} catch (e) {
			error = (e as Error).message;
		}
	}
	$effect(() => {
		load();
	});

	const userName = (id: number) => {
		const u = users.find((x) => x.id === id);
		return u ? u.name || u.email : `#${id}`;
	};

	function toggle(id: number) {
		if (!editing) return;
		const m = new Set(editing.members);
		if (m.has(id)) m.delete(id);
		else m.add(id);
		editing.members = m;
	}

	async function save(ev: SubmitEvent) {
		ev.preventDefault();
		const e = editing!;
		const body = { name: e.name, description: e.description, member_ids: [...e.members] };
		try {
			if (e.id) await api.put(`/api/groups/${e.id}`, body);
			else await api.post('/api/groups', body);
			editing = null;
			await load();
		} catch (err) {
			error = (err as Error).message;
		}
	}

	async function remove(g: Group) {
		if (!confirm(`ลบกลุ่ม "${g.name}"? อุปกรณ์ ${g.device_count} เครื่องในกลุ่มจะไม่มีกลุ่ม`)) return;
		await api.del(`/api/groups/${g.id}`);
		editing = null;
		await load();
	}
</script>

<svelte:head><title>กลุ่มอุปกรณ์ · ReDesk</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">กลุ่มอุปกรณ์</h1>
		<p class="mt-1 max-w-2xl text-sm text-muted">กำหนดว่าผู้ใช้คนไหนเข้าถึงอุปกรณ์ใดได้ อุปกรณ์อยู่ได้หนึ่งกลุ่ม (ตั้งได้ที่หน้าอุปกรณ์ หรือผูกกับ enrollment token)</p>
	</div>
	<button class="btn btn-primary" onclick={() => (editing = { id: 0, name: '', description: '', members: new Set() })}>
		<Icon name="plus" /> สร้างกลุ่ม
	</button>
</div>

{#if settings && settings.access_policy !== 'groups'}
	<div class="mb-6 rounded-xl border border-line bg-raised p-4 text-sm">
		<p class="font-medium">ตอนนี้นโยบายการเข้าถึงเป็นแบบ "เปิด"</p>
		<p class="mt-1 text-muted">
			ผู้ใช้ทุกคนเชื่อมต่อเครื่องใดก็ได้ถ้ารู้ ID และรหัสผ่าน กลุ่มจะมีผลเมื่อเปลี่ยนเป็น "ตามกลุ่ม" ที่หน้า
			<a href="/settings" class="text-accent underline">ตั้งค่าเซิร์ฟเวอร์</a>
		</p>
	</div>
{/if}

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

{#if groups.length === 0}
	<div class="card grid place-items-center gap-2 px-6 py-14 text-center">
		<Icon name="layers" size={28} class="text-muted" />
		<p class="font-medium">ยังไม่มีกลุ่ม</p>
		<p class="max-w-sm text-sm text-muted">เช่น "บัญชี", "ห้องเซิร์ฟเวอร์", "สาขาเชียงใหม่" แล้วเพิ่มผู้ใช้ที่ดูแลเครื่องกลุ่มนั้น</p>
	</div>
{:else}
	<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
		{#each groups as g (g.id)}
			<article class="card flex flex-col p-4">
				<div class="flex items-start justify-between gap-2">
					<div class="min-w-0">
						<h2 class="truncate font-medium">{g.name}</h2>
						{#if g.description}<p class="text-sm text-muted">{g.description}</p>{/if}
					</div>
					<span class="badge shrink-0"><Icon name="monitor" size={12} /> {g.device_count}</span>
				</div>
				<p class="mt-3 text-xs text-muted">สมาชิก {g.member_ids.length} คน</p>
				<p class="mt-1 line-clamp-2 text-sm">{g.member_ids.map(userName).join(', ') || '—'}</p>
				<div class="mt-4 flex gap-2">
					<a class="btn flex-1" href="/devices?group={g.id}">ดูอุปกรณ์</a>
					<button class="btn" onclick={() => (editing = { id: g.id, name: g.name, description: g.description, members: new Set(g.member_ids) })}>
						<Icon name="edit" /> แก้ไข
					</button>
				</div>
			</article>
		{/each}
	</div>
{/if}

{#if editing}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (editing = null)}>
		<form class="card flex max-h-[90vh] w-full max-w-md flex-col gap-4 p-6 shadow-xl" onsubmit={save}>
			<h3 class="text-lg font-semibold">{editing.id ? 'แก้ไขกลุ่ม' : 'สร้างกลุ่ม'}</h3>
			<div><label class="label" for="g-name">ชื่อกลุ่ม</label><input id="g-name" class="input" required bind:value={editing.name} /></div>
			<div><label class="label" for="g-desc">คำอธิบาย</label><input id="g-desc" class="input" bind:value={editing.description} /></div>
			<div class="min-h-0 flex-1">
				<p class="label">สมาชิก ({editing.members.size})</p>
				<ul class="max-h-64 divide-y divide-line overflow-y-auto rounded-lg border border-line">
					{#each users as u (u.id)}
						<li>
							<label class="flex items-center gap-3 px-3 py-2 text-sm hover:bg-raised">
								<input type="checkbox" checked={editing.members.has(u.id)} onchange={() => toggle(u.id)} />
								<span class="min-w-0 flex-1">
									<span class="block truncate">{u.name || u.email}</span>
									{#if u.name}<span class="block truncate text-xs text-muted">{u.email}</span>{/if}
								</span>
								{#if u.role === 'admin'}<span class="badge">admin</span>{/if}
							</label>
						</li>
					{/each}
				</ul>
				<p class="mt-1 text-xs text-muted">ผู้ดูแลระบบเข้าถึงทุกอุปกรณ์อยู่แล้ว</p>
			</div>
			<div class="flex justify-between gap-2 border-t border-line pt-4">
				{#if editing.id}
					<button type="button" class="btn btn-danger" onclick={() => remove(groups.find((g) => g.id === editing!.id)!)}><Icon name="trash" /> ลบ</button>
				{:else}<span></span>{/if}
				<div class="flex gap-2">
					<button type="button" class="btn" onclick={() => (editing = null)}>ยกเลิก</button>
					<button class="btn btn-primary">บันทึก</button>
				</div>
			</div>
		</form>
	</div>
{/if}
