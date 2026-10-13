import { describe, expect, it } from 'vitest';
import {
	formatClockUtc,
	formatDuration,
	formatMs,
	formatUptime,
	formatUtc,
	monitorTarget,
	niceTicks,
	statusSince,
	timeAgo,
	type Incident
} from './uptime';

describe('formatUptime', () => {
	it('shows two decimals and a dash for unknown', () => {
		expect(formatUptime(null)).toBe('—');
		expect(formatUptime(undefined)).toBe('—');
		expect(formatUptime(1)).toBe('100.00%');
		expect(formatUptime(0.75)).toBe('75.00%');
		expect(formatUptime(0)).toBe('0.00%');
	});

	// Review Focus: uptime that is not exactly 100% never displays as 100.00%.
	it('truncates instead of rounding, so any downtime stays visible', () => {
		expect(formatUptime(0.99999)).toBe('99.99%');
		expect(formatUptime(86_399 / 86_400)).toBe('99.99%');
		expect(formatUptime(0.12349)).toBe('12.34%');
	});

	it('is not thrown off by floating-point error', () => {
		expect(formatUptime(0.57)).toBe('57.00%'); // 0.57 * 10000 = 5699.999…
		expect(formatUptime(0.29)).toBe('29.00%');
	});
});

describe('formatMs', () => {
	it('rounds to whole milliseconds', () => {
		expect(formatMs(199.6)).toBe('200 ms');
		expect(formatMs(null)).toBe('—');
	});
});

describe('timeAgo', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('picks the largest whole unit', () => {
		expect(timeAgo(null, now)).toBe('never');
		expect(timeAgo('2026-10-13T11:59:48Z', now)).toBe('12s ago');
		expect(timeAgo('2026-10-13T11:55:00Z', now)).toBe('5m ago');
		expect(timeAgo('2026-10-13T09:00:00Z', now)).toBe('3h ago');
		expect(timeAgo('2026-10-11T12:00:00Z', now)).toBe('2d ago');
	});
	it('never goes negative for timestamps slightly in the future', () => {
		expect(timeAgo('2026-10-13T12:00:02Z', now)).toBe('0s ago');
	});
});

describe('formatDuration', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('uses two units at most', () => {
		expect(formatDuration('2026-10-13T11:59:15Z', '2026-10-13T12:00:00Z', now)).toBe('45s');
		expect(formatDuration('2026-10-13T11:57:30Z', '2026-10-13T12:00:00Z', now)).toBe('2m 30s');
		expect(formatDuration('2026-10-13T09:55:00Z', '2026-10-13T12:00:00Z', now)).toBe('2h 5m');
		expect(formatDuration('2026-10-10T08:00:00Z', '2026-10-13T12:00:00Z', now)).toBe('3d 4h');
	});
	it('measures an ongoing incident up to now', () => {
		expect(formatDuration('2026-10-13T11:00:00Z', null, now)).toBe('1h 0m');
	});
});

describe('UTC formatting', () => {
	it('normalizes engine timestamps of any precision', () => {
		expect(formatUtc('2026-10-13T10:00:30.123456Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatUtc('2026-10-13T10:00:30.5Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatClockUtc('2026-10-13T09:30:00Z')).toBe('09:30');
	});
});

describe('monitorTarget', () => {
	it('shows the url or host:port', () => {
		expect(monitorTarget({ check_type: 'http', url: 'https://x.test', host: null, port: null })).toBe('https://x.test');
		expect(monitorTarget({ check_type: 'tcp', url: null, host: 'db', port: 5432 })).toBe('db:5432');
	});
});

describe('niceTicks', () => {
	it('ends on a round value at or above the max', () => {
		expect(niceTicks(183)).toEqual([0, 50, 100, 150, 200]);
		expect(niceTicks(1234)).toEqual([0, 500, 1000, 1500]);
		expect(niceTicks(40)).toEqual([0, 10, 20, 30, 40]);
		expect(niceTicks(7)).toEqual([0, 2, 4, 6, 8]);
	});
	it('has a default scale when there is no data', () => {
		expect(niceTicks(0)).toEqual([0, 50, 100, 150, 200]);
	});
	// Response times are whole milliseconds, so the axis never needs a step below 1.
	it('uses whole-millisecond steps when the maximum is below one tick', () => {
		expect(niceTicks(0.5)).toEqual([0, 1, 2, 3, 4]);
		expect(niceTicks(0.0001)).toEqual([0, 1, 2, 3, 4]);
		expect(niceTicks(3)).toEqual([0, 1, 2, 3, 4]);
	});
	it('never labels a tick with float noise', () => {
		for (const max of [0.1, 0.3, 0.7, 1.5, 3.3, 9.9, 99.9, 123.456, 1999.9]) {
			for (const tick of niceTicks(max)) expect(Number.isInteger(tick), `max ${max}: ${tick}`).toBe(true);
		}
	});
	it('still ends on a round value for large maxima', () => {
		expect(niceTicks(60_000)).toEqual([0, 20_000, 40_000, 60_000]);
		expect(niceTicks(123_456_789)).toEqual([0, 50_000_000, 100_000_000, 150_000_000]);
	});
});

describe('statusSince', () => {
	const created = '2026-10-01T00:00:00Z';
	const incident = (status: 'open' | 'resolved', started: string, resolved: string | null): Incident => ({
		id: started,
		project_id: 'p',
		monitor_id: 'm',
		heartbeat_id: null,
		source_name: 'API',
		title: 'API is down',
		cause: null,
		status,
		started_at: started,
		resolved_at: resolved
	});
	const history = [
		incident('resolved', '2026-10-05T00:00:00Z', '2026-10-05T01:00:00Z'),
		incident('resolved', '2026-10-09T00:00:00Z', '2026-10-09T00:30:00Z')
	];

	it('is the open incident start while down', () => {
		const open = incident('open', '2026-10-12T00:00:00Z', null);
		expect(statusSince({ current_status: 'down', created_at: created }, [open, ...history])).toBe(open.started_at);
	});
	it('is the latest recovery while up', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, history)).toBe('2026-10-09T00:30:00Z');
	});
	it('falls back to creation', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, [])).toBe(created);
		expect(statusSince({ current_status: 'pending', created_at: created }, history)).toBe(created);
	});
});
