import type { Actions } from './$types';
import { callEngine } from '#lib/server/engine';

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/register', {
			method: 'POST',
			body: JSON.stringify({
				email: form.get('email'),
				password: form.get('password')
			})
		});
		const body = await response.json();
		return { success: response.ok, message: body.message ?? body.error };
	}
};
