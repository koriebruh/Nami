import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	publicEngineUrl,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { heartbeatPayloadFromForm } from '#lib/server/uptime/heartbeat-form';
import type { Heartbeat, MonitorGroup } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad } from './$types';

async function jsonArray<T>(response: Response): Promise<T[]> {
	if (!response.ok) return [];
	try {
		const body: unknown = await response.json();
		return Array.isArray(body) ? (body as T[]) : [];
	} catch {
		return [];
	}
}

export const load: PageServerLoad = async (event) => {
	// The page carries the ping tokens, in its markup and in the data that goes
	// with it: no browser or proxy may keep either. Set first, so that every
	// response of this load has it (a viewer's, an error page, a redirect), and
	// only here: SvelteKit throws when a header is set twice.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const [response, groupsResponse] = await Promise.all([
		callEngine(event, enginePath`/projects/${event.params.id}/heartbeats`),
		callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`)
	]);
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
	// Groups only arrange the list: without them it still renders, flat.
	const groups = await jsonArray<MonitorGroup>(groupsResponse);
	return { heartbeats, groups, engineUrl: publicEngineUrl() };
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
