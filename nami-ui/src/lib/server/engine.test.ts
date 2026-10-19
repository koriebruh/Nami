import { afterEach, describe, it, expect, vi } from 'vitest';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	publicEngineUrl,
	publicSyslogAddress,
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

	// nami-engine accepts state-changing requests only with a JSON body (so a
	// form on another site can never drive it): an action without a body of its
	// own sends `{}`.
	it('sends an empty JSON object for a state-changing request without a body', async () => {
		for (const method of ['POST', 'PUT', 'PATCH', 'post']) {
			const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 200 }));
			const event = {
				fetch: fetchMock,
				request: { headers: new Headers() },
				cookies: { set: vi.fn() },
				getClientAddress: () => '203.0.113.5'
			} as any;

			await callEngine(event, '/auth/logout', { method });

			const [, init] = fetchMock.mock.calls[0];
			expect(init.body, method).toBe('{}');
			expect((init.headers as Headers).get('content-type'), method).toBe('application/json');
		}
	});

	it('keeps a body it was given, and sends none for reads and deletes', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 200 }));
		const event = {
			fetch: fetchMock,
			request: { headers: new Headers() },
			cookies: { set: vi.fn() },
			getClientAddress: () => '203.0.113.5'
		} as any;

		await callEngine(event, '/auth/login', { method: 'POST', body: '{"email":"a@b.c"}' });
		await callEngine(event, '/auth/me');
		await callEngine(event, '/projects/x/monitors/y', { method: 'DELETE' });

		const [login, me, remove] = fetchMock.mock.calls.map(([, init]) => init);
		expect(login.body).toBe('{"email":"a@b.c"}');
		expect((login.headers as Headers).get('content-type')).toBe('application/json');
		for (const init of [me, remove]) {
			expect(init.body).toBeUndefined();
			expect((init.headers as Headers).get('content-type')).toBeNull();
		}
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

describe('publicEngineUrl', () => {
	const original = process.env.PUBLIC_ENGINE_URL;
	afterEach(() => {
		if (original === undefined) delete process.env.PUBLIC_ENGINE_URL;
		else process.env.PUBLIC_ENGINE_URL = original;
	});

	function configure(value: string): string {
		process.env.PUBLIC_ENGINE_URL = value;
		return publicEngineUrl();
	}

	// The engine's public listener (PUBLIC_PORT), not its private API port
	// (8080), which only nami-ui may reach.
	it("defaults to the local engine's public listener", () => {
		delete process.env.PUBLIC_ENGINE_URL;
		expect(publicEngineUrl()).toBe('http://localhost:8081');
	});

	// `PUBLIC_ENGINE_URL=` in an env file is an empty string, not an unset
	// variable: it must not turn every ping URL into a relative path.
	it('treats an empty or blank value as unset', () => {
		for (const blank of ['', ' ', '   ', '\t\n']) {
			expect(configure(blank), JSON.stringify(blank)).toBe('http://localhost:8081');
		}
	});

	it('uses PUBLIC_ENGINE_URL without a trailing slash', () => {
		expect(configure('https://status-engine.example.com/')).toBe('https://status-engine.example.com');
		expect(configure('http://localhost:9000/')).toBe('http://localhost:9000');
		expect(configure('https://status-engine.example.com')).toBe('https://status-engine.example.com');
	});

	it('trims every trailing slash and keeps a path prefix', () => {
		expect(configure('https://example.com/engine///')).toBe('https://example.com/engine');
		expect(configure('https://example.com/a/b/')).toBe('https://example.com/a/b');
		expect(configure('https://example.com////')).toBe('https://example.com');
	});

	it('ignores whitespace around the value', () => {
		expect(configure('  https://status-engine.example.com/  ')).toBe('https://status-engine.example.com');
	});

	// Going through URL parsing also gives one spelling: lower-case scheme and
	// host, no default port.
	it('writes the URL the way the URL parser does', () => {
		expect(configure('HTTPS://Engine.Example.COM:443/Engine/')).toBe('https://engine.example.com/Engine');
		expect(configure('http://[::1]:8080/')).toBe('http://[::1]:8080');
		expect(configure('http://127.0.0.1:8080')).toBe('http://127.0.0.1:8080');
	});

	// A value that is not an absolute http(s) URL would put a broken or foreign
	// URL in front of every member, to be pasted into crontabs: refuse it loudly.
	describe('refuses a value that is not an absolute http(s) URL', () => {
		const message = /PUBLIC_ENGINE_URL must be an absolute http:\/\/ or https:\/\/ URL/;

		it.each([
			['a host without a scheme', 'engine.example.com'],
			['a host and port without a scheme', 'engine.example.com:8080'],
			['localhost with a port (parsed as a scheme)', 'localhost:8080'],
			['a scheme-relative URL', '//engine.example.com'],
			['a path', '/'],
			['a relative path', 'engine/api'],
			['ftp', 'ftp://engine.example.com'],
			['file', 'file:///etc/passwd'],
			['javascript', 'javascript:alert(1)'],
			['data', 'data:text/plain,hello'],
			['a scheme with no host', 'http://'],
			['garbage', 'not a url'],
			['garbage with a scheme', 'https://exa mple.com'],
			['punctuation', '://']
		])('%s', (_name, value) => {
			expect(() => configure(value)).toThrow(message);
		});
	});

	// It is the base that /api/v1/heartbeat/<token> is appended to, and it is
	// shown to every member, so it carries no secret and no query.
	describe('refuses a base that cannot take a path appended', () => {
		const message = /PUBLIC_ENGINE_URL must not contain credentials, a query string or a fragment/;

		it.each([
			['credentials', 'https://user:secret@engine.example.com'],
			['a user name', 'https://user@engine.example.com/'],
			['a query string', 'https://engine.example.com/?key=1'],
			['a fragment', 'https://engine.example.com/#top']
		])('%s', (_name, value) => {
			expect(() => configure(value)).toThrow(message);
		});

		// The error goes to the server log, and the value may be a secret.
		it('does not repeat the value in the error', () => {
			for (const value of ['https://user:hunter2@engine.example.com', 'hunter2:secret@engine.example.com']) {
				process.env.PUBLIC_ENGINE_URL = value;
				let message = '';
				try {
					publicEngineUrl();
				} catch (error) {
					message = (error as Error).message;
				}
				expect(message, value).toContain('PUBLIC_ENGINE_URL');
				expect(message, value).not.toContain('hunter2');
			}
		});
	});
});

