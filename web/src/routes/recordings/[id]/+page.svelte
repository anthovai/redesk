<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { api, type Recording } from '#lib/api.js';
	import { bytes, dateTime, formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';
	import { clock, parseRecording, RecordingPlayer } from '#lib/recording.svelte.js';
	import TerminalPlayback from '#lib/TerminalPlayback.svelte';

	const id = page.params.id ?? '';
	let rec = $state<Recording | null>(null);
	let player = $state<RecordingPlayer | null>(null);
	let canvas = $state<HTMLCanvasElement>();
	let error = $state('');
	let loaded = $state(0);
	/** A terminal session's recording: played as text in xterm.js instead of video. */
	let termBuf = $state<ArrayBuffer | null>(null);
	const speeds = [1, 2, 4, 8];

	onMount(() => {
		let p: RecordingPlayer | null = null;
		(async () => {
			try {
				rec = (await api.get<{ recording: Recording }>(`/api/recordings/${id}`)).recording;
				const res = await fetch(`/api/recordings/${id}/file`, { credentials: 'same-origin' });
				if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error ?? res.statusText);
				// Read with progress: recordings can be large.
				const reader = res.body!.getReader();
				const parts: Uint8Array[] = [];
				for (;;) {
					const { done, value } = await reader.read();
					if (done) break;
					parts.push(value);
					loaded += value.length;
				}
				const buf = new Uint8Array(loaded);
				let o = 0;
				for (const part of parts) (buf.set(part, o), (o += part.length));
				if (rec.codec === 'terminal') {
					termBuf = buf.buffer;
					return;
				}
				p = new RecordingPlayer(parseRecording(buf.buffer), canvas!);
				player = p;
				p.play();
			} catch (e) {
				error = (e as Error).message;
			}
		})();
		return () => p?.destroy();
	});

	function onSeek(ev: Event) {
		player?.seek(Number((ev.currentTarget as HTMLInputElement).value));
	}

	function onKey(ev: KeyboardEvent) {
		if (!player || (ev.target as HTMLElement).tagName === 'INPUT') return;
		if (ev.key === ' ') {
			ev.preventDefault();
			if (player.playing) player.pause();
			else player.play();
		} else if (ev.key === 'm' && player.hasAudio) player.toggleMute();
		else if (ev.key === 'ArrowRight') player.seek(player.time + 10_000);
		else if (ev.key === 'ArrowLeft') player.seek(player.time - 10_000);
	}
</script>

<svelte:head><title>เล่นบันทึกเซสชัน · Aegiron</title></svelte:head>
<svelte:window onkeydown={onKey} />

<div class="mb-4 flex flex-wrap items-center gap-3">
	<a class="btn btn-ghost" href="/recordings">← บันทึกเซสชัน</a>
	{#if rec}
		<div class="min-w-0">
			<h1 class="truncate text-xl font-semibold tracking-tight">{rec.device_name || formatId(rec.device_id)}</h1>
			<p class="text-xs text-muted">
				{dateTime(rec.created_at)} · ผู้ควบคุม {rec.user_email ?? '—'} ({rec.viewer_ip}) · <span class="mono">{formatId(rec.device_id)}</span>
			</p>
		</div>
		<span class="ml-auto inline-flex items-center gap-1.5 rounded-full bg-danger-soft px-3 py-1 text-xs font-medium text-danger">
			<span class="size-2 rounded-full bg-danger"></span> บันทึกเซสชัน
		</span>
	{/if}
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}
{#if player?.error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{player.error}</p>{/if}

{#if termBuf}
	<TerminalPlayback buf={termBuf} />
{:else}
<div class="card overflow-hidden">
	<div class="relative grid min-h-[320px] place-items-center bg-stage">
		<canvas bind:this={canvas} class="block h-auto max-h-[70vh] w-auto max-w-full {player?.frame.w ? '' : 'opacity-0'}"></canvas>
		{#if player?.cursor && canvas && player.frame.w}
			<svg
				class="pointer-events-none absolute"
				style="left: {canvas.offsetLeft + player.cursor.x * canvas.clientWidth}px; top: {canvas.offsetTop + player.cursor.y * canvas.clientHeight}px"
				width="14"
				height="20"
				viewBox="-1 -1 14 20"
				aria-hidden="true"
			>
				<path d="M0 0V16L4.2 12.3L7 18.5L9.6 17.4L6.9 11.3H12Z" fill="#000" stroke="#fff" stroke-width="1.2" stroke-linejoin="round" />
			</svg>
		{/if}
		{#if !player && !error}
			<div class="absolute grid place-items-center gap-3 text-sm text-white/70">
				<div class="size-7 animate-spin rounded-full border-2 border-white/20 border-t-white/80"></div>
				กำลังโหลด… {rec ? `${bytes(loaded)} / ${bytes(rec.size_bytes)}` : ''}
			</div>
		{/if}
	</div>
	{#if player}
		<div class="flex flex-wrap items-center gap-3 border-t border-line px-4 py-3">
			<button class="btn btn-primary w-24" onclick={() => (player!.playing ? player!.pause() : player!.play())}>
				<Icon name={player.playing ? 'pause' : 'play'} size={14} />
				{player.playing ? 'หยุด' : 'เล่น'}
			</button>
			<span class="mono text-sm whitespace-nowrap">{clock(player.time)} / {clock(player.duration)}</span>
			<input class="min-w-40 flex-1 accent-[var(--color-accent)]" type="range" min="0" max={player.duration} step="100" value={player.time} oninput={onSeek} aria-label="ตำแหน่ง" />
			<div class="flex gap-1">
				{#each speeds as s (s)}
					<button class="btn px-2 {player.speed === s ? 'border-accent bg-accent-soft text-accent' : ''}" onclick={() => player!.setSpeed(s)}>{s}×</button>
				{/each}
			</div>
			{#if player.hasAudio}
				<button
					class="btn {player.audioBlocked ? 'border-accent bg-accent-soft text-accent' : ''}"
					onclick={() => player!.toggleMute()}
					aria-pressed={player.muted}
					title={!player.audioAtSpeed ? 'เสียงเล่นได้ที่ความเร็วไม่เกิน 2×' : player.audioBlocked ? 'คลิกเพื่อเปิดเสียง' : player.muted ? 'เปิดเสียง' : 'ปิดเสียง'}
				>
					<Icon name={player.muted || player.audioBlocked || !player.audioAtSpeed ? 'volumeOff' : 'volume'} size={14} />
					{#if player.audioBlocked}<span class="text-xs">เปิดเสียง</span>{/if}
				</button>
			{/if}
			<a class="btn" href="/api/recordings/{id}/file" download title="ดาวน์โหลดไฟล์ .aerec"><Icon name="download" size={14} /></a>
		</div>
		<p class="border-t border-line px-4 py-2 text-xs text-muted">
			{rec?.codec.toUpperCase()}{player.hasAudio ? ' + Opus' : player.rec.audio.length ? ' (เบราว์เซอร์นี้เล่นเสียงไม่ได้)' : ''} · {player.frame.w}×{player.frame.h} · {bytes(rec?.size_bytes ?? 0)} · SHA-256 <span class="mono">{rec?.sha256.slice(0, 16)}…</span> · เว้นวรรค = เล่น/หยุด, ← → = ถอย/ข้าม 10 วินาที
		</p>
	{/if}
</div>
{/if}
