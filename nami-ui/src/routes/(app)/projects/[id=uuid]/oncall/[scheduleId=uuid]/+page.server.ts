import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineFailure, enginePath } from '#lib/server/core/engine';
import { loaded } from '#lib/server/logs/log-alert-load';
import { loadMembers, overridePayloadFromForm, schedulePayloadFromForm } from '#lib/server/incidents/schedule-form';
import { isUuid } from '#lib/core/uuid';
import type { Override, Schedule, Shift } from '#lib/incidents/oncall';
import type { Actions, PageServerLoad } from './$types';

type Params = { id: string; scheduleId: string };

function schedulePath(params: Params): string {
	return enginePath`/projects/${params.id}/schedules/${params.scheduleId}`;
}

export const load: PageServerLoad = async (event) => {
	const path = schedulePath(event.params);
	// The shifts default to the next 14 days (the engine's default range).
	const [schedule, shifts, overrides, members] = await Promise.all([
		callEngine(event, path).then((r) => loaded<Schedule>(r, 'schedule')),
		callEngine(event, `${path}/shifts`).then((r) => loaded<Shift[]>(r, 'shifts')),
		callEngine(event, `${path}/overrides`).then((r) => loaded<Override[]>(r, 'overrides')),
		loadMembers(event)
	]);
	return { schedule, shifts, overrides, members };
};

type Action = 'save' | 'addOverride' | 'deleteOverride' | 'delete';

async function failure(action: Action, response: Response, fallback: string) {
	const { status, data } = await engineFailure(response, fallback);
	return fail(status, { ...data, action });
}

const confirmed = (form: FormData) => String(form.get('confirm') ?? '') === 'yes';

// Admins only: the page hides these forms from everyone else, and the
// engine's 403 still shows as the form message.
export const actions: Actions = {
	save: async (event) => {
		const parsed = schedulePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'save' as const, message: parsed.message });
		const response = await callEngine(event, schedulePath(event.params), { method: 'PUT', body: JSON.stringify(parsed.payload) });
		if (!response.ok) return failure('save', response, 'Could not save the schedule.');
		return { action: 'save' as const, saved: true, message: 'Schedule saved.' };
	},
	addOverride: async (event) => {
		const form = await event.request.formData();
		// The schedule's zone, rendered by the page next to the inputs.
		const parsed = overridePayloadFromForm(form, String(form.get('timezone') ?? ''));
		if (!parsed.ok) return fail(400, { action: 'addOverride' as const, message: parsed.message });
		const response = await callEngine(event, `${schedulePath(event.params)}/overrides`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) return failure('addOverride', response, 'Could not add the override.');
		return { action: 'addOverride' as const, added: true, message: 'Override added.' };
	},
	deleteOverride: async (event) => {
		const form = await event.request.formData();
		const id = form.get('override_id');
		if (!isUuid(id)) return fail(400, { action: 'deleteOverride' as const, message: 'Unknown override.' });
		// The button asks with confirm() and sends `confirm=yes`; without
		// JavaScript a bare POST asks on the page first.
		if (!confirmed(form)) return { action: 'deleteOverride' as const, confirmDeleteOverride: id };
		const response = await callEngine(event, `${schedulePath(event.params)}${enginePath`/overrides/${id}`}`, { method: 'DELETE' });
		if (!response.ok) return failure('deleteOverride', response, 'Could not delete the override.');
		return { action: 'deleteOverride' as const, deleted: true, message: 'Override deleted.' };
	},
	delete: async (event) => {
		if (!confirmed(await event.request.formData())) return { action: 'delete' as const, confirmDelete: true };
		const response = await callEngine(event, schedulePath(event.params), { method: 'DELETE' });
		// A 409 names the escalation policy that still targets the schedule.
		if (!response.ok) return failure('delete', response, 'Could not delete the schedule.');
		throw redirect(303, enginePath`/projects/${event.params.id}/oncall`);
	}
};
