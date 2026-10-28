<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { formatId } from '#lib/format.js';
	import FilePanel from '#lib/FilePanel.svelte';
	import Icon from '#lib/Icon.svelte';
	import { RemoteViewer } from '#lib/viewer.svelte.js';

	const viewer = new RemoteViewer(page.params.id ?? '');
	let canvas = $state<HTMLCanvasElement>();
	let stage = $state<HTMLElement>();
	let password = $state('');
	let quality = $state<'high' | 'balanced' | 'low'>('balanced');
	let focused = $state(false);
	let toast = $state('');
	let showFiles = $state(false);
	let dragging = $state(false);
	const held = new Map<string, string>(); // code -> key, for releasing on blur

	const presets = {
		high: { quality: 85, max_width: 2560, label: 'คมชัด' },
		balanced: { quality: 70, max_width: 1920, label: 'สมดุล' },
		low: { quality: 45, max_width: 1280, label: 'ประหยัดเน็ต' }
	};

	const phaseText: Record<string, string> = {
		requesting: 'กำลังขอเชื่อมต่อ…',
		waiting: 'รอเครื่องปลายทางตอบรับ…',
		handshake: 'กำลังแลกเปลี่ยนกุญแจเข้ารหัส…',
		approval: 'รอผู้ใช้ที่เครื่องปลายทางกดยอมรับ…'
	};

	onMount(() => {
		viewer.attach(canvas!);
		viewer.start();
		return () => viewer.close();
	});

	$effect(() => {
		if (viewer.phase === 'connected') canvas?.focus();
	});

	function norm(ev: MouseEvent): [number, number] {
		const r = canvas!.getBoundingClientRect();
		return [(ev.clientX - r.left) / r.width, (ev.clientY - r.top) / r.height];
	}

	const live = $derived(viewer.phase === 'connected' && viewer.info?.input !== false);

	function onPointerMove(ev: PointerEvent) {
		if (live) viewer.mouseMove(...norm(ev));
	}
	function onPointerButton(ev: PointerEvent, down: boolean) {
		if (!live) return;
		ev.preventDefault();
		if (down) {
			canvas?.focus();
			canvas?.setPointerCapture(ev.pointerId);
		}
		viewer.mouseButton(ev.button, down, ...norm(ev));
	}

	$effect(() => {
		const c = canvas;
		if (!c) return;
		const onWheel = (ev: WheelEvent) => {
			ev.preventDefault();
			if (!live) return;
			const lines = (d: number) => (d === 0 ? 0 : Math.sign(d) * Math.max(1, Math.round(Math.abs(ev.deltaMode === 1 ? d * 40 : d) / 50)));
			viewer.wheel(lines(ev.deltaX), lines(ev.deltaY));
		};
		c.addEventListener('wheel', onWheel, { passive: false });
		return () => c.removeEventListener('wheel', onWheel);
	});

	function onKey(ev: KeyboardEvent, down: boolean) {
		if (!live || !focused || ev.isComposing) return;
		ev.preventDefault();
		if (down) held.set(ev.code, ev.key);
		else held.delete(ev.code);
		viewer.key(ev.code, ev.key, down);
	}

	function releaseAll() {
		for (const [code, key] of held) viewer.key(code, key, false);
		held.clear();
	}

	function flash(msg: string) {
		toast = msg;
		setTimeout(() => (toast = ''), 2500);
	}

	async function sendClipboard(typeIt: boolean) {
		try {
			const text = await navigator.clipboard.readText();
			if (!text) return flash('คลิปบอร์ดว่าง');
			if (typeIt) viewer.typeText(text);
			else viewer.pushClipboard(text);
			flash(typeIt ? 'พิมพ์ข้อความจากคลิปบอร์ดแล้ว' : 'ส่งคลิปบอร์ดไปเครื่องปลายทางแล้ว');
		} catch {
			flash('เบราว์เซอร์ไม่อนุญาตให้อ่านคลิปบอร์ด');
		}
		canvas?.focus();
	}

	function sendCtrlAltDel() {
		for (const [code, key] of [
			['ControlLeft', 'Control'],
			['AltLeft', 'Alt'],
			['Delete', 'Delete']
		]) viewer.key(code, key, true);
		for (const [code, key] of [
			['Delete', 'Delete'],
			['AltLeft', 'Alt'],
			['ControlLeft', 'Control']
		]) viewer.key(code, key, false);
	}

	function setQuality(q: typeof quality) {
		quality = q;
		viewer.configure({ quality: presets[q].quality, max_width: presets[q].max_width });
	}

	function onDrop(ev: DragEvent) {
		ev.preventDefault();
		dragging = false;
		if (!viewer.info?.files) return;
		const files = [...(ev.dataTransfer?.files ?? [])];
		if (!files.length) return;
		// Into the folder open in the file panel, otherwise ~/Downloads/ReDesk on the remote machine.
		for (const f of files) viewer.upload(f, viewer.files.path);
		showFiles = true;
		flash(`กำลังส่ง ${files.length} ไฟล์`);
	}

	function fullscreen() {
		if (document.fullscreenElement) document.exitFullscreen();
		else stage?.requestFullscreen();
	}

	$effect(() => {
		if (viewer.remoteClipboard) flash('ได้รับคลิปบอร์ดจากเครื่องปลายทาง');
	});
