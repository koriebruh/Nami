import { error } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}`);
	if (!response.ok) {
		// Engine always returns 403 here, even for a nonexistent project
		// (anti-enumeration design) — never try to distinguish the two cases.
		throw error(403, 'You do not have access to this project.');
	}
	const project: { id: string; name: string; slug: string } = await response.json();
	return { project };
};
