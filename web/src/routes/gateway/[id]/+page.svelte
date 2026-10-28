<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	// The package ships a default export at runtime; @types describes the same API as a namespace.
	import GuacamoleRuntime from 'guacamole-common-js';
	import type * as G from 'guacamole-common-js';
	import { api, type Endpoint } from '#lib/api.js';
	import Icon from '#lib/Icon.svelte';
	import { providers } from '#lib/providers.js';

	const Guacamole = GuacamoleRuntime as unknown as typeof G;
	const id = page.params.id ?? '';

	let endpoint = $state<Endpoint | null>(null);
	let stage = $state<HTMLElement>();
	let screen = $state<HTMLElement>();
	let status = $state<'connecting' | 'waiting' | 'connected' | 'closed'>('connecting');
	let error = $state('');
	let toast = $state('');
	let client: G.Client | null = null;

	const errors: Record<number, string> = {
		512: 'เกิดข้อผิดพลาดที่ gateway',
		514: 'เชื่อมต่อเครื่องปลายทางไม่สำเร็จ (ตรวจโฮสต์ พอร์ต และ firewall)',
		516: 'เครื่องปลายทางไม่ตอบสนอง',
		519: 'ไม่พบเครื่องปลายทาง',
		769: 'ชื่อผู้ใช้หรือรหัสผ่านไม่ถูกต้อง หรือยังไม่ได้ล็อกอิน',
		771: 'คุณไม่มีสิทธิ์ใช้การเชื่อมต่อนี้',
		776: 'หมดเวลาเพราะไม่มีการใช้งาน'
	};

	function flash(msg: string) {
		toast = msg;
		setTimeout(() => (toast = ''), 2500);
	}

	onMount(() => {
		api.get<{ endpoints: Endpoint[] }>('/api/endpoints').then((r) => (endpoint = r.endpoints.find((e) => String(e.id) === id) ?? null));

		const tunnel = new Guacamole.WebSocketTunnel('/ws/gateway');
		const c = new Guacamole.Client(tunnel);
		client = c;
		const display = c.getDisplay();
		const el = display.getElement();
		screen!.appendChild(el);

		const fit = () => {
			const w = display.getWidth();
			const h = display.getHeight();
			if (!w || !h || !stage) return;
			display.scale(Math.min(stage.clientWidth / w, stage.clientHeight / h, 1));
		};
		display.onresize = fit;

		c.onstatechange = (s: number) => {
			// 1 CONNECTING, 2 WAITING, 3 CONNECTED, 4 DISCONNECTING, 5 DISCONNECTED
			if (s === 2) status = 'waiting';
			else if (s === 3) {
				status = 'connected';
				screen?.focus();
			} else if (s >= 4) status = 'closed';
		};
		const onError = (st: G.Status) => {
			error = errors[st.code] ?? `การเชื่อมต่อล้มเหลว (${st.code}${st.message ? `: ${st.message}` : ''})`;
			status = 'closed';
		};
		c.onerror = onError;
		tunnel.onerror = onError;

		// Remote clipboard -> local clipboard.
		c.onclipboard = (stream, mimetype) => {
			if (!/^text\//.test(mimetype)) return;
			const reader = new Guacamole.StringReader(stream);
			let text = '';
			reader.ontext = (t) => (text += t);
			reader.onend = () => {
				navigator.clipboard?.writeText(text).then(() => flash('ได้รับคลิปบอร์ดจากเครื่องปลายทาง')).catch(() => {});
			};
		};

		const mouse = new Guacamole.Mouse(el);
		mouse.onEach(['mousedown', 'mousemove', 'mouseup'], (e) => c.sendMouseState((e as G.Mouse.Event).state, true));

		// Keyboard only while the remote screen has focus.
		const keyboard = new Guacamole.Keyboard(screen!);
		keyboard.onkeydown = (keysym) => {
			c.sendKeyEvent(1, keysym);
			return false;
		};
		keyboard.onkeyup = (keysym) => c.sendKeyEvent(0, keysym);

		const size = () => ({
			width: Math.floor(stage!.clientWidth * devicePixelRatio),
			height: Math.floor(stage!.clientHeight * devicePixelRatio),
			dpi: Math.round(96 * devicePixelRatio)
		});
		const s = size();
		c.connect(new URLSearchParams({ endpoint: id, width: String(s.width), height: String(s.height), dpi: String(s.dpi) }).toString());

		// Ask the remote side to match the window (RDP display-update), then refit.
		let resizeTimer: ReturnType<typeof setTimeout>;
		const ro = new ResizeObserver(() => {
			fit();
			clearTimeout(resizeTimer);
			resizeTimer = setTimeout(() => {
				const n = size();
				if (status === 'connected') c.sendSize(n.width, n.height);
			}, 400);
		});
		ro.observe(stage!);

		return () => {
			ro.disconnect();
			keyboard.onkeydown = null;
			keyboard.onkeyup = null;
			keyboard.reset();
			c.disconnect();
		};
	});

	async function sendClipboard() {
		if (!client) return;
		try {
			const text = await navigator.clipboard.readText();
			const writer = new Guacamole.StringWriter(client.createClipboardStream('text/plain'));
			writer.sendText(text);
			writer.sendEnd();
			flash('ส่งคลิปบอร์ดแล้ว — กด Ctrl+V ที่เครื่องปลายทาง');
		} catch {
			flash('เบราว์เซอร์ไม่อนุญาตให้อ่านคลิปบอร์ด');
		}
		screen?.focus();
	}

	function ctrlAltDel() {
		for (const k of [0xffe3, 0xffe9, 0xffff]) client?.sendKeyEvent(1, k);
		for (const k of [0xffff, 0xffe9, 0xffe3]) client?.sendKeyEvent(0, k);
		screen?.focus();
	}

	function fullscreen() {
		if (document.fullscreenElement) document.exitFullscreen();
		else stage?.requestFullscreen();
	}
</script>

<svelte:head><title>{endpoint?.name ?? 'Gateway'} · ReDesk</title></svelte:head>

<div class="flex h-screen flex-col bg-canvas">
	<header class="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-surface px-4 py-2">
		<a href="/connections" class="btn btn-ghost p-1.5" title="กลับ"><Icon name="x" /></a>
		<div class="min-w-0 flex-1">
			<p class="flex items-center gap-2 truncate text-sm font-medium">
				{#if endpoint}<span class="badge {providers[endpoint.provider].color}">{providers[endpoint.provider].label}</span>{/if}
				{endpoint?.name ?? '…'}
				<span class="mono text-muted">{endpoint?.address ?? ''}</span>
			</p>
			<p class="text-[11px] text-muted"><Icon name="shield" size={11} class="-mt-0.5 inline" /> ผ่าน ReDesk gateway · รหัสผ่านไม่ถูกส่งมาที่เบราว์เซอร์ · บันทึกใน audit log</p>
		</div>
		{#if status === 'connected'}
			<div class="flex items-center gap-1.5">
				<button class="btn" title="ส่งคลิปบอร์ดไปเครื่องปลายทาง" onclick={sendClipboard}><Icon name="clipboard" /> <span class="hidden xl:inline">ส่งคลิปบอร์ด</span></button>
				{#if endpoint?.provider !== 'ssh'}<button class="btn" onclick={ctrlAltDel}>Ctrl+Alt+Del</button>{/if}
				<button class="btn" title="เต็มจอ" onclick={fullscreen}><Icon name="maximize" /></button>
				<button class="btn btn-danger" onclick={() => client?.disconnect()}>ตัดการเชื่อมต่อ</button>
			</div>
		{/if}
	</header>

	<div bind:this={stage} class="relative grid min-h-0 flex-1 place-items-center overflow-hidden bg-black">
		<!-- svelte-ignore a11y_no_noninteractive_tabindex -- the remote screen must take keyboard focus -->
		<div bind:this={screen} tabindex="0" role="application" aria-label="หน้าจอเครื่องปลายทาง" class="outline-none"></div>

		{#if status !== 'connected'}
			<div class="absolute inset-0 grid place-items-center p-4">
				<div class="card w-full max-w-sm p-6 text-center shadow-2xl">
					{#if status === 'closed'}
						<h2 class="font-semibold">{error ? 'เชื่อมต่อไม่สำเร็จ' : 'ตัดการเชื่อมต่อแล้ว'}</h2>
						{#if error}<p class="mt-2 text-sm text-muted">{error}</p>{/if}
						<div class="mt-5 flex justify-center gap-2">
							<a class="btn" href="/connections">กลับ</a>
							<button class="btn btn-primary" onclick={() => location.reload()}>ลองอีกครั้ง</button>
						</div>
					{:else}
						<div class="mx-auto mb-4 size-8 animate-spin rounded-full border-2 border-line border-t-accent"></div>
						<p class="text-sm">{status === 'waiting' ? 'กำลังเชื่อมต่อเครื่องปลายทาง…' : 'กำลังเปิด gateway…'}</p>
					{/if}
				</div>
			</div>
		{/if}

		{#if toast}
			<div class="absolute bottom-5 left-1/2 -translate-x-1/2 rounded-lg bg-surface px-4 py-2 text-sm shadow-lg">{toast}</div>
		{/if}
	</div>
</div>
