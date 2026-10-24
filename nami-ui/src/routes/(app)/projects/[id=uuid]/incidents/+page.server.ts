import { error } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import type { IncidentPage } from '#lib/incidents/incidents';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// Incident details carry job output, which can hold secrets a job printed:
	// never cached, like every other page that shows output.
	event.setHeaders({ 'cache-control': 'private, no-store' });
	// Anything but "resolved" shows the "open" tab, which lists every
	// unresolved incident (open or acknowledged). Only these two fixed values
	// reach the engine, never the raw query value.
	const status = event.url.searchParams.get('status') === 'resolved' ? 'resolved' : 'open';
	const engineStatus = status === 'resolved' ? 'resolved' : 'unresolved';
	const response = await callEngine(
		event,
		enginePath`/projects/${event.params.id}/incidents?status=${engineStatus}`
	);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load incidents.')
		);
	}
	// The first page only (the newest 50); Task 12 adds paging.
	const { incidents }: IncidentPage = await response.json();
	return { incidents, status };
};
