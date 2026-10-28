<script lang="ts">
	import { bytes } from './format.js';
	import { treesFromPicker, validName } from './filetree.js';
	import Icon from './Icon.svelte';
	import type { RemoteEntry, RemoteViewer, Transfer } from './viewer.svelte.js';

	let { viewer, onclose }: { viewer: RemoteViewer; onclose: () => void } = $props();
	let picker = $state<HTMLInputElement>();
	let folderPicker = $state<HTMLInputElement>();
	let showHidden = $state(false);

	$effect(() => {
		if (!viewer.files.path && !viewer.files.loading) viewer.listDir('');
	});

	const f = $derived(viewer.files);
	const fs2 = $derived(viewer.fs2);
	const shown = $derived(f.entries.filter((e) => showHidden || !e.name.startsWith('.')));
	const join = (name: string) => (f.path.endsWith(f.sep) ? f.path + name : f.path + f.sep + name);
	const crumbs = $derived.by(() => {
		const parts = f.path.split(f.sep).filter(Boolean);
		const root = f.path.startsWith(f.sep) ? f.sep : '';
		return parts.map((p, i) => ({ name: p, path: root + parts.slice(0, i + 1).join(f.sep) }));
	});

	function pick(ev: Event) {
		const input = ev.currentTarget as HTMLInputElement;
		for (const file of input.files ?? []) viewer.upload(file, f.path);
		input.value = '';
	}

	function pickFolder(ev: Event) {
		const input = ev.currentTarget as HTMLInputElement;
		for (const tree of treesFromPicker(input.files ?? [])) viewer.uploadTree(tree, f.path);
		input.value = '';
	}

	function goTo() {
		const path = prompt('ตำแหน่งโฟลเดอร์บนเครื่องปลายทาง', f.path)?.trim();
		if (path) viewer.listDir(path);
	}

	function newFolder() {
		const name = prompt('ชื่อโฟลเดอร์ใหม่', 'โฟลเดอร์ใหม่')?.trim();
		if (!name) return;
		if (!validName(name)) return alert('ชื่อโฟลเดอร์ไม่ถูกต้อง');
		viewer.makeDir(f.path, name);
	}

	function rename(e: RemoteEntry) {
		const name = prompt('เปลี่ยนชื่อเป็น', e.name)?.trim();
		if (!name || name === e.name) return;
		if (!validName(name)) return alert('ชื่อไม่ถูกต้อง');
		viewer.rename(join(e.name), name);
	}

	function remove(e: RemoteEntry) {
		const what = e.dir ? `โฟลเดอร์ "${e.name}" และทุกอย่างข้างใน` : `ไฟล์ "${e.name}"`;
		if (confirm(`ลบ${what} ออกจากเครื่องปลายทาง? ลบแล้วกู้คืนไม่ได้`)) viewer.remove(join(e.name));
	}

	const pct = (done: number, size: number) => (size ? Math.round((done / size) * 100) : 0);
	const counts = (t: Transfer) => `${(t.filesDone ?? 0) + (t.filesFailed ?? 0)}/${t.files ?? 0} ไฟล์`;
</script>

