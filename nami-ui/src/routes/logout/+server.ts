import { redirect } from '@sveltejs/kit';
import { callEngine } from '#lib/server/core/engine';
import type { RequestHandler } from './$types';

export const POST: RequestHandler = async (event) => {
	await callEngine(event, '/auth/logout', { method: 'POST' });
	throw redirect(303, '/login');
};
