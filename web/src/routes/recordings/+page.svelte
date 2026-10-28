<script lang="ts">
	import { api, type Group, type Recording, type RecordingSettings } from '#lib/api.js';
	import { bytes, dateTime, formatId } from '#lib/format.js';
	import Icon from '#lib/Icon.svelte';
	import { clock } from '#lib/recording.svelte.js';

	let recordings = $state<Recording[]>([]);
	let groups = $state<Group[]>([]);
	let settings = $state<RecordingSettings>({ mode: 'off', group_ids: [], retention_days: 90 });
	let error = $state('');
	let saved = $state('');
	let saving = $state(false);

	async function load() {
		try {
			const [r, s, g] = await Promise.all([
				api.get<{ recordings: Recording[] }>('/api/recordings'),
				api.get<{ settings: RecordingSettings }>('/api/recordings/settings'),
				api.get<{ groups: Group[] }>('/api/groups')
			]);
			recordings = r.recordings;
			settings = s.settings;
			groups = g.groups;
		} catch (e) {
			error = (e as Error).message;
		}
	}

	$effect(() => {
		load();
	});

	async function save(ev: SubmitEvent) {
		ev.preventDefault();
		saving = true;
		error = '';
		try {
			settings = (await api.put<{ settings: RecordingSettings }>('/api/recordings/settings', settings)).settings;
			saved = 'บันทึกนโยบายแล้ว';
			setTimeout(() => (saved = ''), 2500);
		} catch (e) {
			error = (e as Error).message;
		} finally {
			saving = false;
		}
	}

	function toggleGroup(id: number) {
		settings.group_ids = settings.group_ids.includes(id) ? settings.group_ids.filter((g) => g !== id) : [...settings.group_ids, id];
	}

	async function remove(r: Recording) {
		if (!confirm(`ลบบันทึกเซสชันของ ${r.device_name || formatId(r.device_id)} (${dateTime(r.created_at)}) ถาวร?`)) return;
		try {
			await api.del(`/api/recordings/${r.id}`);
			recordings = recordings.filter((x) => x.id !== r.id);
		} catch (e) {
			error = (e as Error).message;
		}
	}
</script>

<svelte:head><title>บันทึกเซสชัน · Aegiron</title></svelte:head>

<h1 class="mb-2 text-2xl font-semibold tracking-tight">บันทึกเซสชัน</h1>
<p class="mb-6 max-w-3xl text-sm text-muted">
	เซสชันเข้ารหัสต้นทาง-ปลายทาง เซิร์ฟเวอร์จึงบันทึกเองไม่ได้ เครื่องที่ถูกควบคุมเป็นผู้บันทึกภาพที่ส่งออกไป แล้วอัปโหลดมาเก็บที่เซิร์ฟเวอร์หลังจบเซสชัน
	ทั้งผู้ใช้ที่เครื่องนั้นและผู้ควบคุมจะเห็นป้าย “กำลังบันทึก” ตลอดเซสชัน · ดูและลบได้เฉพาะผู้ดูแลระบบ ทุกครั้งที่เปิดดูจะถูกบันทึกใน Audit log
</p>

