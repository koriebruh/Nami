import type { Actions } from './$types';
import { callEngine } from '#lib/server/core/engine';

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/password/forgot', {
			method: 'POST',
			body: JSON.stringify({
				email: form.get('email')
			})
		});
		const body = await response.json();
		if (!response.ok) {
			return { message: body.message ?? 'If this email exists, a reset link was sent.' };
		}
		return { message: body.message };
	}
};
