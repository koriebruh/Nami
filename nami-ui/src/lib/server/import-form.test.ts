import { describe, expect, it } from 'vitest';
import { ENCODED_TOO_LARGE, FILE_TOO_LARGE, MAX_IMPORT_BYTES, importPayloadFromForm } from './import-form';

function form(fields: Record<string, string | File>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('importPayloadFromForm', () => {
	it('builds the engine body from the textarea, defaults and flags', async () => {
		const result = await importPayloadFromForm(
			form({ format: 'urls', content: 'https://a.test/', interval_seconds: '120', timeout_seconds: ' ', group_id: 'g1', skip_existing: 'on' }),
			true
		);
		expect(result).toEqual({
			ok: true,
			payload: { format: 'urls', content: 'https://a.test/', defaults: { interval_seconds: 120, group_id: 'g1' }, skip_existing: true, dry_run: true }
		});
	});

	it('sends skip_existing false when the box is unchecked', async () => {
		const result = await importPayloadFromForm(form({ format: 'urls', content: 'https://a.test/' }), false);
		expect(result).toMatchObject({ ok: true, payload: { skip_existing: false, dry_run: false } });
	});

	it('refuses an unknown or missing format', async () => {
		expect(await importPayloadFromForm(form({ format: 'xml', content: 'x' }), true)).toEqual({ ok: false, message: 'Choose a format' });
		expect(await importPayloadFromForm(form({ content: 'x' }), true)).toEqual({ ok: false, message: 'Choose a format' });
	});

	it('refuses empty content', async () => {
		expect(await importPayloadFromForm(form({ format: 'urls', content: '  \n' }), true)).toEqual({
			ok: false,
			message: 'Paste the list or choose a file'
		});
	});

	it('prefers a chosen file over the textarea', async () => {
		const file = new File(['https://file.test/'], 'list.txt');
		const result = await importPayloadFromForm(form({ format: 'urls', content: 'https://paste.test/', file }), true);
		expect(result).toMatchObject({ ok: true, payload: { content: 'https://file.test/' } });
	});

	it('falls back to the textarea for an empty file field', async () => {
		const file = new File([], '');
		const result = await importPayloadFromForm(form({ format: 'urls', content: 'https://paste.test/', file }), true);
		expect(result).toMatchObject({ ok: true, payload: { content: 'https://paste.test/' } });
	});

	it('refuses an oversized file by its size, without reading it', async () => {
		let read = false;
		const file = new File(['x'], 'big.txt');
		Object.defineProperty(file, 'size', { value: MAX_IMPORT_BYTES + 1 });
		file.text = async () => {
			read = true;
			return 'x';
		};
		expect(await importPayloadFromForm(form({ format: 'urls', file }), true)).toEqual({ ok: false, message: FILE_TOO_LARGE });
		expect(read).toBe(false);
	});

	it('refuses pasted content over 2 MiB', async () => {
		const content = 'a'.repeat(MAX_IMPORT_BYTES + 1);
		expect(await importPayloadFromForm(form({ format: 'urls', content }), true)).toEqual({ ok: false, message: FILE_TOO_LARGE });
	});

	it('accepts 2 MiB of content whose escaped body fits the engine limit', async () => {
		const content = '"'.repeat(MAX_IMPORT_BYTES);
		expect(await importPayloadFromForm(form({ format: 'urls', content }), true)).toMatchObject({ ok: true });
	});

	it('refuses content that is under 2 MiB but too large once JSON-encoded', async () => {
		// A control character is 6 bytes as \u00XX.
		const content = '\u0001'.repeat(MAX_IMPORT_BYTES / 2);
		expect(await importPayloadFromForm(form({ format: 'urls', content }), true)).toEqual({ ok: false, message: ENCODED_TOO_LARGE });
	});

	it('names the default that is not a whole number', async () => {
		expect(await importPayloadFromForm(form({ format: 'urls', content: 'x', interval_seconds: 'abc' }), true)).toEqual({
			ok: false,
			message: 'interval_seconds must be a whole number'
		});
	});
});
