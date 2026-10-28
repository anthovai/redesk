<script lang="ts">
	import Icon from './Icon.svelte';

	let { value = $bindable(), onchange }: { value: 'grid' | 'list'; onchange?: (v: 'grid' | 'list') => void } = $props();

	function set(v: 'grid' | 'list') {
		value = v;
		onchange?.(v);
	}
</script>

<div class="inline-flex rounded-lg border border-line bg-surface p-0.5" role="group" aria-label="มุมมอง">
	{#each [['grid', 'grid', 'แบบการ์ด'], ['list', 'list', 'แบบรายการ']] as const as [v, icon, label] (v)}
		<button
			class="grid size-8 place-items-center rounded-md transition-colors {value === v ? 'bg-accent-soft text-accent' : 'text-muted hover:text-fg'}"
			title={label}
			aria-pressed={value === v}
			onclick={() => set(v)}
		>
			<Icon name={icon} />
		</button>
	{/each}
</div>
