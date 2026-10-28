<script lang="ts">
	import { api, type ExternalSession, type Recording, type RemoteSession } from '#lib/api.js';
	import Icon from '#lib/Icon.svelte';
	import { bytes, dateTime, duration, formatId } from '#lib/format.js';

	let sessions = $state<RemoteSession[]>([]);
	let active = $state<{ sid: string; state: string }[]>([]);
	let onlineDevices = $state(0);
	let error = $state('');
	let tab = $state<'aegiron' | 'external'>('aegiron');
	let external = $state<ExternalSession[]>([]);
	/** Recordings by session id. */
	let recorded = $state<Record<string, Recording>>({});

	$effect(() => {
		if (tab === 'external') api.get<{ sessions: ExternalSession[] }>('/api/external-sessions').then((r) => (external = r.sessions));
	});

	async function load() {
		try {
			const [s, a, r] = await Promise.all([
				api.get<{ sessions: RemoteSession[] }>('/api/sessions'),
				api.get<{ sessions: { sid: string; state: string }[]; online_devices: number }>('/api/sessions/active'),
				api.get<{ recordings: Recording[] }>('/api/recordings').catch(() => ({ recordings: [] as Recording[] }))
			]);
			recorded = Object.fromEntries(r.recordings.map((x) => [x.sid, x]));
			sessions = s.sessions;
			active = a.sessions;
			onlineDevices = a.online_devices;
		} catch (e) {
			error = (e as Error).message;
		}
	}

	$effect(() => {
		load();
		const t = setInterval(load, 5000);
		return () => clearInterval(t);
	});

	const liveCount = $derived(active.filter((a) => a.state === 'active').length);
	const today = $derived(sessions.filter((s) => new Date(s.created_at).toDateString() === new Date().toDateString()).length);

	function status(s: RemoteSession): [string, string] {
		if (!s.ended_at) return s.started_at ? ['กำลังใช้งาน', 'text-ok'] : ['รอจับคู่', 'text-muted'];
		if (!s.started_at) return ['ไม่สำเร็จ', 'text-danger'];
		return ['จบแล้ว', 'text-muted'];
	}
</script>

<svelte:head><title>เซสชัน · Aegiron</title></svelte:head>

<h1 class="mb-6 text-2xl font-semibold tracking-tight">เซสชันรีโมต</h1>

<div class="mb-6 grid gap-3 sm:grid-cols-3">
	{#each [['กำลังควบคุมอยู่', liveCount], ['อุปกรณ์ออนไลน์', onlineDevices], ['เซสชันวันนี้', today]] as [label, value] (label)}
		<div class="card p-4">
			<p class="text-xs text-muted">{label}</p>
			<p class="mono mt-1 text-2xl font-semibold">{value}</p>
		</div>
	{/each}
</div>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<div class="mb-3 flex gap-1">
	<button class="btn {tab === 'aegiron' ? 'border-accent bg-accent-soft text-accent' : ''}" onclick={() => (tab = 'aegiron')}>Aegiron และ gateway</button>
	<button class="btn {tab === 'external' ? 'border-accent bg-accent-soft text-accent' : ''}" onclick={() => (tab = 'external')}>จาก TeamViewer API</button>
</div>

{#if tab === 'external'}
	<div class="card overflow-x-auto">
		<table class="table">
			<thead><tr><th>เริ่ม</th><th>ผู้ใช้</th><th>เครื่องปลายทาง</th><th>ระยะเวลา</th><th>ผู้ให้บริการ</th></tr></thead>
			<tbody>
				{#each external as x (x.provider + x.external_id)}
					<tr>
						<td class="whitespace-nowrap">{dateTime(x.started_at)}</td>
						<td>{x.user_name || '—'}</td>
						<td>{x.target_name || '—'} <span class="mono text-xs text-muted">{x.target_id}</span></td>
						<td class="whitespace-nowrap">{duration(x.started_at, x.ended_at)}</td>
						<td><span class="badge">{x.provider}</span></td>
					</tr>
				{:else}
					<tr><td colspan="5" class="py-10 text-center text-muted">ยังไม่มีข้อมูล — ตั้งค่า TeamViewer API ที่หน้าตั้งค่าเซิร์ฟเวอร์</td></tr>
				{/each}
			</tbody>
		</table>
	</div>
{:else}
<div class="card overflow-x-auto">
	<table class="table">
		<thead>
			<tr>
				<th>เริ่ม</th>
				<th>ผู้ใช้</th>
				<th>อุปกรณ์</th>
				<th>สถานะ</th>
				<th>ระยะเวลา</th>
				<th class="text-right">ข้อมูล (ลง / ขึ้น)</th>
				<th>เหตุผลที่จบ</th>
				<th>บันทึก</th>
			</tr>
		</thead>
		<tbody>
			{#each sessions as s (s.id)}
				{@const [label, cls] = status(s)}
				<tr>
					<td class="whitespace-nowrap">{dateTime(s.created_at)}</td>
					<td>
						<p>{s.user_email ?? '—'}</p>
						<p class="mono text-xs text-muted">{s.viewer_ip}</p>
					</td>
					<td>
						{#if s.endpoint_id}
							<span class="badge">{s.provider?.toUpperCase()}</span> {s.endpoint_name ?? `#${s.endpoint_id}`}
						{:else}
							<span class="mono">{formatId(s.device_id)}</span>
						{/if}
					</td>
					<td class="{cls} whitespace-nowrap">{label}</td>
					<td class="whitespace-nowrap">{duration(s.started_at, s.ended_at)}</td>
					<td class="mono text-right whitespace-nowrap text-muted">{bytes(s.bytes_to_viewer)} / {bytes(s.bytes_to_agent)}</td>
					<td class="text-xs text-muted">{s.end_reason}</td>
					<td class="whitespace-nowrap">
						{#if recorded[s.id]?.status === 'ready'}
							<a class="inline-flex items-center gap-1 text-sm text-accent" href="/recordings/{recorded[s.id].id}"><Icon name="play" size={13} /> เล่น</a>
						{:else if recorded[s.id]}
							<span class="text-xs text-muted" title="เครื่องปลายทางยังไม่ได้อัปโหลด">รออัปโหลด</span>
						{/if}
					</td>
				</tr>
			{:else}
				<tr><td colspan="8" class="py-10 text-center text-muted">ยังไม่มีเซสชัน</td></tr>
			{/each}
		</tbody>
	</table>
</div>
{/if}
