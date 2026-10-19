import { describe, expect, it } from 'vitest';
import { sourcePayloadFromForm } from './source-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('sourcePayloadFromForm', () => {
	it('builds the engine body, converting the quota from MiB', () => {
		expect(
			sourcePayloadFromForm(
				form({ name: ' api ', platform: 'vector', retention_days: '90', daily_quota_mib: '1.5' })
			)
		).toEqual({
			ok: true,
			payload: { name: 'api', platform: 'vector', retention_days: 90, daily_byte_quota: 1572864 }
		});
	});

	it('sends null for a blank quota and omits blank optional fields', () => {
		expect(sourcePayloadFromForm(form({ name: 'api', daily_quota_mib: '' }))).toEqual({
			ok: true,
			payload: { name: 'api', daily_byte_quota: null }
		});
	});

	it('rejects bad values with a readable message', () => {
		const cases: Record<string, string>[] = [
			{ name: 'x', platform: 'fluentd' },
			{ name: 'x', retention_days: '7.5' },
			{ name: 'x', daily_quota_mib: '0.5' },
			{ name: 'x', daily_quota_mib: 'lots' }
		];
		for (const fields of cases) {
			const result = sourcePayloadFromForm(form(fields));
			expect(result.ok, JSON.stringify(fields)).toBe(false);
		}
	});
});
