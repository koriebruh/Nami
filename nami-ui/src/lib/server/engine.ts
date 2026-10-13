import type { RequestEvent } from '@sveltejs/kit';

// NOTE: this SvelteKit version (3.x, in this repo) replaced the old
// "$env/dynamic/private" arbitrary-runtime-var object with a declared/typed
// env system requiring a schema entry file; an undeclared var like
// ENGINE_URL silently resolves to `undefined` through it (confirmed via a
// `vite build` IMPORT_IS_UNDEFINED warning). `$lib` itself was removed too
// (see `#lib` alias below, from package.json "imports"). To keep this
// minimal and match the brief's intent, read `process.env` directly, which
// Node (the dev/runtime environment here) always provides.
export async function callEngine(
	event: RequestEvent,
	path: string,
	init: RequestInit = {}
): Promise<Response> {
	const engineUrl = process.env.ENGINE_URL ?? 'http://localhost:8080';
	const headers = new Headers(init.headers);
	const incomingCookie = event.request.headers.get('cookie');
	if (incomingCookie) headers.set('cookie', incomingCookie);
	if (!headers.has('content-type') && init.body) headers.set('content-type', 'application/json');

	// nami-engine rate-limits /auth/register and /auth/login per client IP
	// (SmartIpKeyExtractor, reading X-Forwarded-For). Since nami-ui is the
	// only caller of nami-engine, we must forward the real end-user IP here,
	// or every user would collapse into nami-ui's own address for limiting.
	const clientAddress = event.getClientAddress?.();
	if (clientAddress) headers.set('x-forwarded-for', clientAddress);

	const response = await (event.fetch ?? fetch)(`${engineUrl}${path}`, { ...init, headers });

	const setCookie = response.headers.get('set-cookie');
	if (setCookie) {
		const [pair, ...attrs] = setCookie.split(';').map((s) => s.trim());
		const [name, value] = pair.split('=');
		const pathAttr = attrs.find((a) => a.toLowerCase().startsWith('path='));
		event.cookies.set(name, value, {
			path: pathAttr ? pathAttr.split('=')[1] : '/',
			httpOnly: attrs.some((a) => a.toLowerCase() === 'httponly')
		});
	}

	return response;
}
