<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { FitAddon } from '@xterm/addon-fit';
	import { Terminal } from '@xterm/xterm';
	import '@xterm/xterm/css/xterm.css';
	import { formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';
	import { RemoteTerminal } from '#lib/terminal.svelte.js';

	const remote = new RemoteTerminal(page.params.id ?? '');
	let host = $state<HTMLElement>();
	let password = $state('');
	let term: Terminal | null = null;

	// The shell ended by itself: keep its last output on screen instead of a dialog.
	const exited = $derived(remote.phase === 'closed' && remote.exitCode !== null && !remote.error);
	const peerName = $derived(remote.device?.alias || remote.hostname || remote.device?.hostname || formatId(remote.deviceId));
	const phaseText: Record<string, string> = {
		requesting: 'กำลังขอเชื่อมต่อ…',
		waiting: 'รอเครื่องปลายทางตอบรับ…',
		handshake: 'กำลังแลกเปลี่ยนกุญแจเข้ารหัส…',
		approval: 'รอผู้ใช้ที่เครื่องปลายทางกดยอมรับ…',
		starting: 'กำลังเปิด shell…'
	};

	$effect(() => {
		if (remote.phase === 'connected') term?.focus();
	});

	onMount(() => {
		const t = new Terminal({
			cursorBlink: true,
			fontSize: 14,
			fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Noto Sans Mono", "Noto Sans Thai", monospace',
			scrollback: 5000,
			theme: { background: '#18191e' }
		});
		const fit = new FitAddon();
		t.loadAddon(fit);
		t.open(host!);
		fit.fit();
		term = t;
		remote.onOutput = (d) => t.write(d);
		remote.size = () => ({ cols: t.cols, rows: t.rows });
		// Keystrokes (Ctrl-C included) and pastes go to the remote shell as typed.
		const sub1 = t.onData((d) => remote.input(d));
		const sub2 = t.onResize(({ cols, rows }) => remote.resize(cols, rows));
		const ro = new ResizeObserver(() => fit.fit());
		ro.observe(host!);
		remote.start();
		return () => {
			ro.disconnect();
			sub1.dispose();
			sub2.dispose();
			remote.close();
			t.dispose();
		};
	});
</script>

<svelte:head><title>เทอร์มินัล · {peerName} · Aegiron</title></svelte:head>

<div class="flex h-dvh flex-col bg-stage text-white">
	<header class="flex h-11 shrink-0 items-center gap-3 border-b border-white/10 px-3 text-sm">
		<a class="grid size-8 place-items-center rounded-md text-white/70 hover:bg-white/10 hover:text-white" href="/" title="กลับหน้าหลัก" aria-label="กลับหน้าหลัก">
			<Icon name="x" size={16} />
		</a>
		<Icon name="terminal" size={16} class="text-white/60" />
		<span class="truncate font-medium">{peerName}</span>
		<span class="mono text-xs text-white/50">{formatId(remote.deviceId)}</span>
		{#if remote.shell}<span class="mono hidden truncate text-xs text-white/40 sm:inline">{remote.shell}</span>{/if}
		<span class="ml-auto"></span>
		{#if remote.recording && remote.phase === 'connected'}
			<span class="inline-flex items-center gap-1.5 rounded-full bg-red-500/20 px-2.5 py-0.5 text-xs font-medium text-red-300" title="องค์กรกำหนดให้บันทึกเซสชันนี้">
				<span class="size-2 rounded-full bg-red-400"></span> กำลังบันทึก
			</span>
		{/if}
		{#if remote.keyFingerprint}
			<span class="mono hidden text-[11px] text-white/40 md:inline" title="ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `aegiron-agent info`">E2E · {remote.keyFingerprint}</span>
		{/if}
		{#if remote.phase === 'connected'}
			<button class="rounded-md px-2.5 py-1 text-xs text-red-300 hover:bg-white/10" onclick={() => remote.close()}>ตัดการเชื่อมต่อ</button>
		{:else if exited}
			<span class="text-xs text-white/60" data-testid="term-exit">shell จบการทำงาน (รหัส {remote.exitCode})</span>
			<button class="rounded-md px-2.5 py-1 text-xs text-white/80 hover:bg-white/10" onclick={() => location.reload()}>เปิดใหม่</button>
		{/if}
	</header>

	<div class="relative min-h-0 flex-1 p-2">
		<div bind:this={host} class="h-full w-full {remote.phase === 'connected' || remote.exitCode !== null ? '' : 'opacity-30'}" data-testid="terminal"></div>

		{#if remote.phase !== 'connected' && !exited}
			<div class="absolute inset-0 grid place-items-center bg-black/30 p-4">
				<div class="w-full max-w-[400px] overflow-hidden rounded-xl bg-surface text-fg shadow-[0_20px_60px_rgb(0_0_0/0.5)]" role="dialog" aria-modal="true">
					{#if remote.phase === 'auth'}
						<form
							onsubmit={(e) => {
								e.preventDefault();
								remote.submitPassword(password);
								password = '';
							}}
						>
							<div class="flex items-center gap-3 px-6 pt-5">
								<span class="grid size-9 shrink-0 place-items-center rounded-full bg-accent-soft text-accent"><Icon name="lock" size={18} /></span>
								<div class="min-w-0">
									<h2 class="font-semibold">ใส่รหัสผ่านเพื่อเปิดเทอร์มินัล</h2>
									<p class="truncate text-xs text-muted">{peerName} · <span class="mono">{formatId(remote.deviceId)}</span></p>
								</div>
							</div>
							<div class="space-y-3 px-6 py-4">
								<p class="text-sm text-muted">ดูรหัสชั่วคราวได้ที่หน้าจอ Aegiron agent ของเครื่องนั้น</p>
								<!-- svelte-ignore a11y_autofocus -->
								<input id="term-password" class="input mono" type="password" autocomplete="off" placeholder="รหัสผ่าน" autofocus required bind:value={password} />
								{#if remote.error}<p class="text-sm text-danger">{remote.error}</p>{/if}
							</div>
							<div class="flex justify-end gap-2 border-t border-line bg-raised/60 px-6 py-3">
								<button type="button" class="btn min-w-20" onclick={() => goto('/')}>ยกเลิก</button>
								<button class="btn btn-primary min-w-20" disabled={remote.authBusy}>{remote.authBusy ? 'กำลังตรวจสอบ…' : 'ตกลง'}</button>
							</div>
						</form>
					{:else if remote.phase === 'closed'}
						<div class="flex items-start gap-3 px-6 pt-5 pb-4">
							<span class="grid size-9 shrink-0 place-items-center rounded-full {remote.error ? 'bg-danger-soft text-danger' : 'bg-raised text-muted'}">
								<Icon name={remote.error ? 'alert' : 'power'} size={18} />
							</span>
							<div class="min-w-0 pt-1.5">
								<h2 class="font-semibold">{remote.error ? 'เชื่อมต่อไม่สำเร็จ' : 'ตัดการเชื่อมต่อแล้ว'}</h2>
								<p class="mt-1 text-sm text-muted" data-testid="term-status">{remote.error ?? `${peerName} · ${formatId(remote.deviceId)}`}</p>
							</div>
						</div>
						<div class="flex justify-end gap-2 border-t border-line bg-raised/60 px-6 py-3">
							<a class="btn min-w-20" href="/">กลับหน้าหลัก</a>
							<button class="btn btn-primary min-w-20" onclick={() => location.reload()}>เปิดใหม่</button>
						</div>
					{:else}
						<div class="flex items-center gap-3 px-6 pt-5">
							<span class="grid size-9 shrink-0 place-items-center rounded-full bg-accent-soft text-accent"><Icon name="terminal" size={18} /></span>
							<div class="min-w-0">
								<h2 class="font-semibold">กำลังเปิดเทอร์มินัล</h2>
								<p class="truncate text-xs text-muted">{peerName} · <span class="mono">{formatId(remote.deviceId)}</span></p>
							</div>
						</div>
						<div class="px-6 py-5">
							<p class="text-sm">{phaseText[remote.phase]}</p>
							{#if remote.error}<p class="mt-3 text-sm text-danger">{remote.error}</p>{/if}
							{#if remote.keyFingerprint}
								<p class="mono mt-3 text-[11px] text-muted" title="ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `aegiron-agent info`">E2E · {remote.keyFingerprint}</p>
							{/if}
						</div>
						<div class="flex justify-end border-t border-line bg-raised/60 px-6 py-3">
							<button class="btn min-w-20" onclick={() => goto('/')}>ยกเลิก</button>
						</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>
</div>
