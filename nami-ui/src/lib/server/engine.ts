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
		const sameSiteAttr = attrs.find((a) => a.toLowerCase().startsWith('samesite='));
		// Mirror the engine's actual SameSite attribute rather than letting it
		// silently fall through to SvelteKit's own default (Lax). The engine
		// deliberately sets Lax itself (see nami-engine's router.rs) so the
		// OAuth login flow's CSRF-state cookie survives the top-level redirect
		// back from Google/GitHub -- relaying it explicitly here means a
		// future engine-side change to this attribute can't be silently
		// overridden by this proxy again.
		const sameSite = (sameSiteAttr?.split('=')[1]?.toLowerCase() ?? 'lax') as
			| 'lax'
			| 'strict'
			| 'none';
		event.cookies.set(name, value, {
			path: pathAttr ? pathAttr.split('=')[1] : '/',
			httpOnly: attrs.some((a) => a.toLowerCase() === 'httponly'),
			sameSite,
			// Mirror the engine's actual `Secure` attribute rather than
			// hardcoding one: nami-engine's `COOKIE_SECURE` env var controls
			// whether it sends `Secure` on its own Set-Cookie, and this relayed
			// cookie must always match, not silently default to insecure (or
			// hardcode secure before the engine is actually served over HTTPS).
			secure: attrs.some((a) => a.toLowerCase() === 'secure')
		});
	}

	return response;
}

export type CurrentUser = {
	id: string;
	email: string;
	global_role: string;
};

/** Returns the authenticated user via /auth/me, or null if not logged in. */
export async function getCurrentUser(event: RequestEvent): Promise<CurrentUser | null> {
	const response = await callEngine(event, '/auth/me');
	if (!response.ok) return null;
	return (await response.json()) as CurrentUser;
}

/**
 * The engine's `{ "error": "..." }` message, or `fallback` when the body is
 * not that shape (axum's own extractor rejections are plain text).
 */
export async function engineErrorMessage(response: Response, fallback: string): Promise<string> {
	try {
		const body = await response.json();
		if (body && typeof body.error === 'string') return body.error;
	} catch {
		// not JSON
	}
	return fallback;
}
