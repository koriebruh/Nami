import { describe, expect, it, vi } from 'vitest';
import { load as loadDetail } from './+page.server';
import { load as loadEdit } from './edit/+page.server';

// The engine answers every endpoint of a monitor with the same status here,
// so only the status mapping of the page loads is under test.
function eventAnswering(status: number, body: string) {
	return {
		params: { id: 'p', monitorId: 'm' },
		fetch: vi.fn().mockImplementation(async () => new Response(body, { status })),
		request: { headers: new Headers() },
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

describe.each([
	['detail', loadDetail],
	['edit', loadEdit]
] as const)('%s page load', (_name, load) => {
	it('is a 404 for a monitor the engine does not have', async () => {
		await expect(load(eventAnswering(404, '{"error":"not found"}'))).rejects.toMatchObject({ status: 404 });
	});

	// axum rejects an id that is not a UUID with a plain-text 400, before any
	// lookup: for the visitor that is still "no such monitor", not a server error.
	it('is a 404 for a monitor id the engine rejects as malformed', async () => {
		await expect(load(eventAnswering(400, 'Invalid URL: invalid character'))).rejects.toMatchObject({ status: 404 });
	});

	it('keeps a 403 a 403', async () => {
		await expect(load(eventAnswering(403, '{"error":"forbidden"}'))).rejects.toMatchObject({ status: 403 });
	});

	it('turns any other engine failure into a 500', async () => {
		await expect(load(eventAnswering(500, '{"error":"internal error"}'))).rejects.toMatchObject({ status: 500 });
	});
});
