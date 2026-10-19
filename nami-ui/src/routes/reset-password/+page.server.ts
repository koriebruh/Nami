import type { Actions, PageServerLoad } from './$types';
import { redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/core/engine';

export const load: PageServerLoad = async (event) => {
	const token = event.url.searchParams.get('token');
	return { token };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/password/reset', {
			method: 'POST',
			body: JSON.stringify({
				token: form.get('token'),
				new_password: form.get('new_password')
			})
		});
		const body = await response.json();
		if (!response.ok) {
			return { success: false, message: body.error ?? 'reset failed' };
		}
		throw redirect(303, '/login');
	}
};
