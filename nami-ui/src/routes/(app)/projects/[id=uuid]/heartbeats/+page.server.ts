import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	publicEngineUrl,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import { heartbeatPayloadFromForm } from '#lib/server/heartbeat-form';
import type { Heartbeat } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/heartbeats`);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load heartbeats.')
		);
	}
	// A viewer's heartbeats carry `token: null`: the engine only gives the ping
	// token to members, and this load passes the list on as it came.
	const heartbeats: Heartbeat[] = await response.json();
	return { heartbeats, engineUrl: publicEngineUrl() };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = heartbeatPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/heartbeats`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) return engineFailure(response, 'Could not create heartbeat.');
		const created: { id: string } = await response.json();
		throw redirect(303, enginePath`/projects/${event.params.id}/heartbeats/${created.id}`);
	}
};
