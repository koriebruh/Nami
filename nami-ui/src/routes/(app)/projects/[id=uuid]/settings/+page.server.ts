import { fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import { isUuid } from '#lib/uuid';
import type { Actions } from './$types';

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
	}
};
