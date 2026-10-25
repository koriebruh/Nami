import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { loaded } from '#lib/server/logs/log-alert-load';
import { loadMembers, schedulePayloadFromForm } from '#lib/server/incidents/schedule-form';
import { isUuid } from '#lib/core/uuid';
import type { Schedule } from '#lib/incidents/oncall';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [schedules, members] = await Promise.all([
		callEngine(event, enginePath`/projects/${event.params.id}/schedules`).then((r) => loaded<Schedule[]>(r, 'schedules')),
		loadMembers(event)
	]);
	return { schedules, members };
};

// Admins only: the page hides the form from everyone else, and the engine's
// 403 still shows as the form message.
export const actions: Actions = {
	create: async (event) => {
		const parsed = schedulePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'create' as const, message: parsed.message });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/schedules`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			const { status, data } = await engineFailure(response, 'Could not create the schedule.');
			return fail(status, { ...data, action: 'create' as const });
		}
		const created: { id?: unknown } = await response.json();
		if (!isUuid(created.id)) return fail(502, { action: 'create' as const, message: 'Could not create the schedule.' });
		throw redirect(303, enginePath`/projects/${event.params.id}/oncall/${created.id}`);
	}
};
