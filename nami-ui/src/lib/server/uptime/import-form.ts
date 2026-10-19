// Turns the import form's FormData into nami-engine's JSON body for
// `POST /projects/{id}/monitors/import`. The engine caps the pasted content at
// 2 MiB; the BFF checks the same cap itself, before the work of reading a
// file or sending a body that would only come back as a bare 413.
//
// Size checks, in order:
// 1. a file's `size`, before `file.text()` (an oversized upload is never decoded);
// 2. the content's UTF-8 length (a textarea paste, or the decoded file);
// 3. the JSON body that would be sent: `content` travels as a JSON string, so
//    every `"`, `\` and newline costs two bytes and a control character six.
//    The engine accepts 2 x the content cap plus 64 KiB (MAX_IMPORT_REQUEST_BYTES).

import { formText, formWholeNumber, type FormResult } from './monitor-form';

export const MAX_IMPORT_BYTES = 2 * 1024 * 1024;
/** The engine's request-body cap: the escaped content plus the other fields. */
export const MAX_IMPORT_REQUEST_BYTES = 2 * MAX_IMPORT_BYTES + 64 * 1024;
export const IMPORT_FORMATS = ['urls', 'uptime_kuma', 'uptimerobot'] as const;
export type ImportFormat = (typeof IMPORT_FORMATS)[number];

export const FILE_TOO_LARGE = 'The file is larger than 2 MiB';
export const ENCODED_TOO_LARGE = 'The import is larger than 4 MiB once encoded; split it';

const DEFAULT_FIELDS = ['interval_seconds', 'timeout_seconds', 'confirmation_threshold'] as const;

const utf8Length = (text: string) => new TextEncoder().encode(text).length;

export async function importPayloadFromForm(form: FormData, dryRun: boolean): Promise<FormResult> {
	const format = formText(form, 'format');
	if (!(IMPORT_FORMATS as readonly string[]).includes(format)) {
		return { ok: false, message: 'Choose a format' };
	}

	let content: string;
	const file = form.get('file');
	if (file instanceof File && file.size > 0) {
		if (file.size > MAX_IMPORT_BYTES) return { ok: false, message: FILE_TOO_LARGE };
		content = await file.text();
	} else {
		content = String(form.get('content') ?? '');
	}
	if (content.trim() === '') return { ok: false, message: 'Paste the list or choose a file' };
	if (utf8Length(content) > MAX_IMPORT_BYTES) return { ok: false, message: FILE_TOO_LARGE };

	const defaults: Record<string, unknown> = {};
	for (const field of DEFAULT_FIELDS) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) defaults[field] = value;
	}
	const groupId = formText(form, 'group_id');
	if (groupId !== '') defaults.group_id = groupId;

	const payload = { format, content, defaults, skip_existing: form.has('skip_existing'), dry_run: dryRun };
	if (utf8Length(JSON.stringify(payload)) > MAX_IMPORT_REQUEST_BYTES) {
		return { ok: false, message: ENCODED_TOO_LARGE };
	}
	return { ok: true, payload };
}
