// SvelteKit rejects some route-module shapes only at runtime (a 500 on every
// request), which tests that import a module and call its functions never
// see. This test applies the same checks to every route module.
import { describe, expect, it } from 'vitest';

// Every module form SvelteKit loads: a page or layout (optionally `@`-reset to
// another layout, optionally `.server`) and an endpoint, in TS or JS.
const modules: Record<string, Record<string, unknown>> = import.meta.glob(
	[
		'./**/+{page,layout}.{ts,js}',
		'./**/+{page,layout}.server.{ts,js}',
		'./**/+{page,layout}@*.{ts,js}',
		'./**/+server.{ts,js}'
	],
	{ eager: true }
);

// Every `+` script file under routes, found without importing it. The glob
// above must match exactly these, so a form it misses fails the test.
const plusFiles = Object.keys(
	import.meta.glob(['./**/+*.{ts,js}', '!./**/*.{test,spec}.{ts,js}'])
).sort();

// `+page@(app).server.ts` checks as `+page.server.ts`; null for a file that is
// not a route module.
function kindOf(file: string): string | null {
	const match = /\/\+(?:(page|layout)(?:@[^/.]*)?(\.server)?|(server))\.(?:ts|js)$/.exec(file);
	if (!match) return null;
	return `+${match[1] ?? match[3]}${match[2] ?? ''}.ts`;
}

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
		expect(plusFiles.length).toBeGreaterThan(0);
		expect(Object.keys(modules).sort()).toEqual(plusFiles);
	});

	it('recognises every file kind', () => {
		const unknown = Object.keys(modules).filter((file) => kindOf(file) === null);
		expect(unknown).toEqual([]);
		expect(kindOf('./a/+page@(app).server.js')).toBe('+page.server.ts');
		expect(kindOf('./a/+layout@[id].ts')).toBe('+layout.ts');
		expect(kindOf('./a/+server.js')).toBe('+server.ts');
		expect(kindOf('./a/+page.svelte')).toBeNull();
	});

	it('export only what SvelteKit allows in each file kind', () => {
		const invalid: string[] = [];
		for (const [file, module] of Object.entries(modules)) {
			const kind = allowed[kindOf(file) ?? ''];
			if (!kind) continue; // reported by 'recognises every file kind'
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
