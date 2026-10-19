import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import { windowPayloadFromForm } from '#lib/server/maintenance-form';
import type { MaintenanceWindow } from '#lib/maintenance';
import type { MonitorSummary } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [windowResponse, monitorsResponse] = await Promise.all([
		callEngine(
			event,
			enginePath`/projects/${event.params.id}/maintenance-windows/${event.params.windowId}`
		),
		callEngine(event, enginePath`/projects/${event.params.id}/monitors`)
	]);
	redirectToLoginIfUnauthorized(windowResponse);
	if (windowResponse.status === 404) throw error(404, 'Maintenance window not found.');
	if (!windowResponse.ok) {
		throw error(
			windowResponse.status === 403 ? 403 : 500,
			await engineErrorMessage(windowResponse, 'Could not load the maintenance window.')
		);
	}
	const maintenanceWindow: MaintenanceWindow = await windowResponse.json();
	const monitors = monitorsResponse.ok
		? ((await monitorsResponse.json()) as MonitorSummary[]).map(({ id, name }) => ({ id, name }))
		: [];
	return { maintenanceWindow, monitors };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = windowPayloadFromForm(await event.request.formData(), 'edit');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/maintenance-windows/${event.params.windowId}`,
			{ method: 'PATCH', body: JSON.stringify(parsed.payload) }
		);
		if (!response.ok) return engineFailure(response, 'Could not save the maintenance window.');
		throw redirect(303, enginePath`/projects/${event.params.id}/maintenance`);
	}
};
