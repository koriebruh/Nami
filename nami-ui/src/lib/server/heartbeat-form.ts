import { formText, formWholeNumber, type FormResult } from './monitor-form';

/** Heartbeat form -> nami-engine JSON body (name, period, grace). */
export function heartbeatPayloadFromForm(form: FormData): FormResult {
	const payload: Record<string, unknown> = { name: formText(form, 'name') };
	for (const field of ['period_seconds', 'grace_seconds']) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	return { ok: true, payload };
}
