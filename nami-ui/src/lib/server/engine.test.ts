import { describe, it, expect, vi } from 'vitest';
import { callEngine } from './engine';

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
