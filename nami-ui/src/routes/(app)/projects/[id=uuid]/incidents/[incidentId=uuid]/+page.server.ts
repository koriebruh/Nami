import { fail, type RequestEvent } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { loaded } from '#lib/server/logs/log-alert-load';
import { isUuid } from '#lib/core/uuid';
import type { IncidentDetail, Severity } from '#lib/incidents/incidents';
import type { Actions, PageServerLoad } from './$types';

type Params = { id: string; incidentId: string };

function incidentPath(params: Params): string {
	return enginePath`/projects/${params.id}/incidents/${params.incidentId}`;
}

export const load: PageServerLoad = async (event) => {
	// Causes, job output and comments can hold secrets: never cached.
	event.setHeaders({ 'cache-control': 'private, no-store' });
	const [incident, severities] = await Promise.all([
		callEngine(event, incidentPath(event.params)).then((r) => loaded<IncidentDetail>(r, 'incident')),
		callEngine(event, enginePath`/projects/${event.params.id}/severities`).then((r) => loaded<Severity[]>(r, 'severities'))
	]);
	return { incident, severities };
};

type Action = 'acknowledge' | 'resolve' | 'reopen' | 'severity' | 'comment' | 'postmortem';

/** Sends one engine request for `action`; the engine's error (409, 403, ...) becomes the form message. */
async function send(event: RequestEvent<Params>, action: Action, path: string, init: RequestInit, fallback: string) {
	const response = await callEngine(event, path, init);
	if (!response.ok) {
		const { status, data } = await engineFailure(response, fallback);
		return fail(status, { ...data, action });
	}
	return { done: action };
}

// Members and admins only: the page hides these forms from viewers, and the
// engine's 403 still shows as the form message.
export const actions: Actions = {
	acknowledge: (event) =>
		send(event, 'acknowledge', `${incidentPath(event.params)}/acknowledge`, { method: 'POST', body: '{}' }, 'Could not acknowledge the incident.'),
	resolve: (event) =>
		send(event, 'resolve', `${incidentPath(event.params)}/resolve`, { method: 'POST', body: '{}' }, 'Could not resolve the incident.'),
	reopen: (event) =>
		send(event, 'reopen', `${incidentPath(event.params)}/reopen`, { method: 'POST', body: '{}' }, 'Could not reopen the incident.'),
	severity: async (event) => {
		const severityId = String((await event.request.formData()).get('severity_id') ?? '');
		if (!isUuid(severityId)) return fail(400, { action: 'severity' as const, message: 'Pick a severity from the list.' });
		return send(event, 'severity', incidentPath(event.params), { method: 'PATCH', body: JSON.stringify({ severity_id: severityId }) }, 'Could not change the severity.');
	},
	comment: async (event) => {
		const body = String((await event.request.formData()).get('body') ?? '').trim();
		if (body === '') return fail(400, { action: 'comment' as const, message: 'Write a comment first.' });
		return send(event, 'comment', `${incidentPath(event.params)}/comments`, { method: 'POST', body: JSON.stringify({ body }) }, 'Could not add the comment.');
	},
	postmortem: async (event) => {
		// Kept as typed (indentation matters in markdown); blank clears it.
		const text = String((await event.request.formData()).get('postmortem') ?? '');
		const postmortem = text.trim() === '' ? null : text;
		return send(event, 'postmortem', `${incidentPath(event.params)}/postmortem`, { method: 'PUT', body: JSON.stringify({ postmortem }) }, 'Could not save the postmortem.');
	}
};
