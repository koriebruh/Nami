import { error } from '@sveltejs/kit';
import { callEngine, enginePath } from '#lib/server/core/engine';
import { pickLogParams, SEARCH_PARAMS } from '#lib/server/logs/log-proxy';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

/**
 * The explorer's "Load more": one search page, passed through as JSON with
 * the engine's status (a 401 tells the page to go to the login page),
 * streamed rather than buffered. Only the allow-listed search parameters
 * reach the engine.
 */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id)) throw error(404, 'Not found');
	const query = pickLogParams(event.url.searchParams, SEARCH_PARAMS);
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/logs` + query);
	return new Response(response.body, {
		status: response.status,
		headers: {
			'content-type': response.headers.get('content-type') ?? 'application/json',
			'cache-control': 'no-store'
		}
	});
};
