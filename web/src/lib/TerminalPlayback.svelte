<script lang="ts">
	// Playback of a terminal session recording (docs/PROTOCOL.md §11): the shell's output with
	// its timing, like `scriptreplay`. Seeking back replays from the start (terminal output is
	// small, so this is instant).
	import { onMount } from 'svelte';
	import { Terminal } from '@xterm/xterm';
	import '@xterm/xterm/css/xterm.css';
	import Icon from './Icon.svelte';
	import { clock, parseRecording } from './recording.svelte.js';
	import { eventsUntil, termEvents, type TermEvent } from './termproto';

	let { buf }: { buf: ArrayBuffer } = $props();

	let host = $state<HTMLElement>();
	let playing = $state(false);
	let time = $state(0);
	let speed = $state(1);
	let duration = $state(0);
	let size = $state({ cols: 80, rows: 24 });
	const speeds = [1, 2, 4, 8];

	let term: Terminal | null = null;
	let events: TermEvent[] = [];
	let next = 0;
	let raf = 0;
	let last = 0;

	function apply(upTo: number) {
		const end = eventsUntil(events, upTo, next);
		for (; next < end; next++) {
			const e = events[next];
			if ('size' in e) {
				size = e.size;
				term?.resize(e.size.cols, e.size.rows);
			} else term?.write(e.out);
		}
	}

	function tick(now: number) {
		time = Math.min(duration, time + (now - last) * speed);
		last = now;
		apply(time);
		if (time >= duration) playing = false;
		if (playing) raf = requestAnimationFrame(tick);
	}

	function play() {
		if (time >= duration) seek(0);
		playing = true;
		last = performance.now();
		raf = requestAnimationFrame(tick);
	}

	function pause() {
		playing = false;
		cancelAnimationFrame(raf);
	}

	function seek(ms: number) {
		ms = Math.max(0, Math.min(duration, ms));
		if (ms < time || next === 0) {
			term?.reset();
			next = 0;
		}
		time = ms;
		apply(ms);
	}

	onMount(() => {
		const rec = parseRecording(buf);
		events = termEvents(rec.records);
		duration = rec.durationMs;
		const first = events.find((e) => 'size' in e);
		if (first && 'size' in first) size = first.size;
		term = new Terminal({
			cols: size.cols,
			rows: size.rows,
			disableStdin: true,
			convertEol: false,
			scrollback: 5000,
			fontSize: 13,
			fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Noto Sans Mono", "Noto Sans Thai", monospace',
			theme: { background: '#18191e' }
		});
		term.open(host!);
		play();
		return () => {
			cancelAnimationFrame(raf);
			term?.dispose();
		};
	});

	function onKey(ev: KeyboardEvent) {
		if ((ev.target as HTMLElement).tagName === 'INPUT') return;
		if (ev.key === ' ') {
			ev.preventDefault();
			if (playing) pause();
			else play();
		} else if (ev.key === 'ArrowRight') seek(time + 10_000);
		else if (ev.key === 'ArrowLeft') seek(time - 10_000);
	}
</script>

<svelte:window onkeydown={onKey} />

<div class="card overflow-hidden">
	<div class="overflow-auto bg-stage p-3">
		<div bind:this={host} class="inline-block" data-testid="term-playback"></div>
	</div>
	<div class="flex flex-wrap items-center gap-3 border-t border-line px-4 py-3">
		<button class="btn btn-primary w-24" onclick={() => (playing ? pause() : play())}>
			<Icon name={playing ? 'pause' : 'play'} size={14} />
			{playing ? 'หยุด' : 'เล่น'}
		</button>
		<span class="mono text-sm whitespace-nowrap">{clock(time)} / {clock(duration)}</span>
		<input
			class="min-w-40 flex-1 accent-[var(--color-accent)]"
			type="range"
			min="0"
			max={duration}
			step="100"
			value={time}
			oninput={(e) => seek(Number((e.currentTarget as HTMLInputElement).value))}
			aria-label="ตำแหน่ง"
		/>
		<div class="flex gap-1">
			{#each speeds as s (s)}
				<button class="btn px-2 {speed === s ? 'border-accent bg-accent-soft text-accent' : ''}" onclick={() => (speed = s)}>{s}×</button>
			{/each}
		</div>
	</div>
	<p class="border-t border-line px-4 py-2 text-xs text-muted">
		เทอร์มินัล · {size.cols}×{size.rows} · บันทึกเฉพาะสิ่งที่แสดงบนจอ (ไม่บันทึกปุ่มที่กด) · เว้นวรรค = เล่น/หยุด, ← → = ถอย/ข้าม 10 วินาที
	</p>
</div>
