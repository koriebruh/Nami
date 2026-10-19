import { describe, expect, it } from 'vitest';
import {
	bucketEnd,
	bucketParts,
	curlSnippet,
	dockerSnippet,
	engineParams,
	entryHref,
	explorerHref,
	explorerParams,
	fieldAt,
	formatBytes,
	formatFieldValue,
	formatLogTime,
	formatQuota,
	fromInputValue,
	ingestUrl,
	isFieldPath,
	levelClasses,
	MIB,
	parseExplorerState,
	quotaInMib,
	resolveWindow,
	rsyslogSnippet,
	tailParams,
	toggled,
	toInputValue,
	vectorSnippet,
	type ExplorerState
} from './logs';

describe('formatBytes', () => {
	it('uses binary units with at most one decimal', () => {
		expect(formatBytes(0)).toBe('0 B');
		expect(formatBytes(1023)).toBe('1023 B');
		expect(formatBytes(1536)).toBe('1.5 KiB');
		expect(formatBytes(MIB)).toBe('1 MiB');
		expect(formatBytes(150 * MIB)).toBe('150 MiB');
		expect(formatBytes(3 * 1024 * MIB)).toBe('3 GiB');
	});

	it('formats quotas', () => {
		expect(formatQuota(null)).toBe('Unlimited');
		expect(formatQuota(10 * MIB)).toBe('10 MiB / day');
		expect(quotaInMib(null)).toBe('');
		expect(quotaInMib(MIB)).toBe('1');
		expect(quotaInMib(1.5 * MIB)).toBe('1.5');
	});
});

describe('snippet quoting', () => {
	it('single-quotes the URL for the shell, whatever it holds', () => {
		expect(curlSnippet("https://e.test/a'b", 'tok')).toContain("curl -X POST 'https://e.test/a'\\''b' \\");
	});
	it('quotes YAML values', () => {
		expect(vectorSnippet('https://e.test/api/v1/logs', 'tok')).toContain('uri: "https://e.test/api/v1/logs"');
	});
});

describe('setup snippets', () => {
	const url = ingestUrl('https://engine.example.com');
	const token = 'tok_123';

	it('point at the ingest endpoint with the token', () => {
		expect(url).toBe('https://engine.example.com/api/v1/logs');
		for (const snippet of [curlSnippet(url, token), vectorSnippet(url, token), dockerSnippet(url, token)]) {
			expect(snippet).toContain(url);
			expect(snippet).toContain(token);
		}
		expect(curlSnippet(url, token)).toContain("-H 'Authorization: Bearer tok_123'");
		expect(vectorSnippet(url, token)).toContain('strategy: bearer');
		expect(dockerSnippet(url, token)).toContain('type: docker_logs');
	});

	it('send RFC 5424 with the token in structured data for syslog', () => {
		const snippet = rsyslogSnippet('logs.example.com', 5140, token);
		expect(snippet).toContain('<%pri%>1 %timestamp:::date-rfc3339%');
		expect(snippet).toContain('[nami token=\\"tok_123\\"]');
		expect(snippet).toContain('target="logs.example.com" port="5140" protocol="tcp"');
	});
});

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

