import { describe, expect, it } from 'vitest';
import { heartbeatPayloadFromForm } from './heartbeat-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('heartbeatPayloadFromForm', () => {
	it('parses name, period and grace', () => {
		expect(heartbeatPayloadFromForm(form({ name: ' Backup ', period_seconds: '86400', grace_seconds: '600' }))).toEqual({
			ok: true,
			payload: { name: 'Backup', period_seconds: 86400, grace_seconds: 600 }
		});
	});

	it('omits a blank grace so the engine default applies', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '60', grace_seconds: '' }))).toEqual({
			ok: true,
			payload: { name: 'Backup', period_seconds: 60 }
		});
	});

	it('rejects non-whole numbers', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '1h' }))).toEqual({
			ok: false,
			message: 'period_seconds must be a whole number'
		});
	});

	it('rejects a grace that is not a whole number, and a fractional period', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '60', grace_seconds: 'soon' }))).toEqual({
			ok: false,
			message: 'grace_seconds must be a whole number'
		});
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '1.5' }))).toEqual({
			ok: false,
			message: 'period_seconds must be a whole number'
		});
	});

	// The engine owns the limits (30 s to 31 days, grace not negative): a number
	// out of range is forwarded and refused there, with its own message.
	it('forwards a whole number outside the limits for the engine to refuse', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '5', grace_seconds: '-1' }))).toEqual({
			ok: true,
			payload: { name: 'Backup', period_seconds: 5, grace_seconds: -1 }
		});
	});

	it('leaves the period out when the field is blank', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup' }))).toEqual({ ok: true, payload: { name: 'Backup' } });
	});

	// Only name, period and grace are ever sent. A forged field (a token, a
	// paused flag, another heartbeat's id, the delete confirmation) never
	// reaches the engine through this form.
	it('sends nothing but the three fields it knows', () => {
		const result = heartbeatPayloadFromForm(
			form({
				name: 'Backup',
				period_seconds: '60',
				grace_seconds: '10',
				token: 'deadbeef',
				paused: 'true',
				id: '00000000-0000-0000-0000-000000000000',
				project_id: '00000000-0000-0000-0000-000000000000',
				confirm: 'yes'
			})
		);
		expect(result).toEqual({ ok: true, payload: { name: 'Backup', period_seconds: 60, grace_seconds: 10 } });
	});

	it('sends a group on create and omits a blank one', () => {
		const g = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
		expect(heartbeatPayloadFromForm(form({ name: 'B', period_seconds: '60', group_id: g }))).toEqual({
			ok: true,
			payload: { name: 'B', period_seconds: 60, group_id: g }
		});
		expect(heartbeatPayloadFromForm(form({ name: 'B', period_seconds: '60', group_id: '' }))).toEqual({
			ok: true,
			payload: { name: 'B', period_seconds: 60 }
		});
	});

	it('sends null for a blank group on edit, so the heartbeat is ungrouped', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'B', period_seconds: '60', group_id: '' }), 'edit')).toEqual({
			ok: true,
			payload: { name: 'B', period_seconds: 60, group_id: null }
		});
	});
});
