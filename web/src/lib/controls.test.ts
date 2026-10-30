import { describe, expect, test } from 'bun:test';
import { applyCtl, hasControl, noControls, reconnectDelay } from './controls';

describe('session controls', () => {
	test('reconnect backs off to 30 s', () => {
		expect([1, 2, 3, 4, 5, 6, 7].map(reconnectDelay)).toEqual([3000, 6000, 12000, 24000, 30000, 30000, 30000]);
		expect(reconnectDelay(0)).toBe(3000);
	});

	test('controls are offered only when the agent lists them', () => {
		expect(hasControl(undefined, 'restart')).toBe(false); // older agents
		expect(hasControl(['fs2', 'view_only', 'lock'], 'lock')).toBe(true);
		expect(hasControl(['fs2', 'view_only', 'lock'], 'privacy')).toBe(false);
	});

	test('state follows the agent replies', () => {
		let s = noControls();
		s = applyCtl(s, { op: 'block_input', ok: true, on: true }).state;
		s = applyCtl(s, { op: 'lock_after', ok: true, on: true }).state;
		expect(s).toEqual({ block_input: true, privacy: false, lock_after: true });
		const refused = applyCtl(s, { op: 'privacy', ok: false, message: 'not allowed' });
		expect(refused.state).toBe(s);
		expect(refused.notice).toContain('not allowed');
		expect(refused.resume).toBeUndefined();
		expect(applyCtl(s, { op: 'block_input', ok: true, on: false }).state.block_input).toBe(false);
	});

	test('restart hands over the reconnect token, a failed one forgets it', () => {
		const ok = applyCtl(noControls(), { op: 'restart', ok: true, resume: 'tok' });
		expect(ok.resume).toBe('tok');
		expect(ok.notice).toContain('รีสตาร์ท');
		expect(applyCtl(noControls(), { op: 'restart', ok: false, message: 'cancelled' }).resume).toBeNull();
	});
});