describe('explorer URL state', () => {
	it('defaults to the last hour with nothing selected', () => {
		const state = parseExplorerState(new URLSearchParams());
		expect(state).toEqual({
			range: '1h',
			from: null,
			to: null,
			q: '',
			inFields: false,
			levels: [],
			sources: [],
			filters: [],
			columns: [],
			live: false
		});
		expect(explorerParams(state).toString()).toBe('');
		expect(explorerHref('/p/logs', state)).toBe('/p/logs');
	});

	it('round-trips every setting through the URL', () => {
		const state: ExplorerState = {
			range: 'custom',
			from: '2026-10-13T10:00:00.000Z',
			to: '2026-10-13T11:00:00.000Z',
			q: 'disk full',
			inFields: true,
			levels: ['error', 'warn'],
			sources: [SOURCE],
			filters: [
				{ path: 'user.id', op: 'eq', value: '42' },
				{ path: 'trace', op: 'exists' }
			],
			columns: ['user.id', 'status'],
			live: true
		};
		const url = explorerParams(state);
		expect(parseExplorerState(url)).toEqual(state);
		expect(url.getAll('level')).toEqual(['error', 'warn']);
		expect(url.get('filter')).toBe('{"path":"user.id","op":"eq","value":"42"}');
	});

	it('keeps relative presets relative', () => {
		const state = parseExplorerState(new URLSearchParams('range=24h'));
		expect(explorerParams(state).toString()).toBe('range=24h');
		const now = Date.parse('2026-10-13T12:00:00Z');
		expect(resolveWindow(state, now)).toEqual({
			from: '2026-10-12T12:00:00.000Z',
			to: '2026-10-13T12:00:00.000Z'
		});
	});

	it('drops anything invalid instead of failing', () => {
		const params = new URLSearchParams();
		params.set('range', 'custom');
		params.set('from', 'not a date');
		params.append('level', 'verbose');
		params.append('level', 'error');
		params.append('level', 'error');
		params.append('source', '../x');
		params.append('filter', 'not json');
		params.append('filter', JSON.stringify({ path: 'a b', op: 'eq', value: 'x' }));
		params.append('filter', JSON.stringify({ path: 'a', op: 'like', value: 'x' }));
		params.append('filter', JSON.stringify({ path: 'a', op: 'eq' }));
		params.append('filter', JSON.stringify({ path: 'a', op: 'exists', value: 'ignored' }));
		for (let i = 0; i < 12; i++) params.append('col', `c${i}`);
		params.append('col', 'bad path!');
		params.set('token', 'secret');
		const state = parseExplorerState(params);
		expect(state.range).toBe('1h');
		expect(state.levels).toEqual(['error']);
		expect(state.sources).toEqual([]);
		expect(state.filters).toEqual([{ path: 'a', op: 'exists' }]);
		expect(state.columns).toHaveLength(10);
		expect(explorerParams(state).toString()).not.toContain('token');
	});

	it('maps to the engine parameters', () => {
		const state = parseExplorerState(
			new URLSearchParams(
				`source=${SOURCE}&level=error&q=Disk&in_fields=1&filter=${encodeURIComponent('{"path":"a","op":"gt","value":"5"}')}`
			)
		);
		const window = {
			from: '2026-10-13T10:00:00.000Z',
			to: '2026-10-13T11:00:00.000Z'
		};
		const engine = engineParams(state, window);
		expect(engine.getAll('source_id')).toEqual([SOURCE]);
		expect(engine.get('in_fields')).toBe('true');
		expect(engine.get('filter')).toBe('{"path":"a","op":"gt","value":"5"}');
		expect([engine.get('from'), engine.get('to')]).toEqual([window.from, window.to]);
		const tail = tailParams(state);
		expect(tail.has('from') || tail.has('to')).toBe(false);
		expect(tailParams(parseExplorerState(new URLSearchParams('in_fields=1'))).has('in_fields')).toBe(false);
	});
});

describe('row helpers', () => {
	it('reads field paths', () => {
		const fields = { user: { id: 42, tags: ['a'] }, status: 'ok' };
		expect(fieldAt(fields, 'user.id')).toBe(42);
		expect(fieldAt(fields, 'status')).toBe('ok');
		expect(fieldAt(fields, 'user.missing')).toBeUndefined();
		expect(fieldAt(fields, 'status.deeper')).toBeUndefined();
		expect(fieldAt(fields, 'user.tags.0')).toBeUndefined();
		expect(formatFieldValue(fieldAt(fields, 'user'))).toBe('{"id":42,"tags":["a"]}');
		expect(formatFieldValue(undefined)).toBe('');
		expect(formatFieldValue('text')).toBe('text');
	});

	it('validates paths like the engine', () => {
		expect(isFieldPath('a.b-c_d.1')).toBe(true);
		for (const path of ['', 'a..b', 'a b', 'a.b.c.d.e.f', "a'", 'é']) expect(isFieldPath(path)).toBe(false);
	});

	it('formats times, levels and toggles', () => {
		expect(formatLogTime('2026-10-13T14:30:05.123Z')).toBe('2026-10-13 14:30:05.123');
		expect(levelClasses('error')).toContain('text-red-300');
		expect(levelClasses('fatal')).toContain('font-semibold');
		expect(levelClasses('unknown')).toContain('text-neutral-400');
		expect(toggled(['a'], 'b')).toEqual(['a', 'b']);
		expect(toggled(['a', 'b'], 'a')).toEqual(['b']);
		expect(toInputValue('2026-10-13T14:30:00.000Z')).toBe('2026-10-13T14:30');
		expect(fromInputValue('2026-10-13T14:30')).toBe('2026-10-13T14:30:00.000Z');
		expect(fromInputValue('')).toBeNull();
	});
});

describe('histogram helpers', () => {
	it('splits a bucket into stacked parts', () => {
		expect(bucketParts({ start: 'x', count: 10, errors: 2, warnings: 3 })).toEqual({
			errors: 2,
			warnings: 3,
			other: 5
		});
		expect(bucketParts({ start: 'x', count: 1, errors: 2, warnings: 3 }).other).toBe(0);
	});

	it('ends a bucket one step after its start', () => {
		expect(bucketEnd({ start: '2026-10-13T10:00:00Z', count: 0, errors: 0, warnings: 0 }, 300)).toBe(
			'2026-10-13T10:05:00.000Z'
		);
	});
});

describe('entryHref', () => {
	it('points at the BFF entry endpoint with the row dt', () => {
		expect(
			entryHref('/projects/p/logs', {
				id: '1c7f4f4e-7e3b-4d2f-8c8b-3a2e4d5c6b70',
				dt: '2026-10-13T12:00:00.123Z'
			})
		).toBe('/projects/p/logs/entry/1c7f4f4e-7e3b-4d2f-8c8b-3a2e4d5c6b70?dt=2026-10-13T12%3A00%3A00.123Z');
	});
});
