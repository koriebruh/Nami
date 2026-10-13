import { redirect } from '@sveltejs/kit';
import { getCurrentUser } from '#lib/server/engine';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async (event) => {
	const user = await getCurrentUser(event);
	if (!user) throw redirect(303, '/login');
	return { user };
};
