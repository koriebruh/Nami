import { describe, expect, it } from 'vitest';
import { utcFromLocalInput, windowPayloadFromForm } from './maintenance-form';

function form(fields: Record<string, string | string[]>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) {
		for (const item of Array.isArray(value) ? value : [value]) data.append(key, item);
	}
	return data;
}

describe('windowPayloadFromForm', () => {
	it('reads one-off times as UTC', () => {
		expect(
			windowPayloadFromForm(
				form({ title: ' DB upgrade ', kind: 'once', monitor_id: '', starts_at: '2026-10-13T22:00', ends_at: '2026-10-14T02:00' })
			)
		).toEqual({
			ok: true,
			payload: { title: 'DB upgrade', kind: 'once', starts_at: '2026-10-13T22:00:00Z', ends_at: '2026-10-14T02:00:00Z' }
		});
	});

	it('sends the weekly days, times and zone', () => {
		expect(
			windowPayloadFromForm(
				form({
					title: 'Deploys',
					kind: 'weekly',
					monitor_id: 'm-1',
					days_of_week: ['2', '4'],
					start_time: '22:00',
					end_time: '02:00',
					timezone: 'Asia/Jakarta',
					starts_at: '2026-10-13T22:00'
				})
			)
		).toEqual({
			ok: true,
			payload: {
				title: 'Deploys',
				kind: 'weekly',
				monitor_id: 'm-1',
				days_of_week: [2, 4],
				start_time: '22:00',
				end_time: '02:00',
				timezone: 'Asia/Jakarta'
			}
		});
	});

	it('on edit sends a cleared scope as null (the whole project)', () => {
		const result = windowPayloadFromForm(
			form({ title: 'x', kind: 'once', monitor_id: '', starts_at: '2026-10-13T22:00', ends_at: '2026-10-13T23:00' }),
			'edit'
		);
		expect(result.ok && result.payload.monitor_id).toBeNull();
	});

	it('sends a blank weekly time as typed so the engine names the problem', () => {
		const result = windowPayloadFromForm(
			form({ title: 'x', kind: 'weekly', days_of_week: ['1', 'x', '9.5'], start_time: '', end_time: '02:00', timezone: 'Nope/Zone' })
		);
		expect(result).toEqual({
			ok: true,
			payload: { title: 'x', kind: 'weekly', days_of_week: [1], start_time: '', end_time: '02:00', timezone: 'Nope/Zone' }
		});
	});

	it('asks for the missing parts', () => {
		expect(windowPayloadFromForm(form({ title: 'x', kind: 'once', starts_at: '2026-10-13T22:00', ends_at: '' }))).toEqual({
			ok: false,
			message: 'Enter a start and an end date and time'
		});
		expect(windowPayloadFromForm(form({ title: 'x', kind: 'weekly', start_time: '01:00', end_time: '02:00' }))).toEqual({
			ok: false,
			message: 'Pick at least one day'
		});
	});
});

describe('utcFromLocalInput', () => {
	it('accepts datetime-local values only', () => {
		expect(utcFromLocalInput('2026-10-13T22:00:30')).toBe('2026-10-13T22:00:30Z');
		expect(utcFromLocalInput('2026-10-13 22:00')).toBeNull();
		expect(utcFromLocalInput('2026-10-13T22:00Z')).toBeNull();
		expect(utcFromLocalInput('')).toBeNull();
		// Right shape, not a real date or time.
		expect(utcFromLocalInput('2026-13-45T25:99')).toBeNull();
		expect(utcFromLocalInput('2026-02-30T10:00')).toBeNull();
	});
});