<aside class="flex w-full max-w-sm flex-col border-l border-line bg-surface" data-testid="file-panel">
	<div class="flex items-center gap-2 border-b border-line px-3 py-2">
		<Icon name="folder" class="text-muted" />
		<h2 class="flex-1 text-sm font-semibold">ไฟล์บนเครื่องปลายทาง</h2>
		<button class="btn btn-ghost p-1.5" title="ปิด" onclick={onclose}><Icon name="x" /></button>
	</div>

	<div class="flex items-center gap-1 border-b border-line px-3 py-2">
		<button class="btn p-1.5" title="โฟลเดอร์แม่" disabled={!f.parent} onclick={() => f.parent && viewer.listDir(f.parent)}><Icon name="up" /></button>
		<div class="mono flex min-w-0 flex-1 gap-0.5 overflow-x-auto text-xs whitespace-nowrap text-muted" data-testid="file-path" data-path={f.path}>
			{#each crumbs as c (c.path)}
				<button class="hover:text-fg" onclick={() => viewer.listDir(c.path)}>{c.name}</button><span>/</span>
			{/each}
		</div>
		<button class="btn p-1.5" title="ไปยังตำแหน่ง…" aria-label="ไปยังตำแหน่ง" onclick={goTo}><Icon name="search" /></button>
		<button class="btn p-1.5" title="รีเฟรช" onclick={() => viewer.listDir(f.path)}><Icon name="refresh" /></button>
		{#if fs2}
			<button class="btn p-1.5" title="สร้างโฟลเดอร์ใหม่" aria-label="สร้างโฟลเดอร์ใหม่" onclick={newFolder}><Icon name="folderPlus" /></button>
			<button class="btn p-1.5" title="อัปโหลดโฟลเดอร์มาที่นี่" aria-label="อัปโหลดโฟลเดอร์" onclick={() => folderPicker?.click()}><Icon name="folderUp" /></button>
			<input bind:this={folderPicker} type="file" webkitdirectory multiple hidden onchange={pickFolder} data-testid="folder-input" />
		{/if}
		<button class="btn btn-primary p-1.5" title="อัปโหลดไฟล์มาที่โฟลเดอร์นี้" onclick={() => picker?.click()}><Icon name="upload" /></button>
		<input bind:this={picker} type="file" multiple hidden onchange={pick} data-testid="file-input" />
	</div>

	<div class="min-h-0 flex-1 overflow-y-auto">
		{#if f.error}<p class="m-3 rounded-lg bg-danger-soft px-3 py-2 text-xs text-danger" role="alert">{f.error}</p>{/if}
		{#if f.loading && !f.entries.length}<p class="p-3 text-sm text-muted">กำลังโหลด…</p>{/if}
		<ul class="text-sm">
			{#each shown as e (e.name)}
				<li class="group flex items-center gap-2 px-3 py-1.5 hover:bg-raised" data-testid="file-row" data-name={e.name}>
					<Icon name={e.dir ? 'folder' : 'file'} class={e.dir ? 'text-accent' : 'text-muted'} />
					{#if e.dir}
						<button class="min-w-0 flex-1 truncate text-left" onclick={() => viewer.listDir(join(e.name))}>{e.name}</button>
					{:else}
						<span class="min-w-0 flex-1 truncate">{e.name}</span>
						<span class="mono text-xs text-muted">{bytes(e.size)}</span>
					{/if}
					<span class="flex gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100">
						{#if fs2}
							<button class="btn btn-ghost p-1" title="เปลี่ยนชื่อ" aria-label="เปลี่ยนชื่อ" onclick={() => rename(e)}><Icon name="edit" size={14} /></button>
							<button class="btn btn-ghost p-1 hover:text-danger" title="ลบ" aria-label="ลบ" onclick={() => remove(e)}><Icon name="trash" size={14} /></button>
						{/if}
						{#if !e.dir}
							<button class="btn btn-ghost p-1" title="ดาวน์โหลด" aria-label="ดาวน์โหลด" onclick={() => viewer.download(join(e.name), e.name)}>
								<Icon name="download" size={14} />
							</button>
						{:else if fs2}
							<button class="btn btn-ghost p-1" title="ดาวน์โหลดทั้งโฟลเดอร์ (.zip)" aria-label="ดาวน์โหลด" onclick={() => viewer.downloadDir(join(e.name), e.name)}>
								<Icon name="download" size={14} />
							</button>
						{/if}
					</span>
				</li>
			{/each}
		</ul>
	</div>

	<label class="flex items-center gap-2 border-t border-line px-3 py-2 text-xs text-muted">
		<input type="checkbox" bind:checked={showHidden} /> แสดงไฟล์ซ่อน
	</label>

	{#if viewer.transfers.length}
		<div class="max-h-56 overflow-y-auto border-t border-line px-3 py-2" data-testid="transfers">
			<div class="mb-1 flex items-center justify-between text-xs text-muted">
				<span>การโอนไฟล์</span>
				<button class="hover:text-fg" onclick={() => viewer.clearFinishedTransfers()}>ล้างที่เสร็จแล้ว</button>
			</div>
			{#each viewer.transfers as t (t.id)}
				<div class="py-1.5 text-xs" data-testid="transfer" data-status={t.status} data-direction={t.direction} data-name={t.name}>
					<div class="flex items-center gap-1.5">
						<Icon name={t.folder ? 'folder' : t.direction === 'up' ? 'upload' : 'download'} size={12} class="text-muted" />
						<span class="min-w-0 flex-1 truncate">{t.name}</span>
						{#if t.folder && t.files}<span class="mono text-muted">{counts(t)}</span>{/if}
						{#if t.status === 'active'}
							<span class="mono text-muted">{pct(t.done, t.size)}%</span>
							<button class="text-muted hover:text-danger" title="ยกเลิก" aria-label="ยกเลิก" onclick={() => viewer.cancelTransfer(t.id)}><Icon name="x" size={12} /></button>
						{:else if t.status === 'done'}
							<span class="text-ok" title={t.message}>เสร็จ</span>
						{:else}
							<span class="text-danger" title={t.message}>ล้มเหลว</span>
						{/if}
					</div>
					{#if t.status === 'active'}
						<div class="mt-1 h-1 overflow-hidden rounded-full bg-raised"><div class="h-full bg-accent transition-[width]" style="width: {pct(t.done, t.size)}%"></div></div>
						{#if t.folder && t.current}
							<div class="mt-1 flex items-center gap-1.5 text-muted">
								<span class="mono min-w-0 flex-1 truncate" title={t.current}>{t.current}</span>
								<span class="mono">{pct(t.curDone ?? 0, t.curSize ?? 0)}%</span>
							</div>
						{/if}
					{:else if t.message}
						<p class="mt-0.5 truncate {t.status === 'error' ? 'text-danger' : 'text-muted'}">{t.message}</p>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</aside>
