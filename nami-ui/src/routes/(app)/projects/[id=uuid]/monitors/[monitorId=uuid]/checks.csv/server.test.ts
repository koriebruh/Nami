import { describe, expect, it } from 'vitest';
import { fakeEvent, MONITOR_ID, PROJECT_ID } from '#lib/server/fake-event';
import { GET } from './+server';

const FROM = '2026-05-01T00:00:00.000Z';
const TO = '2026-05-10T00:00:00.000Z';
const url = (query: string) => `http://ui.test/checks.csv?${query}`;
const exportPath = `/projects/${PROJECT_ID}/monitors/${MONITOR_ID}/checks/export`;

/** A body that sends `chunks`, then errors (or closes when `fail` is false). */
function upstream(chunks: string[], fail: boolean) {
	const enc = new TextEncoder();
	let i = 0;
	let cancelled = false;
	const body = new ReadableStream<Uint8Array>({
		pull(controller) {
			if (i < chunks.length) controller.enqueue(enc.encode(chunks[i++]));
			else if (fail) controller.error(new Error('engine aborted'));
			else controller.close();
		},
		cancel() {
			cancelled = true;
		}
	});
	return { body, wasCancelled: () => cancelled };
}

describe('checks.csv GET', () => {
	it('forwards from and to, passes the body through and sets the headers', async () => {
		const { event, calls } = fakeEvent({
			url: url(`from=${FROM}&to=${TO}&extra=1`),
			respond: () => new Response('h\r\nrow\r\n', { headers: { 'content-type': 'application/json' } })
		});
		const response = await GET(event);
		expect(response.status).toBe(200);
		expect(await response.text()).toBe('h\r\nrow\r\n');
		expect(response.headers.get('content-type')).toBe('text/csv; charset=utf-8');
		expect(response.headers.get('content-disposition')).toBe(`attachment; filename="checks-${MONITOR_ID}.csv"`);
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(calls()).toHaveLength(1);
		expect(calls()[0].path).toBe(exportPath);
		expect(new URLSearchParams(calls()[0].search).get('from')).toBe(FROM);
		expect(new URLSearchParams(calls()[0].search).get('to')).toBe(TO);
		expect([...new URLSearchParams(calls()[0].search).keys()]).toEqual(['from', 'to']);
	});

	it('re-serialises plain dates and offsets, so only ISO instants go on', async () => {
		const { event, calls } = fakeEvent({ url: url('from=2026-05-01&to=2026-05-10T02:00:00%2B02:00'), body: 'x' });
		await GET(event);
		const search = new URLSearchParams(calls()[0].search);
		expect(search.get('from')).toBe('2026-05-01T00:00:00.000Z');
		expect(search.get('to')).toBe('2026-05-10T00:00:00.000Z');
	});

	it('is a 404 for a non-UUID id, with no engine call', async () => {
		const { event, calls } = fakeEvent({ url: url(`from=${FROM}&to=${TO}`), params: { id: 'not-a-uuid' } });
		await expect(GET(event)).rejects.toMatchObject({ status: 404 });
		expect(calls()).toHaveLength(0);
	});

	it.each([
		['from=abc&to=' + TO],
		['from=' + FROM],
		['from=' + FROM + '&to=2026-13-45'],
		['from=' + FROM + '&to=' + encodeURIComponent('../auth/me')],
		['from=' + FROM + '&to=' + encodeURIComponent(TO + '&x=1')]
	])('is a 400 for %s, with no engine call', async (query) => {
		const { event, calls } = fakeEvent({ url: url(query) });
		const response = await GET(event);
		expect(response.status).toBe(400);
		expect(calls()).toHaveLength(0);
	});

	it('passes an engine 404 on', async () => {
		const { event } = fakeEvent({ url: url(`from=${FROM}&to=${TO}`), status: 404, body: '{"error":"not found"}' });
		const response = await GET(event);
		expect(response.status).toBe(404);
		expect(await response.text()).toBe('not found');
	});

	it('sends a gone session to the login page', async () => {
		const { event } = fakeEvent({ url: url(`from=${FROM}&to=${TO}`), status: 401, body: '{}' });
		await expect(GET(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('turns a busy 503 into a friendly message that keeps Retry-After', async () => {
		const { event } = fakeEvent({
			url: url(`from=${FROM}&to=${TO}`),
			respond: () =>
				new Response('{"error":"too many exports"}', { status: 503, headers: { 'retry-after': '5' } })
		});
		const response = await GET(event);
		expect(response.status).toBe(503);
		expect(response.headers.get('retry-after')).toBe('5');
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(await response.text()).toMatch(/try again/i);
	});

	it('is a 502 when the engine is unreachable', async () => {
		const { event } = fakeEvent({ url: url(`from=${FROM}&to=${TO}`), unreachable: true });
		expect((await GET(event)).status).toBe(502);
	});

	it('ends the response as a broken stream when the engine body errors midway', async () => {
		const { body } = upstream(['head\r\n', 'row1\r\n'], true);
		const { event } = fakeEvent({ url: url(`from=${FROM}&to=${TO}`), respond: () => new Response(body) });
		const response = await GET(event);
		const reader = response.body!.getReader();
		const seen: string[] = [];
		const dec = new TextDecoder();
		let failure: unknown = null;
		try {
			for (;;) {
				const { done, value } = await reader.read();
				if (done) break;
				seen.push(dec.decode(value));
			}
		} catch (cause) {
			failure = cause;
		}
		expect(seen.join('')).toBe('head\r\nrow1\r\n');
		expect(failure).toBeInstanceOf(Error);
		expect((failure as Error).message).toBe('engine aborted');
	});
});
