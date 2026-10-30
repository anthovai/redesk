<script lang="ts">
	// Settings cards for LDAP / Active Directory sign-in and outgoing mail (SMTP).
	import { api, type LDAPSettings, type SMTPSettings } from '#lib/api.js';

	const lines = (s: string) => s.split(/[\n,]+/).map((x) => x.trim()).filter(Boolean);

	let ldap = $state<LDAPSettings | null>(null);
	let defaultFilter = $state('');
	let ldapForm = $state({ url: '', ca_cert: '', bind_dn: '', bind_password: '', base_dn: '', user_filter: '', email_attr: 'mail', name_attr: 'displayName', group_filter: '', admin_groups: '', user_groups: '', auto_create: true, enabled: false });
	let ldapMsg = $state<[string, boolean] | null>(null);
	let ldapTestUser = $state('');
	let ldapBusy = $state(false);

	let smtp = $state<SMTPSettings | null>(null);
	let smtpForm = $state({ host: '', port: 587, security: 'starttls' as SMTPSettings['security'], ca_cert: '', username: '', password: '', from: '', alert_recipients: '', alert_new_device: false, alert_lockout: false, enabled: false });
	let smtpMsg = $state<[string, boolean] | null>(null);
	let smtpTo = $state('');
	let smtpBusy = $state(false);

	async function load() {
		try {
			const l = await api.get<{ settings: LDAPSettings; default_user_filter: string }>('/api/ldap/settings');
			ldap = l.settings;
			defaultFilter = l.default_user_filter;
			const ls = l.settings;
			ldapForm = {
				url: ls.url,
				ca_cert: ls.ca_cert,
				bind_dn: ls.bind_dn,
				bind_password: '',
				base_dn: ls.base_dn,
				user_filter: ls.user_filter,
				email_attr: ls.email_attr,
				name_attr: ls.name_attr,
				group_filter: ls.group_filter,
				auto_create: ls.auto_create,
				enabled: ls.enabled,
				admin_groups: (l.settings.admin_groups ?? []).join('\n'),
				user_groups: (l.settings.user_groups ?? []).join('\n')
			};
			const m = await api.get<{ settings: SMTPSettings }>('/api/smtp/settings');
			smtp = m.settings;
			const ms = m.settings;
			smtpForm = {
				host: ms.host,
				port: ms.port,
				security: ms.security,
				ca_cert: ms.ca_cert,
				username: ms.username,
				password: '',
				from: ms.from,
				alert_recipients: (ms.alert_recipients ?? []).join(', '),
				alert_new_device: ms.alert_new_device,
				alert_lockout: ms.alert_lockout,
				enabled: ms.enabled
			};
		} catch (e) {
			ldapMsg = [(e as Error).message, false];
		}
	}
	$effect(() => {
		load();
	});

	async function saveLdap(ev: SubmitEvent) {
		ev.preventDefault();
		ldapMsg = null;
		try {
			await api.put('/api/ldap/settings', {
				...ldapForm,
				bind_password: ldapForm.bind_password ? ldapForm.bind_password : null,
				admin_groups: lines(ldapForm.admin_groups),
				user_groups: lines(ldapForm.user_groups)
			});
			ldapMsg = ['บันทึกแล้ว', true];
			await load();
		} catch (e) {
			ldapMsg = [(e as Error).message, false];
		}
	}

	async function testLdap() {
		ldapBusy = true;
		ldapMsg = null;
		try {
			const r = await api.post<{ ok: boolean; error?: string; user?: { dn: string; email: string; name: string; groups: string[] } | null; admin?: boolean; allowed?: boolean }>(
				'/api/ldap/test',
				{ username: ldapTestUser }
			);
			if (!r.ok) ldapMsg = [`เชื่อมต่อไม่สำเร็จ: ${r.error}`, false];
			else if (r.user) ldapMsg = [`พบ ${r.user.dn} · อีเมล ${r.user.email || '(ไม่มี)'} · ${r.admin ? 'ผู้ดูแลระบบ' : 'ผู้ใช้'}${r.allowed ? '' : ' · ไม่อยู่ในกลุ่มที่อนุญาต'} · กลุ่ม ${r.user.groups.length}`, true];
			else if (ldapTestUser) ldapMsg = ['เชื่อมต่อได้ แต่ไม่พบผู้ใช้นี้ (หรือพบมากกว่าหนึ่งรายการ)', false];
			else ldapMsg = ['เชื่อมต่อและ bind ด้วยบัญชีบริการสำเร็จ (บันทึกการตั้งค่าก่อนทดสอบ)', true];
		} catch (e) {
			ldapMsg = [(e as Error).message, false];
		} finally {
			ldapBusy = false;
		}
	}

	async function saveSmtp(ev: SubmitEvent) {
		ev.preventDefault();
		smtpMsg = null;
		try {
			await api.put('/api/smtp/settings', {
				...smtpForm,
				port: +smtpForm.port,
				password: smtpForm.password ? smtpForm.password : null,
				alert_recipients: lines(smtpForm.alert_recipients)
			});
			smtpMsg = ['บันทึกแล้ว', true];
			await load();
		} catch (e) {
			smtpMsg = [(e as Error).message, false];
		}
	}

	async function testSmtp() {
		smtpBusy = true;
		smtpMsg = null;
		try {
			const r = await api.post<{ ok: boolean; error?: string }>('/api/smtp/test', { to: smtpTo });
			smtpMsg = r.ok ? ['ส่งอีเมลทดสอบแล้ว', true] : [`ส่งไม่สำเร็จ: ${r.error}`, false];
		} catch (e) {
			smtpMsg = [(e as Error).message, false];
		} finally {
			smtpBusy = false;
		}
	}
