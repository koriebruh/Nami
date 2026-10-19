import { redirect } from '@sveltejs/kit';
import { enginePath } from '#lib/server/engine';
import type { PageServerLoad } from './$types';

// The explorer lands here in the next task; until then the Logs tab opens
// the sources.
export const load: PageServerLoad = async (event) => {
	throw redirect(307, enginePath`/projects/${event.params.id}/logs/sources`);
};