</script>

<svelte:head><title>{viewer.device?.alias || viewer.device?.hostname || formatId(viewer.deviceId)} · ReDesk</title></svelte:head>
<svelte:window onkeydown={(e) => onKey(e, true)} onkeyup={(e) => onKey(e, false)} onblur={releaseAll} />

<div class="flex h-screen flex-col bg-canvas">
	<header class="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-surface px-4 py-2">
		<a href="/" class="btn btn-ghost p-1.5" title="กลับ"><Icon name="x" /></a>
		<div class="min-w-0">
			<p class="truncate text-sm font-medium">
				{viewer.device?.alias || viewer.info?.hostname || viewer.device?.hostname || 'เครื่องปลายทาง'}
				<span class="mono ml-1 text-muted">{formatId(viewer.deviceId)}</span>
			</p>
			{#if viewer.keyFingerprint}
				<p class="mono text-[11px] text-muted" title="ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `redesk-agent info`">
					<Icon name="lock" size={11} class="-mt-0.5 inline" /> E2E · {viewer.keyFingerprint}
				</p>
			{/if}
		</div>

		{#if viewer.phase === 'connected'}
			<div class="flex flex-1 flex-wrap items-center justify-end gap-1.5">
				<span class="badge mono mr-2">{viewer.stats.fps} fps · {viewer.stats.kbps} kbps</span>
				{#if viewer.info && viewer.info.displays.length > 1}
					<select class="input w-auto py-1" value={viewer.info.display} onchange={(e) => viewer.configure({ display: +e.currentTarget.value })}>
						{#each viewer.info.displays as d (d.index)}
							<option value={d.index}>{d.name}{d.primary ? ' (หลัก)' : ''}</option>
						{/each}
					</select>
				{/if}
				<select class="input w-auto py-1" value={quality} onchange={(e) => setQuality(e.currentTarget.value as typeof quality)}>
					{#each Object.entries(presets) as [k, p] (k)}<option value={k}>{p.label}</option>{/each}
				</select>
				{#if viewer.info?.clipboard}
					<button class="btn" title="ส่งคลิปบอร์ดของเราไปยังเครื่องปลายทาง" onclick={() => sendClipboard(false)}>
						<Icon name="clipboard" /> <span class="hidden 2xl:inline">ส่งคลิปบอร์ด</span>
					</button>
				{/if}
				{#if viewer.info?.input}
					<button class="btn" title="พิมพ์ข้อความในคลิปบอร์ดลงเครื่องปลายทาง" onclick={() => sendClipboard(true)}>
						<Icon name="keyboard" /> <span class="hidden 2xl:inline">พิมพ์จากคลิปบอร์ด</span>
					</button>
					<button class="btn hidden md:inline-flex" onclick={sendCtrlAltDel}>Ctrl+Alt+Del</button>
				{/if}
				{#if viewer.info?.files}
					<button class="btn {showFiles ? 'border-accent text-accent' : ''}" title="โอนไฟล์" onclick={() => (showFiles = !showFiles)}>
						<Icon name="folder" /> <span class="hidden 2xl:inline">ไฟล์</span>
					</button>
				{/if}
				<button class="btn" title="รีเฟรชภาพ" onclick={() => viewer.refresh()}><Icon name="refresh" /></button>
				<button class="btn" title="เต็มจอ" onclick={fullscreen}><Icon name="maximize" /></button>
				<button class="btn btn-danger" onclick={() => viewer.close()}>ตัดการเชื่อมต่อ</button>
			</div>
		{/if}
	</header>

	<div class="flex min-h-0 flex-1">
	<div
		bind:this={stage}
		class="relative grid min-h-0 flex-1 place-items-center overflow-hidden bg-black"
		role="application"
		ondragover={(e) => {
			if (viewer.phase === 'connected' && viewer.info?.files) {
				e.preventDefault();
				dragging = true;
			}
		}}
		ondragleave={(e) => {
			if (e.currentTarget === e.target) dragging = false;
		}}
		ondrop={onDrop}
	>
		<canvas
			bind:this={canvas}
			tabindex="0"
			class="max-h-full max-w-full outline-none {viewer.phase === 'connected' ? '' : 'opacity-30'} {live ? 'cursor-default' : ''}"
			style="image-rendering: auto;"
			onpointermove={onPointerMove}
			onpointerdown={(e) => onPointerButton(e, true)}
			onpointerup={(e) => onPointerButton(e, false)}
			oncontextmenu={(e) => e.preventDefault()}
			onfocus={() => (focused = true)}
			onblur={() => {
				focused = false;
				releaseAll();
			}}
		></canvas>

		{#if viewer.phase === 'connected' && viewer.frame.w === 0}
			<p class="absolute text-sm text-white/70">กำลังรอภาพหน้าจอ…</p>
		{/if}

		{#if viewer.phase !== 'connected'}
			<div class="absolute inset-0 grid place-items-center p-4">
				<div class="card w-full max-w-sm p-6 text-center shadow-2xl">
					{#if viewer.phase === 'auth'}
						<Icon name="lock" size={28} class="mx-auto mb-3 text-accent" />
						<h2 class="font-semibold">ใส่รหัสผ่านของเครื่องปลายทาง</h2>
						<p class="mt-1 text-sm text-muted">ดูรหัสชั่วคราวได้ที่หน้าจอ ReDesk agent ของเครื่องนั้น</p>
						<form
							class="mt-4 space-y-3"
							onsubmit={(e) => {
								e.preventDefault();
								viewer.submitPassword(password);
								password = '';
							}}
						>
							<!-- svelte-ignore a11y_autofocus -->
							<input class="input mono text-center" type="password" autocomplete="off" autofocus required bind:value={password} />
							{#if viewer.error}<p class="text-sm text-danger">{viewer.error}</p>{/if}
							<button class="btn btn-primary w-full" disabled={viewer.authBusy}>{viewer.authBusy ? 'กำลังตรวจสอบ…' : 'เชื่อมต่อ'}</button>
						</form>
					{:else if viewer.phase === 'closed'}
						<h2 class="font-semibold">{viewer.error ? 'เชื่อมต่อไม่สำเร็จ' : 'ตัดการเชื่อมต่อแล้ว'}</h2>
						{#if viewer.error}<p class="mt-2 text-sm text-muted">{viewer.error}</p>{/if}
						<div class="mt-5 flex justify-center gap-2">
							<a class="btn" href="/">กลับหน้าหลัก</a>
							<button class="btn btn-primary" onclick={() => location.reload()}>ลองอีกครั้ง</button>
						</div>
					{:else}
						<div class="mx-auto mb-4 size-8 animate-spin rounded-full border-2 border-line border-t-accent"></div>
						<p class="text-sm">{phaseText[viewer.phase]}</p>
						{#if viewer.error}<p class="mt-2 text-sm text-danger">{viewer.error}</p>{/if}
					{/if}
				</div>
			</div>
		{/if}

		{#if toast}
			<div class="absolute bottom-5 left-1/2 -translate-x-1/2 rounded-lg bg-surface px-4 py-2 text-sm shadow-lg">{toast}</div>
		{/if}

		{#if dragging}
			<div class="pointer-events-none absolute inset-3 grid place-items-center rounded-xl border-2 border-dashed border-accent bg-black/60 text-white">
				<div class="text-center">
					<Icon name="upload" size={32} class="mx-auto mb-2" />
					<p class="font-medium">วางไฟล์เพื่อส่งไปยังเครื่องปลายทาง</p>
					<p class="mono mt-1 text-xs text-white/70">{viewer.files.path || '~/Downloads/ReDesk'}</p>
				</div>
			</div>
		{/if}
	</div>
	{#if showFiles && viewer.phase === 'connected'}
		<FilePanel {viewer} onclose={() => (showFiles = false)} />
	{/if}
	</div>
</div>
