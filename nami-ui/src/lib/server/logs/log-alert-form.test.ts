import { describe, expect, it } from 'vitest';
import { logAlertPayloadFromForm } from './log-alert-form';

const SOURCE = 'c13bc10a-d17f-443b-8976-c6d9b54e2798';

function form(fields: [string, string][]) {
	const data = new FormData();
	for (const [name, value] of fields) data.append(name, value);
	return data;
}

const base: [string, string][] = [
	['name', 'Checkout errors'], ['threshold', '10'], ['comparison', 'gt'], ['window_seconds', '300'], ['interval_seconds', '60'], ['enabled', 'on']
];

describe('logAlertPayloadFromForm', () => {
	it('builds the engine body, query included', () => {
		const result = logAlertPayloadFromForm(form([...base, ['q', 'disk'], ['level', 'error'], ['level', 'fatal'], ['source', SOURCE],
			['filter', '{"path":"http.status","op":"gte","value":"500"}'], ['pattern', '42']]));
		expect(result).toEqual({ ok: true, payload: {
			name: 'Checkout errors', threshold: 10, comparison: 'gt', window_seconds: 300, interval_seconds: 60, enabled: true,
			query: { q: 'disk', levels: ['error', 'fatal'], source_ids: [SOURCE], filters: [{ path: 'http.status', op: 'gte', value: '500' }], pattern_hashes: ['42'] }
		} });
		const disabled = logAlertPayloadFromForm(form(base.filter(([k]) => k !== 'enabled')));
		expect(disabled.ok && disabled.payload.enabled).toBe(false);
	});
	it('rejects what the engine would reject', () => {
		const set = (name: string, value: string) => form(base.map(([k, v]) => [k, k === name ? value : v] as [string, string]));
		for (const [name, value] of [['name', ''], ['threshold', '-1'], ['threshold', '1.5'], ['comparison', 'eq'],
			['window_seconds', '59'], ['window_seconds', '86401'], ['interval_seconds', '30']]) {
			expect(logAlertPayloadFromForm(set(name, value)).ok, `${name}=${value}`).toBe(false);
		}
		expect(logAlertPayloadFromForm(set('comparison', 'lt')).ok).toBe(true);
		expect(logAlertPayloadFromForm(form([...base.filter(([k]) => k !== 'threshold'), ['threshold', '0'], ['comparison', 'lt']])).ok).toBe(false);
		expect(logAlertPayloadFromForm(form([...base, ['source', 'not-a-uuid']])).ok).toBe(false);
		expect(logAlertPayloadFromForm(form([...base, ['filter', '{nope']])).ok).toBe(false);
	});
});
