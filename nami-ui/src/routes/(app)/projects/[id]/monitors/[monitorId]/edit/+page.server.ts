import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { Monitor } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	// A monitor id that is not a UUID is a 400 from the engine's path extractor:
	// still "no such monitor", not a server error.
	if (response.status === 404 || response.status === 400) throw error(404, 'Monitor not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load monitor.'));
	}
	const monitor: Monitor = await response.json();
	return { monitor };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not save monitor.') });
		}
		throw redirect(303, `/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	}
};
