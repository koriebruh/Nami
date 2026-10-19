import { describe, expect, it } from 'vitest';
import { describeSchedule, describeScope, shortTime, toUtcInputValue } from './maintenance';

const weekly = (days: number[], start: string, end: string) => ({
	kind: 'weekly' as const,
	starts_at: null,
	ends_at: null,
	days_of_week: days,
	start_time: start,
	end_time: end,
	timezone: 'Asia/Jakarta'
});

describe('describeSchedule', () => {
	it('shows a one-off window in UTC', () => {
		expect(
			describeSchedule({
				kind: 'once',
				starts_at: '2026-10-13T22:00:00Z',
				ends_at: '2026-10-14T02:00:00Z',
				days_of_week: null,
				start_time: null,
				end_time: null,
				timezone: null
			})
		).toBe('2026-10-13 22:00:00 UTC → 2026-10-14 02:00:00 UTC');
	});

	it('names the days, the times and the zone of a weekly window', () => {
		expect(describeSchedule(weekly([4, 2], '09:00:00', '17:30:00'))).toBe('Tue, Thu, 09:00–17:30 Asia/Jakarta');
		expect(describeSchedule(weekly([1, 2, 3, 4, 5], '01:00:00', '02:00:00'))).toBe('Weekdays, 01:00–02:00 Asia/Jakarta');
		expect(describeSchedule(weekly([6, 7], '01:00:00', '02:00:00'))).toBe('Weekends, 01:00–02:00 Asia/Jakarta');
		expect(describeSchedule(weekly([1, 2, 3, 4, 5, 6, 7], '00:00:00', '00:00:00'))).toBe(
			'Every day, 00:00–00:00 (next day) Asia/Jakarta'
		);
	});

	it('marks a window that runs past midnight', () => {
		expect(describeSchedule(weekly([5], '22:00:00', '02:00:00'))).toBe('Fri, 22:00–02:00 (next day) Asia/Jakarta');
	});
});

describe('describeScope', () => {
	it('names the covered monitor, or says the whole project', () => {
		const monitors = [{ id: 'm1', name: 'Checkout API' }];
		expect(describeScope(null, monitors)).toBe('All monitors and heartbeats');
		expect(describeScope('m1', monitors)).toBe('Checkout API');
		expect(describeScope('gone', monitors)).toBe('One monitor');
	});
});

describe('time helpers', () => {
	it('shortens times and renders datetime-local values in UTC', () => {
		expect(shortTime('22:00:00')).toBe('22:00');
		expect(shortTime(null)).toBe('');
		expect(toUtcInputValue('2026-10-13T22:05:30.123Z')).toBe('2026-10-13T22:05');
		expect(toUtcInputValue('2026-10-14T05:00:00+07:00')).toBe('2026-10-13T22:00');
		expect(toUtcInputValue(null)).toBe('');
	});
});
