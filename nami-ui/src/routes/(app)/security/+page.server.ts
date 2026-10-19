import type { Actions } from './$types';
import { callEngine } from '#lib/server/core/engine';

export const actions: Actions = {
	setup: async (event) => {
		const response = await callEngine(event, '/auth/totp/setup', {
			method: 'POST'
		});
		const body = await response.json();
		if (!response.ok) {
			return { step: 'setup' as const, error: 'could not start 2FA setup' };
		}
		return { step: 'setup' as const, provisioningUri: body.provisioning_uri };
	},

	enable: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/totp/enable', {
			method: 'POST',
			body: JSON.stringify({ code: form.get('code') })
		});
		const body = await response.json();
		if (!response.ok) {
			return { step: 'enable' as const, success: false, message: body.error ?? 'invalid code' };
		}
		return { step: 'enable' as const, success: true };
	}
};
