// Where the remote screen is drawn inside the viewer's stage, for each scaling mode (same modes
// as the desktop app and RustDesk): original 1:1, shrink (fit, never enlarge) and stretch (fit
// up and down, keeping the aspect ratio).

export type ScaleMode = 'original' | 'shrink' | 'stretch';

export const SCALE_MODES: readonly ScaleMode[] = ['stretch', 'shrink', 'original'];

export interface Size {
	w: number;
	h: number;
}

/** A rectangle in stage CSS pixels (x/y may be negative while a large picture is panned). */
export interface Rect {
	x: number;
	y: number;
	w: number;
	h: number;
}

/** How far a picture larger than the stage is panned, 0..1 on each axis (0.5 = centered). */
export interface Pan {
	x: number;
	y: number;
}

/** CSS pixels per frame pixel. `dpr` is the browser's devicePixelRatio (1:1 = device pixels). */
export function scaleFactor(mode: ScaleMode, frame: Size, stage: Size, dpr = 1): number {
	if (!frame.w || !frame.h || !stage.w || !stage.h) return 0;
	const fit = Math.min(stage.w / frame.w, stage.h / frame.h);
	const one = 1 / (dpr > 0 ? dpr : 1);
	if (mode === 'original') return one;
	if (mode === 'shrink') return Math.min(fit, one);
	return fit;
}

/** Places the picture: centered on an axis where it fits, otherwise offset by `pan`. */
export function layout(mode: ScaleMode, frame: Size, stage: Size, dpr = 1, pan: Pan = { x: 0.5, y: 0.5 }): Rect {
	const k = scaleFactor(mode, frame, stage, dpr);
	if (!k) return { x: 0, y: 0, w: 0, h: 0 };
	const w = Math.floor(frame.w * k);
	const h = Math.floor(frame.h * k);
	const axis = (size: number, room: number, p: number) => (size <= room ? (room - size) / 2 : clamp01(p) * (room - size) + 0); // + 0: no -0
	return { x: axis(w, stage.w, pan.x), y: axis(h, stage.h, pan.y), w, h };
}

/** True when the picture is bigger than the stage on some axis (so it can be panned). */
export function overflows(r: Rect, stage: Size): boolean {
	return r.w > stage.w + 0.5 || r.h > stage.h + 0.5;
}

/** Pan that follows the local pointer: its position across the stage, 0..1. */
export function panFromPointer(px: number, py: number, stage: Size): Pan {
	return { x: stage.w ? clamp01(px / stage.w) : 0.5, y: stage.h ? clamp01(py / stage.h) : 0.5 };
}

/**
 * Pan that keeps the remote cursor (normalised over the picture) at least `margin` CSS pixels
 * inside the stage, moving as little as possible ("follow remote cursor").
 */
export function panToCursor(pan: Pan, cursor: { x: number; y: number }, r: Rect, stage: Size, margin = 48): Pan {
	const axis = (p: number, c: number, size: number, room: number) => {
		if (size <= room) return p;
		const m = Math.min(margin, room / 2);
		let offset = -clamp01(p) * (size - room);
		const at = offset + c * size;
		if (at < m) offset += m - at;
		else if (at > room - m) offset -= at - (room - m);
		return clamp01(-offset / (size - room));
	};
	return { x: axis(pan.x, cursor.x, r.w, stage.w), y: axis(pan.y, cursor.y, r.h, stage.h) };
}

/** Pointer position (stage CSS pixels) normalised over the picture, clamped to 0..1. */
export function normalise(px: number, py: number, r: Rect): [number, number] {
	if (!r.w || !r.h) return [0, 0];
	return [clamp01((px - r.x) / r.w), clamp01((py - r.y) / r.h)];
}

/** File name for a saved remote screenshot: Aegiron-<peer>-YYYYMMDD-HHMMSS.png. */
export function screenshotName(peer: string, at: Date): string {
	const p = (n: number) => String(n).padStart(2, '0');
	const stamp = `${at.getFullYear()}${p(at.getMonth() + 1)}${p(at.getDate())}-${p(at.getHours())}${p(at.getMinutes())}${p(at.getSeconds())}`;
	// Keep letters of any script (Thai names), drop characters file systems reject.
	const safe = peer.replace(/[\\/:*?"<>|\u0000-\u001f]+/g, '_').trim() || 'remote';
	return `Aegiron-${safe}-${stamp}.png`;
}

function clamp01(v: number): number {
	return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0.5;
}
