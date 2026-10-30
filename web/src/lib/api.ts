// Typed client for the Aegiron server REST API.

export interface User {
	id: number;
	email: string;
	name: string;
	role: 'admin' | 'user';
	totp_enabled: boolean;
	disabled: boolean;
	created_at: string;
	last_login_at: string | null;
}

/** A browser or app remembered on the 2FA step (skips the TOTP code, never the password). */
export interface TrustedDevice {
	id: number;
	label: string;
	user_agent: string;
	ip: string;
	last_ip: string;
	created_at: string;
	last_used_at: string | null;
	expires_at: string;
	/** This browser. */
	current: boolean;
}

export interface Device {
	id: string;
	public_key: string | null; // base64
	hostname: string;
	os: string;
	agent_version: string;
	last_ip: string;
	last_seen_at: string | null;
	alias: string;
	note: string;
	disabled: boolean;
	created_at: string;
	group_id: number | null;
	online: boolean;
	/** Offline, MAC known and an online agent on its LAN can send Wake-on-LAN. */
	can_wake: boolean;
	/** Admin only. */
	macs?: string[];
	/** Admin only: IPv4 addresses reported by the agent. */
	lan?: { ip: string; prefix: number }[];
}

export interface WakeResult {
	ok: boolean;
	/** Admin only: device id of the agent that sent the packet. */
	helper?: string;
}

export interface Group {
	id: number;
	name: string;
	description: string;
	created_at: string;
	member_ids: number[];
	device_count: number;
}

export interface AddressEntry {
	device_id: string;
	alias: string;
	tags: string[];
	favorite: boolean;
	created_at: string;
	device: Device | null;
}

export interface AuditEntry {
	id: number;
	at: string;
	actor_id: number | null;
	actor_email: string | null;
	actor_ip: string;
	action: string;
	target: string;
	detail: Record<string, unknown>;
}

export interface RemoteSession {
	id: string;
	device_id: string;
	user_id: number | null;
	user_email: string | null;
	viewer_ip: string;
	created_at: string;
	started_at: string | null;
	ended_at: string | null;
	bytes_to_viewer: number;
	bytes_to_agent: number;
	end_reason: string;
	endpoint_id: number | null;
	endpoint_name: string | null;
	provider: string | null;
	/** "screen", "tunnel" or "terminal" (absent on older servers). */
	type?: string;
	/** Tunnel target host:port as seen from the device. */
	target?: string;
}

export interface EnrollToken {
	id: number;
	note: string;
	created_at: string;
	expires_at: string | null;
	max_uses: number;
	uses: number;
	group_id: number | null;
}

export interface Settings {
	require_enroll: boolean;
	session_hours: number;
	access_policy: 'open' | 'groups';
}

export interface ServerInfo {
	name: string;
	version: string;
	public_key: string;
	require_enroll: boolean;
	gateway: boolean;
	oidc?: { enabled: boolean; name: string };
}

export interface SessionTicket {
	/** `type`: the session type the server started (absent on servers before tunnels and terminals). */
	ticket: { sid: string; token: string; relay_path: string; type?: string; target?: string };
	device: { id: string; public_key: string; hostname: string; os: string; alias: string };
}

export class ApiError extends Error {
	constructor(
		public status: number,
		message: string,
		public code?: string
	) {
		super(message);
	}
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
	const res = await fetch(path, {
		method,
		credentials: 'same-origin',
		headers: {
			'X-Aegiron': '1',
			...(body !== undefined ? { 'Content-Type': 'application/json' } : {})
		},
		body: body !== undefined ? JSON.stringify(body) : undefined
	});
	const data = await res.json().catch(() => ({}));
	if (!res.ok) throw new ApiError(res.status, data.error ?? res.statusText, data.code);
	return data as T;
}

/** Multipart upload (e.g. agent releases); the browser sets the boundary header. */
async function upload<T>(path: string, form: FormData): Promise<T> {
	const res = await fetch(path, { method: 'POST', credentials: 'same-origin', headers: { 'X-Aegiron': '1' }, body: form });
	const data = await res.json().catch(() => ({}));
	if (!res.ok) throw new ApiError(res.status, data.error ?? res.statusText);
	return data as T;
}

