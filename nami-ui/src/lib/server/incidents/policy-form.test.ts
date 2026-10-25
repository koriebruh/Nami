import { describe, expect, it } from 'vitest';
import { addRouting, policyPayloadFromForm, policyToFormState, targetChoices } from './policy-form';

const U = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const S = '7d1e2f3a-4b5c-4d6e-8f70-8192a3b4c5d6';

function form(entries: [string, string][]) {
	const f = new FormData();
	for (const [k, v] of entries) f.append(k, v);
	return f;
}

describe('policyPayloadFromForm', () => {
	it('builds steps with targets and an optional condition', () => {
		const result = policyPayloadFromForm(form([
			['name', ' Business hours '], ['repeat_count', '2'], ['fallback_policy_id', ''], ['step_count', '2'],
			['step_0_delay', '0'], ['step_0_target', `user:${U}`], ['step_0_target', 'all_members'],
			['step_0_condition', 'on'], ['step_0_weekday', '1'], ['step_0_weekday', '5'],
			['step_0_from', '09:00'], ['step_0_to', '17:00'], ['step_0_timezone', 'Asia/Jakarta'],
			['step_1_delay', '15'], ['step_1_target', `schedule:${S}`]
		]));
		expect(result).toEqual({ ok: true, payload: { name: 'Business hours', repeat_count: 2, fallback_policy_id: null, steps: [
			{ delay_minutes: 0, targets: [{ type: 'user', id: U }, { type: 'all_members' }],
			  condition: { weekdays: [1, 5], from: '09:00', to: '17:00', timezone: 'Asia/Jakarta' } },
			{ delay_minutes: 15, targets: [{ type: 'schedule', id: S }], condition: null }
		] } });
	});

	it('maps channel targets to project_channel and keeps a fallback id', () => {
		const result = policyPayloadFromForm(form([
			['name', 'x'], ['fallback_policy_id', S], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', `channel:${U}`]
		]));
		expect(result).toEqual({ ok: true, payload: { name: 'x', repeat_count: 0, fallback_policy_id: S, steps: [
			{ delay_minutes: 0, targets: [{ type: 'project_channel', id: U }], condition: null }
		] } });
	});

	// Adaptation: the plan's `as [string, string][][]` cast is a type error
	// under `npm run check`, so the table is typed as [entries, message].
	it.each([
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0']], 'Step 1 needs at least one target'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '1441'], ['step_0_target', 'all_members']], 'delay'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'user:../../auth/me']], 'target'],
		[[['name', 'x'], ['step_count', '21']], '20'],
		[[['name', 'x'], ['repeat_count', '11'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members']], 'repeat'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members'], ['step_0_condition', 'on'], ['step_0_from', '9'], ['step_0_to', '17:00'], ['step_0_timezone', 'UTC']], 'HH:MM'],
		[[['name', ''], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members']], 'Name'],
		[[['name', 'x'], ['fallback_policy_id', '../x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members']], 'fallback'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members'], ['step_0_target', 'all_members']], 'once']
	] as [[string, string][], string][])('refuses %j', (entries, message) => {
		expect(policyPayloadFromForm(form(entries))).toMatchObject({ ok: false, message: expect.stringContaining(message) });
	});
});

describe('policyToFormState', () => {
	it('turns a stored policy into editor values, round-tripping the target values', () => {
		const state = policyToFormState({
			id: S, project_id: U, name: 'Main', repeat_count: 1, fallback_policy_id: null, is_default: true, updated_at: '',
			steps: [{ position: 0, delay_minutes: 5, condition: { weekdays: [6, 7], from: '22:00', to: '06:00', timezone: 'UTC' },
				targets: [{ type: 'project_channel', id: U, label: 'Ops (slack)' }, { type: 'all_members', label: 'All members' }] }]
		});
		expect(state).toEqual({ name: 'Main', repeat_count: 1, fallback_policy_id: '', steps: [{
			delay: 5, condition: true, weekdays: [6, 7], from: '22:00', to: '06:00', timezone: 'UTC',
			targets: [{ value: `channel:${U}`, label: 'Ops (slack)' }, { value: 'all_members', label: 'All members' }]
		}] });
	});
});

describe('targetChoices', () => {
	it('offers all members, then members, schedules and channels', () => {
		expect(targetChoices(
			[{ user_id: U, email: 'ann@b.test', project_role: 'member' }],
			[{ id: S, name: 'Primary' }],
			[{ id: U, name: 'Ops', kind: 'slack' }]
		)).toEqual([
			{ value: 'all_members', label: 'All members' },
			{ value: `user:${U}`, label: 'ann@b.test (member)' },
			{ value: `schedule:${S}`, label: 'Schedule: Primary' },
			{ value: `channel:${U}`, label: 'Channel: Ops (slack)' }
		]);
	});
});

describe('addRouting', () => {
	it('omits empty and missing values on create', () => {
		const payload = {};
		expect(addRouting(form([['escalation_policy_id', ''], ['severity_id', '']]), payload, 'create')).toBeNull();
		expect(payload).toEqual({});
	});

	it('sends the ids, and on edit null for empty (back to the project default)', () => {
		const set = {};
		addRouting(form([['escalation_policy_id', U], ['severity_id', S]]), set, 'create');
		expect(set).toEqual({ escalation_policy_id: U, severity_id: S });
		const cleared = {};
		addRouting(form([['escalation_policy_id', ''], ['severity_id', '']]), cleared, 'edit');
		expect(cleared).toEqual({ escalation_policy_id: null, severity_id: null });
		// A form without the selects keeps the stored values.
		const kept = {};
		addRouting(form([]), kept, 'edit');
		expect(kept).toEqual({});
	});

	it('refuses an id that is not a UUID', () => {
		expect(addRouting(form([['escalation_policy_id', '../x']]), {}, 'create')).toMatch(/policy/);
		expect(addRouting(form([['severity_id', 'high']]), {}, 'edit')).toMatch(/severity/);
	});
});
