import { error } from '@sveltejs/kit';
import { callEngine, enginePath } from '#lib/server/core/engine';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

/**
 * One whole log row for the explorer's expanded view (list rows cut long
 * messages and leave out large fields). Only `dt` reaches the engine; the
 * engine's status and JSON are passed through (a 401 tells the page to go to
 * the login page), streamed rather than buffered.
 */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id) || !isUuid(event.params.logId)) throw error(404, 'Not found');
	const query = new URLSearchParams({ dt: event.url.searchParams.get('dt') ?? '' });
	const response = await callEngine(
		event,
		enginePath`/projects/${event.params.id}/logs/${event.params.logId}` + `?${query}`
	);
	return new Response(response.body, {
		status: response.status,
		headers: {
			'content-type': response.headers.get('content-type') ?? 'application/json',
			'cache-control': 'no-store'
		}
	});
};
