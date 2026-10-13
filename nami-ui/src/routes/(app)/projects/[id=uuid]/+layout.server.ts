import { error } from '@sveltejs/kit';
import { callEngine, enginePath, redirectToLoginIfUnauthorized } from '#lib/server/engine';
import type { LayoutServerLoad } from './$types';

const ENGINE_UNAVAILABLE = 'Engine unavailable, retrying';

export const load: LayoutServerLoad = async (event) => {
	let response: Response;
	try {
		response = await callEngine(event, enginePath`/projects/${event.params.id}`);
	} catch {
		// Connection refused, DNS failure, reset: the engine did not answer.
		throw error(502, ENGINE_UNAVAILABLE);
	}

	redirectToLoginIfUnauthorized(response);
	// Engine answers 403 for a project the caller may not see, whether or not
	// it exists (anti-enumeration design), so a 403 says only "no access". The
	// 404 is a global admin's, who passes the access check for any id.
	if (response.status === 403) throw error(403, 'You do not have access to this project.');
	if (response.status === 404) throw error(404, 'Project not found.');
	// Anything else (a 5xx, an unexpected 4xx) says nothing about access: it
	// must not read as "no access".
	if (!response.ok) throw error(502, ENGINE_UNAVAILABLE);

	let project: { id: string; name: string; slug: string };
	try {
		project = await response.json();
	} catch {
		throw error(502, ENGINE_UNAVAILABLE);
	}
	return { project };
};
