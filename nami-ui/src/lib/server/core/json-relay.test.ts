import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { MAX_RELAY_BYTES, relayJsonPost } from './json-relay';

function post(event: any, body: string, headers: Record<string, string> = {}) {
	event.request = new Request('http://ui.test/x', {
		method: 'POST',
		body,
		headers: { 'content-type': 'application/json', origin: 'http://ui.test', ...headers }
	});
	event.url = new URL('http://ui.test/x');
	return event;
}

describe('relayJsonPost', () => {
	it('forwards the JSON body and passes the answer through', async () => {
		const { event, calls } = fakeEvent({ respond: () => new Response('{"results":[]}', { status: 200 }) });
		const response = await relayJsonPost(post(event, '{"rules":[],"events":[]}'), `/projects/${PROJECT_ID}/sources/s/pipeline/test`);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ results: [] });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/sources/s/pipeline/test`, body: '{"rules":[],"events":[]}' });
	});

	it('refuses other content types, other origins, bad JSON and large bodies', async () => {
		const cases: [string, Record<string, string>, number][] = [
			['{}', { 'content-type': 'text/plain' }, 415],
			['{}', { origin: 'http://evil.test' }, 403],
			['{nope', {}, 400],
			['"x"'.padEnd(MAX_RELAY_BYTES + 10, ' '), {}, 413]
		];
		for (const [body, headers, status] of cases) {
			const { event, calls } = fakeEvent();
			const response = await relayJsonPost(post(event, body, headers), '/x');
			expect(response.status, JSON.stringify(headers)).toBe(status);
			expect(calls()).toHaveLength(0);
		}
	});

	it('passes engine errors through with their message', async () => {
		for (const status of [400, 403, 404, 429]) {
			const { event } = fakeEvent({ respond: () => new Response('{"error":"rules[1]: bad"}', { status }) });
			const response = await relayJsonPost(post(event, '{}'), '/x');
			expect(response.status).toBe(status);
			expect(await response.json()).toEqual({ error: 'rules[1]: bad' });
		}
	});
});
