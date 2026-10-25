import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { loaded } from '#lib/server/logs/log-alert-load';
import { policyCommand, policyPayloadFromForm, type PolicyView } from '#lib/server/incidents/policy-form';
import { isUuid } from '#lib/core/uuid';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const policies = await loaded<PolicyView[]>(
		await callEngine(event, enginePath`/projects/${event.params.id}/escalation-policies`),
		'escalation policies'
	);
	return { policies };
};

// Admins only: the page hides these forms from everyone else, and the
// engine's 403 still shows as the form message.
export const actions: Actions = {
	// The form sends a name and one step to all members (the default policy's
	// shape); the editor it opens changes the rest.
	create: async (event) => {
		const parsed = policyPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'create' as const, message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/escalation-policies`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not create the policy.');
			return fail(status, { ...data, action: 'create' as const });
		}
		const created: { id?: unknown } = await response.json();
		if (!isUuid(created.id)) return fail(502, { action: 'create' as const, message: 'Could not create the policy.' });
		throw redirect(303, enginePath`/projects/${event.params.id}/escalation/${created.id}`);
	},
	makeDefault: async (event) => {
		const form = await event.request.formData();
		return (await policyCommand(event, 'default', form.get('policy_id'), form)) ?? { action: 'default' as const, done: true, message: 'Default policy changed.' };
	},
	delete: async (event) => {
		const form = await event.request.formData();
		return (await policyCommand(event, 'delete', form.get('policy_id'), form)) ?? { action: 'delete' as const, done: true, message: 'Policy deleted.' };
	}
};
