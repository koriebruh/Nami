import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const SOURCE_ID = '7a1c3e5f-2b4d-4f60-8a9b-0c1d2e3f4a5b';
const TOKEN = 'e'.repeat(64);
const PATH = `/projects/${PROJECT_ID}/sources/${SOURCE_ID}`;
const params = { id: PROJECT_ID, sourceId: SOURCE_ID };
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const source = { id: SOURCE_ID, name: 'api', platform: 'http', retention_days: 30, daily_byte_quota: null };

describe('source detail load', () => {
	it('sends no-store once, loads the source and 30 days of usage, and never carries a token', async () => {
		const usage = [
			{ day: '2026-10-12', events: 1, bytes: 30 },
			{ day: '2026-10-13', events: 3, bytes: 90 }
		];
		const { event, setHeaders, calls } = fakeEvent({
			params,
			respond: (url) => (url.pathname.endsWith('/usage') ? json(usage) : json(source))
		});
		const data = await load(event);
		expect(data).toMatchObject({ source, today: usage[1], usage, syslog: { port: 5140 } });
		expect(JSON.stringify(data)).not.toContain(TOKEN);
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
		expect(calls().map((c) => c.path + c.search)).toEqual([PATH, `${PATH}/usage?days=30`]);
	});

	it('still loads when usage is unavailable', async () => {
		const { event } = fakeEvent({
			params,
			respond: (url) => (url.pathname.endsWith('/usage') ? json({}, 500) : json(source))
		});
		expect(await load(event)).toMatchObject({ today: null, usage: [] });
	});

	it('maps 401 to login, 403 to 403, 404 to 404', async () => {
		await expect(load(fakeEvent({ params, status: 401 }).event)).rejects.toMatchObject({ status: 303, location: '/login' });
		await expect(load(fakeEvent({ params, status: 403 }).event)).rejects.toMatchObject({ status: 403 });
		await expect(load(fakeEvent({ params, status: 404 }).event)).rejects.toMatchObject({ status: 404 });
	});
});

describe('source token actions', () => {
	it('reveal puts the token in the action result only', async () => {
		const { event, calls } = fakeEvent({ params, respond: () => json({ token: TOKEN }) });
		expect(await actions.reveal(event)).toEqual({ token: TOKEN });
		expect(calls()).toEqual([{ method: 'GET', path: `${PATH}/token`, search: '', body: undefined }]);
	});

	it('reveal and rotate show a viewer the member-only message', async () => {
		const message = 'Only project members and admins can see or rotate the token.';
		expect(await actions.reveal(fakeEvent({ params, status: 403 }).event)).toMatchObject({
			status: 403,
			data: { action: 'reveal', message }
		});
		expect(await actions.rotate(fakeEvent({ params, status: 403, form: { confirm: 'yes' } }).event)).toMatchObject({
			status: 403,
			data: { action: 'rotate', message }
		});
	});

	it('rotate asks first without confirm=yes, and does not call the engine', async () => {
		const { event, calls } = fakeEvent({ params, form: {} });
		expect(await actions.rotate(event)).toEqual({ confirmRotate: true });
		expect(calls()).toEqual([]);
	});

	it('rotate with confirm=yes posts JSON and returns the new token', async () => {
		const { event, calls } = fakeEvent({ params, form: { confirm: 'yes' }, respond: () => json({ ...source, token: TOKEN }) });
		expect(await actions.rotate(event)).toEqual({ token: TOKEN, rotated: true });
		expect(calls()).toEqual([{ method: 'POST', path: `${PATH}/rotate`, search: '', body: '{}' }]);
	});

	it('sends a lost session to login', async () => {
		await expect(actions.reveal(fakeEvent({ params, status: 401 }).event)).rejects.toMatchObject({ location: '/login' });
		await expect(actions.rotate(fakeEvent({ params, status: 401, form: { confirm: 'yes' } }).event)).rejects.toMatchObject({
			location: '/login'
		});
	});
});

describe('source delete and update actions', () => {
	it('delete asks first without confirm=yes', async () => {
		const { event, calls } = fakeEvent({ params, form: {} });
		expect(await actions.delete(event)).toEqual({ confirmDelete: true });
		expect(calls()).toEqual([]);
	});

	it('delete with confirm=yes redirects to the list; a non-admin gets the admin message', async () => {
		const ok = fakeEvent({ params, form: { confirm: 'yes' }, status: 200 });
		await expect(actions.delete(ok.event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/logs/sources` });
		expect(ok.calls()).toEqual([{ method: 'DELETE', path: PATH, search: '', body: undefined }]);

		const denied = await actions.delete(fakeEvent({ params, form: { confirm: 'yes' }, status: 403 }).event);
		expect(denied).toMatchObject({ status: 403, data: { message: 'Only project admins can delete a source.' } });
	});

	it('update sends the quota as null when cleared', async () => {
		const { event, calls } = fakeEvent({
			params,
			form: { name: 'api', platform: 'http', retention_days: '7', daily_quota_mib: '' }
		});
		expect(await actions.update(event)).toEqual({ saved: true });
		expect(JSON.parse(calls()[0].body!)).toEqual({ name: 'api', platform: 'http', retention_days: 7, daily_byte_quota: null });
		expect(calls()[0].method).toBe('PATCH');
	});
});
