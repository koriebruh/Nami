import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

// The project page is now a section (see +layout.svelte): land on its
// first tab. The member forms moved to ./settings.
export const load: PageServerLoad = async (event) => {
	// Wait for the layouts first. They run in parallel with this load, and a
	// redirect that wins the race ends the response before their engine calls
	// relay the session cookie: SvelteKit then logs cookies_set_after_response
	// on every visit. It also gives a project the user cannot open the
	// layout's 403 here, instead of after the redirect.
	await event.parent();
	throw redirect(307, `/projects/${event.params.id}/monitors`);
};
