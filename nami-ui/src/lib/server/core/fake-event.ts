// Test support: a stand-in for SvelteKit's RequestEvent, for calling the
// section's load functions and form actions in vitest. The engine is a mock
// `fetch`, so a test can pick its answers and read back the requests.
import { vi } from 'vitest';

export const PROJECT_ID = 'c13bc10a-d17f-443b-8976-c6d9b54e2798';
export const MONITOR_ID = 'f41ffdb1-c497-4b17-8d3f-70477470ef6e';
export const HEARTBEAT_ID = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';

type Options = {
	/** Route params; by default the ids above. */
	params?: Record<string, string>;
	/** The request URL, for a load that reads the query string. */
	url?: string;
	/** What every engine call answers, unless `respond` is given. */
	status?: number;
	body?: string;
	/** Answers per request; wins over `status` and `body`. */
	respond?: (url: URL, init: RequestInit) => Response | Promise<Response>;
	/** Every engine call fails the way an unreachable engine does. */
	unreachable?: boolean;
	/** The fields of the submitted form, for an action. */
	form?: Record<string, string>;
};

export function fakeEvent(options: Options = {}) {
	const fetch = vi.fn(async (input: string, init: RequestInit = {}) => {
		if (options.unreachable) throw new TypeError('fetch failed');
		if (options.respond) return options.respond(new URL(input), init);
		return new Response(options.body ?? '{}', { status: options.status ?? 200 });
	});

	let request = new Request('http://ui.test/');
	if (options.form) {
		const body = new FormData();
		for (const [name, value] of Object.entries(options.form)) body.set(name, value);
		request = new Request('http://ui.test/', { method: 'POST', body });
	}

	// What a load sets on its response. SvelteKit throws when a header is set
	// twice, so a test that cares reads the call count too.
	const setHeaders = vi.fn();

	const event = {
		params: { id: PROJECT_ID, monitorId: MONITOR_ID, heartbeatId: HEARTBEAT_ID, ...options.params },
		url: new URL(options.url ?? 'http://ui.test/'),
		fetch,
		request,
		parent: async () => ({}),
		cookies: { set: vi.fn() },
		setHeaders,
		getClientAddress: () => '203.0.113.5'
	};

	return {
		// Cast: only the parts of RequestEvent that the code under test reads exist.
		event: event as any,
		setHeaders,
		/** The engine requests made so far, as method, path, query string and body. */
		calls: () =>
			fetch.mock.calls.map(([input, init]) => {
				const url = new URL(input);
				return { method: init?.method ?? 'GET', path: url.pathname, search: url.search, body: init?.body as string | undefined };
			})
	};
}
