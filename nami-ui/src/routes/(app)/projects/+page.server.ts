import type { Actions, PageServerLoad } from './$types';
import { redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, '/projects');
	if (!response.ok) {
		// Non-admin users with no memberships legitimately get an empty list;
		// treat any non-ok response as "no projects to show" rather than an error.
		return { projects: [] };
	}
	const projects = await response.json();
	return { projects };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/projects', {
			method: 'POST',
			body: JSON.stringify({
				name: form.get('name'),
				slug: form.get('slug')
			})
		});
		const body = await response.json();
		if (!response.ok) {
			// Commonly a 403 for non-admins (project creation is admin-only) —
			// show the engine's message as-is rather than reinterpreting it.
			return { success: false, message: body.error ?? 'could not create project' };
		}
		throw redirect(303, '/projects');
	}
};
