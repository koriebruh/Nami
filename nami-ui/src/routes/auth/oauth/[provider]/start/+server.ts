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
		throw error(502, 'nami-engine did not return a redirect for this OAuth provider');
	}

	throw redirect(303, location);
};
