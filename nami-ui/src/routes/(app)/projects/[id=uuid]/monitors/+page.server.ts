import { error } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import type { MonitorGroup, MonitorSummary } from '#lib/uptime/uptime';
import { loadOnCall } from '#lib/server/incidents/schedule-form';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// The data sent with the page carries every monitor URL as saved for
	// members, credentials and query strings included: no browser or proxy may
	// keep it. Set first and only here (SvelteKit throws on a second set).
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const [response, groupsResponse, onCall] = await Promise.all([
		callEngine(event, enginePath`/projects/${event.params.id}/monitors`),
		callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`),
		// The project dashboard's "on call now" card (NAMI-INC-15).
		loadOnCall(event)
	]);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load monitors.')
		);
	}
	const monitors: MonitorSummary[] = await response.json();
	// Groups only arrange the list: without them it still renders, flat.
	const groups: MonitorGroup[] = groupsResponse.ok ? await groupsResponse.json() : [];
	return { monitors, groups, onCall };
};
