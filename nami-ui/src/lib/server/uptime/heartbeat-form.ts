import { addRouting } from '../incidents/policy-form';
import { formText, formWholeNumber, type FormMode, type FormResult } from './monitor-form';

/**
 * Heartbeat form -> nami-engine JSON body (name, period, grace, group, policy,
 * severity). A blank group is left out on create and sent as null on edit
 * (ungroup); policy and severity the same way (null: the project default).
 */
export function heartbeatPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult {
	const payload: Record<string, unknown> = { name: formText(form, 'name') };
	for (const field of ['period_seconds', 'grace_seconds']) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	const group = formText(form, 'group_id');
	if (group !== '') payload.group_id = group;
	else if (mode === 'edit') payload.group_id = null;
	const routingError = addRouting(form, payload, mode);
	if (routingError) return { ok: false, message: routingError };
	return { ok: true, payload };
}
