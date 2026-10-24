import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, enginePath, redirectToLoginIfUnauthorized } from '#lib/server/core/engine';
import type { Source } from '#lib/logs/logs';
import type { RequestEvent } from '@sveltejs/kit';

/** An engine answer for a page load: 401 → login, 404 → 404, 403 stays 403, the rest is a 500. */
export async function loaded<T>(response: Response, what: string): Promise<T> {
	redirectToLoginIfUnauthorized(response);
	if (response.status === 404) throw error(404, `${what[0].toUpperCase()}${what.slice(1)} not found.`);
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, `Could not load the ${what}.`));
	}
	return (await response.json()) as T;
}

/** The project's sources, for the alert form's source checkboxes. */
export async function loadSources(event: RequestEvent): Promise<Source[]> {
	const sources = await loaded<unknown>(await callEngine(event, enginePath`/projects/${event.params.id}/sources`), 'sources');
	return Array.isArray(sources) ? (sources as Source[]) : [];
}
