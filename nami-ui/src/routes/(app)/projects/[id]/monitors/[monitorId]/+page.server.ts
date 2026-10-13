import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { Incident, Monitor, MonitorStats, UptimeCheck } from '#lib/uptime';
import type { Actions, PageServerLoad, RequestEvent } from './$types';

export const load: PageServerLoad = async (event) => {
	const base = `/projects/${event.params.id}/monitors/${event.params.monitorId}`;
	const [monitorRes, statsRes, checksRes, incidentsRes] = await Promise.all([
		callEngine(event, base),
		callEngine(event, `${base}/stats`),
		callEngine(event, `${base}/checks?limit=20`),
		callEngine(event, `/projects/${event.params.id}/incidents?monitor_id=${event.params.monitorId}`)
	]);
	// The engine answers a monitor id that is not a UUID with a 400 (the path
	// extractor rejects it): still "no such monitor", not a server error.
	if (monitorRes.status === 404 || monitorRes.status === 400) throw error(404, 'Monitor not found.');
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
	return { monitor, stats, checks, incidents };
};

async function setPaused(event: RequestEvent, paused: boolean) {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
		method: 'PATCH',
		body: JSON.stringify({ paused })
	});
	if (!response.ok) {
		return fail(response.status, { message: await engineErrorMessage(response, 'Could not update monitor.') });
	}
	return { success: true };
}

export const actions: Actions = {
	pause: (event) => setPaused(event, true),
	resume: (event) => setPaused(event, false),
	delete: async (event) => {
		const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
			method: 'DELETE'
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not delete monitor.') });
		}
		throw redirect(303, `/projects/${event.params.id}/monitors`);
	}
};
