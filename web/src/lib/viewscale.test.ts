/// <reference types="bun" />
// bun test src/lib/viewscale.test.ts

import { expect, test } from 'bun:test';
import { layout, normalise, overflows, panFromPointer, panToCursor, scaleFactor, screenshotName } from './viewscale';

const stage = { w: 1000, h: 500 };

test('stretch fits up and down keeping the aspect ratio', () => {
	expect(scaleFactor('stretch', { w: 2000, h: 500 }, stage)).toBe(0.5);
	expect(scaleFactor('stretch', { w: 500, h: 250 }, stage)).toBe(2);
	expect(layout('stretch', { w: 500, h: 250 }, stage)).toEqual({ x: 0, y: 0, w: 1000, h: 500 });
	expect(layout('stretch', { w: 2000, h: 500 }, stage)).toEqual({ x: 0, y: 125, w: 1000, h: 250 });
});

test('shrink never enlarges', () => {
	expect(scaleFactor('shrink', { w: 500, h: 250 }, stage)).toBe(1);
	expect(layout('shrink', { w: 500, h: 250 }, stage)).toEqual({ x: 250, y: 125, w: 500, h: 250 });
	expect(scaleFactor('shrink', { w: 2000, h: 500 }, stage)).toBe(0.5);
	// On a 2x screen 1:1 means device pixels, so a small frame is drawn at half the CSS size.
	expect(scaleFactor('shrink', { w: 500, h: 250 }, stage, 2)).toBe(0.5);
});

test('original is 1:1 device pixels and pans when larger than the stage', () => {
	expect(scaleFactor('original', { w: 3000, h: 1000 }, stage, 2)).toBe(0.5);
	const f = { w: 3000, h: 1000 };
	expect(layout('original', f, stage, 1, { x: 0, y: 0 })).toEqual({ x: 0, y: 0, w: 3000, h: 1000 });
	expect(layout('original', f, stage, 1, { x: 1, y: 1 })).toEqual({ x: -2000, y: -500, w: 3000, h: 1000 });
	expect(layout('original', f, stage, 1, { x: 0.5, y: 0.5 })).toEqual({ x: -1000, y: -250, w: 3000, h: 1000 });
	expect(overflows(layout('original', f, stage), stage)).toBe(true);
	// Smaller than the stage: centered, pan ignored.
	expect(layout('original', { w: 400, h: 200 }, stage, 1, { x: 1, y: 0 })).toEqual({ x: 300, y: 150, w: 400, h: 200 });
});

test('nothing to draw yet', () => {
	expect(layout('stretch', { w: 0, h: 0 }, stage)).toEqual({ x: 0, y: 0, w: 0, h: 0 });
	expect(normalise(10, 10, { x: 0, y: 0, w: 0, h: 0 })).toEqual([0, 0]);
});

test('pointer pan and normalisation', () => {
	expect(panFromPointer(250, 500, stage)).toEqual({ x: 0.25, y: 1 });
	expect(panFromPointer(-5, 900, stage)).toEqual({ x: 0, y: 1 });
	const r = { x: -1000, y: -250, w: 3000, h: 1000 };
	expect(normalise(500, 250, r)).toEqual([0.5, 0.5]);
	expect(normalise(-2000, 9999, r)).toEqual([0, 1]);
});

test('follow remote cursor pans only as far as needed', () => {
	const f = { w: 3000, h: 1000 };
	const pan = { x: 0, y: 0 };
	const r = layout('original', f, stage, 1, pan);
	// Cursor already visible: no change.
	expect(panToCursor(pan, { x: 0.1, y: 0.2 }, r, stage)).toEqual(pan);
	// Cursor at x = 2000 px (2/3 of 3000): offset so it sits 48 px inside the right edge.
	const p = panToCursor(pan, { x: 2 / 3, y: 0.2 }, r, stage);
	const moved = layout('original', f, stage, 1, p);
	expect(moved.x + (2 / 3) * moved.w).toBeCloseTo(1000 - 48, 5);
	expect(p.y).toBe(0);
	// Far right: clamped to the end.
	expect(panToCursor(pan, { x: 1, y: 1 }, r, stage)).toEqual({ x: 1, y: 1 });
	// Picture fits: pan unchanged.
	const small = layout('original', { w: 400, h: 200 }, stage);
	expect(panToCursor({ x: 0.3, y: 0.3 }, { x: 1, y: 1 }, small, stage)).toEqual({ x: 0.3, y: 0.3 });
});

test('screenshot names', () => {
	const at = new Date(2026, 9, 3, 7, 5, 9);
	expect(screenshotName('บัญชี-PC01', at)).toBe('Aegiron-บัญชี-PC01-20261003-070509.png');
	expect(screenshotName('a/b:c', at)).toBe('Aegiron-a_b_c-20261003-070509.png');
	expect(screenshotName('', at)).toBe('Aegiron-remote-20261003-070509.png');
});
