// Escalation policy editor form -> nami-engine JSON body, plus the policy and
// severity selects of the monitor, heartbeat and log-alert forms. The engine
// validates everything again (same-project ids, zones, windows) and its
// message is shown; these checks catch what would otherwise be a bare 422 or
// junk in a request.
import { fail, type RequestEvent } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { isUuid } from '#lib/core/uuid';
import {
	MAX_DELAY_MINUTES,
	MAX_REPEAT_COUNT,
	MAX_STEPS,
	MAX_TARGETS_PER_STEP,
	type StepCondition,
	type Target,
	type TargetChoice
} from '#lib/incidents/escalation';
import type { PolicyOption, Severity } from '#lib/incidents/incidents';
import type { Member } from '#lib/incidents/oncall';

export { MAX_STEPS, policyToFormState, type EditorState, type PolicyView, type TargetChoice } from '#lib/incidents/escalation';

type Step = { delay_minutes: number; targets: Target[]; condition: StepCondition | null };
export type PolicyPayload = { name: string; repeat_count: number; fallback_policy_id: string | null; steps: Step[] };

const text = (form: FormData, name: string) => String(form.get(name) ?? '').trim();
const HH_MM = /^([01]\d|2[0-3]):[0-5]\d$/;

function parseTarget(value: string): Target | null {
	if (value === 'all_members') return { type: 'all_members' };
	const m = /^(user|schedule|channel):(.*)$/.exec(value);
	if (!m || !isUuid(m[2])) return null;
	return m[1] === 'channel' ? { type: 'project_channel', id: m[2] } : { type: m[1] as 'user' | 'schedule', id: m[2] };
}

export function policyPayloadFromForm(
	form: FormData
): { ok: true; payload: PolicyPayload } | { ok: false; message: string } {
	const fail = (message: string) => ({ ok: false as const, message });
	const name = text(form, 'name');
	if (name === '') return fail('Name is required.');
	const repeat = text(form, 'repeat_count') || '0';
	if (!/^\d{1,2}$/.test(repeat) || Number(repeat) > MAX_REPEAT_COUNT) {
		return fail(`The policy can repeat 0 to ${MAX_REPEAT_COUNT} times.`);
	}
	const fallback = text(form, 'fallback_policy_id');
	if (fallback !== '' && !isUuid(fallback)) return fail('Pick the fallback policy from the list.');
	const countText = text(form, 'step_count');
	const count = /^\d{1,3}$/.test(countText) ? Number(countText) : 0;
	if (count < 1 || count > MAX_STEPS) return fail(`A policy has 1 to ${MAX_STEPS} steps.`);

	const steps: Step[] = [];
	for (let i = 0; i < count; i++) {
		const n = i + 1;
		const delay = text(form, `step_${i}_delay`);
		if (!/^\d{1,4}$/.test(delay) || Number(delay) > MAX_DELAY_MINUTES) {
			return fail(`Step ${n}: the delay must be 0 to ${MAX_DELAY_MINUTES} minutes.`);
		}
		const values = form.getAll(`step_${i}_target`).map((v) => String(v).trim());
		if (values.length === 0) return fail(`Step ${n} needs at least one target`);
		if (values.length > MAX_TARGETS_PER_STEP) return fail(`Step ${n}: at most ${MAX_TARGETS_PER_STEP} targets.`);
		if (new Set(values).size !== values.length) return fail(`Step ${n}: each target can be listed once.`);
		const targets = values.map(parseTarget);
		if (targets.some((t) => t === null)) return fail(`Step ${n}: pick each target from the list.`);

		let condition: StepCondition | null = null;
		if (form.get(`step_${i}_condition`) === 'on') {
			const from = text(form, `step_${i}_from`);
			const to = text(form, `step_${i}_to`);
			if (!HH_MM.test(from) || !HH_MM.test(to)) return fail(`Step ${n}: times must be HH:MM.`);
			const weekdays = form.getAll(`step_${i}_weekday`).map(String);
			if (weekdays.length === 0) return fail(`Step ${n}: pick at least one day.`);
			if (!weekdays.every((d) => /^[1-7]$/.test(d))) return fail(`Step ${n}: unknown day.`);
			const timezone = text(form, `step_${i}_timezone`);
			if (timezone === '') return fail(`Step ${n}: enter a time zone.`);
			condition = { weekdays: weekdays.map(Number), from, to, timezone };
		}
		steps.push({ delay_minutes: Number(delay), targets: targets as Target[], condition });
	}
	return { ok: true, payload: { name, repeat_count: Number(repeat), fallback_policy_id: fallback || null, steps } };
}

