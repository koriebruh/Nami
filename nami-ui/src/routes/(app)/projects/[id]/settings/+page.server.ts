import type { Actions } from './$types';
import { callEngine } from '#lib/server/engine';

// Moved unchanged from the old /projects/[id] page.
export const actions: Actions = {
	addMember: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, `/projects/${event.params.id}/members`, {
			method: 'POST',
			body: JSON.stringify({
				user_id: form.get('user_id'),
				project_role: form.get('project_role')
			})
		});
		const body = await response.json();
		return {
			action: 'add' as const,
			success: response.ok,
			message: response.ok ? 'Member added.' : (body.error ?? 'could not add member')
		};
	},
	removeMember: async (event) => {
		const form = await event.request.formData();
		const userId = form.get('user_id');
		const response = await callEngine(
			event,
			`/projects/${event.params.id}/members/${userId}`,
			{ method: 'DELETE' }
		);
		const body = await response.json();
		return {
			action: 'remove' as const,
			success: response.ok,
			message: response.ok ? 'Member removed.' : (body.error ?? 'could not remove member')
		};
	}
};
