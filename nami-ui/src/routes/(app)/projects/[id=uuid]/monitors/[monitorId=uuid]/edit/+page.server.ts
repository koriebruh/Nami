import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { loadRoutingChoices } from '#lib/server/incidents/policy-form';
import { monitorPayloadFromForm } from '#lib/server/uptime/monitor-form';
import type { Monitor, MonitorGroup } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// The page carries the monitor URL as saved for members, credentials and
	// query string included: no browser or proxy may keep it. Set first, so
	// that every response of this load has it (an error page and a redirect
	// too), and only here: SvelteKit throws when a header is set twice.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const [response, groupsResponse, routing] = await Promise.all([
		callEngine(event, enginePath`/projects/${event.params.id}/monitors/${event.params.monitorId}`),
		callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`),
		loadRoutingChoices(event)
	]);
	redirectToLoginIfUnauthorized(response);
	if (response.status === 404) throw error(404, 'Monitor not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load monitor.'));
	}
	const monitor: Monitor = await response.json();
	const groups: MonitorGroup[] = groupsResponse.ok ? await groupsResponse.json() : [];
	return { monitor, groups, ...routing };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData(), 'edit');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/monitors/${event.params.monitorId}`,
			{ method: 'PATCH', body: JSON.stringify(parsed.payload) }
		);
		if (!response.ok) return engineFailure(response, 'Could not save monitor.');
		throw redirect(303, enginePath`/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	}
};
