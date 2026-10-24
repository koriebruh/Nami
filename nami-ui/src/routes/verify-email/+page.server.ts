import type { Actions, PageServerLoad } from './$types';
import { callEngine, enginePath } from '#lib/server/core/engine';

// The link in the verification email lands here. Loading the page must not
// use the token up: link scanners (Outlook Safe Links, ...) fetch it before
// the user clicks, and a refresh would then show "invalid". Only the button
// (the form action) calls the engine.
export const load: PageServerLoad = async (event) => {
	return { hasToken: !!event.url.searchParams.get('token') };
};

export const actions: Actions = {
	// Any failure (no token, a used or expired one, an engine error) shows the
	// same generic message.
	default: async (event) => {
		const token = (await event.request.formData()).get('token');
		if (typeof token !== 'string' || !token) return { verified: false };
		try {
			const response = await callEngine(event, enginePath`/auth/verify-email?token=${token}`);
			return { verified: response.ok };
		} catch {
			return { verified: false };
		}
	}
};
