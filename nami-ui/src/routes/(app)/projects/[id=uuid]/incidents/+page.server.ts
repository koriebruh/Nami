import { error } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import type { Incident } from '#lib/uptime/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// Incident details carry job output, which can hold secrets a job printed:
	// never cached, like every other page that shows output.
	event.setHeaders({ 'cache-control': 'private, no-store' });
	// Anything but "resolved" shows open incidents: the engine only accepts
	// open|resolved, so never forward the raw query value.
	const status = event.url.searchParams.get('status') === 'resolved' ? 'resolved' : 'open';
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/incidents?status=${status}`);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load incidents.')
		);
	}
	const incidents: Incident[] = await response.json();
	return { incidents, status };
};
