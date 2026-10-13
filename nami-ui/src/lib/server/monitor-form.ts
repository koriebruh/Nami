// Turns the monitor form's FormData into nami-engine's JSON body. Only the
// fields of the selected check type are sent; numbers must be whole. The
// engine still validates everything — this only shapes the request and
// catches non-numeric input before the round trip.

export type FormResult =
	| { ok: true; payload: Record<string, unknown> }
	| { ok: false; message: string };

const COMMON_NUMBERS = ['interval_seconds', 'timeout_seconds', 'confirmation_threshold'] as const;
const HTTP_NUMBERS = ['expected_status_min', 'expected_status_max'] as const;

export function formText(form: FormData, field: string): string {
	return String(form.get(field) ?? '').trim();
}

/** `undefined` when blank (the engine applies its default). */
export function formWholeNumber(form: FormData, field: string): number | undefined | 'invalid' {
	const raw = formText(form, field);
	if (raw === '') return undefined;
	return /^-?\d+$/.test(raw) ? Number(raw) : 'invalid';
}

export function monitorPayloadFromForm(form: FormData): FormResult {
	const checkType = formText(form, 'check_type');
	const payload: Record<string, unknown> = { name: formText(form, 'name'), check_type: checkType };

	const numbers: string[] = [...COMMON_NUMBERS];
	if (checkType === 'tcp') {
		payload.host = formText(form, 'host');
		numbers.push('port');
	} else {
		payload.url = formText(form, 'url');
		payload.http_method = formText(form, 'http_method') || 'GET';
		numbers.push(...HTTP_NUMBERS);
		if (checkType === 'keyword') {
			// Not trimmed: leading/trailing spaces can be part of the keyword.
			payload.keyword = String(form.get('keyword') ?? '');
			payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
		}
	}

	for (const field of numbers) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	return { ok: true, payload };
}
