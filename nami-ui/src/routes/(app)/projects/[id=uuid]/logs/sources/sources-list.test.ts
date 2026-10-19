import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const TOKEN = 'd'.repeat(64);
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

describe('sources list load', () => {
	it('sends no-store, calls the engine under the project and passes the list on', async () => {
		const { event, setHeaders, calls } = fakeEvent({ body: '[]' });
		await expect(load(event)).resolves.toEqual({ sources: [] });
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
		expect(calls().map((c) => c.path)).toEqual([`/projects/${PROJECT_ID}/sources`]);
	});

	it('sends a lost session to the login page', async () => {
		const { event } = fakeEvent({ status: 401 });
		await expect(load(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('answers 403 for a non-member and 500 for other engine failures', async () => {
		await expect(load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({
			status: 403
		});
		await expect(load(fakeEvent({ status: 502 }).event)).rejects.toMatchObject({ status: 500 });
	});
});

describe('sources create action', () => {
	it('posts JSON with the quota in bytes and returns the token in the result only', async () => {
		const { event, calls } = fakeEvent({
			form: { name: 'api', platform: 'http', retention_days: '30', daily_quota_mib: '2' },
			respond: () => json({ id: 's1', name: 'api', token: TOKEN, platform: 'http' }, 201)
		});
		const result = await actions.create(event);
		expect(result).toEqual({ created: { id: 's1', name: 'api', token: TOKEN } });
		const [call] = calls();
		expect(call).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/sources` });
		expect(JSON.parse(call.body!)).toEqual({
			name: 'api',
			platform: 'http',
			retention_days: 30,
			daily_byte_quota: 2 * 1024 * 1024
		});
	});

	it('rejects a bad form without calling the engine', async () => {
		const { event, calls } = fakeEvent({ form: { name: 'x', daily_quota_mib: 'lots' } });
		expect(await actions.create(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});

	it('relays the engine failure, shows a viewer the 403, and sends a lost session to login', async () => {
		const form = { name: 'api' };
		const denied = await actions.create(
			fakeEvent({ form, status: 403, body: '{"error":"insufficient project role"}' }).event
		);
		expect(denied).toMatchObject({ status: 403, data: { message: 'insufficient project role' } });
		await expect(actions.create(fakeEvent({ form, status: 401 }).event)).rejects.toMatchObject({
			status: 303,
			location: '/login'
		});
	});
});
