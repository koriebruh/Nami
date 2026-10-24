import { describe, expect, it } from 'vitest';
import { describeRule, draftFromRule, emptyDraft, parseSampleEvents, ruleFromDraft, type Rule } from './pipeline';

describe('rule drafts', () => {
	it('round-trip every kind', () => {
		const rules: Rule[] = [
			{ type: 'drop', match: { field: 'level', equals: 'debug' } },
			{ type: 'drop', match: { message_regex: '^healthz' } },
			{ type: 'remove_fields', paths: ['user.email', 'password'] },
			{ type: 'redact', keys: ['password', 'token'], regex: 'sk_[a-z0-9]+' },
			{ type: 'rename', from: 'msg', to: 'message' },
			{ type: 'sample', n: 10 },
			{ type: 'sample', n: 2, match: { field: 'level', equals: 'info' } }
		];
		for (const rule of rules) expect(ruleFromDraft(draftFromRule(rule))).toEqual({ ok: true, rule });
	});

	it('keep a whole-number level as a number, so a pino level still matches after a re-save', () => {
		const level: Rule = { type: 'drop', match: { field: 'level', equals: 50 } };
		expect(ruleFromDraft(draftFromRule(level))).toEqual({ ok: true, rule: level });
		const status = ruleFromDraft({ ...emptyDraft('drop'), field: 'http.status', equals: '200' });
		expect(status).toEqual({ ok: true, rule: { type: 'drop', match: { field: 'http.status', equals: '200' } } });
	});

	it('reject what the engine would reject, with a readable message', () => {
		const cases: [Partial<ReturnType<typeof emptyDraft>> & { type: Rule['type'] }, string][] = [
			[{ type: 'drop', matchMode: 'none' }, 'match'],
			[{ type: 'drop', matchMode: 'field', field: 'a..b', equals: 'x' }, 'path'],
			[{ type: 'redact' }, 'keys or a regex'],
			[{ type: 'redact', regex: 'a'.repeat(257) }, '256'],
			[{ type: 'sample', n: '1' }, '2'],
			[{ type: 'sample', n: '10001' }, '10000'],
			[{ type: 'remove_fields', paths: 'level' }, 'level'],
			[{ type: 'rename', from: '', to: 'x' }, 'from']
		];
		for (const [patch, needle] of cases) {
			const result = ruleFromDraft({ ...emptyDraft(patch.type), ...patch });
			expect(result.ok, JSON.stringify(patch)).toBe(false);
			if (!result.ok) expect(result.message.toLowerCase()).toContain(needle);
		}
	});

	it('describes rules in one line', () => {
		expect(describeRule({ type: 'sample', n: 10 })).toBe('Keep 1 in 10 events');
		expect(describeRule({ type: 'drop', match: { field: 'level', equals: 'debug' } })).toBe('Drop when level = debug');
	});
});

describe('sample events', () => {
	it('accept an array, one object or NDJSON', () => {
		expect(parseSampleEvents('[{"a":1},{"b":2}]')).toEqual({ ok: true, events: [{ a: 1 }, { b: 2 }] });
		expect(parseSampleEvents('{"a":1}')).toEqual({ ok: true, events: [{ a: 1 }] });
		expect(parseSampleEvents('{"a":1}\n\n{"b":2}\n')).toEqual({ ok: true, events: [{ a: 1 }, { b: 2 }] });
	});
	it('reject non-objects, bad JSON and more than 100 events', () => {
		expect(parseSampleEvents('[1]').ok).toBe(false);
		expect(parseSampleEvents('{nope').ok).toBe(false);
		expect(parseSampleEvents('').ok).toBe(false);
		expect(parseSampleEvents(JSON.stringify(Array(101).fill({ a: 1 }))).ok).toBe(false);
	});
});
