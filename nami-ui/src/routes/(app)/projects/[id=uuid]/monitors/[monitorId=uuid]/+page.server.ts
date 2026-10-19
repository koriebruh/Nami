import { error, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { loadAvailability } from '#lib/server/uptime/availability';
import { formText } from '#lib/server/uptime/monitor-form';
import type { Incident, Monitor, MonitorStats, UptimeCheck } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad, RequestEvent } from './$types';

export const load: PageServerLoad = async (event) => {
	// The page carries the monitor URL as saved for members, credentials and
	// query string included: no browser or proxy may keep it. Set first, so
	// that every response of this load has it (an error page and a redirect
	// too), and only here: SvelteKit throws when a header is set twice.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const { id, monitorId } = event.params;
	const [monitorRes, statsRes, checksRes, incidentsRes, availability] = await Promise.all([
		callEngine(event, enginePath`/projects/${id}/monitors/${monitorId}`),
		callEngine(event, enginePath`/projects/${id}/monitors/${monitorId}/stats`),
		callEngine(event, enginePath`/projects/${id}/monitors/${monitorId}/checks?limit=20`),
		callEngine(event, enginePath`/projects/${id}/incidents?monitor_id=${monitorId}`),
		loadAvailability(event, enginePath`/projects/${id}/monitors/${monitorId}/availability`)
	]);
	redirectToLoginIfUnauthorized(monitorRes);
	if (monitorRes.status === 404) throw error(404, 'Monitor not found.');
	if (!monitorRes.ok) {
		throw error(
			monitorRes.status === 403 ? 403 : 500,
			await engineErrorMessage(monitorRes, 'Could not load monitor.')
		);
	}
	const monitor: Monitor = await monitorRes.json();
	// History lives in ClickHouse: if it is unavailable, still render the
	// monitor (its status comes from Postgres) with the history sections empty.
	const stats: MonitorStats | null = statsRes.ok ? await statsRes.json() : null;
	const checks: UptimeCheck[] = checksRes.ok ? await checksRes.json() : [];
	const incidents: Incident[] = incidentsRes.ok ? await incidentsRes.json() : [];
	return { monitor, stats, checks, incidents, availability };
};

async function setPaused(event: RequestEvent, paused: boolean) {
	const response = await callEngine(
		event,
		enginePath`/projects/${event.params.id}/monitors/${event.params.monitorId}`,
		{ method: 'PATCH', body: JSON.stringify({ paused }) }
	);
	if (!response.ok) return engineFailure(response, 'Could not update monitor.');
	return { success: true };
}

export const actions: Actions = {
	pause: (event) => setPaused(event, true),
	resume: (event) => setPaused(event, false),
	delete: async (event) => {
		// The Delete button asks with confirm() and then sends `confirm=yes`.
		// Without JavaScript there is no dialog, and a bare POST must not delete
		// a monitor and its incidents: send the page back to ask first.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmDelete: true as const };

		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/monitors/${event.params.monitorId}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) return engineFailure(response, 'Could not delete monitor.');
		throw redirect(303, enginePath`/projects/${event.params.id}/monitors`);
	}
};
