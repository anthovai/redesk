<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { api, type ServerInfo, type User } from '#lib/api.js';
	import { auth } from '#lib/session.svelte.js';

	let email = $state('');
	let password = $state('');
	let totp = $state('');
	let needTotp = $state(false);
	let busy = $state(false);
	let error = $state('');
	let sso = $state<ServerInfo['oidc'] | null>(null);

	const ssoErrors: Record<string, string> = {
		bad_state: 'คำขอเข้าสู่ระบบไม่ถูกต้องหรือหมดอายุ กรุณาลองใหม่',
		idp_error: 'ผู้ให้บริการ SSO ปฏิเสธการเข้าสู่ระบบ',
		disabled: 'ยังไม่ได้เปิดการเข้าสู่ระบบด้วย SSO',
		provider_unavailable: 'ติดต่อผู้ให้บริการ SSO ไม่ได้',
		exchange_failed: 'ยืนยันตัวตนกับผู้ให้บริการ SSO ไม่สำเร็จ',
		bad_token: 'ข้อมูลยืนยันตัวตนจากผู้ให้บริการ SSO ไม่ถูกต้อง',
		no_email: 'บัญชี SSO ไม่มีอีเมล',
		email_unverified: 'อีเมลของบัญชี SSO ยังไม่ได้รับการยืนยัน',
		domain_not_allowed: 'โดเมนอีเมลนี้ไม่ได้รับอนุญาต',
		no_account: 'ยังไม่มีบัญชี ReDesk สำหรับอีเมลนี้ กรุณาติดต่อผู้ดูแลระบบ',
		disabled_account: 'บัญชีนี้ถูกปิดใช้งาน'
	};
	const ssoError = $derived.by(() => {
		const code = page.url.searchParams.get('oidc_error');
		return code ? (ssoErrors[code] ?? 'เข้าสู่ระบบด้วย SSO ไม่สำเร็จ') : '';
	});
	const ssoHref = $derived(`/api/oidc/login?next=${encodeURIComponent(page.url.searchParams.get('next') || '/')}`);

	$effect(() => {
		api
			.get<ServerInfo>('/api/server-info')
			.then((i) => (sso = i.oidc ?? null))
			.catch(() => {});
	});

	$effect(() => {
		if (auth.user) goto(page.url.searchParams.get('next') || '/');
	});

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		busy = true;
		error = '';
		try {
			const r = await api.post<{ user?: User; need_totp?: boolean }>('/api/auth/login', { email, password, totp });
			if (r.need_totp) {
				needTotp = true;
			} else if (r.user) {
				auth.user = r.user;
			}
		} catch (err) {
			error = (err as Error).message;
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>เข้าสู่ระบบ · ReDesk</title></svelte:head>

<div class="grid min-h-screen place-items-center px-4">
	<div class="w-full max-w-sm">
		<div class="mb-8 flex flex-col items-center gap-3 text-center">
			<img src="/favicon.svg" alt="" class="size-12" />
			<div>
				<h1 class="text-xl font-semibold tracking-tight">ReDesk</h1>
				<p class="mt-1 text-sm text-muted">ระบบรีโมตเดสก์ท็อปที่โฮสต์เอง</p>
			</div>
		</div>
		<form class="card space-y-4 p-6" onsubmit={submit}>
			{#if !needTotp}
				<div>
					<label class="label" for="email">อีเมล</label>
					<input id="email" class="input" type="email" autocomplete="username" required bind:value={email} />
				</div>
				<div>
					<label class="label" for="password">รหัสผ่าน</label>
					<input id="password" class="input" type="password" autocomplete="current-password" required bind:value={password} />
				</div>
			{:else}
				<div>
					<label class="label" for="totp">รหัสยืนยันตัวตน 6 หลัก</label>
					<input
						id="totp"
						class="input mono text-center text-lg tracking-[0.4em]"
						inputmode="numeric"
						autocomplete="one-time-code"
						maxlength="6"
						required
						bind:value={totp}
					/>
					<p class="mt-2 text-xs text-muted">เปิดแอป Authenticator แล้วใส่รหัสของ ReDesk</p>
				</div>
			{/if}
			{#if error || ssoError}
				<p class="rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error || ssoError}</p>
			{/if}
			<button class="btn btn-primary w-full py-2" disabled={busy}>{busy ? 'กำลังตรวจสอบ…' : 'เข้าสู่ระบบ'}</button>
			{#if sso?.enabled && !needTotp}
				<div class="flex items-center gap-3 text-xs text-muted"><span class="h-px flex-1 bg-line"></span>หรือ<span class="h-px flex-1 bg-line"></span></div>
				<a class="btn w-full justify-center py-2" href={ssoHref} data-sveltekit-reload>เข้าสู่ระบบด้วย {sso.name}</a>
			{/if}
		</form>
	</div>
</div>
