import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT) {
	return {
		fetch: fetchMock,
		params: { id },
		url: new URL(url),
		request: new Request(url, { headers: { cookie: 'id=session' } }),
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

describe('GET /projects/[id]/logs/search', () => {
	it('forwards only search parameters and passes the page through', async () => {
		const page = { logs: [], next_cursor: null };
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify(page), {
				status: 200,
				headers: { 'content-type': 'application/json' }
			})
		);
		const event = fakeEvent(
			fetchMock,
			`http://ui.test/projects/${PROJECT}/logs/search?from=a&to=b&cursor=c&limit=100&level=error&col=x&live=1&token=s`
		);
		const response = await GET(event);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual(page);
		expect(fetchMock.mock.calls[0][0]).toBe(
			`http://localhost:8080/projects/${PROJECT}/logs?from=a&to=b&cursor=c&limit=100&level=error`
		);
	});

	it('passes engine errors (400, 401, 403) through with their status', async () => {
		for (const status of [400, 401, 403]) {
			const fetchMock = vi
				.fn()
				.mockResolvedValue(new Response(JSON.stringify({ error: 'nope' }), { status }));
			const response = await GET(
				fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/search?cursor=x`)
			);
			expect(response.status).toBe(status);
			expect(await response.json()).toEqual({ error: 'nope' });
		}
	});

	it('refuses a project id that is not a UUID', async () => {
		const fetchMock = vi.fn();
		await expect(GET(fakeEvent(fetchMock, 'http://ui.test/x', 'nope'))).rejects.toMatchObject({
			status: 404
		});
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
