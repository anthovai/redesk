<script lang="ts">
	// Generic, non-trademark OS glyphs: window panes, a laptop, a terminal, a monitor.
	import type { OsKind } from './peer.js';

	let { kind, size = 48, class: cls = '' }: { kind: OsKind; size?: number; class?: string } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 48 48" class={cls} aria-hidden="true">
	{#if kind === 'windows'}
		<g fill="currentColor">
			<rect x="7" y="7" width="15.5" height="15.5" rx="2" />
			<rect x="25.5" y="7" width="15.5" height="15.5" rx="2" />
			<rect x="7" y="25.5" width="15.5" height="15.5" rx="2" />
			<rect x="25.5" y="25.5" width="15.5" height="15.5" rx="2" />
		</g>
	{:else if kind === 'mac'}
		<g fill="none" stroke="currentColor" stroke-width="3" stroke-linejoin="round">
			<rect x="10" y="9" width="28" height="20" rx="2.5" />
		</g>
		<path d="M3 34h42l-1.6 3.2A3 3 0 0 1 40.7 39H7.3a3 3 0 0 1-2.7-1.8z" fill="currentColor" />
		<rect x="20" y="34" width="8" height="1.6" rx=".8" fill="currentColor" opacity=".35" />
	{:else if kind === 'linux'}
		<g fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
			<rect x="5" y="8" width="38" height="32" rx="4" />
			<path d="M13 19l6 5-6 5M23 30h11" />
		</g>
	{:else}
		<g fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
			<rect x="5" y="8" width="38" height="25" rx="3" />
			<path d="M17 41h14M24 33v8" />
		</g>
	{/if}
</svg>
