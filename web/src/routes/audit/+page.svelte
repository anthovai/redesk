<script lang="ts">
	import { api, type AuditEntry } from '#lib/api.js';
	import { dateTime } from '#lib/format.js';

	const PAGE = 100;
	const filters = [
		['', 'ทั้งหมด'],
		['auth.', 'การเข้าสู่ระบบ'],
		['session.', 'เซสชัน'],
		['device.', 'อุปกรณ์'],
		['agent.', 'เหตุการณ์จาก agent'],
		['endpoint.', 'การเชื่อมต่อภายนอก'],
		['gateway.', 'Gateway'],
		['integration.', 'Integration'],
		['group.', 'กลุ่ม'],
		['user.', 'ผู้ใช้'],
		['settings.', 'ตั้งค่า'],
		['enroll_token.', 'Enrollment token']
	];

	let entries = $state<AuditEntry[]>([]);
	let action = $state('');
	let offset = $state(0);
	let error = $state('');

	$effect(() => {
		const q = new URLSearchParams({ action, limit: String(PAGE), offset: String(offset) });
		api
			.get<{ entries: AuditEntry[] }>(`/api/audit?${q}`)
			.then((r: { entries: AuditEntry[] }) => (entries = r.entries))
			.catch((e: Error) => (error = e.message));
	});

	const warn = (a: string) => /(_failed|_denied|\.denied|key_mismatch|\.locked)$/.test(a);
	const detail = (d: Record<string, unknown>) =>
		Object.entries(d)
			.map(([k, v]) => `${k}=${typeof v === 'object' ? JSON.stringify(v) : v}`)
			.join('  ');
</script>

<svelte:head><title>Audit log · ReDesk</title></svelte:head>

<div class="mb-6 flex flex-wrap items-end justify-between gap-4">
	<div>
		<h1 class="text-2xl font-semibold tracking-tight">Audit log</h1>
		<p class="mt-1 text-sm text-muted">บันทึกการเข้าสู่ระบบ เซสชัน และการเปลี่ยนแปลงทั้งหมด</p>
	</div>
	<div class="flex flex-wrap gap-1">
		{#each filters as [value, label] (value)}
			<button
				class="btn {action === value ? 'border-accent bg-accent-soft text-accent' : ''}"
				onclick={() => {
					action = value;
					offset = 0;
				}}>{label}</button
			>
		{/each}
	</div>
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<div class="card overflow-x-auto">
	<table class="table">
		<thead>
			<tr><th>เวลา</th><th>ผู้กระทำ</th><th>การกระทำ</th><th>เป้าหมาย</th><th>รายละเอียด</th></tr>
		</thead>
		<tbody>
			{#each entries as e (e.id)}
				<tr>
					<td class="whitespace-nowrap text-muted">{dateTime(e.at)}</td>
					<td>
						<p>{e.actor_email ?? 'ระบบ / agent'}</p>
						<p class="mono text-xs text-muted">{e.actor_ip}</p>
					</td>
					<td><span class="mono text-xs {warn(e.action) ? 'text-danger' : ''}">{e.action}</span></td>
					<td class="mono text-xs">{e.target}</td>
					<td class="mono max-w-md truncate text-xs text-muted" title={detail(e.detail)}>{detail(e.detail)}</td>
				</tr>
			{:else}
				<tr><td colspan="5" class="py-10 text-center text-muted">ไม่มีรายการ</td></tr>
			{/each}
		</tbody>
	</table>
</div>

<div class="mt-4 flex justify-end gap-2">
	<button class="btn" disabled={offset === 0} onclick={() => (offset = Math.max(0, offset - PAGE))}>ใหม่กว่า</button>
	<button class="btn" disabled={entries.length < PAGE} onclick={() => (offset += PAGE)}>เก่ากว่า</button>
</div>
