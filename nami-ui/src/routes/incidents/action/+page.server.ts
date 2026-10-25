import { error, fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { callEngine, engineErrorMessage } from '#lib/server/core/engine';

/** Longest token passed on to the engine; real ones are under 250 chars. */
const MAX_TOKEN_CHARS = 1024;

export type Inspected = {
	action: 'acknowledge' | 'resolve';
	incident: {
		id: string;
		project_id: string;
		project_name: string;
		title: string;
		status: string;
		severity: string;
		started_at: string;
	};
	user_email: string;
};

const INVALID = 'This link is invalid.';

// The acknowledge/resolve link in an incident email lands here, without a
// login. Loading only inspects the link: mail scanners and link previews
// fetch it before the responder clicks, so only the button (the form action)
// acts. The page is never cached and never leaks its URL (the token) through
// a Referer.
export const load: PageServerLoad = async (event) => {
	event.setHeaders({ 'cache-control': 'private, no-store', 'referrer-policy': 'no-referrer', 'x-robots-tag': 'noindex' });
	const token = event.url.searchParams.get('token');
	if (!token || token.length > MAX_TOKEN_CHARS) error(400, INVALID);
	const response = await callEngine(event, '/incident-actions/inspect', { method: 'POST', body: JSON.stringify({ token }) });
	if (response.ok) return { inspected: (await response.json()) as Inspected, token };
	if ([400, 403, 410].includes(response.status)) return { problem: await engineErrorMessage(response, INVALID) };
	error(500, 'This link could not be checked. Try again later.');
};

export const actions = {
	// The token comes from the form body, never the URL. The routes are
	// sessionless, so an engine error is shown as it is (no login redirect).
	// SvelteKit's origin check guards this action.
	confirm: async (event) => {
		const token = (await event.request.formData()).get('token');
		if (typeof token !== 'string' || !token || token.length > MAX_TOKEN_CHARS) return fail(400, { message: INVALID });
		const response = await callEngine(event, '/incident-actions/confirm', { method: 'POST', body: JSON.stringify({ token }) });
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'The incident could not be updated.') });
		}
		const { status } = (await response.json()) as { status: string };
		return { done: true as const, status };
	}
} satisfies Actions;
