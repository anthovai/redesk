<script lang="ts">
	import QRCode from 'qrcode';
	import { goto } from '$app/navigation';
	import { api } from '#lib/api.js';
	import { auth, loadMe, logout } from '#lib/session.svelte.js';

	let pw = $state({ current: '', new: '', repeat: '' });
	let pwMsg = $state<[string, boolean] | null>(null);

	let totpSetup = $state<{ secret: string; qr: string } | null>(null);
	let totpCode = $state('');
	let disablePw = $state('');
	let totpMsg = $state<[string, boolean] | null>(null);

	async function changePassword(ev: SubmitEvent) {
		ev.preventDefault();
		if (pw.new !== pw.repeat) return (pwMsg = ['รหัสผ่านใหม่ไม่ตรงกัน', false]);
		try {
			await api.post('/api/auth/password', { current: pw.current, new: pw.new });
			pw = { current: '', new: '', repeat: '' };
			pwMsg = ['เปลี่ยนรหัสผ่านแล้ว', true];
		} catch (e) {
			pwMsg = [(e as Error).message, false];
		}
	}

	async function startTotp() {
		const r = await api.post<{ secret: string; uri: string }>('/api/auth/totp/setup');
		totpSetup = { secret: r.secret, qr: await QRCode.toDataURL(r.uri, { margin: 1, width: 200 }) };
	}

	async function enableTotp(ev: SubmitEvent) {
		ev.preventDefault();
		try {
			await api.post('/api/auth/totp/enable', { code: totpCode });
			totpSetup = null;
			totpCode = '';
			totpMsg = ['เปิดใช้ 2FA แล้ว', true];
			await loadMe();
		} catch (e) {
			totpMsg = [(e as Error).message, false];
		}
	}

	async function disableTotp(ev: SubmitEvent) {
		ev.preventDefault();
		try {
			await api.post('/api/auth/totp/disable', { password: disablePw });
			disablePw = '';
			totpMsg = ['ปิด 2FA แล้ว', true];
			await loadMe();
		} catch (e) {
			totpMsg = [(e as Error).message, false];
		}
	}
</script>

<svelte:head><title>บัญชีของฉัน · ReDesk</title></svelte:head>

<h1 class="mb-1 text-2xl font-semibold tracking-tight">บัญชีของฉัน</h1>
<p class="mb-6 text-sm text-muted">{auth.user?.email}</p>

<div class="grid gap-6 lg:grid-cols-2">
	<form class="card space-y-4 p-6" onsubmit={changePassword}>
		<h2 class="font-semibold">เปลี่ยนรหัสผ่าน</h2>
		<div><label class="label" for="p1">รหัสผ่านปัจจุบัน</label><input id="p1" class="input" type="password" autocomplete="current-password" required bind:value={pw.current} /></div>
		<div><label class="label" for="p2">รหัสผ่านใหม่ (อย่างน้อย 10 ตัว)</label><input id="p2" class="input" type="password" autocomplete="new-password" minlength="10" required bind:value={pw.new} /></div>
		<div><label class="label" for="p3">ยืนยันรหัสผ่านใหม่</label><input id="p3" class="input" type="password" autocomplete="new-password" required bind:value={pw.repeat} /></div>
		{#if pwMsg}<p class="text-sm {pwMsg[1] ? 'text-ok' : 'text-danger'}">{pwMsg[0]}</p>{/if}
		<button class="btn btn-primary">บันทึก</button>
	</form>

	<section class="card space-y-4 p-6">
		<div>
			<h2 class="font-semibold">การยืนยันตัวตนสองขั้นตอน (2FA)</h2>
			<p class="mt-1 text-sm text-muted">ใช้แอป Authenticator (TOTP) — รหัสไม่ถูกส่งผ่านบริการภายนอกใดๆ</p>
		</div>
		{#if auth.user?.totp_enabled}
			<p class="text-sm text-ok">เปิดใช้งานอยู่</p>
			<form class="flex gap-2" onsubmit={disableTotp}>
				<input class="input" type="password" placeholder="ยืนยันด้วยรหัสผ่าน" required bind:value={disablePw} />
				<button class="btn btn-danger shrink-0">ปิด 2FA</button>
			</form>
		{:else if totpSetup}
			<div class="flex flex-wrap items-start gap-4">
				<img src={totpSetup.qr} alt="QR code สำหรับแอป Authenticator" class="size-40 rounded-lg bg-white p-1" />
				<div class="min-w-0 flex-1 text-sm">
					<p>สแกน QR ด้วยแอป Authenticator หรือใส่รหัสนี้เอง</p>
					<code class="mono mt-2 block text-xs break-all">{totpSetup.secret}</code>
				</div>
			</div>
			<form class="flex gap-2" onsubmit={enableTotp}>
				<input class="input mono" inputmode="numeric" maxlength="6" placeholder="รหัส 6 หลัก" required bind:value={totpCode} />
				<button class="btn btn-primary shrink-0">ยืนยันและเปิดใช้</button>
			</form>
		{:else}
			<button class="btn btn-primary" onclick={startTotp}>ตั้งค่า 2FA</button>
		{/if}
		{#if totpMsg}<p class="text-sm {totpMsg[1] ? 'text-ok' : 'text-danger'}">{totpMsg[0]}</p>{/if}
	</section>
</div>

<button
	class="btn mt-8 md:hidden"
	onclick={async () => {
		await logout();
		goto('/login');
	}}>ออกจากระบบ</button
>
