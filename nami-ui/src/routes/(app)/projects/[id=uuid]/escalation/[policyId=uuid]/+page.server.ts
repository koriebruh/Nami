import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { loaded } from '#lib/server/logs/log-alert-load';
import {
	policyCommand,
	policyPayloadFromForm,
	policyToFormState,
	targetChoices,
	type PolicyView
} from '#lib/server/incidents/policy-form';
import type { Channel } from '#lib/incidents/channels';
import type { Member, Schedule } from '#lib/incidents/oncall';
import type { Actions, PageServerLoad } from './$types';

type Params = { id: string; policyId: string };

function policyPath(params: Params): string {
	return enginePath`/projects/${params.id}/escalation-policies/${params.policyId}`;
}

export const load: PageServerLoad = async (event) => {
	const project = event.params.id;
	// Every list is required: a save replaces the whole policy, so the editor
	// must not open with pickers that silently failed to load.
	const [policy, schedules, channels, members, policies] = await Promise.all([
		callEngine(event, policyPath(event.params)).then((r) => loaded<PolicyView>(r, 'escalation policy')),
		callEngine(event, enginePath`/projects/${project}/schedules`).then((r) => loaded<Schedule[]>(r, 'schedules')),
		callEngine(event, enginePath`/projects/${project}/notification-channels`).then((r) => loaded<Channel[]>(r, 'channels')),
		callEngine(event, enginePath`/projects/${project}/members`).then((r) => loaded<Member[]>(r, 'members')),
		callEngine(event, enginePath`/projects/${project}/escalation-policies`).then((r) => loaded<PolicyView[]>(r, 'escalation policies'))
	]);
	return {
		policy,
		editor: policyToFormState(policy),
		choices: targetChoices(members, schedules, channels),
		// The fallback: any other policy of the project, never this one.
		fallbacks: policies.filter((p) => p.id !== policy.id).map(({ id, name }) => ({ id, name }))
	};
};

// Admins only: the page hides these forms from everyone else, and the
// engine's 403 still shows as the form message.
export const actions: Actions = {
	save: async (event) => {
		const parsed = policyPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'save' as const, message: parsed.message });
		const response = await callEngine(event, policyPath(event.params), { method: 'PUT', body: JSON.stringify(parsed.payload) });
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not save the policy.');
			return fail(status, { ...data, action: 'save' as const });
		}
		return { action: 'save' as const, saved: true, message: 'Policy saved.' };
	},
	makeDefault: async (event) => {
		const form = await event.request.formData();
		return (await policyCommand(event, 'default', event.params.policyId, form)) ?? { action: 'default' as const, saved: true, message: 'This is now the default policy.' };
	},
	delete: async (event) => {
		const form = await event.request.formData();
		const result = await policyCommand(event, 'delete', event.params.policyId, form);
		if (result) return result;
		throw redirect(303, enginePath`/projects/${event.params.id}/escalation`);
	}
};
