import { describe, expect, it } from 'vitest';
import {
	EXPORT_PARAMS,
	FILTER_PARAMS,
	lastEventIdHeader,
	PATTERN_PARAMS,
	pickLogParams,
	SEARCH_PARAMS,
	TAIL_PARAMS
} from './log-proxy';

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

	it('lets an export carry the filters, the window, the format and its columns only', () => {
		expect(EXPORT_PARAMS).toEqual([...FILTER_PARAMS, 'from', 'to', 'format', 'columns']);
		expect(Object.isFrozen(EXPORT_PARAMS)).toBe(true);
		expect(EXPORT_PARAMS).not.toContain('cursor');
		expect(EXPORT_PARAMS).not.toContain('limit');
	});
});

describe('PATTERN_PARAMS', () => {
	it('lets patterns carry the filters and the window only', () => {
		expect(PATTERN_PARAMS).toEqual([...FILTER_PARAMS, 'from', 'to']);
		expect(Object.isFrozen(PATTERN_PARAMS)).toBe(true);
		expect(PATTERN_PARAMS).not.toContain('limit');
		expect(PATTERN_PARAMS).not.toContain('cursor');
	});
});

describe('lastEventIdHeader', () => {
	it('forwards only a plausible cursor', () => {
		expect(lastEventIdHeader('MTc5MjAwMDAwMDAwMDpjMTNiYzEwYS1kMTdm')).toBe('MTc5MjAwMDAwMDAwMDpjMTNiYzEwYS1kMTdm');
		expect(lastEventIdHeader(null)).toBeNull();
		expect(lastEventIdHeader('')).toBeNull();
		expect(lastEventIdHeader('a b')).toBeNull();
		expect(lastEventIdHeader('x'.repeat(128))).toBe('x'.repeat(128));
		expect(lastEventIdHeader('x'.repeat(129))).toBeNull();
	});
});
