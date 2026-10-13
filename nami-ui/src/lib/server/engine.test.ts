import { describe, it, expect, vi } from 'vitest';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from './engine';

describe('callEngine', () => {
	it('forwards the cookie header and relays set-cookie back', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ ok: true }), {
				status: 200,
				headers: { 'set-cookie': 'id=abc123; Path=/; HttpOnly' }
			})
		);
		const setCookie = vi.fn();

		const event = {
			fetch: fetchMock,
			request: { headers: new Headers({ cookie: 'id=existing' }) },
			cookies: { set: setCookie },
			getClientAddress: () => '203.0.113.5'
		} as any;

		const response = await callEngine(event, '/auth/login', { method: 'POST' });

		expect(fetchMock).toHaveBeenCalledTimes(1);
		const [calledUrl, calledInit] = fetchMock.mock.calls[0];
		expect(calledUrl).toBe('http://localhost:8080/auth/login');
		expect(calledInit.method).toBe('POST');
		// Note: Node's native Headers does not expose header values as plain
		// enumerable properties, so `expect.objectContaining({ cookie: ... })`
		// against a Headers instance never matches in this runtime/Node
		// version regardless of implementation correctness. Assert via the
		// real Headers API instead so this test is actually meaningful.
		expect((calledInit.headers as Headers).get('cookie')).toBe('id=existing');

		expect(setCookie).toHaveBeenCalledWith('id', 'abc123', expect.objectContaining({ path: '/' }));
		expect(response.status).toBe(200);
	});

	it('relays secure=false when the engine Set-Cookie omits Secure (COOKIE_SECURE=false/unset)', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ ok: true }), {
				status: 200,
				headers: { 'set-cookie': 'id=abc123; Path=/; HttpOnly' }
			})
		);
		const setCookie = vi.fn();
		const event = {
			fetch: fetchMock,
			request: { headers: new Headers() },
			cookies: { set: setCookie },
			getClientAddress: () => '203.0.113.5'
		} as any;

		await callEngine(event, '/auth/login', { method: 'POST' });

		expect(setCookie).toHaveBeenCalledWith('id', 'abc123', expect.objectContaining({ secure: false }));
	});

	it('relays secure=true when the engine Set-Cookie includes Secure (COOKIE_SECURE=true)', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ ok: true }), {
				status: 200,
				headers: { 'set-cookie': 'id=abc123; Path=/; HttpOnly; Secure' }
			})
		);
		const setCookie = vi.fn();
		const event = {
			fetch: fetchMock,
			request: { headers: new Headers() },
			cookies: { set: setCookie },
			getClientAddress: () => '203.0.113.5'
		} as any;

		await callEngine(event, '/auth/login', { method: 'POST' });

		expect(setCookie).toHaveBeenCalledWith('id', 'abc123', expect.objectContaining({ secure: true }));
	});

	it('sets x-forwarded-for from the real client address', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 }));

		const event = {
			fetch: fetchMock,
			request: { headers: new Headers() },
			cookies: { set: vi.fn() },
			getClientAddress: () => '198.51.100.7'
		} as any;

		await callEngine(event, '/auth/register', { method: 'POST' });

		const [, init] = fetchMock.mock.calls[0];
		const headers = init.headers as Headers;
		expect(headers.get('x-forwarded-for')).toBe('198.51.100.7');
	});
});

describe('engineErrorMessage', () => {
	it("returns the engine's error message", async () => {
		const response = new Response(JSON.stringify({ error: 'interval_seconds must be at least 30' }), { status: 400 });
		expect(await engineErrorMessage(response, 'fallback')).toBe('interval_seconds must be at least 30');
	});

	it('falls back for a non-JSON body', async () => {
		const response = new Response('Failed to deserialize the JSON body', { status: 422 });
		expect(await engineErrorMessage(response, 'Could not save monitor.')).toBe('Could not save monitor.');
	});
});

