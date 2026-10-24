import { error, fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { sourcePayloadFromForm } from '#lib/server/logs/source-form';
import type { Source, SourceWithToken } from '#lib/logs/logs';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// A page that has just created a source shows its token (the no-JavaScript
	// form post renders it from this same load): no browser or proxy may keep
	// it. Set first and only here, since SvelteKit throws on a second set.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const response = await callEngine(event, enginePath`/projects/${event.params.id}/sources`);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load sources.')
		);
	}
	const sources: Source[] = await response.json();
	return { sources };
};

export const actions: Actions = {
	// Returns the new token instead of redirecting, so the page can show it
	// right away (it is never put in a URL).
	create: async (event) => {
		const parsed = sourcePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/sources`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (response.status === 403) {
			return fail(403, { message: 'Only project members and admins can create a source.' });
		}
		if (!response.ok) return engineFailure(response, 'Could not create the source.');
		const created: SourceWithToken = await response.json();
		return { created: { id: created.id, name: created.name, token: created.token } };
	}
};