{#if error}<p class="mb-4 rounded-lg bg-danger-soft px-3 py-2 text-sm text-danger">{error}</p>{/if}

<form class="card mb-6 space-y-4 p-6" onsubmit={save}>
	<h2 class="font-semibold">นโยบายการบันทึก</h2>
	<div class="grid gap-2 sm:grid-cols-3">
		{#each [['off', 'ไม่บันทึก', 'เซสชันทั้งหมดไม่ถูกบันทึก'], ['all', 'ทุกเครื่อง', 'บันทึกทุกเซสชันขององค์กร'], ['groups', 'เฉพาะกลุ่มอุปกรณ์', 'บันทึกเมื่อเชื่อมต่อเครื่องในกลุ่มที่เลือก']] as [value, label, hint] (value)}
			<label class="flex cursor-pointer items-start gap-2.5 rounded-lg border p-3 {settings.mode === value ? 'border-accent bg-accent-soft' : 'border-line'}">
				<input type="radio" name="mode" class="mt-1" {value} bind:group={settings.mode} />
				<span><span class="block text-sm font-medium">{label}</span><span class="block text-xs text-muted">{hint}</span></span>
			</label>
		{/each}
	</div>
	{#if settings.mode === 'groups'}
		<div>
			<p class="mb-2 text-sm font-medium">กลุ่มอุปกรณ์ที่บันทึก</p>
			{#if groups.length === 0}
				<p class="text-sm text-muted">ยังไม่มีกลุ่มอุปกรณ์ — สร้างได้ที่หน้า <a class="text-accent" href="/groups">กลุ่มอุปกรณ์</a></p>
			{/if}
			<div class="flex flex-wrap gap-2">
				{#each groups as g (g.id)}
					<label class="flex cursor-pointer items-center gap-2 rounded-full border px-3 py-1 text-sm {settings.group_ids.includes(g.id) ? 'border-accent bg-accent-soft text-accent' : 'border-line'}">
						<input type="checkbox" checked={settings.group_ids.includes(g.id)} onchange={() => toggleGroup(g.id)} />
						{g.name} <span class="text-xs text-muted">({g.device_count})</span>
					</label>
				{/each}
			</div>
		</div>
	{/if}
	<div class="flex flex-wrap items-end gap-4">
		<label class="block">
			<span class="mb-1 block text-sm font-medium">เก็บไว้ (วัน)</span>
			<input class="input w-32" type="number" min="0" max="3650" bind:value={settings.retention_days} />
		</label>
		<p class="pb-2 text-xs text-muted">0 = เก็บจนกว่าจะลบเอง · บันทึกที่เก่ากว่านี้จะถูกลบอัตโนมัติทุกชั่วโมง</p>
		<div class="ml-auto flex items-center gap-3">
			{#if saved}<span class="text-sm text-ok">{saved}</span>{/if}
			<button class="btn btn-primary" disabled={saving}>{saving ? 'กำลังบันทึก…' : 'บันทึกนโยบาย'}</button>
		</div>
	</div>
	<p class="text-xs text-muted">Aegiron agent รุ่นเก่าที่ยังไม่รองรับการบันทึกจะเชื่อมต่อได้โดยไม่ถูกบันทึก (ดูได้จากรายการด้านล่าง: เซสชันนั้นจะไม่มีบันทึก)</p>
</form>

<div class="card overflow-x-auto">
	<table class="table">
		<thead>
			<tr>
				<th>เริ่ม</th>
				<th>ผู้ควบคุม</th>
				<th>อุปกรณ์</th>
				<th>ความยาว</th>
				<th class="text-right">ขนาด</th>
				<th>Codec</th>
				<th></th>
			</tr>
		</thead>
		<tbody>
			{#each recordings as r (r.id)}
				<tr>
					<td class="whitespace-nowrap">{dateTime(r.created_at)}</td>
					<td>
						<p>{r.user_email ?? '—'}</p>
						<p class="mono text-xs text-muted">{r.viewer_ip}</p>
					</td>
					<td>
						<p>{r.device_name || '—'}</p>
						<p class="mono text-xs text-muted">{formatId(r.device_id)}</p>
					</td>
					{#if r.status === 'ready'}
						<td class="mono whitespace-nowrap">{clock(r.duration_ms)}</td>
						<td class="mono text-right whitespace-nowrap text-muted">{bytes(r.size_bytes)}</td>
						<td><span class="badge">{r.codec.toUpperCase() || '—'}</span></td>
						<td class="text-right whitespace-nowrap">
							<a class="btn" href="/recordings/{r.id}"><Icon name="play" size={14} /> เล่น</a>
							<button class="btn btn-ghost text-danger" title="ลบ" onclick={() => remove(r)}><Icon name="trash" size={15} /></button>
						</td>
					{:else}
						<td colspan="3" class="text-sm text-muted">รอเครื่องอัปโหลด (จะลองใหม่ทุกครั้งที่เครื่องออนไลน์)</td>
						<td class="text-right">
							<button class="btn btn-ghost text-danger" title="ลบ" onclick={() => remove(r)}><Icon name="trash" size={15} /></button>
						</td>
					{/if}
				</tr>
			{:else}
				<tr><td colspan="7" class="py-10 text-center text-muted">ยังไม่มีบันทึกเซสชัน</td></tr>
			{/each}
		</tbody>
	</table>
</div>
