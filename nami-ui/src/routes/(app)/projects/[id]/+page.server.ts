import type { Actions, PageServerLoad } from './$types';
import { error } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}`);
	if (!response.ok) {
		// Engine always returns 403 here, even for a nonexistent project
		// (anti-enumeration design) — never try to distinguish the two cases.
		throw error(403, 'You do not have access to this project.');
	}
	const project = await response.json();
	return { project };
};

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
