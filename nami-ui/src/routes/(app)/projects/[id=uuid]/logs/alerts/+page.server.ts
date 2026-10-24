import { callEngine, enginePath } from '#lib/server/core/engine';
import { loaded, loadSources } from '#lib/server/logs/log-alert-load';
import type { LogAlert } from '#lib/logs/log-alerts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const alerts = await loaded<LogAlert[]>(
		await callEngine(event, enginePath`/projects/${event.params.id}/log-alerts`),
		'log alerts'
	);
	return { alerts, sources: await loadSources(event) };
};