export const api = {
	upload,
	get: <T>(p: string) => request<T>('GET', p),
	post: <T>(p: string, b: unknown = {}) => request<T>('POST', p, b),
	put: <T>(p: string, b: unknown) => request<T>('PUT', p, b),
	patch: <T>(p: string, b: unknown) => request<T>('PATCH', p, b),
	del: <T>(p: string) => request<T>('DELETE', p)
};

export type Provider = 'anydesk' | 'teamviewer' | 'rustdesk' | 'rdp' | 'vnc' | 'ssh';

export interface Endpoint {
	id: number;
	name: string;
	provider: Provider;
	address: string;
	port: number | null;
	username: string;
	has_secret: boolean;
	options: { security?: string; ignore_cert?: boolean; domain?: string; auth?: 'password' | 'key' };
	group_id: number | null;
	tags: string[];
	note: string;
	source: 'manual' | 'import' | 'teamviewer_api';
	external_id: string | null;
	online: boolean | null;
	synced_at: string | null;
	created_by: number | null;
	created_at: string;
	updated_at: string;
}

export interface LaunchInfo {
	provider: Provider;
	url?: string;
	file?: { name: string; content: string };
	secret?: string;
}

export interface ExternalSession {
	provider: string;
	external_id: string;
	user_name: string;
	target_name: string;
	target_id: string;
	started_at: string | null;
	ended_at: string | null;
}

export interface Integration {
	provider: string;
	enabled: boolean;
	group_id: number | null;
	last_sync_at: string | null;
	last_error: string;
}

/** Sends Wake-on-LAN via an online agent on the device's LAN; returns a Thai message for a toast. */
export async function wakeDevice(id: string): Promise<{ ok: boolean; message: string }> {
	try {
		await api.post<WakeResult>(`/api/devices/${id}/wake`);
		return { ok: true, message: 'ส่งคำสั่งปลุกเครื่องแล้ว เครื่องอาจใช้เวลาสักครู่ก่อนออนไลน์' };
	} catch (e) {
		const err = e as ApiError;
		const known: Record<string, string> = {
			online_already: 'เครื่องนี้ออนไลน์อยู่แล้ว',
			no_mac: 'ยังไม่ทราบ MAC address ของเครื่องนี้',
			no_helper: 'ไม่มีเครื่องที่ออนไลน์ในเครือข่ายเดียวกันเพื่อช่วยปลุก',
			helper_busy: 'เครื่องที่ช่วยปลุกไม่ว่าง กรุณาลองใหม่',
			rate_limited: 'ส่งคำสั่งปลุกบ่อยเกินไป กรุณารอสักครู่'
		};
		return { ok: false, message: (err.code && known[err.code]) || `ปลุกเครื่องไม่สำเร็จ: ${err.message}` };
	}
}

export interface OIDCSettings {
	name: string;
	issuer: string;
	client_id: string;
	has_secret: boolean;
	allowed_domains: string[];
	auto_create: boolean;
	enabled: boolean;
}

export interface AgentRelease {
	id: number;
	version: string;
	os: 'windows' | 'macos' | 'linux';
	arch: 'x86_64' | 'aarch64' | 'universal';
	filename: string;
	size: number;
	sha256: string;
	notes: string;
	enabled: boolean;
	created_by: number | null;
	created_at: string;
}

/** Recording of an Aegiron remote session, made and uploaded by the controlled device. */
export interface Recording {
	id: number;
	sid: string;
	device_id: string;
	device_name: string;
	user_id: number | null;
	user_email: string | null;
	viewer_ip: string;
	/** pending = the session was recorded but the device has not uploaded it yet. */
	status: 'pending' | 'ready';
	created_at: string;
	uploaded_at: string | null;
	duration_ms: number;
	size_bytes: number;
	sha256: string;
	codec: string;
	frames: number;
	/** "opus" when the recording has sound, "" for silent sessions and older servers. */
	audio?: string;
}

export interface RecordingSettings {
	mode: 'off' | 'all' | 'groups';
	group_ids: number[];
	/** 0 = keep until deleted. */
	retention_days: number;
}
