// Turns the monitor form's FormData into nami-engine's JSON body. Only the
// fields of the selected check type are sent; numbers must be whole. The
// engine still validates everything — this only shapes the request and
// catches malformed input before the round trip.
//
// Secrets (custom header values, the basic-auth password) are write-only:
// on create they are sent when filled in; on edit the form says what to do
// with the stored ones — keep (field omitted), replace (new value sent) or
// clear (null sent). Blank optional fields are omitted on create (engine
// defaults apply) and sent as null on edit (the stored value is cleared).
// The stored secrets cannot be kept when the URL moves to another origin or
// certificate checks are turned off (the engine refuses it too): the edit
// form renders what it needs to tell (`initial_url`, `initial_verify_ssl`,
// `stored_headers`, `stored_password`) as hidden fields.
//
// Blank numbers are omitted on edit, so the stored value stays (the form
// says so), except the expiry threshold, which null resets to the type's
// default.

import { IP_VERSION_TYPES, proxyMustBeReentered, secretsMustBeReentered, unescapePayload, type CheckType } from '../../uptime/uptime';
import { addRouting } from '../incidents/policy-form';

export type FormResult =
	| { ok: true; payload: Record<string, unknown> }
	| { ok: false; message: string };

export type FormMode = 'create' | 'edit';

const COMMON_NUMBERS = ['interval_seconds', 'timeout_seconds', 'confirmation_threshold', 'recovery_threshold'] as const;
const BODY_METHODS = ['POST', 'PUT', 'PATCH'];

export function formText(form: FormData, field: string): string {
	return String(form.get(field) ?? '').trim();
}

/** `undefined` when blank (the engine applies its default). */
export function formWholeNumber(form: FormData, field: string): number | undefined | 'invalid' {
	const raw = formText(form, field);
	if (raw === '') return undefined;
	return /^-?\d+$/.test(raw) ? Number(raw) : 'invalid';
}

/** "Name: value" per line; blank lines are skipped, a value is required. */
export function parseHeaderLines(
	text: string
): { ok: true; headers: { name: string; value: string }[] } | { ok: false; message: string } {
	const headers: { name: string; value: string }[] = [];
	for (const [index, line] of text.split(/\r?\n/).entries()) {
		if (line.trim() === '') continue;
		const colon = line.indexOf(':');
		if (colon <= 0 || line.slice(0, colon).trim() === '') {
			return { ok: false, message: `Header line ${index + 1} must look like "Name: value"` };
		}
		const value = line.slice(colon + 1).trim();
		// A "Name: " line left unfilled would store an empty secret.
		if (value === '') return { ok: false, message: `Header line ${index + 1} needs a value` };
		headers.push({ name: line.slice(0, colon).trim(), value });
	}
	return { ok: true, headers };
}

export function monitorPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult {
	const checkType = formText(form, 'check_type');
	const payload: Record<string, unknown> = { name: formText(form, 'name'), check_type: checkType };
	const optional = (field: string, value: string) => {
		if (value !== '') payload[field] = value;
		else if (mode === 'edit') payload[field] = null;
	};
	optional('group_id', formText(form, 'group_id'));

	if (IP_VERSION_TYPES.includes(checkType as CheckType)) payload.ip_version = formText(form, 'ip_version') || 'any';

	const numbers: string[] = [...COMMON_NUMBERS];
	switch (checkType) {
		case 'http':
		case 'keyword': {
			const error = addRequestConfig(form, mode, checkType, payload, optional);
			if (error) return { ok: false, message: error };
			numbers.push('expected_status_min', 'expected_status_max');
			break;
		}
		case 'tcp':
			payload.host = formText(form, 'host');
			numbers.push('port');
			break;
		case 'udp': {
			payload.host = formText(form, 'host');
			numbers.push('port');
			payload.udp_payload = unescapePayload(String(form.get('udp_payload') ?? ''));
			// Not trimmed: spaces can be part of the expected reply.
			const keyword = String(form.get('keyword') ?? '');
			if (keyword !== '') {
				payload.keyword = keyword;
				payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
			}
			break;
		}
		case 'ping':
			payload.host = formText(form, 'host');
			break;
		case 'dns':
			payload.host = formText(form, 'host');
			payload.dns_record_type = formText(form, 'dns_record_type') || 'A';
			optional('dns_nameserver', formText(form, 'dns_nameserver'));
			optional('expected_value', formText(form, 'expected_value'));
			break;
		case 'ssl_expiry':
			payload.host = formText(form, 'host');
			numbers.push('port', 'expiry_threshold_days');
			break;
		case 'domain_expiry':
			payload.host = formText(form, 'host');
			numbers.push('expiry_threshold_days');
			break;
		case 'smtp':
		case 'pop3':
		case 'imap':
			payload.host = formText(form, 'host');
			payload.use_tls = form.has('use_tls');
			numbers.push('port');
			break;
	}

	const switched = typeSwitchDefaults(form, mode, checkType);
	for (const field of numbers) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
		else if (field in switched) payload[field] = switched[field];
		else if (mode === 'edit' && field === 'expiry_threshold_days') payload[field] = null;
	}
	const routingError = addRouting(form, payload, mode);
	if (routingError) return { ok: false, message: routingError };
	return { ok: true, payload };
}

