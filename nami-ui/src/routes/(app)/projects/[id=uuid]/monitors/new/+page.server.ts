import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { Actions } from './$types';

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData());
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
