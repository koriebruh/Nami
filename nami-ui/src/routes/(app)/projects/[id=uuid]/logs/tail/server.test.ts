import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT) {
	const controller = new AbortController();
	return {
		event: {
			fetch: fetchMock,
			params: { id },
			url: new URL(url),
			request: new Request(url, {
				headers: { cookie: 'id=session' },
				signal: controller.signal
			}),
			cookies: { set: vi.fn() },
			getClientAddress: () => '203.0.113.5'
		} as any,
		controller
	};
}

describe('GET /projects/[id]/logs/tail', () => {
	it('streams the engine body through without waiting for it to end', async () => {
		let push!: (text: string) => void;
		const body = new ReadableStream<Uint8Array>({
			start(controller) {
				push = (text) => controller.enqueue(new TextEncoder().encode(text));
			}
		});
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(body, {
				status: 200,
				headers: { 'content-type': 'text/event-stream' }
			})
		);
		const { event } = fakeEvent(
			fetchMock,
			`http://ui.test/projects/${PROJECT}/logs/tail?level=error&secret=x`
		);

		const response = await GET(event);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('text/event-stream');
		expect(response.headers.get('cache-control')).toContain('no-store');
		expect(response.headers.get('x-accel-buffering')).toBe('no');

		// The engine stream is still open: the first event must come through anyway.
		push('event: log\ndata: {"message":"hi"}\n\n');
		const reader = response.body!.getReader();
		const { value } = await reader.read();
		expect(new TextDecoder().decode(value)).toContain('"message":"hi"');
		await reader.cancel();

		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe(`http://localhost:8080/projects/${PROJECT}/logs/tail?level=error`);
		expect((init.headers as Headers).get('cookie')).toBe('id=session');
		expect(init.signal).toBe(event.request.signal);
	});

	it('passes engine errors through', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'unauthorized' }), {
				status: 401,
				headers: { 'content-type': 'application/json' }
			})
		);
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail`);
		const response = await GET(event);
		expect(response.status).toBe(401);
		expect(await response.json()).toEqual({ error: 'unauthorized' });
	});

	it('cancelling the browser side cancels the engine stream', async () => {
		const cancelled = vi.fn();
		const body = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(new TextEncoder().encode(': hi\n\n'));
			},
			cancel: cancelled
		});
		const fetchMock = vi.fn().mockResolvedValue(new Response(body, { status: 200 }));
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail`);
		const response = await GET(event);
		const reader = response.body!.getReader();
		await reader.read();
		await reader.cancel();
		expect(cancelled).toHaveBeenCalledTimes(1);
	});

	it.each([403, 429])('passes an engine %i through with its body', async (status) => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'nope' }), {
				status,
				headers: { 'content-type': 'application/json' }
			})
		);
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail`);
		const response = await GET(event);
		expect(response.status).toBe(status);
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(await response.json()).toEqual({ error: 'nope' });
	});

	it('answers 502 when the engine is unreachable', async () => {
		const fetchMock = vi.fn().mockRejectedValue(new TypeError('fetch failed'));
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail`);
		const response = await GET(event);
		expect(response.status).toBe(502);
		expect(await response.json()).toEqual({
			error: 'the log service is unreachable'
		});
	});

	it('refuses a project id that is not a UUID before calling the engine', async () => {
		const fetchMock = vi.fn();
		const { event } = fakeEvent(fetchMock, 'http://ui.test/projects/x/logs/tail', '../monitors');
		await expect(GET(event)).rejects.toMatchObject({ status: 404 });
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
