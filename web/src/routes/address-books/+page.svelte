<script lang="ts">
	import { api, cleanDeviceId, validDeviceId, type AddressBook, type BookEntry, type BookPermission, type BookShare } from '#lib/api.js';
	import { ago, formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';

	type Target = { id: number; name: string; email?: string };

	const permLabel: Record<BookPermission, string> = { read: 'ดูอย่างเดียว', rw: 'อ่าน-เขียน', full: 'เต็มสิทธิ์' };
	const rank = (p: BookPermission) => ({ read: 1, rw: 2, full: 3 })[p];

	let books = $state<AddressBook[]>([]);
	let selected = $state<number | null>(null);
	let entries = $state<BookEntry[]>([]);
	let error = $state('');
	let bookForm = $state<{ id: number; name: string; note: string } | null>(null);
	let entryForm = $state<{ isNew: boolean; id: string; alias: string; tags: string; note: string } | null>(null);
	let shareForm = $state<{ shares: BookShare[]; users: Target[]; groups: Target[]; pick: string; perm: BookPermission } | null>(null);

	const book = $derived(books.find((b) => b.id === selected) ?? null);

	async function loadBooks() {
		try {
			books = (await api.get<{ books: AddressBook[] }>('/api/address-books')).books;
			if (selected === null || !books.some((b) => b.id === selected)) selected = books[0]?.id ?? null;
			error = '';
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function loadEntries(id: number) {
		try {
			entries = (await api.get<{ entries: BookEntry[] }>(`/api/address-books/${id}/entries`)).entries;
		} catch (e) {
			error = (e as Error).message;
		}
	}

	$effect(() => {
		loadBooks();
	});
	$effect(() => {
		if (selected !== null) loadEntries(selected);
		else entries = [];
	});

	async function act(fn: () => Promise<unknown>) {
		try {
			await fn();
			await loadBooks();
			if (selected !== null) await loadEntries(selected);
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function saveBook(ev: SubmitEvent) {
		ev.preventDefault();
		const f = bookForm!;
		await act(async () => {
			if (f.id) await api.patch(`/api/address-books/${f.id}`, { name: f.name, note: f.note });
			else selected = (await api.post<{ id: number }>('/api/address-books', { name: f.name, note: f.note })).id;
		});
		bookForm = null;
	}

	async function deleteBook(b: AddressBook) {
		if (!confirm(`ลบสมุดที่อยู่ "${b.name}" และรายการทั้งหมดในสมุด? ผู้ที่ได้รับแชร์จะไม่เห็นอีก`)) return;
		await act(() => api.del(`/api/address-books/${b.id}`));
		selected = null;
		await loadBooks();
	}

	async function saveEntry(ev: SubmitEvent) {
		ev.preventDefault();
		const f = entryForm!;
		const id = cleanDeviceId(f.id);
		const tags = f.tags.split(',').map((t) => t.trim()).filter(Boolean);
		await act(() => api.put(`/api/address-books/${selected}/entries/${id}`, { alias: f.alias, tags, note: f.note }));
		entryForm = null;
	}

	async function removeEntry(e: BookEntry) {
		if (!confirm(`เอา ${formatId(e.device_id)} ออกจากสมุดนี้?`)) return;
		await act(() => api.del(`/api/address-books/${selected}/entries/${e.device_id}`));
	}

	async function openShares() {
		try {
			const [s, t] = await Promise.all([
				api.get<{ shares: BookShare[] }>(`/api/address-books/${selected}/shares`),
				api.get<{ users: Target[]; groups: Target[] }>('/api/share-targets')
			]);
			shareForm = { shares: s.shares, users: t.users, groups: t.groups, pick: '', perm: 'read' };
		} catch (e) {
			error = (e as Error).message;
		}
	}

	function targetName(sh: BookShare): string {
		if (!shareForm) return '';
		if (sh.group_id !== null) return `กลุ่ม ${shareForm.groups.find((g) => g.id === sh.group_id)?.name ?? '#' + sh.group_id}`;
		const u = shareForm.users.find((x) => x.id === sh.user_id);
		return u ? u.name || u.email || '' : `#${sh.user_id}`;
	}

	function addShare() {
		const f = shareForm!;
		if (!f.pick) return;
		const [kind, raw] = f.pick.split(':');
		const id = +raw;
		const rest = f.shares.filter((s) => (kind === 'u' ? s.user_id !== id : s.group_id !== id));
		f.shares = [...rest, { user_id: kind === 'u' ? id : null, group_id: kind === 'g' ? id : null, permission: f.perm }];
		f.pick = '';
	}

	async function saveShares() {
		await act(() => api.put(`/api/address-books/${selected}/shares`, { shares: shareForm!.shares }));
		shareForm = null;
	}
</script>

<svelte:head><title>สมุดที่อยู่ที่แชร์ · Aegiron</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">สมุดที่อยู่ที่แชร์</h1>
		<p class="mt-1 max-w-2xl text-sm text-muted">
			รายชื่อเครื่องที่ใช้ร่วมกันในทีม แชร์ให้ผู้ใช้หรือกลุ่มแบบดูอย่างเดียว อ่าน-เขียน หรือเต็มสิทธิ์ (แก้ชื่อ ลบ และแชร์ต่อ) การเห็นเครื่องในสมุดไม่ได้ให้สิทธิ์เชื่อมต่อ
			นโยบายการเข้าถึงและรหัสผ่านของเครื่องยังใช้ตามปกติ
		</p>
	</div>
	<button class="btn btn-primary" onclick={() => (bookForm = { id: 0, name: '', note: '' })}><Icon name="plus" /> สร้างสมุด</button>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

{#if books.length === 0}
	<div class="card grid place-items-center gap-2 px-6 py-14 text-center">
		<Icon name="book" size={28} class="text-muted" />
		<p class="font-medium">ยังไม่มีสมุดที่อยู่ที่แชร์</p>
		<p class="max-w-sm text-sm text-muted">สร้างสมุด เช่น "เครื่องสาขาเชียงใหม่" แล้วแชร์ให้ทีมที่ดูแล</p>
	</div>
{:else}
	<div class="grid gap-4 md:grid-cols-[16rem_1fr]">
		<nav class="card h-fit divide-y divide-line">
			{#each books as b (b.id)}
				<button class="block w-full px-4 py-3 text-left hover:bg-raised {b.id === selected ? 'bg-accent-soft' : ''}" onclick={() => (selected = b.id)}>
					<span class="block truncate font-medium {b.id === selected ? 'text-accent' : ''}">{b.name}</span>
					<span class="block text-xs text-muted">{b.entry_count} เครื่อง · {permLabel[b.permission]}</span>
				</button>
			{/each}
		</nav>
		{#if book}
			<section class="min-w-0">
				<div class="mb-3 flex flex-wrap items-start justify-between gap-3">
					<div class="min-w-0">
						<h2 class="text-lg font-semibold">{book.name}</h2>
						<p class="text-sm text-muted">{book.note || ''} {book.owner_email ? `· เจ้าของ ${book.owner_email}` : ''}</p>
					</div>
					<div class="flex flex-wrap gap-2">
						{#if rank(book.permission) >= 2}
							<button class="btn btn-primary" onclick={() => (entryForm = { isNew: true, id: '', alias: '', tags: '', note: '' })}><Icon name="plus" /> เพิ่มเครื่อง</button>
						{/if}
						{#if book.permission === 'full'}
							<button class="btn" onclick={openShares}><Icon name="users" /> แชร์</button>
							<button class="btn" onclick={() => (bookForm = { id: book.id, name: book.name, note: book.note })}><Icon name="edit" /></button>
							<button class="btn btn-danger" onclick={() => deleteBook(book)}><Icon name="trash" /></button>
						{/if}
					</div>
				</div>
				<div class="card overflow-x-auto">
					<table class="table">
						<thead><tr><th>ID</th><th>ชื่อ</th><th>แท็ก</th><th>หมายเหตุ</th><th>สถานะ</th><th class="text-right">จัดการ</th></tr></thead>
						<tbody>
							{#each entries as e (e.device_id)}
								<tr>
									<td class="mono whitespace-nowrap">{formatId(e.device_id)}</td>
									<td>
										<p class="font-medium">{e.alias || e.device?.hostname || '—'}</p>
										{#if e.alias && e.device?.hostname}<p class="text-xs text-muted">{e.device.hostname}</p>{/if}
									</td>
									<td>{#each e.tags as t (t)}<span class="badge mr-1">{t}</span>{/each}</td>
									<td class="max-w-xs text-sm text-muted">{e.note}</td>
									<td class="whitespace-nowrap text-xs">
										{#if !e.device}<span class="text-muted">ยังไม่ลงทะเบียน</span>
										{:else if e.device.online}<span class="text-ok">ออนไลน์</span>
										{:else}<span class="text-muted">เห็นล่าสุด {ago(e.device.last_seen_at)}</span>{/if}
									</td>
									<td>
										<div class="flex justify-end gap-1.5">
											<a class="btn btn-primary {e.device?.online ? '' : 'pointer-events-none opacity-40'}" href="/connect/{e.device_id}">เชื่อมต่อ</a>
											{#if rank(book.permission) >= 2}
												<button class="btn" title="แก้ไข" onclick={() => (entryForm = { isNew: false, id: e.device_id, alias: e.alias, tags: e.tags.join(', '), note: e.note })}><Icon name="edit" /></button>
												<button class="btn" title="เอาออก" onclick={() => removeEntry(e)}><Icon name="trash" /></button>
											{/if}
										</div>
									</td>
								</tr>
							{:else}
								<tr><td colspan="6" class="py-10 text-center text-muted">ยังไม่มีเครื่องในสมุดนี้</td></tr>
							{/each}
						</tbody>
					</table>
				</div>
			</section>
		{/if}
	</div>
{/if}

{#if bookForm}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (bookForm = null)}>
		<form class="card w-full max-w-md space-y-4 p-6 shadow-xl" onsubmit={saveBook}>
			<h3 class="text-lg font-semibold">{bookForm.id ? 'แก้ไขสมุด' : 'สร้างสมุดที่อยู่'}</h3>
			<div><label class="label" for="b-name">ชื่อ</label><input id="b-name" class="input" required maxlength="64" bind:value={bookForm.name} /></div>
			<div><label class="label" for="b-note">คำอธิบาย</label><input id="b-note" class="input" bind:value={bookForm.note} /></div>
			<div class="flex justify-end gap-2">
				<button type="button" class="btn" onclick={() => (bookForm = null)}>ยกเลิก</button>
				<button class="btn btn-primary">บันทึก</button>
			</div>
		</form>
	</div>
{/if}

{#if entryForm}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (entryForm = null)}>
		<form class="card w-full max-w-md space-y-4 p-6 shadow-xl" onsubmit={saveEntry}>
			<h3 class="text-lg font-semibold">{entryForm.isNew ? 'เพิ่มเครื่อง' : 'แก้ไขรายการ'}</h3>
			<div><label class="label" for="e-id">ID</label><input id="e-id" class="input mono" required disabled={!entryForm.isNew} bind:value={entryForm.id} /></div>
			<div><label class="label" for="e-alias">ชื่อเรียก</label><input id="e-alias" class="input" bind:value={entryForm.alias} /></div>
			<div><label class="label" for="e-tags">แท็ก (คั่นด้วย ,)</label><input id="e-tags" class="input" bind:value={entryForm.tags} /></div>
			<div><label class="label" for="e-note">หมายเหตุ</label><textarea id="e-note" class="input" rows="2" bind:value={entryForm.note}></textarea></div>
			<div class="flex justify-end gap-2">
				<button type="button" class="btn" onclick={() => (entryForm = null)}>ยกเลิก</button>
				<button class="btn btn-primary" disabled={!validDeviceId(cleanDeviceId(entryForm.id))}>บันทึก</button>
			</div>
		</form>
	</div>
{/if}

{#if shareForm}
	<div class="fixed inset-0 z-20 grid place-items-center bg-black/40 p-4" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (shareForm = null)}>
		<div class="card w-full max-w-lg space-y-4 p-6 shadow-xl">
			<h3 class="text-lg font-semibold">แชร์ "{book?.name}"</h3>
			<ul class="divide-y divide-line rounded-lg border border-line">
				{#each shareForm.shares as sh, i (`${sh.user_id}-${sh.group_id}`)}
					<li class="flex items-center gap-2 px-3 py-2 text-sm">
						<span class="min-w-0 flex-1 truncate">{targetName(sh)}</span>
						<select class="input w-36" bind:value={shareForm.shares[i].permission}>
							<option value="read">{permLabel.read}</option>
							<option value="rw">{permLabel.rw}</option>
							<option value="full">{permLabel.full}</option>
						</select>
						<button class="btn" title="เลิกแชร์" onclick={() => (shareForm!.shares = shareForm!.shares.filter((_, j) => j !== i))}><Icon name="trash" /></button>
					</li>
				{:else}
					<li class="px-3 py-2 text-sm text-muted">ยังไม่ได้แชร์ให้ใคร (เจ้าของและผู้ดูแลระบบเห็นเสมอ)</li>
				{/each}
			</ul>
			<div class="flex gap-2">
				<select class="input flex-1" bind:value={shareForm.pick} aria-label="ผู้ใช้หรือกลุ่ม">
					<option value="">เลือกผู้ใช้หรือกลุ่ม…</option>
					<optgroup label="กลุ่ม">{#each shareForm.groups as g (g.id)}<option value="g:{g.id}">{g.name}</option>{/each}</optgroup>
					<optgroup label="ผู้ใช้">
						{#each shareForm.users.filter((u) => u.id !== book?.owner_id) as u (u.id)}<option value="u:{u.id}">{u.name ? `${u.name} (${u.email})` : u.email}</option>{/each}
					</optgroup>
				</select>
				<select class="input w-36" bind:value={shareForm.perm} aria-label="สิทธิ์">
					<option value="read">{permLabel.read}</option>
					<option value="rw">{permLabel.rw}</option>
					<option value="full">{permLabel.full}</option>
				</select>
				<button class="btn" onclick={addShare} disabled={!shareForm.pick}>เพิ่ม</button>
			</div>
			<p class="text-xs text-muted">ถ้าได้สิทธิ์หลายทาง (ตัวเองและผ่านกลุ่ม) ใช้สิทธิ์ที่สูงที่สุด</p>
			<div class="flex justify-end gap-2 border-t border-line pt-4">
				<button class="btn" onclick={() => (shareForm = null)}>ยกเลิก</button>
				<button class="btn btn-primary" onclick={saveShares}>บันทึก</button>
			</div>
		</div>
	</div>
{/if}
