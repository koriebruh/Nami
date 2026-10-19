import { fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { formText, formWholeNumber } from '#lib/server/uptime/monitor-form';
import { isUuid } from '#lib/core/uuid';
import type { MonitorGroup } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`);
	redirectToLoginIfUnauthorized(response);
	// The member forms below work without the group list, so a failure is
	// shown in its section instead of replacing the page.
	if (!response.ok) {
		return {
			groups: [] as MonitorGroup[],
			groupsError: await engineErrorMessage(response, 'Could not load monitor groups.')
		};
	}
	const groups: MonitorGroup[] = await response.json();
	return { groups, groupsError: null };
};

const GROUP_MAX_POSITION = 1_000_000;

/** Name and position from a group form, or the message to show. */
// A blank position means 0 on create but "keep the current one" on update,
// so clearing the field never silently moves a group to the top.
function groupBody(form: FormData, mode: 'create' | 'update'): { name: string; position?: number } | string {
	const position = formWholeNumber(form, 'position');
	if (position === 'invalid' || (position !== undefined && (position < 0 || position > GROUP_MAX_POSITION))) {
		return `Position must be a whole number from 0 to ${GROUP_MAX_POSITION}`;
	}
	const name = formText(form, 'name');
	if (position === undefined) return mode === 'create' ? { name, position: 0 } : { name };
	return { name, position };
}

const groupFail = (status: number, message: string) =>
	fail(status, { action: 'group' as const, success: false, message });

async function groupFailure(response: Response, fallback: string) {
	const { status, data } = await engineFailure(response, fallback);
	return groupFail(status, data.message);
}

// Members are added and removed by user ID. The field is user input that goes
// into an engine path, so it must be a UUID before anything is sent.
function userIdFrom(form: FormData): string | null {
	const userId = String(form.get('user_id') ?? '').trim();
	return isUuid(userId) ? userId : null;
}

const NOT_A_UUID = 'User ID must be a UUID.';

export const actions: Actions = {
	addMember: async (event) => {
		const form = await event.request.formData();
		const userId = userIdFrom(form);
		if (!userId) return fail(400, { action: 'add' as const, success: false, message: NOT_A_UUID });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/members`, {
			method: 'POST',
			body: JSON.stringify({
				user_id: userId,
				project_role: form.get('project_role')
			})
		});
		redirectToLoginIfUnauthorized(response);
		return {
			action: 'add' as const,
			success: response.ok,
			message: response.ok ? 'Member added.' : await engineErrorMessage(response, 'could not add member')
		};
	},
	removeMember: async (event) => {
		const form = await event.request.formData();
		const userId = userIdFrom(form);
		if (!userId) return fail(400, { action: 'remove' as const, success: false, message: NOT_A_UUID });
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/members/${userId}`,
			{ method: 'DELETE' }
		);
		redirectToLoginIfUnauthorized(response);
		return {
			action: 'remove' as const,
			success: response.ok,
			message: response.ok ? 'Member removed.' : await engineErrorMessage(response, 'could not remove member')
		};
	},
	createGroup: async (event) => {
		const body = groupBody(await event.request.formData(), 'create');
		if (typeof body === 'string') return groupFail(400, body);
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`, {
			method: 'POST',
			body: JSON.stringify(body)
		});
		if (!response.ok) return groupFailure(response, 'Could not create the group.');
		return { action: 'group' as const, success: true, message: 'Group created.' };
	},
	updateGroup: async (event) => {
		const form = await event.request.formData();
		const groupId = form.get('group_id');
		if (!isUuid(groupId)) return groupFail(400, 'Unknown group.');
		const body = groupBody(form, 'update');
		if (typeof body === 'string') return groupFail(400, body);
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/monitor-groups/${groupId}`,
			{ method: 'PATCH', body: JSON.stringify(body) }
		);
		if (!response.ok) return groupFailure(response, 'Could not save the group.');
		return { action: 'group' as const, success: true, message: 'Group saved.' };
	},
	deleteGroup: async (event) => {
		const form = await event.request.formData();
		const groupId = form.get('group_id');
		if (!isUuid(groupId)) return groupFail(400, 'Unknown group.');
		// The Delete button asks with confirm() and then sends `confirm=yes`.
		// Without JavaScript a bare POST must not delete: send the page back to
		// ask first.
		if (formText(form, 'confirm') !== 'yes') return { action: 'group' as const, confirmDelete: groupId };
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/monitor-groups/${groupId}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) return groupFailure(response, 'Could not delete the group.');
		return {
			action: 'group' as const,
			success: true,
			message: 'Group deleted; its monitors are now ungrouped.'
		};
	}
};
