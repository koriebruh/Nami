import { error, redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';
import type { RequestHandler } from './$types';

// The browser never talks to nami-engine directly (BFF architecture): OAuth
// start/callback must be proxied through nami-ui like every other auth call,
// so the session cookie (holding the CSRF state, then the final session)
// lives on nami-ui's origin throughout the flow, not the engine's.
export const GET: RequestHandler = async (event) => {
	const { provider } = event.params;
	const response = await callEngine(event, `/auth/oauth/${provider}/start`, {
		redirect: 'manual'
	});

	const location = response.headers.get('location');
	if (!location) {
		// Not every non-redirect response here is an engine outage -- an
		// unrecognized `provider` (e.g. a typo in a link) gets a real 400 from
		// the engine, not a 3xx. Surface that actual reason instead of a
		// generic 502 that would mask it.
		if (response.status < 300 || response.status >= 400) {
			const body = await response.json().catch(() => ({}));
			throw error(response.status || 502, body.error ?? 'OAuth start failed');
		}
		throw error(502, 'nami-engine did not return a redirect for this OAuth provider');
	}

	throw redirect(303, location);
};
