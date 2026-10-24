import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { POST } from './+server';

const BODY = '{"query":{"levels":["error"]},"window_seconds":300}';

function preview(params: Record<string, string>, respond?: () => Response) {
	const fake = fakeEvent({ params, respond });
	fake.event.request = new Request('http://ui.test/x', {
		method: 'POST',
		body: BODY,
		headers: { 'content-type': 'application/json', origin: 'http://ui.test' }
	});
	return fake;
}

describe('POST /projects/[id]/logs/alerts/preview', () => {
	it('relays the JSON body to the engine preview', async () => {
		const { event, calls } = preview({}, () => new Response('{"count":3}', { status: 200 }));
		const response = await POST(event);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ count: 3 });
		expect(calls()).toEqual([{ method: 'POST', path: `/projects/${PROJECT_ID}/log-alerts/preview`, search: '', body: BODY }]);
	});

	it('refuses a project id that is not a UUID before calling the engine', async () => {
		const { event, calls } = preview({ id: '../../auth/me' });
		await expect(POST(event)).rejects.toMatchObject({ status: 404 });
		expect(calls()).toHaveLength(0);
	});
});
