import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { logAlertPayloadFromForm } from '#lib/server/logs/log-alert-form';
import { loadSources } from '#lib/server/logs/log-alert-load';
import { alertQueryFromExplorer, NEW_ALERT, type LogAlert, type LogAlertSettings } from '#lib/logs/log-alerts';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// "Create alert from this search" links here with the explorer's query string.
	const initial: LogAlertSettings = { ...NEW_ALERT, query: alertQueryFromExplorer(event.url.searchParams) };
	return { initial, sources: await loadSources(event) };
};

export const actions: Actions = {
	create: async (event) => {
		const parsed = logAlertPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/log-alerts`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (response.status === 403) {
			return fail(403, { message: 'Only project members and admins can create alerts.' });
		}
		if (!response.ok) return engineFailure(response, 'Could not create the alert.');
		const created: LogAlert = await response.json();
		throw redirect(303, enginePath`/projects/${event.params.id}/logs/alerts/${created.id}`);
	}
};
