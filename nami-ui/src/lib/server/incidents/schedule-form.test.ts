import { describe, expect, it } from 'vitest';
import { overridePayloadFromForm, schedulePayloadFromForm } from './schedule-form';

const A = '951106f5-21e1-4c23-9a60-9df4fc9e79ed';
const B = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';

function form(fields: [string, string][]): FormData {
	const fd = new FormData();
	for (const [name, value] of fields) fd.append(name, value);
	return fd;
}

const weekly: [string, string][] = [
	['name', ' Primary '],
	['timezone', 'Europe/Berlin'],
	['rotation', 'weekly'],
	['rotation_hours', '12'],
	['handoff_time', '09:00'],
	['start_date', '2026-10-19'],
	['participant', B],
	['participant', A],
	['restriction_count', '1'],
	['restriction_0_weekday', '5'],
	['restriction_0_weekday', '1'],
	['restriction_0_from', '09:00'],
	['restriction_0_to', '17:00']
];

describe('schedulePayloadFromForm', () => {
	it('turns a full weekly schedule into the payload, participants in submitted order', () => {
		expect(schedulePayloadFromForm(form(weekly))).toEqual({
			ok: true,
			payload: {
				name: 'Primary',
				timezone: 'Europe/Berlin',
				rotation: 'weekly',
				handoff_time: '09:00',
				start_date: '2026-10-19',
				participants: [B, A],
				restrictions: [{ weekdays: [5, 1], from: '09:00', to: '17:00' }]
			}
		});
	});

	it('drops rotation_hours for daily and weekly, keeps it for custom', () => {
		const daily = schedulePayloadFromForm(form(weekly.map(([k, v]) => [k, k === 'rotation' ? 'daily' : v])));
		expect(daily.ok && 'rotation_hours' in daily.payload).toBe(false);
		const custom = schedulePayloadFromForm(form(weekly.map(([k, v]) => [k, k === 'rotation' ? 'custom' : v])));
		expect(custom).toMatchObject({ ok: true, payload: { rotation: 'custom', rotation_hours: 12 } });
	});

	it('asks for the length of a custom rotation', () => {
		for (const hours of ['', '0', '8761', '1.5', 'x']) {
			const fields = weekly.map(([k, v]): [string, string] => [k, k === 'rotation' ? 'custom' : k === 'rotation_hours' ? hours : v]);
			expect(schedulePayloadFromForm(form(fields)), hours).toEqual({ ok: false, message: 'Enter the rotation length in hours (1-8760)' });
		}
	});

	it('refuses duplicate participants', () => {
		expect(schedulePayloadFromForm(form([...weekly, ['participant', A]]))).toMatchObject({ ok: false });
	});

	it('refuses a participant that is not a UUID', () => {
		expect(schedulePayloadFromForm(form([...weekly, ['participant', '../x']]))).toMatchObject({ ok: false });
	});

	it('refuses a schedule without participants', () => {
		expect(schedulePayloadFromForm(form(weekly.filter(([k]) => k !== 'participant')))).toMatchObject({ ok: false });
	});

	it('refuses a restriction with no weekday', () => {
		const fields = weekly.filter(([k]) => k !== 'restriction_0_weekday');
		expect(schedulePayloadFromForm(form(fields))).toEqual({ ok: false, message: 'Pick at least one day for restriction 1.' });
	});

	it('sends no restrictions when there are none, and refuses more than 14', () => {
		const none = weekly.filter(([k]) => !k.startsWith('restriction'));
		expect(schedulePayloadFromForm(form(none))).toMatchObject({ ok: true, payload: { restrictions: [] } });
		const many = weekly.map(([k, v]): [string, string] => [k, k === 'restriction_count' ? '15' : v]);
		expect(schedulePayloadFromForm(form(many))).toMatchObject({ ok: false });
	});
});

describe('overridePayloadFromForm', () => {
	it('converts the times from the schedule zone to UTC', () => {
		const fd = form([['user_id', A], ['starts_at', '2026-10-19T09:00'], ['ends_at', '2026-10-20T09:00']]);
		expect(overridePayloadFromForm(fd, 'Europe/Berlin')).toEqual({
			ok: true,
			payload: { user_id: A, starts_at: '2026-10-19T07:00:00.000Z', ends_at: '2026-10-20T07:00:00.000Z' }
		});
	});

	it('refuses an override that ends before it starts', () => {
		const fd = form([['user_id', A], ['starts_at', '2026-10-20T09:00'], ['ends_at', '2026-10-19T09:00']]);
		expect(overridePayloadFromForm(fd, 'UTC')).toMatchObject({ ok: false });
	});

	it('refuses a time in a skipped hour with the DST message', () => {
		const fd = form([['user_id', A], ['starts_at', '2026-03-29T02:30'], ['ends_at', '2026-03-29T09:00']]);
		expect(overridePayloadFromForm(fd, 'Europe/Berlin')).toEqual({
			ok: false,
			message: 'That time does not exist in Europe/Berlin (daylight saving change)'
		});
	});

	it('refuses a user that is not a UUID and a missing time', () => {
		expect(overridePayloadFromForm(form([['user_id', 'x'], ['starts_at', '2026-10-19T09:00'], ['ends_at', '2026-10-20T09:00']]), 'UTC')).toMatchObject({ ok: false });
		expect(overridePayloadFromForm(form([['user_id', A], ['starts_at', ''], ['ends_at', '2026-10-20T09:00']]), 'UTC')).toMatchObject({ ok: false });
	});
});
