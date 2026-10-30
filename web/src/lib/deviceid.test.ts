import { expect, test } from 'bun:test';
import { cleanDeviceId, validDeviceId } from './api';
import { formatId } from './format';

test('device ids: 9-16 digits or a custom id', () => {
	for (const ok of ['123456789', '1234567890123456', 'front-desk', 'pc_01a']) expect(validDeviceId(ok)).toBe(true);
	for (const bad of ['12345678', '12345678901234567', 'Front-desk', '1abcdef', 'abc', 'pc 01', 'pc/../x', '']) expect(validDeviceId(bad)).toBe(false);
	expect(cleanDeviceId(' 123 456 789 ')).toBe('123456789');
	expect(cleanDeviceId('Front-Desk')).toBe('front-desk');
});

test('formatId groups digits only', () => {
	expect(formatId('123456789')).toBe('123 456 789');
	expect(formatId('pc123456')).toBe('pc123456');
});
