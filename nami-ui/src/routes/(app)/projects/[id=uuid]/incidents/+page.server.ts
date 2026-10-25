import { error, fail, redirect, type RequestEvent } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { formText } from '#lib/server/uptime/monitor-form';
import { isUuid } from '#lib/core/uuid';
import { engineListQuery, parseListQuery, type IncidentPage, type PolicyOption, type Severity } from '#lib/incidents/incidents';
import type { Actions, PageServerLoad } from './$types';

/** A list the "New incident" form offers; empty when it cannot be read (the engine then applies its defaults). */
async function optionalList<T>(event: RequestEvent, path: string): Promise<T[]> {
	try {
		const response = await callEngine(event, path);
		if (!response.ok) return [];
		const body: unknown = await response.json();
		return Array.isArray(body) ? (body as T[]) : [];
	} catch {
		return [];
	}
}

export const load: PageServerLoad = async (event) => {
	// Incident details carry job output, which can hold secrets a job printed:
	// never cached, like every other page that shows output.
	event.setHeaders({ 'cache-control': 'private, no-store' });
	// Only values parseListQuery kept reach the engine, never the raw query.
	const query = parseListQuery(event.url);
	const id = event.params.id;
	const [response, severities, policies] = await Promise.all([
		callEngine(event, enginePath`/projects/${id}/incidents` + engineListQuery(query)),
		optionalList<Severity>(event, enginePath`/projects/${id}/severities`),
		optionalList<PolicyOption>(event, enginePath`/projects/${id}/escalation-policies`)
	]);
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load incidents.')
		);
	}
	const { incidents, next_cursor }: IncidentPage = await response.json();
	return { incidents, nextCursor: next_cursor, query, severities, policies };
};

export const actions: Actions = {
	// A manual incident. Members and admins only: the page hides the form from
	// viewers, and the engine's 403 still shows as the form message.
	create: async (event) => {
		const form = await event.request.formData();
		const title = formText(form, 'title');
		const description = formText(form, 'description');
		const severityId = formText(form, 'severity_id');
		const policyId = formText(form, 'escalation_policy_id');
		if (title === '') return fail(400, { action: 'create' as const, message: 'Enter a title.' });
		if ((severityId && !isUuid(severityId)) || (policyId && !isUuid(policyId))) {
			return fail(400, { action: 'create' as const, message: 'Pick a severity and policy from the lists.' });
		}
		const payload: Record<string, string> = { title };
		if (description) payload.description = description;
		if (severityId) payload.severity_id = severityId;
		if (policyId) payload.escalation_policy_id = policyId;
		const list = enginePath`/projects/${event.params.id}/incidents`;
		const response = await callEngine(event, list, { method: 'POST', body: JSON.stringify(payload) });
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not create the incident.');
			return fail(status, { ...data, action: 'create' as const });
		}
		const created: { id?: unknown } = await response.json().catch(() => ({}));
		throw redirect(303, isUuid(created.id) ? enginePath`/projects/${event.params.id}/incidents/${created.id}` : list);
	}
};
