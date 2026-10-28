<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { formatId } from '#lib/format.js';
	import FilePanel from '#lib/FilePanel.svelte';
	import Icon from '#lib/Icon.svelte';
	import { RemoteViewer } from '#lib/viewer.svelte.js';

	const viewer = new RemoteViewer(page.params.id ?? '');
	let canvas = $state<HTMLCanvasElement>();
	let pointerOver = $state(false);
	let stage = $state<HTMLElement>();
	let password = $state('');
	let quality = $state<'high' | 'balanced' | 'low'>('balanced');
	let focused = $state(false);
	let toast = $state('');
	let showFiles = $state(false);
	let dragging = $state(false);
	const held = new Map<string, string>(); // code -> key, for releasing on blur

	// Floating toolbar (RustDesk-style): pinned stays open, unpinned collapses to a handle.
	function pref(key: string, fallback: boolean): boolean {
		try {
			const v = localStorage.getItem(key);
			return v === null ? fallback : v === '1';
		} catch {
			return fallback;
		}
	}
	function setPref(key: string, v: boolean) {
		try {
			localStorage.setItem(key, v ? '1' : '0');
		} catch {
			// ignore
		}
	}
	let pinned = $state(pref('redesk.viewer.pinned', true));
	let barShown = $state(false);
	let menu = $state<'' | 'actions' | 'display'>('');
	let showMonitor = $state(pref('redesk.viewer.monitor', false));
	let isFullscreen = $state(false);
	let toolbar = $state<HTMLElement>();
	let hideTimer: ReturnType<typeof setTimeout> | undefined;
	const barOpen = $derived(pinned || barShown || menu !== '');

	function togglePin() {
		pinned = !pinned;
		setPref('redesk.viewer.pinned', pinned);
		barShown = !pinned;
	}
	function collapse() {
		pinned = false;
		setPref('redesk.viewer.pinned', false);
		barShown = false;
		menu = '';
		canvas?.focus();
	}
	function revealBar() {
		clearTimeout(hideTimer);
		barShown = true;
	}
	function scheduleHide() {
		clearTimeout(hideTimer);
		if (!pinned) hideTimer = setTimeout(() => (barShown = false), 700);
	}
	function toggleMenu(m: 'actions' | 'display') {
		menu = menu === m ? '' : m;
	}
	/** Run a toolbar action, close the menu and give the keyboard back to the remote screen. */
	function act(fn: () => unknown) {
		menu = '';
		fn();
		canvas?.focus();
	}
	function toggleMonitor() {
		showMonitor = !showMonitor;
		setPref('redesk.viewer.monitor', showMonitor);
	}

	// Scale the remote screen to fill the stage while keeping its aspect ratio.
	let stageW = $state(0);
	let stageH = $state(0);
	const fit = $derived.by(() => {
		const { w, h } = viewer.frame;
		if (!w || !h || !stageW || !stageH) return '';
		const k = Math.min(stageW / w, stageH / h);
		return `width: ${Math.floor(w * k)}px; height: ${Math.floor(h * k)}px;`;
	});
	const peerName = $derived(viewer.device?.alias || viewer.info?.hostname || viewer.device?.hostname || formatId(viewer.deviceId));
	const codecLabel = $derived((viewer.info?.codec ?? 'jpeg').toUpperCase());

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
		if (down && ev.key === 'Escape' && menu) {
			menu = '';
			return;
		}
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

	function onWindowPointerDown(ev: PointerEvent) {
		if (menu && toolbar && !toolbar.contains(ev.target as Node)) menu = '';
	}

	function fullscreen() {
		if (document.fullscreenElement) document.exitFullscreen();
		else stage?.requestFullscreen();
	}

	$effect(() => {
		const sync = () => (isFullscreen = !!document.fullscreenElement);
		document.addEventListener('fullscreenchange', sync);
		return () => document.removeEventListener('fullscreenchange', sync);
	});

	$effect(() => {
		if (viewer.remoteClipboard) flash('ได้รับคลิปบอร์ดจากเครื่องปลายทาง');
	});

	const tb = (active = false) =>
		`grid size-8 shrink-0 place-items-center rounded-md transition-colors ${active ? 'bg-accent-soft text-accent' : 'text-fg/75 hover:bg-raised hover:text-fg'}`;
	const mi = 'flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-sm transition-colors hover:bg-accent hover:text-accent-fg';
