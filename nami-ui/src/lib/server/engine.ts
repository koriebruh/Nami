import { fail, redirect, type RequestEvent } from '@sveltejs/kit';

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
	// nami-engine accepts a state-changing request only with a JSON body (415
	// otherwise), so that no form on another site can ever drive it: an action
	// that has no body of its own (logout, rotate-token, 2FA setup) sends `{}`.
	let body = init.body;
	const method = (init.method ?? 'GET').toUpperCase();
	if (body == null && (method === 'POST' || method === 'PUT' || method === 'PATCH')) body = '{}';
	if (!headers.has('content-type') && body) headers.set('content-type', 'application/json');

	// nami-engine rate-limits /auth/register and /auth/login per client IP
	// (SmartIpKeyExtractor, reading X-Forwarded-For). nami-ui is the only
	// caller of nami-engine's API port (ENGINE_URL), which must never be
	// exposed, so we must forward the real end-user IP here, or every user
	// would collapse into nami-ui's own address for limiting.
	const clientAddress = event.getClientAddress?.();
	if (clientAddress) headers.set('x-forwarded-for', clientAddress);

	const response = await (event.fetch ?? fetch)(`${engineUrl}${path}`, { ...init, body, headers });

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

/**
 * Builds a path from a template literal, percent-encoding every interpolated
 * value so that it stays one path segment (or one query value). Use it for
 * every engine path and every redirect target that has a route param or a
 * form field in it: SvelteKit hands those over decoded, so a value like
 * `../auth/me?` would otherwise turn `/projects/${id}/monitors` into a request
 * for `/auth/me` (URL parsing resolves the dots) or cut the path short at the
 * `?`.
 *
 * A value that is exactly `.` or `..` is refused: URL parsing removes dot
 * segments even when they are percent-encoded (`%2E%2E`), so encoding cannot
 * make them safe.
 */
export function enginePath(strings: TemplateStringsArray, ...values: unknown[]): string {
	let path = strings[0];
	values.forEach((value, i) => {
		const text = String(value);
		if (text === '.' || text === '..') {
			throw new Error(`enginePath: "${text}" would be removed from the path as a dot segment`);
		}
		path += encodeURIComponent(text) + strings[i + 1];
	});
	return path;
}

/**
 * The engine answers 401 when the session is gone (expired, or ended by a
 * password change). A visitor in that state goes to the login page, where a
 * form or a page error would only say "unauthorized". Throws the redirect.
 */
export function redirectToLoginIfUnauthorized(response: Response): void {
	if (response.status === 401) throw redirect(303, '/login');
}

/**
 * The `fail()` for a form action whose engine call did not succeed: the
 * engine's own message, or `fallback`, with the engine's status. A 401 goes to
 * the login page instead (see `redirectToLoginIfUnauthorized`).
 */
export async function engineFailure(response: Response, fallback: string) {
	redirectToLoginIfUnauthorized(response);
	return fail(response.status, { message: await engineErrorMessage(response, fallback) });
}

/**
 * nami-engine's public listener (its PUBLIC_PORT, 8081 by default) as your
 * cron jobs reach it, for heartbeat ping URLs. Pings go straight to the engine
 * (cron jobs are not browser sessions), but never to the API port this BFF
 * calls (ENGINE_URL), which stays private: the public listener serves the
 * ping endpoint and nothing of the API.
 *
 * An empty or blank `PUBLIC_ENGINE_URL` (`PUBLIC_ENGINE_URL=` in an env file)
 * counts as unset. Anything else must be an absolute http(s) URL, a base that
 * `/api/v1/heartbeat/<token>` can be appended to: it is shown to every member
 * of a project and pasted into crontabs, so a scheme-less host, another scheme,
 * credentials or a query string is a configuration error, not something to
 * show. The message leaves the value out, since it may hold a secret.
 */
export function publicEngineUrl(): string {
	const configured = (process.env.PUBLIC_ENGINE_URL ?? '').trim();
	if (configured === '') return 'http://localhost:8081';

	let url: URL | undefined;
	try {
		url = new URL(configured);
	} catch {
		// Reported below, together with a URL of the wrong scheme.
	}
	if (!url || (url.protocol !== 'http:' && url.protocol !== 'https:')) {
		throw new Error(
			'PUBLIC_ENGINE_URL must be an absolute http:// or https:// URL, for example https://engine.example.com'
		);
	}
	if (url.username || url.password || url.search || url.hash) {
		throw new Error(
			'PUBLIC_ENGINE_URL must not contain credentials, a query string or a fragment: it is the base that /api/v1/heartbeat/<token> is appended to'
		);
	}
	// Scheme, host and port as the URL parser writes them, plus the path prefix
	// without its trailing slash(es).
	return (url.origin + url.pathname).replace(/\/+$/, '');
}

/**
 * Where syslog senders reach nami-engine's syslog listener, for the setup
 * snippets: PUBLIC_SYSLOG_ADDRESS ("host" or "host:port", an IPv6 host in
 * brackets), else the public engine host on the default port 5140. The host
 * ends up inside a quoted rsyslog setting and is shown to every project
 * member, so anything but a plain host name, IPv4 or IPv6 address and a port
 * from 1 to 65535 is a configuration error. The message leaves the value out.
 * The returned host has no IPv6 brackets (rsyslog's `target` takes it bare).
 */
export function publicSyslogAddress(): { host: string; port: number } {
	const configured = (process.env.PUBLIC_SYSLOG_ADDRESS ?? '').trim();
	if (configured === '') {
		return { host: new URL(publicEngineUrl()).hostname.replace(/^\[|\]$/g, ''), port: 5140 };
	}
	const match = /^(\[[0-9a-fA-F:.]+\]|[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?)(?::(\d{1,5}))?$/.exec(configured);
	const port = match?.[2] === undefined ? 5140 : Number(match[2]);
	if (!match || port < 1 || port > 65535) {
		throw new Error(
			'PUBLIC_SYSLOG_ADDRESS must be a host name or IP address with an optional port, for example logs.example.com:5140 (an IPv6 address in brackets: [2001:db8::1]:5140)'
		);
	}
	return { host: match[1].replace(/^\[|\]$/g, ''), port };
}
