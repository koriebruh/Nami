import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath, redirectToLoginIfUnauthorized } from '#lib/server/core/engine';
import { monitorPayloadFromForm } from '#lib/server/uptime/monitor-form';
import type { MonitorGroup } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`);
	redirectToLoginIfUnauthorized(response);
	// The group picker is optional: offer "No group" alone if this fails.
	const groups: MonitorGroup[] = response.ok ? await response.json() : [];
	return { groups };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData(), 'create');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitors`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) return engineFailure(response, 'Could not create monitor.');
		const created: { id: string } = await response.json();
		throw redirect(303, enginePath`/projects/${event.params.id}/monitors/${created.id}`);
	}
};