/** URL, method, body, redirects/TLS, basic auth and custom headers. */
function addRequestConfig(
	form: FormData,
	mode: FormMode,
	checkType: string,
	payload: Record<string, unknown>,
	optional: (field: string, value: string) => void
): string | null {
	payload.url = formText(form, 'url');
	const method = formText(form, 'http_method') || 'GET';
	payload.http_method = method;
	if (checkType === 'keyword') {
		// Not trimmed: leading/trailing spaces can be part of the keyword.
		payload.keyword = String(form.get('keyword') ?? '');
		payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
	}
	// The engine only accepts a body with POST/PUT/PATCH: switching the
	// method away clears a stored one.
	const body = String(form.get('request_body') ?? '');
	optional('request_body', BODY_METHODS.includes(method) ? body : '');
	payload.follow_redirects = form.has('follow_redirects');
	payload.verify_ssl = form.has('verify_ssl');
	optional('auth_username', formText(form, 'auth_username'));

	const headersText = String(form.get('request_headers') ?? '');
	const headersAction = mode === 'create' ? (headersText.trim() ? 'replace' : 'keep') : formText(form, 'headers_action');
	if (headersAction === 'replace') {
		const parsed = parseHeaderLines(headersText);
		if (!parsed.ok) return parsed.message;
		payload.request_headers = parsed.headers;
	} else if (headersAction === 'clear') {
		payload.request_headers = null;
	}

	const password = String(form.get('auth_password') ?? '');
	const passwordAction = mode === 'create' ? (password ? 'replace' : 'keep') : formText(form, 'password_action');
	if (passwordAction === 'replace') {
		if (password === '') return 'Enter the new password, or keep the current one';
		payload.auth_password = password;
	} else if (passwordAction === 'clear') {
		payload.auth_password = null;
	}

	payload.remember_cookies = form.has('remember_cookies');
	optional('proxy_url', formText(form, 'proxy_url'));
	optional('proxy_username', formText(form, 'proxy_username'));
	const proxyPassword = String(form.get('proxy_password') ?? '');
	const proxyAction = mode === 'create' ? (proxyPassword ? 'replace' : 'keep') : formText(form, 'proxy_password_action');
	if (proxyAction === 'replace') {
		if (proxyPassword === '') return 'Enter the new proxy password, or keep the current one';
		payload.proxy_password = proxyPassword;
	} else if (proxyAction === 'clear') {
		payload.proxy_password = null;
	}

	const keepsSecret =
		(headersAction === 'keep' && form.get('stored_headers') === 'on') ||
		(passwordAction === 'keep' && form.get('stored_password') === 'on');
	const keepsProxyPassword = proxyAction === 'keep' && form.get('stored_proxy_password') === 'on';
	if (mode === 'edit' && (keepsSecret || keepsProxyPassword)) {
		const proxyReason = proxyMustBeReentered({
			storedProxyUrl: formText(form, 'initial_proxy_url') || null,
			proxyUrl: formText(form, 'proxy_url')
		});
		if (proxyReason) return proxyReason;
	}
	if (mode === 'edit' && keepsSecret) {
		const reason = secretsMustBeReentered({
			storedUrl: formText(form, 'initial_url') || null,
			url: String(payload.url),
			storedVerifySsl: form.get('initial_verify_ssl') === 'on',
			verifySsl: payload.verify_ssl === true
		});
		if (reason) return reason;
	}
	return null;
}

const EXPIRY_TYPES = ['ssl_expiry', 'domain_expiry'];

/**
 * On edit, an empty field means "the default", but a PATCH that omits a field
 * keeps the stored value, which may not fit the new type (a 60 s interval on
 * a monitor turned into an expiry check). So when the form moved to another
 * kind of check than the monitor had (`initial_check_type`), the empty
 * fields are sent as the new type's defaults.
 */
function typeSwitchDefaults(form: FormData, mode: FormMode, checkType: string): Record<string, number> {
	const initial = formText(form, 'initial_check_type');
	if (mode !== 'edit' || initial === '' || initial === checkType) return {};
	const defaults: Record<string, number> = {};
	if (EXPIRY_TYPES.includes(initial) !== EXPIRY_TYPES.includes(checkType)) {
		const expiry = EXPIRY_TYPES.includes(checkType);
		defaults.interval_seconds = expiry ? 86400 : 60;
		defaults.confirmation_threshold = expiry ? 1 : 2;
	}
	if (checkType === 'ssl_expiry') defaults.expiry_threshold_days = 14;
	if (checkType === 'domain_expiry') defaults.expiry_threshold_days = 30;
	return defaults;
}
