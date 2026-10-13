import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { MonitorSummary } from '#lib/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors`);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load monitors.')
		);
	}
	const monitors: MonitorSummary[] = await response.json();
	return { monitors };
};