/** The editor's target picker: all members, then members, schedules and project channels. */
export function targetChoices(
	members: Member[],
	schedules: { id: string; name: string }[],
	channels: { id: string; name: string; kind: string }[]
): TargetChoice[] {
	return [
		{ value: 'all_members', label: 'All members' },
		...members.map((m) => ({ value: `user:${m.user_id}`, label: m.email })),
		...schedules.map((s) => ({ value: `schedule:${s.id}`, label: `Schedule: ${s.name}` })),
		...channels.map((c) => ({ value: `channel:${c.id}`, label: `Channel: ${c.name} (${c.kind})` }))
	];
}

/**
 * The `escalation_policy_id` and `severity_id` selects of a source form
 * ('' = the project default). Create omits an empty one; edit sends null for
 * it. A form without the select (field absent) leaves the stored value
 * alone. Returns the message for a value that is not a UUID, else null.
 */
export function addRouting(form: FormData, body: object, mode: 'create' | 'edit'): string | null {
	const payload = body as Record<string, unknown>;
	for (const [field, what] of [['escalation_policy_id', 'escalation policy'], ['severity_id', 'severity']] as const) {
		if (!form.has(field)) continue;
		const value = text(form, field);
		if (value === '') {
			if (mode === 'edit') payload[field] = null;
		} else if (isUuid(value)) payload[field] = value;
		else return `Pick the ${what} from the list.`;
	}
	return null;
}

async function optionalList<T>(event: RequestEvent, path: string): Promise<T[]> {
	try {
		const response = await callEngine(event, path);
		if (!response.ok) return [];
		const body: unknown = await response.json();
		return Array.isArray(body) ? (body as T[]) : [];
	} catch {
		return [];
	}
}

/** The choices of the source forms' policy and severity selects. They only
 *  fill the selects, so a failure leaves them empty and the page opens. */
export async function loadRoutingChoices(event: RequestEvent): Promise<{ policies: PolicyOption[]; severities: Severity[] }> {
	const [policies, severities] = await Promise.all([
		optionalList<PolicyOption>(event, enginePath`/projects/${event.params.id}/escalation-policies`),
		optionalList<Severity>(event, enginePath`/projects/${event.params.id}/severities`)
	]);
	return {
		policies: policies.map(({ id, name, is_default }) => ({ id, name, is_default })),
		severities
	};
}

/**
 * "Make default" and the two-step delete, shared by the policy list and the
 * editor. A delete without `confirm=yes` asks first (the button asks with
 * confirm(); without JavaScript the page asks). `null`: done.
 */
export async function policyCommand(event: RequestEvent, action: 'default' | 'delete', policyId: unknown, form: FormData) {
	if (!isUuid(policyId)) return fail(400, { action, message: 'Unknown policy.' });
	if (action === 'delete' && form.get('confirm') !== 'yes') return { action, confirmDelete: policyId };
	const path = enginePath`/projects/${event.params.id}/escalation-policies/${policyId}`;
	const response =
		action === 'default'
			? await callEngine(event, `${path}/default`, { method: 'POST' })
			: await callEngine(event, path, { method: 'DELETE' });
	if (response.ok) return null;
	// A delete's 409 says why: the default policy cannot be deleted.
	const { status, data } = await engineFailure(response, action === 'default' ? 'Could not change the default.' : 'Could not delete the policy.');
	return fail(status, { ...data, action });
}
