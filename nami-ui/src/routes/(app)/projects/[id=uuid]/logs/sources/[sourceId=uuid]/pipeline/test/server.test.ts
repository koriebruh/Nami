import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { POST } from './+server';

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const PATH = `/projects/${PROJECT_ID}/sources/${SOURCE}`;

function preview(params: Record<string, string>, respond?: () => Response) {
	const fake = fakeEvent({ params, respond });
	fake.event.request = new Request('http://ui.test/x', {
		method: 'POST',
		body: '{"rules":[],"events":[{"a":1}]}',
		headers: { 'content-type': 'application/json', origin: 'http://ui.test' }
	});
	return fake;
}

describe('POST /projects/[id]/logs/sources/[sourceId]/pipeline/test', () => {
	it('relays the JSON body to the engine preview', async () => {
		const { event, calls } = preview({ sourceId: SOURCE }, () => new Response('{"results":[]}', { status: 200 }));
		const response = await POST(event);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ results: [] });
		expect(calls()).toEqual([
			{ method: 'POST', path: `${PATH}/pipeline/test`, search: '', body: '{"rules":[],"events":[{"a":1}]}' }
		]);
	});

	it('refuses ids that are not UUIDs before calling the engine', async () => {
		const cases: Record<string, string>[] = [{ sourceId: '../../auth/me' }, { id: 'nope', sourceId: SOURCE }];
		for (const params of cases) {
			const { event, calls } = preview(params);
			await expect(POST(event)).rejects.toMatchObject({ status: 404 });
			expect(calls()).toHaveLength(0);
		}
	});
});
