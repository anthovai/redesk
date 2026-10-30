<script lang="ts">
	import './layout.css';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import Icon, { type IconName } from '#lib/Icon.svelte';
	import { auth, loadMe, logout } from '#lib/session.svelte.js';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	const nav: { href: string; label: string; icon: IconName; admin?: boolean }[] = [
		{ href: '/', label: 'สมุดที่อยู่', icon: 'book' },
		{ href: '/connections', label: 'การเชื่อมต่อ', icon: 'link' },
		{ href: '/devices', label: 'อุปกรณ์ทั้งหมด', icon: 'monitor', admin: true },
		{ href: '/groups', label: 'กลุ่มอุปกรณ์', icon: 'layers', admin: true },
		{ href: '/sessions', label: 'เซสชัน', icon: 'activity', admin: true },
		{ href: '/recordings', label: 'บันทึกเซสชัน', icon: 'video', admin: true },
		{ href: '/audit', label: 'Audit log', icon: 'shield', admin: true },
		{ href: '/users', label: 'ผู้ใช้', icon: 'users', admin: true },
		{ href: '/settings', label: 'ตั้งค่าเซิร์ฟเวอร์', icon: 'settings', admin: true }
	];

	const bare = $derived(
		page.url.pathname === '/login' ||
			page.url.pathname.startsWith('/connect/') ||
			page.url.pathname.startsWith('/terminal/') ||
			page.url.pathname.startsWith('/gateway/')
	);
	const isActive = (href: string) => (href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href));

	$effect(() => {
		if (!auth.loaded) loadMe();
	});
	$effect(() => {
		if (auth.loaded && !auth.user && page.url.pathname !== '/login') {
			goto(`/login?next=${encodeURIComponent(page.url.pathname)}`);
		}
	});

	async function signOut() {
		await logout();
		goto('/login');
	}
</script>

<svelte:head>
	<link rel="icon" href="/favicon.svg" />
	<title>Aegiron</title>
</svelte:head>

{#if !auth.loaded}
	<div class="grid min-h-screen place-items-center text-sm text-muted">กำลังโหลด…</div>
{:else if bare || !auth.user}
	{@render children()}
{:else}
	<div class="flex min-h-screen flex-col md:flex-row">
		<aside class="flex shrink-0 flex-col border-line bg-surface md:sticky md:top-0 md:h-screen md:w-60 md:border-r">
			<a href="/" class="flex items-center gap-2.5 px-5 py-4">
				<img src="/favicon.svg" alt="" class="size-7" />
				<span class="text-[15px] font-semibold tracking-tight">Aegiron</span>
			</a>
			<nav class="flex gap-1 overflow-x-auto px-3 pb-3 md:flex-1 md:flex-col md:overflow-visible">
				{#each nav.filter((n) => !n.admin || auth.user?.role === 'admin') as n (n.href)}
					<a
						href={n.href}
						class="flex shrink-0 items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors {isActive(n.href)
							? 'bg-accent-soft font-medium text-accent'
							: 'text-muted hover:bg-raised hover:text-fg'}"
					>
						<Icon name={n.icon} />
						{n.label}
					</a>
				{/each}
				<a href="/account" class="flex shrink-0 items-center gap-2.5 rounded-lg px-3 py-2 text-sm text-muted hover:bg-raised md:hidden">
					<Icon name="user" /> บัญชี
				</a>
			</nav>
			<div class="hidden border-t border-line p-3 md:block">
				<a href="/account" class="flex items-center gap-2.5 rounded-lg px-3 py-2 hover:bg-raised {isActive('/account') ? 'bg-raised' : ''}">
					<span class="grid size-8 place-items-center rounded-full bg-accent-soft text-xs font-semibold text-accent uppercase">
						{(auth.user.name || auth.user.email).slice(0, 2)}
					</span>
					<span class="min-w-0 flex-1">
						<span class="block truncate text-sm font-medium">{auth.user.name || auth.user.email}</span>
						<span class="block truncate text-xs text-muted">{auth.user.role === 'admin' ? 'ผู้ดูแลระบบ' : 'ผู้ใช้'}</span>
					</span>
				</a>
				<button class="btn btn-ghost mt-1 w-full justify-start text-muted" onclick={signOut}>
					<Icon name="logout" /> ออกจากระบบ
				</button>
			</div>
		</aside>
		<main class="min-w-0 flex-1 px-4 py-6 md:px-8 md:py-8">
			<div class="mx-auto max-w-6xl">
				{@render children()}
			</div>
		</main>
	</div>
{/if}