</script>

<svelte:head><title>{peerName} · ReDesk</title></svelte:head>
<svelte:window onkeydown={(e) => onKey(e, true)} onkeyup={(e) => onKey(e, false)} onblur={releaseAll} onpointerdown={onWindowPointerDown} />

<div class="flex h-screen bg-stage">
	<div
		bind:this={stage}
		bind:clientWidth={stageW}
		bind:clientHeight={stageH}
		class="relative grid min-h-0 min-w-0 flex-1 place-items-center overflow-hidden bg-stage text-white"
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
			class="outline-none {viewer.phase === 'connected' && viewer.frame.w ? '' : 'opacity-0'} {live ? 'cursor-default' : ''}"
			style="image-rendering: auto; {fit}"
			onpointermove={onPointerMove}
			onpointerenter={() => (pointerOver = true)}
			onpointerleave={() => (pointerOver = false)}
			onpointerdown={(e) => onPointerButton(e, true)}
			onpointerup={(e) => onPointerButton(e, false)}
			oncontextmenu={(e) => e.preventDefault()}
			onfocus={() => (focused = true)}
			onblur={() => {
				focused = false;
				releaseAll();
			}}
		></canvas>

		{#if viewer.cursor && canvas && !pointerOver}
			<!-- Remote pointer: screen captures do not include it. -->
			<svg
				class="pointer-events-none absolute"
				style="left: {canvas.offsetLeft + viewer.cursor.x * canvas.clientWidth}px; top: {canvas.offsetTop + viewer.cursor.y * canvas.clientHeight}px"
				width="14"
				height="20"
				viewBox="-1 -1 14 20"
				aria-hidden="true"
			>
				<path d="M0 0V16L4.2 12.3L7 18.5L9.6 17.4L6.9 11.3H12Z" fill="#000" stroke="#fff" stroke-width="1.2" stroke-linejoin="round" />
			</svg>
		{/if}

		{#if viewer.phase === 'connected' && viewer.frame.w === 0}
			<div class="absolute grid place-items-center gap-3 text-sm text-white/60">
				<div class="size-7 animate-spin rounded-full border-2 border-white/20 border-t-white/80"></div>
				กำลังรอภาพหน้าจอ…
			</div>
		{/if}

		{#if viewer.phase === 'connected' && showMonitor}
			<!-- Quality monitor -->
			<dl
				class="mono pointer-events-none absolute top-3 left-3 grid grid-cols-[auto_auto] gap-x-3 gap-y-0.5 rounded-md bg-black/60 px-3 py-2 text-[11px] leading-snug text-white/90 shadow-lg backdrop-blur-sm"
			>
				<dt class="text-white/55">FPS</dt>
				<dd>{viewer.stats.fps}</dd>
				<dt class="text-white/55">บิตเรต</dt>
				<dd>{viewer.stats.kbps} kbps</dd>
				<dt class="text-white/55">Codec</dt>
				<dd>{codecLabel}</dd>
				<dt class="text-white/55">ความละเอียด</dt>
				<dd>{viewer.frame.w ? `${viewer.frame.w}×${viewer.frame.h}` : '—'}</dd>
				<dt class="text-white/55">E2E</dt>
				<dd>{viewer.keyFingerprint || '—'}</dd>
			</dl>
		{/if}

		{#if viewer.phase === 'connected'}
			<!-- Floating toolbar -->
			<div
				class="absolute top-0 left-1/2 z-10 -translate-x-1/2"
				bind:this={toolbar}
				role="toolbar"
				tabindex="-1"
				aria-label="แถบเครื่องมือ"
				onmouseenter={revealBar}
				onmouseleave={scheduleHide}
			>
				{#if barOpen}
					<div class="mt-2 flex items-center gap-0.5 rounded-xl bg-surface/95 p-1 text-fg shadow-[0_4px_20px_rgb(0_0_0/0.35)] ring-1 ring-line backdrop-blur">
						<button
							class={tb(pinned)}
							title={pinned ? 'เลิกปักหมุดแถบเครื่องมือ' : 'ปักหมุดแถบเครื่องมือ'}
							aria-pressed={pinned}
							onclick={togglePin}
						>
							<Icon name="pin" />
						</button>

						<div class="mx-1 flex min-w-0 items-center gap-1.5 px-1" title={viewer.keyFingerprint ? `เข้ารหัสต้นทางถึงปลายทาง · ลายนิ้วมือ ${viewer.keyFingerprint}` : ''}>
							<Icon name="lock" size={13} class="shrink-0 text-ok" />
							<span class="max-w-40 truncate text-[13px] font-medium">{peerName}</span>
						</div>

						{#if viewer.info && viewer.info.displays.length > 1}
							<span class="mx-1 h-5 w-px bg-line"></span>
							{#each viewer.info.displays as d (d.index)}
								<button
									class="relative grid h-8 w-9 shrink-0 place-items-center rounded-md transition-colors {viewer.info.display === d.index
										? 'text-accent'
										: 'text-fg/60 hover:bg-raised hover:text-fg'}"
									title="{d.name}{d.primary ? ' (หลัก)' : ''} · {d.width}×{d.height}"
									onclick={() => act(() => viewer.configure({ display: d.index }))}
								>
									<Icon name="monitor" size={22} />
									<span class="absolute top-[7px] text-[10px] font-bold">{d.index + 1}</span>
								</button>
							{/each}
						{/if}

						<span class="mx-1 h-5 w-px bg-line"></span>

						<div class="relative">
							<button
								class={tb(menu === 'actions')}
								title="คำสั่ง"
								aria-haspopup="menu"
								aria-expanded={menu === 'actions'}
								onclick={() => toggleMenu('actions')}
							>
								<Icon name="zap" />
							</button>
							{#if menu === 'actions'}
								<div class="absolute top-full left-1/2 mt-2 w-60 -translate-x-1/2 rounded-lg bg-surface py-1 text-fg shadow-[0_10px_30px_rgb(0_0_0/0.35)] ring-1 ring-line" role="menu">
									{#if viewer.info?.input}
										<button class={mi} role="menuitem" onclick={() => act(sendCtrlAltDel)}><Icon name="keyboard" size={15} /> ส่ง Ctrl+Alt+Del</button>
									{/if}
									<button class={mi} role="menuitem" onclick={() => act(() => viewer.refresh())}><Icon name="refresh" size={15} /> รีเฟรชภาพ (ขอคีย์เฟรมใหม่)</button>
									{#if viewer.info?.clipboard || viewer.info?.input}
										<div class="my-1 border-t border-line"></div>
									{/if}
									{#if viewer.info?.clipboard}
										<button class={mi} role="menuitem" title="ส่งคลิปบอร์ดของเราไปยังเครื่องปลายทาง" onclick={() => act(() => sendClipboard(false))}>
											<Icon name="clipboard" size={15} /> ส่งคลิปบอร์ด
										</button>
									{/if}
									{#if viewer.info?.input}
										<button class={mi} role="menuitem" title="พิมพ์ข้อความในคลิปบอร์ดลงเครื่องปลายทาง" onclick={() => act(() => sendClipboard(true))}>
											<Icon name="copy" size={15} /> พิมพ์จากคลิปบอร์ด
										</button>
									{/if}
									{#if viewer.info?.files}
										<div class="my-1 border-t border-line"></div>
										<button class={mi} role="menuitemcheckbox" aria-checked={showFiles} onclick={() => act(() => (showFiles = !showFiles))}>
											<Icon name="folder" size={15} /> <span class="flex-1">โอนไฟล์</span>
											{#if showFiles}<Icon name="check" size={15} />{/if}
										</button>
									{/if}
								</div>
							{/if}
						</div>

						<div class="relative">
							<button
								class={tb(menu === 'display')}
								title="การแสดงผล"
								aria-haspopup="menu"
								aria-expanded={menu === 'display'}
								onclick={() => toggleMenu('display')}
							>
								<Icon name="sliders" />
							</button>
							{#if menu === 'display'}
								<div class="absolute top-full left-1/2 mt-2 w-60 -translate-x-1/2 rounded-lg bg-surface py-1 text-fg shadow-[0_10px_30px_rgb(0_0_0/0.35)] ring-1 ring-line" role="menu">
									<p class="px-3 pt-1 pb-1 text-xs font-medium text-muted">คุณภาพภาพ</p>
									{#each Object.entries(presets) as [k, p] (k)}
										<button class={mi} role="menuitemradio" aria-checked={quality === k} onclick={() => act(() => setQuality(k as typeof quality))}>
											<span class="grid size-[15px] place-items-center">
												<span class="size-2 rounded-full {quality === k ? 'bg-accent ring-2 ring-surface' : ''}"></span>
											</span>
											{p.label}
										</button>
									{/each}
									<div class="my-1 border-t border-line"></div>
									<button class={mi} role="menuitemcheckbox" aria-checked={showMonitor} onclick={() => act(toggleMonitor)}>
										<Icon name="activity" size={15} /> <span class="flex-1">แสดงตัวตรวจวัดคุณภาพ</span>
										{#if showMonitor}<Icon name="check" size={15} />{/if}
									</button>
									<div class="my-1 border-t border-line"></div>
									<div class="flex items-center justify-between px-3 py-1.5 text-sm">
										<span class="text-muted">Codec</span>
										<span class="mono text-xs">{codecLabel}{viewer.info?.codec === 'h264' ? ' · WebCodecs' : ''}</span>
									</div>
								</div>
							{/if}
						</div>

						<button class={tb()} title={isFullscreen ? 'ออกจากเต็มจอ' : 'เต็มจอ'} onclick={() => act(fullscreen)}>
							<Icon name={isFullscreen ? 'minimize' : 'maximize'} />
						</button>

						<span class="mx-1 h-5 w-px bg-line"></span>

						<button class={tb()} title="ซ่อนแถบเครื่องมือ" onclick={collapse}><Icon name="chevronUp" /></button>
						<button
							class="grid size-8 shrink-0 place-items-center rounded-md bg-danger text-white transition-[filter] hover:brightness-110"
							title="ตัดการเชื่อมต่อ"
							onclick={() => viewer.close()}
						>
							<Icon name="x" />
						</button>
					</div>
				{:else}
					<button
						class="grid h-5 w-14 place-items-center rounded-b-lg bg-surface/90 pb-0.5 text-fg/70 shadow-[0_2px_10px_rgb(0_0_0/0.35)] ring-1 ring-line transition-all hover:h-6 hover:text-accent"
						title="แสดงแถบเครื่องมือ"
						aria-label="▾ แสดงแถบเครื่องมือ"
						onclick={revealBar}><Icon name="chevronDown" size={16} /></button
					>
				{/if}
			</div>
		{/if}

		{#if viewer.phase !== 'connected'}
			<div class="absolute inset-0 grid place-items-center bg-black/30 p-4">
				<div class="w-full max-w-[400px] overflow-hidden rounded-xl bg-surface text-fg shadow-[0_20px_60px_rgb(0_0_0/0.5)]" role="dialog" aria-modal="true">
					{#if viewer.phase === 'auth'}
						<form
							onsubmit={(e) => {
								e.preventDefault();
								viewer.submitPassword(password);
								password = '';
							}}
						>
							<div class="flex items-center gap-3 px-6 pt-5">
								<span class="grid size-9 shrink-0 place-items-center rounded-full bg-accent-soft text-accent"><Icon name="lock" size={18} /></span>
								<div class="min-w-0">
									<h2 class="font-semibold">ใส่รหัสผ่าน</h2>
									<p class="truncate text-xs text-muted">{peerName} · <span class="mono">{formatId(viewer.deviceId)}</span></p>
								</div>
							</div>
							<div class="space-y-3 px-6 py-4">
								<p class="text-sm text-muted">ดูรหัสชั่วคราวได้ที่หน้าจอ ReDesk agent ของเครื่องนั้น</p>
								<!-- svelte-ignore a11y_autofocus -->
								<input class="input mono" type="password" autocomplete="off" placeholder="รหัสผ่าน" autofocus required bind:value={password} />
								{#if viewer.error}<p class="text-sm text-danger">{viewer.error}</p>{/if}
								{#if viewer.keyFingerprint}
									<p class="mono text-[11px] text-muted" title="ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `redesk-agent info`">E2E · {viewer.keyFingerprint}</p>
								{/if}
							</div>
							<div class="flex justify-end gap-2 border-t border-line bg-raised/60 px-6 py-3">
								<button type="button" class="btn min-w-20" onclick={() => goto('/')}>ยกเลิก</button>
								<button class="btn btn-primary min-w-20" disabled={viewer.authBusy}>{viewer.authBusy ? 'กำลังตรวจสอบ…' : 'ตกลง'}</button>
							</div>
						</form>
					{:else if viewer.phase === 'closed'}
						<div class="flex items-start gap-3 px-6 pt-5 pb-4">
							<span class="grid size-9 shrink-0 place-items-center rounded-full {viewer.error ? 'bg-danger-soft text-danger' : 'bg-raised text-muted'}">
								<Icon name={viewer.error ? 'alert' : 'power'} size={18} />
							</span>
							<div class="min-w-0 pt-1.5">
								<h2 class="font-semibold">{viewer.error ? 'เชื่อมต่อไม่สำเร็จ' : 'ตัดการเชื่อมต่อแล้ว'}</h2>
								<p class="mt-1 text-sm text-muted">{viewer.error ?? `${peerName} · ${formatId(viewer.deviceId)}`}</p>
							</div>
						</div>
						<div class="flex justify-end gap-2 border-t border-line bg-raised/60 px-6 py-3">
							<a class="btn min-w-20" href="/">กลับหน้าหลัก</a>
							<button class="btn btn-primary min-w-20" onclick={() => location.reload()}>ลองอีกครั้ง</button>
						</div>
					{:else}
						<div class="flex items-center gap-3 px-6 pt-5">
							<span class="grid size-9 shrink-0 place-items-center rounded-full bg-accent-soft text-accent"><Icon name="link" size={18} /></span>
							<div class="min-w-0">
								<h2 class="font-semibold">กำลังเชื่อมต่อ</h2>
								<p class="truncate text-xs text-muted">{peerName} · <span class="mono">{formatId(viewer.deviceId)}</span></p>
							</div>
						</div>
						<div class="px-6 py-5">
							<p class="text-sm">{phaseText[viewer.phase]}</p>
							<div class="mt-3 h-1 overflow-hidden rounded-full bg-raised">
								<div class="progress h-full w-1/3 rounded-full bg-accent"></div>
							</div>
							{#if viewer.error}<p class="mt-3 text-sm text-danger">{viewer.error}</p>{/if}
							{#if viewer.keyFingerprint}
								<p class="mono mt-3 text-[11px] text-muted" title="ลายนิ้วมือกุญแจของเครื่อง ตรวจสอบกับ `redesk-agent info`">E2E · {viewer.keyFingerprint}</p>
							{/if}
						</div>
						<div class="flex justify-end border-t border-line bg-raised/60 px-6 py-3">
							<button class="btn min-w-20" onclick={() => goto('/')}>ยกเลิก</button>
						</div>
					{/if}
				</div>
			</div>
		{/if}

		{#if toast}
			<div class="absolute bottom-6 left-1/2 -translate-x-1/2 rounded-full bg-black/75 px-4 py-2 text-sm text-white shadow-lg backdrop-blur" role="status">{toast}</div>
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

<style>
	.progress {
		animation: slide 1.2s ease-in-out infinite;
	}
	@keyframes slide {
		from {
			transform: translateX(-100%);
		}
		to {
			transform: translateX(300%);
		}
	}
</style>
