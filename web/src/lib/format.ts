const dtf = new Intl.DateTimeFormat('th-TH', { dateStyle: 'medium', timeStyle: 'short' });
const rtf = new Intl.RelativeTimeFormat('th', { numeric: 'auto' });

export function dateTime(s: string | null | undefined): string {
	return s ? dtf.format(new Date(s)) : '—';
}

export function ago(s: string | null | undefined): string {
	if (!s) return 'ไม่เคย';
	const sec = (new Date(s).getTime() - Date.now()) / 1000;
	const steps: [number, Intl.RelativeTimeFormatUnit][] = [
		[60, 'second'],
		[60, 'minute'],
		[24, 'hour'],
		[30, 'day'],
		[12, 'month'],
		[Infinity, 'year']
	];
	let v = sec;
	for (const [n, unit] of steps) {
		if (Math.abs(v) < n) return rtf.format(Math.round(v), unit);
		v /= n;
	}
	return dateTime(s);
}

export function bytes(n: number): string {
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let i = 0;
	while (n >= 1024 && i < units.length - 1) {
		n /= 1024;
		i++;
	}
	return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

export function duration(from: string | null, to: string | null): string {
	if (!from) return '—';
	const s = Math.max(0, ((to ? new Date(to) : new Date()).getTime() - new Date(from).getTime()) / 1000);
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	const r = Math.floor(s % 60);
	return h ? `${h} ชม. ${m} นาที` : m ? `${m} นาที ${r} วิ` : `${r} วิ`;
}

/** "123456789" -> "123 456 789" */
export function formatId(id: string): string {
	return id.replace(/(\d{3})(?=\d)/g, '$1 ');
}

/** SHA-256 based fingerprint of a base64 public key; matches `aegiron-agent info`. */
export async function fingerprint(b64: string): Promise<string> {
	const raw = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
	const h = new Uint8Array(await crypto.subtle.digest('SHA-256', raw)).slice(0, 8);
	const hex = [...h].map((b) => b.toString(16).padStart(2, '0'));
	return [0, 2, 4, 6].map((i) => hex[i] + hex[i + 1]).join(':');
}
