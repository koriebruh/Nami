import { describe, expect, it } from 'vitest';
import { customRange, exportRange, formatPct, selectRange } from './availability';

const now = new Date('2026-05-17T12:00:00Z');
const q = (s: string) => new URLSearchParams(s);

describe('selectRange', () => {
	it('defaults to 30 days and ends at now', () => {
		expect(selectRange(q(''), now)).toMatchObject({
			key: '30d',
			from: '2026-04-17T12:00:00.000Z',
			to: '2026-05-17T12:00:00.000Z'
		});
	});
	it.each([
		['24h', '2026-05-16T12:00:00.000Z'],
		['7d', '2026-05-10T12:00:00.000Z'],
		['90d', '2026-02-16T12:00:00.000Z']
	])('knows the %s preset', (key, from) => {
		expect(selectRange(q(`range=${key}`), now)).toMatchObject({ key, from });
	});
	it('falls back to the default for an unknown preset', () => {
		expect(selectRange(q('range=../auth'), now).key).toBe('30d');
	});
	it('takes custom dates, the end day included', () => {
		expect(selectRange(q('range=custom&from=2026-01-01&to=2026-01-31'), now)).toMatchObject({
			key: 'custom',
			from: '2026-01-01T00:00:00.000Z',
			to: '2026-02-01T00:00:00.000Z',
			error: null
		});
	});
	it('asks for dates when custom has none, and refuses bad ones', () => {
		expect(selectRange(q('range=custom'), now)).toMatchObject({ key: 'custom', from: null, error: null });
		expect(selectRange(q('range=custom&from=2026-02-01&to=2026-01-01'), now)).toMatchObject({
			from: null,
			error: expect.stringContaining('valid dates')
		});
	});
});

describe('customRange', () => {
	it('accepts only sane ranges', () => {
		expect(customRange('2026-01-01', '2026-01-31')).toEqual({
			from: '2026-01-01T00:00:00.000Z',
			to: '2026-02-01T00:00:00.000Z'
		});
		expect(customRange('2026-03-05', '2026-03-05')).toEqual({
			from: '2026-03-05T00:00:00.000Z',
			to: '2026-03-06T00:00:00.000Z'
		});
		for (const [f, t] of [
			['2026-02-01', '2026-01-01'],
			['2026-13-01', '2026-01-01'],
			['2026-02-30', '2026-03-05'],
			['x', 'y'],
			[null, '2026-01-01'],
			['2020-01-01', '2026-01-01']
		] as const) {
			expect(customRange(f, t)).toBeNull();
		}
	});
});

describe('exportRange', () => {
	it('keeps a short range as it is', () => {
		expect(exportRange({ from: '2026-05-10T00:00:00.000Z', to: '2026-05-17T00:00:00.000Z' })).toEqual({
			from: '2026-05-10T00:00:00.000Z',
			to: '2026-05-17T00:00:00.000Z',
			clipped: false
		});
	});
	it('clips a long range to its last 180 days', () => {
		expect(exportRange({ from: '2025-01-01T00:00:00.000Z', to: '2026-01-01T00:00:00.000Z' })).toEqual({
			from: '2025-07-05T00:00:00.000Z',
			to: '2026-01-01T00:00:00.000Z',
			clipped: true
		});
	});
	it('has nothing to export without a range', () => {
		expect(exportRange({ from: null, to: null })).toBeNull();
	});
});

describe('formatPct', () => {
	it('shows no data for null, never 0', () => {
		expect(formatPct(null)).toBe('No data');
	});
	it('formats with three truncated decimals', () => {
		expect(formatPct(100)).toBe('100');
		expect(formatPct(99.95)).toBe('99.950');
		expect(formatPct(99.9996)).toBe('99.999');
		expect(formatPct(0)).toBe('0.000');
	});
});
