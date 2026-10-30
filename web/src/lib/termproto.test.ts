import { describe, expect, test } from 'bun:test';
import { eventsUntil, explainRefusal, jsonMessage, MSG_TERM, parseJson, termEvents, termInput } from './termproto';

describe('terminal messages', () => {
	test('keystrokes are UTF-8 after the type byte', () => {
		const m = termInput('สวัสดี\r');
		expect(m[0]).toBe(MSG_TERM);
		expect(new TextDecoder().decode(m.subarray(1))).toBe('สวัสดี\r');
		expect([...termInput('\x03')]).toEqual([MSG_TERM, 3]);
	});

	test('json round trip', () => {
		expect(parseJson(jsonMessage({ t: 'term_resize', cols: 120, rows: 40 }))).toEqual({ t: 'term_resize', cols: 120, rows: 40 });
		expect(parseJson(termInput('x'))).toBeNull();
	});

	test('refusals are explained', () => {
		expect(explainRefusal('terminal_disabled')).toContain('ไม่อนุญาตเทอร์มินัล');
		expect(explainRefusal('target_not_allowed', '10.0.0.1:22')).toContain('10.0.0.1:22');
		expect(explainRefusal('weird')).toContain('weird');
	});
});

describe('terminal recordings', () => {
	const rec = [
		{ t: 0, data: jsonMessage({ t: 'term_size', cols: 80, rows: 24 }) },
		{ t: 10, data: termInput('$ ') },
		{ t: 10, data: jsonMessage({ t: 'cursor', x: 0, y: 0 }) },
		{ t: 500, data: termInput('ls\r\n') },
		{ t: 900, data: jsonMessage({ t: 'term_size', cols: 120, rows: 40 }) },
		{ t: 1000, data: jsonMessage({ t: 'end' }) }
	];

	test('only output and sizes are replayed', () => {
		const ev = termEvents(rec);
		expect(ev.map((e) => e.t)).toEqual([0, 10, 500, 900]);
		expect('size' in ev[0] && ev[0].size).toEqual({ cols: 80, rows: 24 });
		expect('out' in ev[2] && new TextDecoder().decode(ev[2].out)).toBe('ls\r\n');
	});

	test('position in time', () => {
		const ev = termEvents(rec);
		expect(eventsUntil(ev, 0)).toBe(1);
		expect(eventsUntil(ev, 499)).toBe(2);
		expect(eventsUntil(ev, 500)).toBe(3);
		expect(eventsUntil(ev, 5000, 3)).toBe(4);
	});
});
