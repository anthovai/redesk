<script lang="ts">
	import { bytes } from './format.js';
	import Icon from './Icon.svelte';
	import type { RemoteViewer } from './viewer.svelte.js';

	let { viewer, onclose }: { viewer: RemoteViewer; onclose: () => void } = $props();
	let picker = $state<HTMLInputElement>();
	let showHidden = $state(false);

	$effect(() => {
		if (!viewer.files.path && !viewer.files.loading) viewer.listDir('');
	});

	const f = $derived(viewer.files);
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

	const pct = (t: { done: number; size: number }) => (t.size ? Math.round((t.done / t.size) * 100) : 0);
</script>

<aside class="flex w-full max-w-sm flex-col border-l border-line bg-surface">
	<div class="flex items-center gap-2 border-b border-line px-3 py-2">
		<Icon name="folder" class="text-muted" />
		<h2 class="flex-1 text-sm font-semibold">ไฟล์บนเครื่องปลายทาง</h2>
		<button class="btn btn-ghost p-1.5" title="ปิด" onclick={onclose}><Icon name="x" /></button>
	</div>

	<div class="flex items-center gap-1 border-b border-line px-3 py-2">
		<button class="btn p-1.5" title="โฟลเดอร์แม่" disabled={!f.parent} onclick={() => f.parent && viewer.listDir(f.parent)}><Icon name="up" /></button>
		<div class="mono flex min-w-0 flex-1 gap-0.5 overflow-x-auto text-xs whitespace-nowrap text-muted">
			{#each crumbs as c (c.path)}
				<button class="hover:text-fg" onclick={() => viewer.listDir(c.path)}>{c.name}</button><span>/</span>
			{/each}
		</div>
		<button class="btn p-1.5" title="รีเฟรช" onclick={() => viewer.listDir(f.path)}><Icon name="refresh" /></button>
		<button class="btn btn-primary p-1.5" title="อัปโหลดมาที่โฟลเดอร์นี้" onclick={() => picker?.click()}><Icon name="upload" /></button>
		<input bind:this={picker} type="file" multiple hidden onchange={pick} />
	</div>

	<div class="min-h-0 flex-1 overflow-y-auto">
		{#if f.error}<p class="m-3 rounded-lg bg-danger-soft px-3 py-2 text-xs text-danger">{f.error}</p>{/if}
		{#if f.loading && !f.entries.length}<p class="p-3 text-sm text-muted">กำลังโหลด…</p>{/if}
		<ul class="text-sm">
			{#each shown as e (e.name)}
				<li class="group flex items-center gap-2 px-3 py-1.5 hover:bg-raised">
					<Icon name={e.dir ? 'folder' : 'file'} class={e.dir ? 'text-accent' : 'text-muted'} />
					{#if e.dir}
						<button class="min-w-0 flex-1 truncate text-left" onclick={() => viewer.listDir(join(e.name))}>{e.name}</button>
					{:else}
						<span class="min-w-0 flex-1 truncate">{e.name}</span>
						<span class="mono text-xs text-muted">{bytes(e.size)}</span>
						<button class="btn btn-ghost p-1 opacity-0 group-hover:opacity-100 focus:opacity-100" title="ดาวน์โหลด" onclick={() => viewer.download(join(e.name), e.name)}>
							<Icon name="download" size={14} />
						</button>
					{/if}
				</li>
			{/each}
		</ul>
	</div>

	<label class="flex items-center gap-2 border-t border-line px-3 py-2 text-xs text-muted">
		<input type="checkbox" bind:checked={showHidden} /> แสดงไฟล์ซ่อน
	</label>

	{#if viewer.transfers.length}
		<div class="max-h-56 overflow-y-auto border-t border-line px-3 py-2">
			<div class="mb-1 flex items-center justify-between text-xs text-muted">
				<span>การโอนไฟล์</span>
				<button class="hover:text-fg" onclick={() => viewer.clearFinishedTransfers()}>ล้างที่เสร็จแล้ว</button>
			</div>
			{#each viewer.transfers as t (t.id)}
				<div class="py-1.5 text-xs">
					<div class="flex items-center gap-1.5">
						<Icon name={t.direction === 'up' ? 'upload' : 'download'} size={12} class="text-muted" />
						<span class="min-w-0 flex-1 truncate">{t.name}</span>
						{#if t.status === 'active'}
							<span class="mono text-muted">{pct(t)}%</span>
							<button class="text-muted hover:text-danger" title="ยกเลิก" onclick={() => viewer.cancelTransfer(t.id)}><Icon name="x" size={12} /></button>
						{:else if t.status === 'done'}
							<span class="text-ok">เสร็จ</span>
						{:else}
							<span class="text-danger" title={t.message}>ล้มเหลว</span>
						{/if}
					</div>
					{#if t.status === 'active'}
						<div class="mt-1 h-1 overflow-hidden rounded-full bg-raised"><div class="h-full bg-accent transition-[width]" style="width: {pct(t)}%"></div></div>
					{:else if t.status === 'error' && t.message}
						<p class="mt-0.5 truncate text-danger">{t.message}</p>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</aside>
