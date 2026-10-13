import { defineParams } from '@sveltejs/kit/params';
// The ".ts" is needed: `vite build` loads this file with Node itself, which
// does not resolve extensionless imports.
import { isUuid } from './lib/uuid.ts';

// SvelteKit decodes a route param (%2F becomes "/") before it runs the matcher
// of a `[id=uuid]` segment, and a value the matcher refuses never becomes a
// param: the URL is a 404. Ids that end up in engine paths are therefore
// UUIDs, never something like `../auth/me?`.
export const params = defineParams({
	uuid: (param) => (isUuid(param) ? param : undefined)
});
