import type { Actions } from './$types';
import { redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';

export const actions: Actions = {
	login: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/login', {
			method: 'POST',
			body: JSON.stringify({
				email: form.get('email'),
				password: form.get('password')
			})
		});
		const body = await response.json();
		if (!response.ok) {
			return { success: false, message: body.error ?? 'login failed' };
		}
		if (body.requires_totp) {
			return { success: true, requiresTotp: true };
		}
		throw redirect(303, '/');
	},

	verifyTotp: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/totp/verify', {
			method: 'POST',
			body: JSON.stringify({ code: form.get('code') })
		});
		if (!response.ok) {
			return { success: true, requiresTotp: true, totpError: 'Invalid code. Try again.' };
		}
		throw redirect(303, '/');
	}
};
