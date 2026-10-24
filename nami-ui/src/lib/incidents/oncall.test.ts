import { describe, expect, it } from 'vitest';
import { formatInZone, localInputToUtc, shiftRows } from './oncall';

describe('localInputToUtc', () => {
	it('converts in the schedule zone, across DST', () => {
		expect(localInputToUtc('2026-03-30T09:00', 'Europe/Berlin')).toBe('2026-03-30T07:00:00.000Z');
		expect(localInputToUtc('2026-03-27T09:00', 'Europe/Berlin')).toBe('2026-03-27T08:00:00.000Z');
	});
	it('takes the earlier instant of a repeated hour and refuses a skipped one', () => {
		expect(localInputToUtc('2026-10-25T02:30', 'Europe/Berlin')).toBe('2026-10-25T00:30:00.000Z');
		expect(localInputToUtc('2026-03-29T02:30', 'Europe/Berlin')).toBeNull();
	});
	it('refuses junk', () => {
		expect(localInputToUtc('yesterday', 'UTC')).toBeNull();
		expect(localInputToUtc('2026-10-19T09:00', 'Mars/Olympus')).toBeNull();
	});
	// Beyond the plan's list: seconds some browsers send, an impossible date,
	// midnight, and a zone west of UTC.
	it('takes seconds, refuses an impossible date, and handles midnight and negative offsets', () => {
		expect(localInputToUtc('2026-10-19T09:00:00', 'UTC')).toBe('2026-10-19T09:00:00.000Z');
		expect(localInputToUtc('2026-02-30T09:00', 'UTC')).toBeNull();
		expect(localInputToUtc('2026-10-19T00:00', 'Europe/Berlin')).toBe('2026-10-18T22:00:00.000Z');
		expect(localInputToUtc('2026-10-19T09:00', 'America/New_York')).toBe('2026-10-19T13:00:00.000Z');
	});
});

// ICU abbreviates September as "Sept" in en-GB: no month may depend on its names.
describe('every month', () => {
	it('converts and formats September like any other month', () => {
		expect(localInputToUtc('2026-09-15T09:00', 'Europe/Berlin')).toBe('2026-09-15T07:00:00.000Z');
		expect(formatInZone('2026-09-15T07:00:00Z', 'Europe/Berlin')).toBe('Tue 15 Sep 09:00');
	});
	it('round-trips a time on the first of every month', () => {
		for (let m = 1; m <= 12; m++) {
			const local = `2026-${String(m).padStart(2, '0')}-01T12:00`;
			expect(localInputToUtc(local, 'UTC'), local).toBe(`${local}:00.000Z`);
		}
	});
});

describe('shiftRows', () => {
	it('formats times in the schedule zone and marks overrides', () => {
		const rows = shiftRows([{ user_id: 'u', email: 'a@b.test', starts_at: '2026-10-19T07:00:00Z', ends_at: '2026-10-20T07:00:00Z', override_id: 'o' }], 'Europe/Berlin');
		expect(rows).toEqual([{ who: 'a@b.test', from: 'Mon 19 Oct 09:00', to: 'Tue 20 Oct 09:00', isOverride: true }]);
	});
	it('marks rotation shifts as such and writes midnight as 00:00', () => {
		const rows = shiftRows([{ user_id: 'u', email: 'a@b.test', starts_at: '2026-10-18T22:00:00Z', ends_at: '2026-10-19T22:00:00Z', override_id: null }], 'Europe/Berlin');
		expect(rows).toEqual([{ who: 'a@b.test', from: 'Mon 19 Oct 00:00', to: 'Tue 20 Oct 00:00', isOverride: false }]);
	});
});

describe('formatInZone', () => {
	it('falls back to UTC for a zone the runtime does not know', () => {
		expect(formatInZone('2026-10-19T07:00:00Z', 'Mars/Olympus')).toBe('Mon 19 Oct 07:00');
	});
});
