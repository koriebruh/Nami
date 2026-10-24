// SvelteKit rejects some route-module shapes only at runtime (a 500 on every
// request), which tests that import a module and call its functions never
// see. This test applies the same checks to every route module.
import { describe, expect, it } from 'vitest';

const modules: Record<string, Record<string, unknown>> = import.meta.glob(
	['./**/+page.server.ts', './**/+page.ts', './**/+layout.server.ts', './**/+layout.ts', './**/+server.ts'],
	{ eager: true }
);

// Copied from @sveltejs/kit 3.0.1, src/utils/exports.js (not importable: it is
// outside the package's exports map). Update this when SvelteKit adds an export.
const layout = ['load', 'prerender', 'csr', 'ssr', 'trailingSlash', 'config'];
const allowed: Record<string, Set<string>> = {
	'+layout.ts': new Set(layout),
	'+layout.server.ts': new Set(layout),
	'+page.ts': new Set([...layout, 'entries']),
	'+page.server.ts': new Set([...layout, 'actions', 'entries']),
	'+server.ts': new Set([
		...['GET', 'POST', 'PATCH', 'PUT', 'DELETE', 'OPTIONS', 'HEAD', 'QUERY'],
		...['fallback', 'prerender', 'trailingSlash', 'config', 'entries']
	])
};

describe('route modules', () => {
	it('finds every route module', () => {
		// A glob that matched nothing would pass the checks below vacuously.
		expect(Object.keys(modules).length).toBeGreaterThanOrEqual(46);
	});

	it('export only what SvelteKit allows in each file kind', () => {
		const invalid: string[] = [];
		for (const [file, module] of Object.entries(modules)) {
			const kind = allowed[file.slice(file.lastIndexOf('/') + 1)];
			for (const key of Object.keys(module)) {
				if (key[0] !== '_' && !kind.has(key)) invalid.push(`${file}: export ${key}`);
			}
		}
		expect(invalid).toEqual([]);
	});

	it('never mix a default action with named ones', () => {
		const mixed = Object.entries(modules)
			.filter(([, module]) => {
				const actions = module.actions as Record<string, unknown> | undefined;
				return actions?.default && Object.keys(actions).length > 1;
			})
			.map(([file]) => file);
		expect(mixed).toEqual([]);
	});
});
