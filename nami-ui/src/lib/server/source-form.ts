import { MIB, PLATFORMS, type Platform } from '#lib/logs';
import { formText, formWholeNumber, type FormResult } from './monitor-form';

/**
 * Source form -> nami-engine JSON body. The daily quota is entered in MiB;
 * blank means unlimited (`null`, which also clears a quota on edit).
 */
export function sourcePayloadFromForm(form: FormData): FormResult {
	const payload: Record<string, unknown> = { name: formText(form, 'name') };
	const platform = formText(form, 'platform');
	if (platform !== '') {
		if (!PLATFORMS.includes(platform as Platform)) return { ok: false, message: 'Pick a platform from the list.' };
		payload.platform = platform;
	}
	const retention = formWholeNumber(form, 'retention_days');
	if (retention === 'invalid') return { ok: false, message: 'Retention must be a whole number of days.' };
	if (retention !== undefined) payload.retention_days = retention;
	const quota = formText(form, 'daily_quota_mib');
	if (quota === '') {
		payload.daily_byte_quota = null;
	} else {
		const mib = Number(quota);
		if (!Number.isFinite(mib) || mib < 1) {
			return { ok: false, message: 'The daily quota must be at least 1 MiB (leave it blank for unlimited).' };
		}
		payload.daily_byte_quota = Math.round(mib * MIB);
	}
	return { ok: true, payload };
}
