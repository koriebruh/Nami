import { describe, expect, it } from 'vitest';
import { alertQueryFromExplorer, describeCondition, formatSeconds } from './log-alerts';

describe('log alerts', () => {
	it('describes conditions in words', () => {
		expect(describeCondition({ comparison: 'gt', threshold: 10, window_seconds: 300 })).toBe('more than 10 matching events in 5 min');
		expect(describeCondition({ comparison: 'lt', threshold: 1, window_seconds: 3600 })).toBe('fewer than 1 matching event in 1 h');
		expect(formatSeconds(90)).toBe('90 s');
		expect(formatSeconds(86400)).toBe('24 h');
	});
	it('prefills from an explorer link, dropping window, columns and live', () => {
		const query = alertQueryFromExplorer(new URLSearchParams('range=7d&level=error&q=disk&col=a&live=1&pattern=5'));
		expect(query).toEqual({ q: 'disk', levels: ['error'], pattern_hashes: ['5'] });
	});
});
