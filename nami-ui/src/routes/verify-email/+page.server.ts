import type { PageServerLoad } from './$types';
import { callEngine, enginePath } from '#lib/server/core/engine';

// The link in the verification email lands here. Any failure (no token, a
// used or expired one, an engine error) shows the same generic message.
export const load: PageServerLoad = async (event) => {
	const token = event.url.searchParams.get('token');
	if (!token) return { verified: false };
	try {
		const response = await callEngine(event, enginePath`/auth/verify-email?token=${token}`);
		return { verified: response.ok };
	} catch {
		return { verified: false };
	}
};
