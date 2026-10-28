// Helpers for the peer card grid (device tiles on the address book and devices pages).

export type OsKind = 'windows' | 'mac' | 'linux' | 'other';

/** Agents report `std::env::consts::OS` (optionally followed by the arch), e.g. "windows x86_64". */
export function osKind(os: string | null | undefined): OsKind {
	const s = (os ?? '').toLowerCase();
	if (s.startsWith('win')) return 'windows';
	if (s.startsWith('mac') || s.startsWith('darwin') || s.startsWith('ios')) return 'mac';
	if (s.startsWith('linux') || s.includes('bsd') || s.startsWith('android')) return 'linux';
	return 'other';
}

export const osLabel: Record<OsKind, string> = { windows: 'Windows', mac: 'macOS', linux: 'Linux', other: 'ไม่ทราบระบบ' };

/** Stable tile color from the device ID: FNV-1a hash -> hue, fixed lightness so white text stays legible. */
export function peerColor(id: string): string {
	let h = 0x811c9dc5;
	for (let i = 0; i < id.length; i++) {
		h ^= id.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	h >>>= 0;
	const hue = h % 360;
	const chroma = 0.11 + ((h >>> 9) % 5) * 0.01;
	return `oklch(0.56 ${chroma.toFixed(2)} ${hue})`;
}

/** Per-viewer UI preference (grid vs list); storage may be unavailable. */
export function savedView(key: string, fallback: 'grid' | 'list' = 'grid'): 'grid' | 'list' {
	try {
		const v = localStorage.getItem(key);
		return v === 'grid' || v === 'list' ? v : fallback;
	} catch {
		return fallback;
	}
}

export function saveView(key: string, v: 'grid' | 'list') {
	try {
		localStorage.setItem(key, v);
	} catch {
		// ignore
	}
}
