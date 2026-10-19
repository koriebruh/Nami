import { error, fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { windowPayloadFromForm } from '#lib/server/uptime/maintenance-form';
import { formText } from '#lib/server/uptime/monitor-form';
import { isUuid } from '#lib/core/uuid';
import type { MaintenanceWindow } from '#lib/uptime/maintenance';
import type { MonitorSummary } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [windowsResponse, monitorsResponse] = await Promise.all([
		callEngine(event, enginePath`/projects/${event.params.id}/maintenance-windows`),
		callEngine(event, enginePath`/projects/${event.params.id}/monitors`)
	]);
	redirectToLoginIfUnauthorized(windowsResponse);
	if (!windowsResponse.ok) {
		throw error(
			windowsResponse.status === 403 ? 403 : 500,
			await engineErrorMessage(windowsResponse, 'Could not load maintenance windows.')
		);
	}
	const windows: MaintenanceWindow[] = await windowsResponse.json();
	// Monitor names only label the scope; without them windows still list.
	const monitors = monitorsResponse.ok
		? ((await monitorsResponse.json()) as MonitorSummary[]).map(({ id, name }) => ({ id, name }))
		: [];
	return { windows, monitors };
};

export const actions: Actions = {
	create: async (event) => {
		const parsed = windowPayloadFromForm(await event.request.formData(), 'create');
		if (!parsed.ok) return fail(400, { action: 'create' as const, success: false, message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/maintenance-windows`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not create the maintenance window.');
			return fail(status, { action: 'create' as const, success: false, message: data.message });
		}
		return { action: 'create' as const, success: true, message: 'Maintenance window created.' };
	},
	delete: async (event) => {
		const form = await event.request.formData();
		const windowId = form.get('window_id');
		if (!isUuid(windowId)) {
			return fail(400, { action: 'delete' as const, success: false, message: 'Unknown maintenance window.' });
		}
		// The Delete button asks with confirm() and then sends `confirm=yes`.
		// Without JavaScript there is no dialog, and a bare POST must not
		// delete: send the page back to ask first.
		if (formText(form, 'confirm') !== 'yes') return { action: 'delete' as const, confirmDelete: windowId };
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/maintenance-windows/${windowId}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not delete the maintenance window.');
			return fail(status, { action: 'delete' as const, success: false, message: data.message });
		}
		return { action: 'delete' as const, success: true, message: 'Maintenance window deleted.' };
	}
};
