import { error } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import type { MonitorSummary } from '#lib/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// The data sent with the page carries every monitor URL as saved for
	// members, credentials and query strings included: no browser or proxy may
	// keep it. Set first and only here (SvelteKit throws on a second set).
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitors`);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load monitors.')
		);
	}
	const monitors: MonitorSummary[] = await response.json();
	return { monitors };
};