</script>

<section class="card space-y-4 p-6 lg:col-span-2">
	<div>
		<h2 class="font-semibold">เข้าสู่ระบบด้วย LDAP / Active Directory</h2>
		<p class="mt-1 text-sm text-muted">
			ผู้ใช้เข้าสู่ระบบคอนโซลและโปรแกรม Aegiron ด้วยชื่อผู้ใช้หรืออีเมลและรหัสผ่านของไดเรกทอรี เชื่อมต่อผ่าน TLS เสมอ (<code>ldaps://</code> หรือ
			<code>ldap://</code> + StartTLS) และตรวจใบรับรอง บัญชีภายในที่อีเมลซ้ำกันจะไม่ถูกใช้แทน
		</p>
	</div>
	<form class="grid gap-3 sm:grid-cols-2" onsubmit={saveLdap}>
		<div>
			<label class="label" for="ldap-url">URL เซิร์ฟเวอร์</label>
			<input id="ldap-url" class="input mono" placeholder="ldaps://dc01.corp.local" bind:value={ldapForm.url} />
		</div>
		<div>
			<label class="label" for="ldap-base">Base DN</label>
			<input id="ldap-base" class="input mono" placeholder="dc=corp,dc=local" bind:value={ldapForm.base_dn} />
		</div>
		<div>
			<label class="label" for="ldap-bind">Bind DN (บัญชีบริการ)</label>
			<input id="ldap-bind" class="input mono" placeholder="cn=aegiron,ou=service,dc=corp,dc=local" autocomplete="off" bind:value={ldapForm.bind_dn} />
		</div>
		<div>
			<label class="label" for="ldap-pw">รหัสผ่านบัญชีบริการ</label>
			<input
				id="ldap-pw"
				class="input mono"
				type="password"
				autocomplete="off"
				placeholder={ldap?.has_bind_password ? 'เก็บไว้แล้ว (เข้ารหัส) — เว้นว่างถ้าไม่เปลี่ยน' : ''}
				bind:value={ldapForm.bind_password}
			/>
		</div>
		<div class="sm:col-span-2">
			<label class="label" for="ldap-filter">ตัวกรองผู้ใช้ ({'{username}'} = สิ่งที่ผู้ใช้พิมพ์)</label>
			<input id="ldap-filter" class="input mono" placeholder={defaultFilter} bind:value={ldapForm.user_filter} />
		</div>
		<div>
			<label class="label" for="ldap-mail">Attribute อีเมล</label>
			<input id="ldap-mail" class="input mono" bind:value={ldapForm.email_attr} />
		</div>
		<div>
			<label class="label" for="ldap-name">Attribute ชื่อ</label>
			<input id="ldap-name" class="input mono" bind:value={ldapForm.name_attr} />
		</div>
		<div>
			<label class="label" for="ldap-admins">กลุ่มผู้ดูแลระบบ (บรรทัดละกลุ่ม, ชื่อหรือ DN)</label>
			<textarea id="ldap-admins" class="input mono" rows="2" bind:value={ldapForm.admin_groups}></textarea>
			<p class="mt-1 text-xs text-muted">กำหนดแล้วบทบาทจะตามไดเรกทอรีทุกครั้งที่เข้าสู่ระบบ</p>
		</div>
		<div>
			<label class="label" for="ldap-users">กลุ่มที่อนุญาต (ไม่บังคับ)</label>
			<textarea id="ldap-users" class="input mono" rows="2" bind:value={ldapForm.user_groups}></textarea>
			<p class="mt-1 text-xs text-muted">เว้นว่าง = ทุกคนที่ค้นเจอด้วยตัวกรอง</p>
		</div>
		<div class="sm:col-span-2">
			<label class="label" for="ldap-groupf">ค้นหากลุ่มเพิ่ม (ไม่บังคับ, {'{dn}'} = DN ของผู้ใช้)</label>
			<input id="ldap-groupf" class="input mono" placeholder="(&(objectClass=group)(member={'{dn}'}))" bind:value={ldapForm.group_filter} />
			<p class="mt-1 text-xs text-muted">ใช้เมื่อเซิร์ฟเวอร์ไม่มี memberOf</p>
		</div>
		<div class="sm:col-span-2">
			<label class="label" for="ldap-ca">CA เพิ่มเติม (PEM, ไม่บังคับ)</label>
			<textarea id="ldap-ca" class="input mono text-xs" rows="3" placeholder="-----BEGIN CERTIFICATE-----" bind:value={ldapForm.ca_cert}></textarea>
		</div>
		<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={ldapForm.auto_create} /> สร้างบัญชีให้อัตโนมัติเมื่อเข้าสู่ระบบครั้งแรก</label>
		<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={ldapForm.enabled} /> เปิดใช้งาน</label>
		<div class="flex flex-wrap items-center gap-2 sm:col-span-2">
			<button class="btn btn-primary">บันทึก</button>
			<input class="input w-48" placeholder="ชื่อผู้ใช้สำหรับทดสอบ" bind:value={ldapTestUser} aria-label="ชื่อผู้ใช้สำหรับทดสอบ" />
			<button type="button" class="btn" disabled={ldapBusy} onclick={testLdap}>{ldapBusy ? 'กำลังทดสอบ…' : 'ทดสอบการเชื่อมต่อ'}</button>
		</div>
	</form>
	{#if ldapMsg}<p class="text-sm {ldapMsg[1] ? 'text-ok' : 'text-danger'}">{ldapMsg[0]}</p>{/if}
</section>

<section class="card space-y-4 p-6 lg:col-span-2">
	<div>
		<h2 class="font-semibold">อีเมล (SMTP)</h2>
		<p class="mt-1 text-sm text-muted">
			ใช้ส่งรหัสยืนยันการเข้าสู่ระบบทางอีเมล (ผู้ใช้เปิดเองที่หน้าบัญชี) และแจ้งเตือนผู้ดูแลระบบ ค่าเริ่มต้นบังคับ STARTTLS และตรวจใบรับรอง
		</p>
	</div>
	<form class="grid gap-3 sm:grid-cols-2" onsubmit={saveSmtp}>
		<div>
			<label class="label" for="smtp-host">เซิร์ฟเวอร์</label>
			<input id="smtp-host" class="input mono" placeholder="smtp.office365.com" bind:value={smtpForm.host} />
		</div>
		<div class="grid grid-cols-2 gap-3">
			<div>
				<label class="label" for="smtp-port">พอร์ต</label>
				<input id="smtp-port" class="input mono" type="number" min="1" max="65535" bind:value={smtpForm.port} />
			</div>
			<div>
				<label class="label" for="smtp-sec">การเข้ารหัส</label>
				<select id="smtp-sec" class="input" bind:value={smtpForm.security}>
					<option value="starttls">STARTTLS (587)</option>
					<option value="tls">TLS (465)</option>
					<option value="none">ไม่เข้ารหัส (localhost เท่านั้น)</option>
				</select>
			</div>
		</div>
		<div>
			<label class="label" for="smtp-user">ชื่อผู้ใช้</label>
			<input id="smtp-user" class="input mono" autocomplete="off" bind:value={smtpForm.username} />
		</div>
		<div>
			<label class="label" for="smtp-pw">รหัสผ่าน</label>
			<input
				id="smtp-pw"
				class="input mono"
				type="password"
				autocomplete="off"
				placeholder={smtp?.has_password ? 'เก็บไว้แล้ว (เข้ารหัส) — เว้นว่างถ้าไม่เปลี่ยน' : ''}
				bind:value={smtpForm.password}
			/>
		</div>
		<div>
			<label class="label" for="smtp-from">ผู้ส่ง</label>
			<input id="smtp-from" class="input mono" placeholder="aegiron@example.co.th" bind:value={smtpForm.from} />
		</div>
		<div>
			<label class="label" for="smtp-to">ผู้รับแจ้งเตือน (คั่นด้วย ,)</label>
			<input id="smtp-to" class="input mono" placeholder="เว้นว่าง = ผู้ดูแลระบบทุกคน" bind:value={smtpForm.alert_recipients} />
		</div>
		<div class="sm:col-span-2">
			<label class="label" for="smtp-ca">CA เพิ่มเติม (PEM, ไม่บังคับ)</label>
			<textarea id="smtp-ca" class="input mono text-xs" rows="2" placeholder="-----BEGIN CERTIFICATE-----" bind:value={smtpForm.ca_cert}></textarea>
		</div>
		<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={smtpForm.alert_new_device} /> แจ้งเมื่อมีเครื่องใหม่ลงทะเบียน</label>
		<label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={smtpForm.alert_lockout} /> แจ้งเมื่อบัญชีถูกล็อกเพราะใส่รหัสผิดหลายครั้ง</label>
		<label class="flex items-center gap-2 text-sm sm:col-span-2"><input type="checkbox" bind:checked={smtpForm.enabled} /> เปิดใช้งาน</label>
		<div class="flex flex-wrap items-center gap-2 sm:col-span-2">
			<button class="btn btn-primary">บันทึก</button>
			<input class="input w-56" placeholder="ส่งอีเมลทดสอบถึง (เว้นว่าง = ตัวเอง)" bind:value={smtpTo} aria-label="อีเมลทดสอบ" />
			<button type="button" class="btn" disabled={smtpBusy} onclick={testSmtp}>{smtpBusy ? 'กำลังส่ง…' : 'ส่งอีเมลทดสอบ'}</button>
		</div>
	</form>
	{#if smtpMsg}<p class="text-sm {smtpMsg[1] ? 'text-ok' : 'text-danger'}">{smtpMsg[0]}</p>{/if}
</section>
