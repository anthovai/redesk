<script lang="ts">
	import Icon from './Icon.svelte';
	import { CHAT_MAX, type RemoteViewer } from './viewer.svelte.js';

	let { viewer, peer, onclose }: { viewer: RemoteViewer; peer: string; onclose: () => void } = $props();
	let draft = $state('');
	let list = $state<HTMLElement>();
	let input = $state<HTMLInputElement>();

	// Everything shown here counts as read; keep the newest message in view.
	$effect(() => {
		void viewer.chat.length;
		viewer.chatUnread = 0;
		queueMicrotask(() => list?.scrollTo({ top: list.scrollHeight }));
	});

	function send(ev: SubmitEvent) {
		ev.preventDefault();
		viewer.sendChat(draft);
		draft = '';
		input?.focus();
	}

	const time = (at: number) => new Date(at).toLocaleTimeString('th-TH', { hour: '2-digit', minute: '2-digit' });
</script>

<aside class="flex w-80 shrink-0 flex-col border-l border-line bg-surface text-fg" aria-label="แชท">
	<div class="flex items-center gap-2 border-b border-line px-3 py-2.5">
		<Icon name="message" size={16} class="text-accent" />
		<p class="flex-1 truncate text-sm font-semibold">แชทกับ {peer}</p>
		<button class="grid size-7 place-items-center rounded-md text-muted hover:bg-raised hover:text-fg" title="ปิด" onclick={onclose}><Icon name="x" size={15} /></button>
	</div>
	<div bind:this={list} class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-3 py-3" role="log" aria-live="polite">
		{#if !viewer.chat.length}
			<div class="m-auto max-w-56 text-center text-xs text-muted">
				<p>พิมพ์ข้อความถึงคนที่เครื่องปลายทาง</p>
				<p class="mt-1">ข้อความเข้ารหัสต้นทาง-ปลายทาง ไม่ถูกเก็บไว้ที่เซิร์ฟเวอร์</p>
			</div>
		{/if}
		{#each viewer.chat as m, i (i)}
			<div class="flex flex-col {m.mine ? 'items-end' : 'items-start'}">
				<p class="max-w-[85%] rounded-xl px-3 py-1.5 text-sm break-words whitespace-pre-wrap {m.mine ? 'bg-accent text-accent-fg' : 'bg-raised'}">{m.text}</p>
				<span class="mt-0.5 text-[10px] text-muted">{time(m.at)}</span>
			</div>
		{/each}
	</div>
	<form class="flex gap-2 border-t border-line p-2.5" onsubmit={send}>
		<input bind:this={input} bind:value={draft} class="input min-w-0 flex-1" placeholder="ข้อความ" maxlength={CHAT_MAX} aria-label="ข้อความ" />
		<button class="btn btn-primary" disabled={!draft.trim()}>ส่ง</button>
	</form>
</aside>
