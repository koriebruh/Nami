import { describe, expect, it } from 'vitest';
import { monitorPayloadFromForm } from './monitor-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('monitorPayloadFromForm', () => {
	it('sends http fields and parsed numbers for an http monitor', () => {
		const result = monitorPayloadFromForm(
			form({
				name: ' API ',
				check_type: 'http',
				url: ' https://example.com ',
				http_method: 'HEAD',
				expected_status_min: '200',
				expected_status_max: '299',
				interval_seconds: '30',
				timeout_seconds: '5',
				confirmation_threshold: '3',
				host: 'ignored',
				keyword: 'ignored'
			})
		);
		expect(result).toEqual({
			ok: true,
			payload: {
				name: 'API',
				check_type: 'http',
				url: 'https://example.com',
				http_method: 'HEAD',
				expected_status_min: 200,
				expected_status_max: 299,
				interval_seconds: 30,
				timeout_seconds: 5,
				confirmation_threshold: 3
			}
		});
	});

	it('sends host and port, never a url, for a tcp monitor', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'DB', check_type: 'tcp', host: 'db.internal', port: '5432', url: 'https://stale.test' })
		);
		expect(result).toEqual({ ok: true, payload: { name: 'DB', check_type: 'tcp', host: 'db.internal', port: 5432 } });
	});

	it('keeps the keyword untrimmed and defaults the mode', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'Home', check_type: 'keyword', url: 'https://example.com', keyword: ' Welcome ' })
		);
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.payload.keyword).toBe(' Welcome ');
		expect(result.payload.keyword_mode).toBe('contains');
	});

	it('omits blank numbers so the engine defaults apply', () => {
		const result = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', interval_seconds: '' }));
		expect(result.ok).toBe(true);
		if (!result.ok) return;
		expect(result.payload).not.toHaveProperty('interval_seconds');
	});

	it('rejects non-whole numbers', () => {
		expect(monitorPayloadFromForm(form({ name: 'API', check_type: 'http', interval_seconds: '1.5' }))).toEqual({
			ok: false,
			message: 'interval_seconds must be a whole number'
		});
		expect(monitorPayloadFromForm(form({ name: 'DB', check_type: 'tcp', port: 'abc' }))).toEqual({
			ok: false,
			message: 'port must be a whole number'
		});
	});
});
