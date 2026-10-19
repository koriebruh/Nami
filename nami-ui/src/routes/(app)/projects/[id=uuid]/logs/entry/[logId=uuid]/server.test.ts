import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const LOG = '1c7f4f4e-7e3b-4d2f-8c8b-3a2e4d5c6b70';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT, logId = LOG) {
	return {
		fetch: fetchMock,
		params: { id, logId },
		url: new URL(url),
		request: new Request(url, { headers: { cookie: 'id=session' } }),
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

describe('GET /projects/[id]/logs/entry/[logId]', () => {
	it('forwards only dt and passes the row through, never cached', async () => {
		const row = { id: LOG, message: 'whole', truncated: false };
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify(row), { status: 200, headers: { 'content-type': 'application/json' } })
		);
		const response = await GET(
			fakeEvent(
				fetchMock,
				`http://ui.test/projects/${PROJECT}/logs/entry/${LOG}?dt=2026-10-13T12:00:00.123Z&cursor=x&limit=9`
			)
		);
		expect(response.status).toBe(200);
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(await response.json()).toEqual(row);
		expect(fetchMock.mock.calls[0][0]).toBe(
			`http://localhost:8080/projects/${PROJECT}/logs/${LOG}?dt=2026-10-13T12%3A00%3A00.123Z`
		);
	});

	it('passes engine errors through with their status', async () => {
		for (const status of [400, 401, 403, 404, 429]) {
			const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'nope' }), { status }));
			const response = await GET(fakeEvent(fetchMock, `http://ui.test/x?dt=y`));
			expect(response.status).toBe(status);
			expect(await response.json()).toEqual({ error: 'nope' });
		}
	});

	it('refuses ids that are not UUIDs before calling the engine', async () => {
		const fetchMock = vi.fn();
		for (const [id, logId] of [
			['nope', LOG],
			[PROJECT, '../../auth/me']
		]) {
			await expect(GET(fakeEvent(fetchMock, 'http://ui.test/x', id, logId))).rejects.toMatchObject({
				status: 404
			});
		}
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
