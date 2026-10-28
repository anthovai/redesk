<script lang="ts" module>
	import type { IconName } from './Icon.svelte';

	export interface PeerAction {
		label: string;
		icon?: IconName;
		onclick?: () => void;
		href?: string;
		disabled?: boolean;
		danger?: boolean;
		/** Draw a separator above this item. */
		divider?: boolean;
	}

	// Only one card menu is open at a time.
	const menu = $state({ open: '' });
</script>

<script lang="ts">
	import { goto } from '$app/navigation';
	import { formatId } from './format.js';
	import Icon from './Icon.svelte';
	import OsGlyph from './OsGlyph.svelte';
	import { osKind, osLabel, peerColor } from './peer.js';

	let {
		id,
		name,
		hostname = '',
		os = '',
		online,
		canConnect,
		status = '',
		favorite = false,
		dim = false,
		detail = '',
		actions
	}: {
		id: string;
		name: string;
		hostname?: string;
		os?: string;
		online: boolean;
		canConnect: boolean;
		/** Badge on the tile, e.g. "ปิดใช้งาน". */
		status?: string;
		favorite?: boolean;
		dim?: boolean;
		/** Extra tooltip line (last seen, tags…). */
		detail?: string;
		actions: PeerAction[];
	} = $props();

	let root = $state<HTMLElement>();
	const open = $derived(menu.open === id);
	const kind = $derived(osKind(os));

	function connect() {
		if (canConnect) goto(`/connect/${id}`);
	}

	function toggleMenu(ev: Event) {
		ev.stopPropagation();
		menu.open = open ? '' : id;
	}

	function run(a: PeerAction) {
		menu.open = '';
		if (a.href) goto(a.href);
		else a.onclick?.();
	}

	function onWindowClick(ev: MouseEvent) {
		if (open && root && !root.contains(ev.target as Node)) menu.open = '';
	}
</script>

<svelte:window onclick={onWindowClick} onkeydown={(e) => open && e.key === 'Escape' && (menu.open = '')} />

<div
	bind:this={root}
	class="group relative flex flex-col rounded-lg bg-surface shadow-[0_1px_2px_rgb(0_0_0/0.06)] ring-1 ring-line transition-shadow hover:shadow-[0_6px_18px_rgb(0_0_0/0.12)] {dim
		? 'opacity-60'
		: ''} {open ? 'z-20' : ''}"
>
	<div
		role="button"
		tabindex="0"
		title={[canConnect ? 'คลิกเพื่อเชื่อมต่อ' : online ? 'เชื่อมต่อไม่ได้' : 'ออฟไลน์', detail].filter(Boolean).join('\n')}
		aria-label="เชื่อมต่อ {name}"
		aria-disabled={!canConnect}
		class="relative flex h-32 flex-col items-center justify-center gap-2 rounded-t-lg px-3 text-white outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 {canConnect
			? 'cursor-pointer'
			: 'cursor-default'}"
		style="background: {peerColor(id)}"
		onclick={connect}
		onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), connect())}
	>
		<OsGlyph {kind} size={46} class="drop-shadow-sm transition-transform duration-150 {canConnect ? 'group-hover:scale-105' : ''}" />
		<p class="max-w-full truncate text-[13px] font-medium text-white/90" title={osLabel[kind]}>{hostname || osLabel[kind]}</p>
		{#if status}
			<span class="absolute top-2 left-2 rounded bg-black/30 px-1.5 py-0.5 text-[11px] leading-none font-medium">{status}</span>
		{/if}
		{#if favorite}
			<span class="absolute top-2 right-2 text-amber-300" title="รายการโปรด">
				<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
					><path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01z" /></svg
				>
			</span>
		{/if}
	</div>

	<div class="flex h-12 items-center gap-2 pr-1 pl-3">
		<span
			class="size-2.5 shrink-0 rounded-full {online ? 'bg-ok' : 'bg-[#c4c4cc]'}"
			title={online ? 'ออนไลน์' : 'ออฟไลน์'}
		></span>
		<div class="min-w-0 flex-1 leading-tight">
			<p class="truncate text-[13px] font-medium" title={name}>{name}</p>
			<p class="mono truncate text-[11px] text-muted">{formatId(id)}</p>
		</div>
		<button
			class="grid size-8 shrink-0 place-items-center rounded-md text-lg leading-none text-muted transition-colors hover:bg-raised hover:text-fg {open
				? 'bg-raised text-fg'
				: ''}"
			title="เมนู"
			aria-haspopup="menu"
			aria-expanded={open}
			onclick={toggleMenu}>⋮</button
		>
	</div>

	{#if open}
		<div
			class="absolute top-[calc(100%-6px)] right-1 z-30 min-w-48 rounded-lg border border-line bg-surface py-1 text-sm shadow-[0_10px_30px_rgb(0_0_0/0.18)]"
			role="menu"
		>
			{#each actions as a (a.label)}
				{#if a.divider}<div class="my-1 border-t border-line"></div>{/if}
				<button
					role="menuitem"
					class="flex w-full items-center gap-2.5 px-3 py-1.5 text-left transition-colors hover:bg-accent hover:text-accent-fg disabled:pointer-events-none disabled:opacity-40 {a.danger
						? 'text-danger'
						: ''}"
					disabled={a.disabled}
					onclick={() => run(a)}
				>
					{#if a.icon}<Icon name={a.icon} size={15} />{:else}<span class="w-[15px]"></span>{/if}
					{a.label}
				</button>
			{/each}
		</div>
	{/if}
</div>