describe('publicSyslogAddress', () => {
	const original = { syslog: process.env.PUBLIC_SYSLOG_ADDRESS, engine: process.env.PUBLIC_ENGINE_URL };
	afterEach(() => {
		for (const [key, value] of [
			['PUBLIC_SYSLOG_ADDRESS', original.syslog],
			['PUBLIC_ENGINE_URL', original.engine]
		] as const) {
			if (value === undefined) delete process.env[key];
			else process.env[key] = value;
		}
	});

	it('defaults to the public engine host on port 5140', () => {
		delete process.env.PUBLIC_SYSLOG_ADDRESS;
		process.env.PUBLIC_ENGINE_URL = 'https://engine.example.com';
		expect(publicSyslogAddress()).toEqual({ host: 'engine.example.com', port: 5140 });
		process.env.PUBLIC_ENGINE_URL = 'http://[::1]:8081';
		expect(publicSyslogAddress()).toEqual({ host: '::1', port: 5140 });
		process.env.PUBLIC_SYSLOG_ADDRESS = '  ';
		expect(publicSyslogAddress()).toEqual({ host: '::1', port: 5140 });
	});

	it('uses PUBLIC_SYSLOG_ADDRESS when set', () => {
		process.env.PUBLIC_SYSLOG_ADDRESS = 'logs.example.com:6514';
		expect(publicSyslogAddress()).toEqual({ host: 'logs.example.com', port: 6514 });
		process.env.PUBLIC_SYSLOG_ADDRESS = 'logs.example.com';
		expect(publicSyslogAddress()).toEqual({ host: 'logs.example.com', port: 5140 });
		process.env.PUBLIC_SYSLOG_ADDRESS = '10.0.0.5:514';
		expect(publicSyslogAddress()).toEqual({ host: '10.0.0.5', port: 514 });
		process.env.PUBLIC_SYSLOG_ADDRESS = '[2001:db8::1]:6514';
		expect(publicSyslogAddress()).toEqual({ host: '2001:db8::1', port: 6514 });
		process.env.PUBLIC_SYSLOG_ADDRESS = '[2001:db8::1]';
		expect(publicSyslogAddress()).toEqual({ host: '2001:db8::1', port: 5140 });
	});

	it('refuses bad values without echoing them', () => {
		for (const bad of [
			'2001:db8::9',
			'logs.example.com:0',
			'logs.example.com:65536',
			'logs.example.com:abc',
			'a b',
			'logs"; rm -rf /',
			'tcp://logs.example.com:5140',
			'user@logs.example.com',
			'logs.example.com/path',
			'logs.example.com\nx'
		]) {
			process.env.PUBLIC_SYSLOG_ADDRESS = bad;
			expect(() => publicSyslogAddress(), bad).toThrow(/PUBLIC_SYSLOG_ADDRESS must be/);
			try {
				publicSyslogAddress();
			} catch (e) {
				expect(String(e)).not.toContain(bad);
			}
		}
	});
});
