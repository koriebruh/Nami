import { describe, expect, it } from 'vitest';
import { FILTER_PARAMS, pickLogParams, SEARCH_PARAMS, TAIL_PARAMS } from './log-proxy';

describe('pickLogParams', () => {
	it('keeps only the allowed keys, repeats included, in order', () => {
		const params = new URLSearchParams(
			'level=error&token=secret&level=warn&q=disk%20full&cursor=abc&filter=%7B%22path%22%3A%22a%22%7D'
		);
		expect(pickLogParams(params, TAIL_PARAMS)).toBe(
			'?level=error&level=warn&q=disk+full&filter=%7B%22path%22%3A%22a%22%7D'
		);
		expect(pickLogParams(params, SEARCH_PARAMS)).toContain('cursor=abc');
		expect(pickLogParams(params, SEARCH_PARAMS)).not.toContain('token');
	});

	it('returns an empty string when nothing is allowed', () => {
		expect(pickLogParams(new URLSearchParams('x=1'), TAIL_PARAMS)).toBe('');
		expect(pickLogParams(new URLSearchParams(), TAIL_PARAMS)).toBe('');
	});

	it('keeps the allow-lists frozen and separate', () => {
		expect(Object.isFrozen(TAIL_PARAMS)).toBe(true);
		expect(Object.isFrozen(FILTER_PARAMS)).toBe(true);
		expect(Object.isFrozen(SEARCH_PARAMS)).toBe(true);
		expect(TAIL_PARAMS).not.toBe(FILTER_PARAMS);
		expect(() => (TAIL_PARAMS as string[]).push('cursor')).toThrow();
	});
});