describe('enginePath', () => {
	const id = 'c13bc10a-d17f-443b-8976-c6d9b54e2798';

	it('leaves ids as they are', () => {
		expect(enginePath`/projects/${id}/monitors/${id}/stats`).toBe(`/projects/${id}/monitors/${id}/stats`);
	});

	// A route param or a form field reaches a load or action decoded, so it can
	// carry the characters that change where a request goes.
	it('keeps a hostile value inside its own path segment', () => {
		const path = enginePath`/projects/${'../auth/me?'}/monitors`;
		expect(path).toBe('/projects/..%2Fauth%2Fme%3F/monitors');

		// Starts with the intended prefix, ends with the intended suffix, and
		// nothing between them is a raw "/", "?" or "#".
		const value = path.slice('/projects/'.length, -'/monitors'.length);
		expect(value).not.toMatch(/[/?#]/);

		// Built as a URL it stays one segment instead of becoming /auth/me.
		const url = new URL(path, 'http://engine.test');
		expect(url.pathname).toBe('/projects/..%2Fauth%2Fme%3F/monitors');
		expect(url.search).toBe('');
	});

	it('encodes a query value', () => {
		expect(enginePath`/projects/${id}/incidents?monitor_id=${'a&b=c#d e'}`).toBe(
			`/projects/${id}/incidents?monitor_id=a%26b%3Dc%23d%20e`
		);
	});

	it('writes other values as text', () => {
		expect(enginePath`/n/${42}/${true}`).toBe('/n/42/true');
	});

	// URL parsing removes "." and ".." segments even when they are percent-encoded,
	// so encoding cannot make them safe: the helper refuses them.
	it('refuses a value that is a dot segment', () => {
		expect(new URL('/projects/%2E%2E/monitors', 'http://engine.test').pathname).toBe('/monitors');
		expect(() => enginePath`/projects/${'..'}/monitors`).toThrow(/dot segment/);
		expect(() => enginePath`/projects/${'.'}/monitors`).toThrow(/dot segment/);
	});

	it('reaches the engine as the path that was built', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 200 }));
		const event = {
			fetch: fetchMock,
			request: { headers: new Headers() },
			cookies: { set: vi.fn() },
			getClientAddress: () => '203.0.113.5'
		} as any;

		await callEngine(event, enginePath`/projects/${'../auth/me?'}/monitors`);

		const url = new URL(fetchMock.mock.calls[0][0]);
		expect(url.pathname).toBe('/projects/..%2Fauth%2Fme%3F/monitors');
		expect(url.search).toBe('');
	});
});

function thrownBy(fn: () => unknown): unknown {
	try {
		fn();
	} catch (thrown) {
		return thrown;
	}
	return undefined;
}

describe('redirectToLoginIfUnauthorized', () => {
	it('sends a visitor whose session is gone to the login page', () => {
		const thrown = thrownBy(() => redirectToLoginIfUnauthorized(new Response(null, { status: 401 })));
		expect(thrown).toMatchObject({ status: 303, location: '/login' });
	});

	it('lets every other status through', () => {
		for (const status of [200, 400, 403, 404, 500]) {
			expect(thrownBy(() => redirectToLoginIfUnauthorized(new Response(null, { status })))).toBeUndefined();
		}
	});
});

describe('engineFailure', () => {
	it("is a failure with the engine's message and status", async () => {
		const response = new Response(JSON.stringify({ error: 'forbidden' }), { status: 403 });
		expect(await engineFailure(response, 'fallback')).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});

	it('uses the fallback for a body that is not the engine error shape', async () => {
		const response = new Response('Failed to deserialize the JSON body', { status: 422 });
		expect(await engineFailure(response, 'Could not save monitor.')).toMatchObject({
			status: 422,
			data: { message: 'Could not save monitor.' }
		});
	});

	it('redirects to the login page instead when the session is gone', async () => {
		const response = new Response(JSON.stringify({ error: 'unauthorized' }), { status: 401 });
		await expect(engineFailure(response, 'fallback')).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});
