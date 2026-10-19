import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(
	fetchMock: ReturnType<typeof vi.fn>,
	url: string,
	id = PROJECT,
	signal?: AbortSignal
) {
	return {
		fetch: fetchMock,
		params: { id },
		url: new URL(url),
		request: new Request(url, { headers: { cookie: 'id=session' }, signal }),
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

/** One chunk, then nothing: the stream never ends. */
function neverEndingStream() {
	return new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(new TextEncoder().encode('{"a":1}\n'));
		}
	});
}

describe('GET /projects/[id]/logs/export', () => {
	it('relays the stream without waiting for its end, with only safe headers', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(neverEndingStream(), {
				status: 200,
				headers: {
					'content-type': 'application/x-ndjson',
					'content-disposition': 'attachment; filename="nami-logs-20261019T080503Z.ndjson"',
					'x-nami-export-limit': '100000',
					'set-cookie': 'leak=1',
					'x-internal': 'no'
				}
			})
		);
		const response = await GET(
			fakeEvent(
				fetchMock,
				`http://ui.test/projects/${PROJECT}/logs/export?format=ndjson&from=a&to=b&columns=x.y&level=error&live=1&cursor=c&token=s`
			)
		);
		expect(response.status).toBe(200);
		expect(fetchMock.mock.calls[0][0]).toBe(
			`http://localhost:8080/projects/${PROJECT}/logs/export?format=ndjson&from=a&to=b&columns=x.y&level=error`
		);
		expect(response.headers.get('content-type')).toBe('application/x-ndjson');
		expect(response.headers.get('content-disposition')).toBe(
			'attachment; filename="nami-logs-20261019T080503Z.ndjson"'
		);
		expect(response.headers.get('x-nami-export-limit')).toBe('100000');
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(response.headers.get('x-content-type-options')).toBe('nosniff');
		expect(response.headers.get('set-cookie')).toBeNull();
		expect(response.headers.get('x-internal')).toBeNull();
		const first = await response.body!.getReader().read();
		expect(new TextDecoder().decode(first.value)).toBe('{"a":1}\n');
	});

	it('relays chunk by chunk, pulling from the engine only as the client reads', async () => {
		let pulls = 0;
		const upstream = new ReadableStream<Uint8Array>(
			{
				pull(controller) {
					pulls += 1;
					controller.enqueue(new TextEncoder().encode(`chunk ${pulls}\n`));
				}
			},
			{ highWaterMark: 0 }
		);
		const fetchMock = vi
			.fn()
			.mockResolvedValue(
				new Response(upstream, {
					status: 200,
					headers: { 'content-type': 'text/csv; charset=utf-8' }
				})
			);
		const response = await GET(
			fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
		);
		const reader = response.body!.getReader();
		expect(new TextDecoder().decode((await reader.read()).value)).toBe('chunk 1\n');
		expect(new TextDecoder().decode((await reader.read()).value)).toBe('chunk 2\n');
		await new Promise((resolve) => setTimeout(resolve, 20));
		// An endless upstream, yet only about what was read has been pulled.
		expect(pulls).toBeLessThanOrEqual(4);
		await reader.cancel();
	});

	it('ends the download with an error when the engine stream breaks', async () => {
		let fail!: (reason: unknown) => void;
		const broken = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(new TextEncoder().encode('dt,level\r\n'));
				fail = (reason) => controller.error(reason);
			}
		});
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(broken, {
				status: 200,
				headers: { 'content-type': 'text/csv; charset=utf-8' }
			})
		);
		const response = await GET(
			fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
		);
		const reader = response.body!.getReader();
		expect(new TextDecoder().decode((await reader.read()).value)).toBe('dt,level\r\n');
		fail(new TypeError('terminated'));
		// Never a clean `done`: the adapter turns this into a cut connection.
		await expect(reader.read()).rejects.toThrow('terminated');
	});

	it('drops an unexpected disposition and refuses an unexpected content type', async () => {
		const odd = vi.fn().mockResolvedValue(
			new Response('x', {
				status: 200,
				headers: {
					'content-type': 'text/csv; charset=utf-8',
					'content-disposition': 'attachment; filename="../../evil.sh"',
					'x-nami-export-limit': '1e9; drop'
				}
			})
		);
		const response = await GET(
			fakeEvent(odd, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
		);
		expect(response.headers.get('content-disposition')).toBe(
			'attachment; filename="nami-logs.csv"'
		);
		expect(response.headers.get('x-nami-export-limit')).toBeNull();
		const cancelled = vi.fn();
		const htmlBody = new ReadableStream({ cancel: cancelled });
		const html = vi.fn().mockResolvedValue(
			new Response(htmlBody, {
				status: 200,
				headers: { 'content-type': 'text/html' }
			})
		);
		const refused = await GET(
			fakeEvent(html, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
		);
		expect(refused.status).toBe(502);
		expect(await refused.json()).toEqual({
			error: 'unexpected export response'
		});
		expect(cancelled).toHaveBeenCalled();
	});

	it('passes engine errors through and rejects a bad project id', async () => {
		for (const status of [400, 401, 403, 404, 429, 500]) {
			const fetchMock = vi
				.fn()
				.mockResolvedValue(new Response(JSON.stringify({ error: 'nope' }), { status }));
			const response = await GET(
				fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
			);
			expect(response.status).toBe(status);
			expect(response.headers.get('cache-control')).toBe('no-store');
			expect(await response.json()).toEqual({ error: 'nope' });
		}
		await expect(GET(fakeEvent(vi.fn(), 'http://ui.test/x', 'not-a-uuid'))).rejects.toMatchObject({
			status: 404
		});
	});

	it('answers 502 when the engine is unreachable', async () => {
		const fetchMock = vi.fn().mockRejectedValue(new TypeError('fetch failed'));
		const response = await GET(
			fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`)
		);
		expect(response.status).toBe(502);
		expect(await response.json()).toEqual({
			error: 'the log service is unreachable'
		});
	});

	it('aborts the engine request when the browser goes away', async () => {
		const controller = new AbortController();
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(neverEndingStream(), {
				status: 200,
				headers: { 'content-type': 'text/csv; charset=utf-8' }
			})
		);
		await GET(
			fakeEvent(
				fetchMock,
				`http://ui.test/projects/${PROJECT}/logs/export?format=csv`,
				PROJECT,
				controller.signal
			)
		);
		const init = fetchMock.mock.calls[0][1] as RequestInit;
		controller.abort();
		expect(init.signal?.aborted).toBe(true);
	});
});
