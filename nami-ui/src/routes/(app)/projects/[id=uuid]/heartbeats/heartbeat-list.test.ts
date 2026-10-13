import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fakeEvent, HEARTBEAT_ID, PROJECT_ID } from '#lib/server/fake-event';
import { actions, load } from './+page.server';

const heartbeatsPath = `/projects/${PROJECT_ID}/heartbeats`;

type Scenario = [name: string, engine: Parameters<typeof fakeEvent>[0], rejection: { status: number } | null];

describe('heartbeat list load', () => {
	const originalPublicUrl = process.env.PUBLIC_ENGINE_URL;
	beforeEach(() => {
		delete process.env.PUBLIC_ENGINE_URL;
	});
	afterEach(() => {
		if (originalPublicUrl === undefined) delete process.env.PUBLIC_ENGINE_URL;
		else process.env.PUBLIC_ENGINE_URL = originalPublicUrl;
	});

	it('returns the heartbeats with the engine URL that ping URLs are built on', async () => {
		process.env.PUBLIC_ENGINE_URL = 'https://engine.example.com/';
		const heartbeats = [{ id: HEARTBEAT_ID, name: 'Backup', token: 'a'.repeat(64) }];
		const { event, calls } = fakeEvent({ body: JSON.stringify(heartbeats) });
		expect(await load(event)).toEqual({ heartbeats, engineUrl: 'https://engine.example.com' });
		expect(calls()).toEqual([{ method: 'GET', path: heartbeatsPath, search: '', body: undefined }]);
	});

	// The engine leaves the ping token out (null) for a viewer. The load passes
	// that on as it is: it never makes up a token, and never drops the field.
	it('passes a null token (a viewer) through untouched', async () => {
		const heartbeats = [{ id: HEARTBEAT_ID, name: 'Backup', token: null }];
		const { event } = fakeEvent({ body: JSON.stringify(heartbeats) });
		const data = await load(event);
		expect(data).toEqual({ heartbeats, engineUrl: 'http://localhost:8081' });
		expect(data).toHaveProperty('heartbeats.0.token', null);
	});

	it('keeps a 403 a 403', async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 403, body: { message: 'forbidden' } });
	});

	it("turns any other engine failure into a 500 with the engine's message", async () => {
		const { event } = fakeEvent({ status: 500, body: '{"error":"internal error"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 500, body: { message: 'internal error' } });
	});

	it('uses a fixed message when the engine error is not its JSON shape', async () => {
		const { event } = fakeEvent({ status: 502, body: '<html>bad gateway</html>' });
		await expect(load(event)).rejects.toMatchObject({ status: 500, body: { message: 'Could not load heartbeats.' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, body: '[]' });
		await load(event);
		expect(calls().map((call) => call.path + call.search)).toEqual(['/projects/..%2Fauth%2Fme%3F/heartbeats']);
	});

	// The page carries the ping token, in its markup and in the data sent with
	// it: no browser or proxy may keep either. The header is set before anything
	// can fail, so every response of the load has it, and only once (SvelteKit
	// throws when a header is set twice).
	const scenarios: Scenario[] = [
		['a member', { body: JSON.stringify([{ id: HEARTBEAT_ID, name: 'Backup', token: 'a'.repeat(64) }]) }, null],
		['a viewer, who gets no token', { body: JSON.stringify([{ id: HEARTBEAT_ID, name: 'Backup', token: null }]) }, null],
		['an empty list', { body: '[]' }, null],
		['a caller the engine refuses', { status: 403, body: '{"error":"forbidden"}' }, { status: 403 }],
		['an engine failure', { status: 500, body: '{"error":"internal error"}' }, { status: 500 }],
		['an engine error that is not JSON', { status: 502, body: '<html>bad gateway</html>' }, { status: 500 }],
		['a session that is gone', { status: 401, body: '{"error":"unauthorized"}' }, { status: 303 }]
	];
	it.each(scenarios)('forbids caching for %s', async (_name, engine, rejection) => {
		const { event, setHeaders } = fakeEvent(engine);
		if (rejection) await expect(load(event)).rejects.toMatchObject(rejection);
		else await load(event);
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
	});

	it('fails with the configuration error when PUBLIC_ENGINE_URL is not an http(s) URL', async () => {
		process.env.PUBLIC_ENGINE_URL = 'engine.example.com';
		const { event, setHeaders } = fakeEvent({ body: '[]' });
		await expect(load(event)).rejects.toThrow(/PUBLIC_ENGINE_URL/);
		expect(setHeaders).toHaveBeenCalledTimes(1);
	});
});

describe('new heartbeat action', () => {
	const fields = { name: 'Nightly backup', period_seconds: '86400', grace_seconds: '60' };

	it('creates the heartbeat and opens its page', async () => {
		const { event, calls } = fakeEvent({ form: fields, status: 201, body: JSON.stringify({ id: HEARTBEAT_ID }) });
		await expect(actions.default(event)).rejects.toMatchObject({
			status: 303,
			location: `${heartbeatsPath}/${HEARTBEAT_ID}`
		});
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: heartbeatsPath,
				search: '',
				body: '{"name":"Nightly backup","period_seconds":86400,"grace_seconds":60}'
			}
		]);
	});

	it('leaves a blank grace out, so the engine default applies', async () => {
		const { event, calls } = fakeEvent({
			form: { ...fields, grace_seconds: '' },
			status: 201,
			body: JSON.stringify({ id: HEARTBEAT_ID })
		});
		await expect(actions.default(event)).rejects.toMatchObject({ status: 303 });
		expect(calls()[0].body).toBe('{"name":"Nightly backup","period_seconds":86400}');
	});

	it('stops at a number the engine would not parse, without calling it', async () => {
		const { event, calls } = fakeEvent({ form: { ...fields, period_seconds: '1h' } });
		expect(await actions.default(event)).toMatchObject({
			status: 400,
			data: { message: 'period_seconds must be a whole number' }
		});
		expect(calls()).toEqual([]);
	});

	it("shows the engine's validation message", async () => {
		const { event } = fakeEvent({
			form: { ...fields, period_seconds: '5' },
			status: 400,
			body: '{"error":"period_seconds must be at least 30"}'
		});
		expect(await actions.default(event)).toMatchObject({
			status: 400,
			data: { message: 'period_seconds must be at least 30' }
		});
	});

	it("shows the engine's refusal for a viewer", async () => {
		const { event } = fakeEvent({ form: fields, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.default(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});

	it('falls back to a fixed message when the engine body is not its error shape', async () => {
		const { event } = fakeEvent({ form: fields, status: 422, body: 'Failed to deserialize the JSON body' });
		expect(await actions.default(event)).toMatchObject({
			status: 422,
			data: { message: 'Could not create heartbeat.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: fields, status: 401, body: '{"error":"unauthorized"}' });
		await expect(actions.default(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({
			params: { id: '../auth/me?' },
			form: fields,
			body: JSON.stringify({ id: HEARTBEAT_ID })
		});
		await expect(actions.default(event)).rejects.toMatchObject({
			location: `/projects/..%2Fauth%2Fme%3F/heartbeats/${HEARTBEAT_ID}`
		});
		expect(calls().map((call) => call.path)).toEqual(['/projects/..%2Fauth%2Fme%3F/heartbeats']);
	});

	// The redirect target is built from what the engine answered.
	it('keeps an odd id from the engine inside one path segment of the redirect', async () => {
		const { event } = fakeEvent({ form: fields, status: 201, body: JSON.stringify({ id: '../../auth/me?' }) });
		await expect(actions.default(event)).rejects.toMatchObject({
			location: `${heartbeatsPath}/..%2F..%2Fauth%2Fme%3F`
		});
	});
});
