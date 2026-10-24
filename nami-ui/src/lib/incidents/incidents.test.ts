import { describe, expect, it } from 'vitest';
import { duration, engineListQuery, eventLabel, parseListQuery, sourceLink, type Incident } from './incidents';

const PROJECT = 'c13bc10a-d17f-443b-8976-c6d9b54e2798';
const ID = '3f0c8a52-7d1e-4b6a-9c2f-5e8d7a6b4c31';
const url = (query: string) => new URL(`http://ui.test/x${query}`);

describe('parseListQuery', () => {
	it('keeps a known tab, source and a well-formed cursor', () => {
		expect(parseListQuery(url('?tab=resolved&source=log_alert&cursor=MjAyNi0x_-'))).toEqual({
			tab: 'resolved',
			source: 'log_alert',
			cursor: 'MjAyNi0x_-'
		});
	});

	it('defaults to the unresolved tab with no source or cursor', () => {
		expect(parseListQuery(url(''))).toEqual({ tab: 'unresolved', source: null, cursor: null });
	});

	it.each([
		['?tab=bogus&source=x&cursor=%%'],
		['?tab=Resolved&source=Monitor&cursor=a%20b'],
		['?tab=constructor&source=__proto__&cursor=a/b'],
		[`?tab=all%26status%3Dopen&source=manual%26x&cursor=${'a'.repeat(201)}`]
	])('drops an unknown tab, source and a malformed cursor: %s', (query) => {
		expect(parseListQuery(url(query))).toEqual({ tab: 'unresolved', source: null, cursor: null });
	});
});

describe('engineListQuery', () => {
	it('maps each tab to its engine status', () => {
		expect(engineListQuery({ tab: 'unresolved', source: null, cursor: null })).toBe('?status=unresolved');
		expect(engineListQuery({ tab: 'acknowledged', source: null, cursor: null })).toBe('?status=acknowledged');
		expect(engineListQuery({ tab: 'resolved', source: null, cursor: null })).toBe('?status=resolved');
		expect(engineListQuery({ tab: 'all', source: null, cursor: null })).toBe('');
	});

	it('adds the source and the cursor', () => {
		expect(engineListQuery({ tab: 'all', source: 'manual', cursor: 'abc' })).toBe('?source=manual&cursor=abc');
		expect(engineListQuery({ tab: 'unresolved', source: 'monitor', cursor: 'c' })).toBe('?status=unresolved&source=monitor&cursor=c');
	});
});

describe('duration', () => {
	const start = '2026-10-13T10:00:00Z';
	const at = (seconds: number) => new Date(Date.parse(start) + seconds * 1000).toISOString();
	it.each([
		[45, '45 s'],
		[12 * 60 + 30, '12 min'],
		[3 * 3600 + 12 * 60, '3 h 12 min'],
		[3 * 3600, '3 h'],
		[2 * 86_400 + 4 * 3600 + 59, '2 d 4 h'],
		[2 * 86_400, '2 d']
	])('formats %i seconds as %s', (seconds, expected) => {
		expect(duration(start, at(seconds), new Date(0))).toBe(expected);
	});

	it('runs to now while the incident is ongoing, never below zero', () => {
		expect(duration(start, null, new Date(at(90)))).toBe('1 min');
		expect(duration(start, null, new Date(at(-5)))).toBe('0 s');
	});
});

describe('eventLabel', () => {
	it('names known kinds and returns an unknown kind as is', () => {
		expect(eventLabel('opened')).toBe('Incident opened');
		expect(eventLabel('escalation_fallback')).toBe('Moved to fallback policy');
		expect(eventLabel('something_new')).toBe('something_new');
		expect(eventLabel('constructor')).toBe('constructor');
	});
});

describe('sourceLink', () => {
	const base = { monitor_id: null, heartbeat_id: null, log_alert_id: null } as Pick<
		Incident,
		'monitor_id' | 'heartbeat_id' | 'log_alert_id'
	>;
	it('links a monitor, a heartbeat and a log alert', () => {
		expect(sourceLink(PROJECT, { ...base, monitor_id: ID })).toBe(`/projects/${PROJECT}/monitors/${ID}`);
		expect(sourceLink(PROJECT, { ...base, heartbeat_id: ID })).toBe(`/projects/${PROJECT}/heartbeats/${ID}`);
		expect(sourceLink(PROJECT, { ...base, log_alert_id: ID })).toBe(`/projects/${PROJECT}/logs/alerts/${ID}`);
	});

	it('links nothing for a manual incident or an id that is not a UUID', () => {
		expect(sourceLink(PROJECT, base)).toBeNull();
		expect(sourceLink(PROJECT, { ...base, monitor_id: '../../auth/me' })).toBeNull();
	});
});
