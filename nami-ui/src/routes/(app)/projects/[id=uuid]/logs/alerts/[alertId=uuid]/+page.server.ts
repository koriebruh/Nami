import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { formText } from '#lib/server/uptime/monitor-form';
import { logAlertPayloadFromForm } from '#lib/server/logs/log-alert-form';
import { loaded, loadSources } from '#lib/server/logs/log-alert-load';
import type { LogAlert } from '#lib/logs/log-alerts';
import type { Actions, PageServerLoad } from './$types';

function alertPath(params: { id: string; alertId: string }): string {
	return enginePath`/projects/${params.id}/log-alerts/${params.alertId}`;
}

export const load: PageServerLoad = async (event) => {
	const alert = await loaded<LogAlert>(await callEngine(event, alertPath(event.params)), 'log alert');
	return { alert, sources: await loadSources(event) };
};

export const actions: Actions = {
	update: async (event) => {
		const parsed = logAlertPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'update' as const, message: parsed.message });
		const response = await callEngine(event, alertPath(event.params), {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (response.status === 403) {
			return fail(403, { action: 'update' as const, message: 'Only project members and admins can edit alerts.' });
		}
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not save the alert.');
			return fail(status, { ...data, action: 'update' as const });
		}
		return { saved: true as const };
	},
	delete: async (event) => {
		// Two-step: the button asks with confirm() and sends `confirm=yes`;
		// without JavaScript a bare POST sends the page back to ask first.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmDelete: true as const };
		const response = await callEngine(event, alertPath(event.params), { method: 'DELETE' });
		if (response.status === 403) {
			return fail(403, { action: 'delete' as const, message: 'Only project admins can delete alerts.' });
		}
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not delete the alert.');
			return fail(status, { ...data, action: 'delete' as const });
		}
		throw redirect(303, enginePath`/projects/${event.params.id}/logs/alerts`);
	}
};
