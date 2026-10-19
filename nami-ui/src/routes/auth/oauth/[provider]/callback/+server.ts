import { error, redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/core/engine';
import type { RequestHandler } from './$types';

// Mirrors /start: the OAuth provider redirects the browser back here (on
// nami-ui's origin, matching OAUTH_REDIRECT_BASE_URL), carrying the
// CSRF-state cookie set during /start. We forward the code/state query
// string and that cookie to the engine, relay the engine's final
// authenticated-session cookie back to the browser, then land on the
// dashboard.
export const GET: RequestHandler = async (event) => {
	const { provider } = event.params;
	const response = await callEngine(event, `/auth/oauth/${provider}/callback${event.url.search}`);

	if (!response.ok) {
		const body = await response.json().catch(() => ({}));
		throw error(response.status, body.error ?? 'OAuth login failed');
	}

	throw redirect(303, '/');
};
