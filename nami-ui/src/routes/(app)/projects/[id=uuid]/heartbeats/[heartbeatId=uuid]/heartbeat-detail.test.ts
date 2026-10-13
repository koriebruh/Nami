import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fakeEvent, HEARTBEAT_ID, PROJECT_ID } from '#lib/server/fake-event';
import { actions, load } from './+page.server';

const heartbeatPath = `/projects/${PROJECT_ID}/heartbeats/${HEARTBEAT_ID}`;
const listPath = `/projects/${PROJECT_ID}/heartbeats`;
const hostile = '../auth/me?';
const hostileSegment = '..%2Fauth%2Fme%3F';

type Scenario = [name: string, engine: Parameters<typeof fakeEvent>[0], rejection: { status: number } | null];

describe('heartbeat detail load', () => {
	const originalPublicUrl = process.env.PUBLIC_ENGINE_URL;
	beforeEach(() => {
		delete process.env.PUBLIC_ENGINE_URL;
	});
	afterEach(() => {
		if (originalPublicUrl === undefined) delete process.env.PUBLIC_ENGINE_URL;
		else process.env.PUBLIC_ENGINE_URL = originalPublicUrl;
	});

	it('returns the heartbeat with the engine URL that its ping URL is built on', async () => {
		process.env.PUBLIC_ENGINE_URL = 'https://engine.example.com/';
		const heartbeat = { id: HEARTBEAT_ID, name: 'Backup', token: 'b'.repeat(64) };
		const { event, calls } = fakeEvent({ body: JSON.stringify(heartbeat) });
		expect(await load(event)).toEqual({ heartbeat, engineUrl: 'https://engine.example.com' });
		expect(calls()).toEqual([{ method: 'GET', path: heartbeatPath, search: '', body: undefined }]);
	});

	// A viewer's heartbeat has no token (null). The load keeps it that way.
	it('passes a null token (a viewer) through untouched', async () => {
		const heartbeat = { id: HEARTBEAT_ID, name: 'Backup', token: null };
		const { event } = fakeEvent({ body: JSON.stringify(heartbeat) });
		const data = await load(event);
		expect(data).toEqual({ heartbeat, engineUrl: 'http://localhost:8081' });
		expect(data).toHaveProperty('heartbeat.token', null);
	});

	it('is a 404 for a heartbeat the engine does not have', async () => {
		const { event } = fakeEvent({ status: 404, body: '{"error":"not found"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 404, body: { message: 'Heartbeat not found.' } });
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
		const { event } = fakeEvent({ status: 400, body: 'Invalid URL: invalid character' });
		await expect(load(event)).rejects.toMatchObject({ status: 500, body: { message: 'Could not load heartbeat.' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const project = fakeEvent({ params: { id: hostile }, body: '{}' });
		await load(project.event);
		expect(project.calls().map((call) => call.path)).toEqual([`/projects/${hostileSegment}/heartbeats/${HEARTBEAT_ID}`]);

		const heartbeat = fakeEvent({ params: { heartbeatId: hostile }, body: '{}' });
		await load(heartbeat.event);
		expect(heartbeat.calls().map((call) => call.path)).toEqual([`${listPath}/${hostileSegment}`]);
	});

	// The page carries the ping token, in its markup and in the data sent with
	// it: no browser or proxy may keep either. The header is set before anything
	// can fail, so every response of the load has it, and only once (SvelteKit
	// throws when a header is set twice).
	const scenarios: Scenario[] = [
		['a member', { body: JSON.stringify({ id: HEARTBEAT_ID, name: 'Backup', token: 'b'.repeat(64) }) }, null],
		['a viewer, who gets no token', { body: JSON.stringify({ id: HEARTBEAT_ID, name: 'Backup', token: null }) }, null],
		['a heartbeat that is not there', { status: 404, body: '{"error":"not found"}' }, { status: 404 }],
		['a caller the engine refuses', { status: 403, body: '{"error":"forbidden"}' }, { status: 403 }],
		['an engine failure', { status: 500, body: '{"error":"internal error"}' }, { status: 500 }],
		['an engine error that is not JSON', { status: 400, body: 'Invalid URL: invalid character' }, { status: 500 }],
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
		process.env.PUBLIC_ENGINE_URL = 'ftp://engine.example.com';
		const { event, setHeaders } = fakeEvent({ body: JSON.stringify({ id: HEARTBEAT_ID, name: 'Backup', token: 'b'.repeat(64) }) });
		await expect(load(event)).rejects.toThrow(/PUBLIC_ENGINE_URL/);
		expect(setHeaders).toHaveBeenCalledTimes(1);
	});
});

describe('heartbeat settings action', () => {
	const fields = { name: 'Nightly backup', period_seconds: '3600', grace_seconds: '120' };

	it('patches name, period and grace and says it saved', async () => {
		const { event, calls } = fakeEvent({ form: fields });
		expect(await actions.update(event)).toEqual({ saved: true });
		expect(calls()).toEqual([
			{
				method: 'PATCH',
				path: heartbeatPath,
				search: '',
				body: '{"name":"Nightly backup","period_seconds":3600,"grace_seconds":120}'
			}
		]);
	});

	it('stops at a number the engine would not parse, without calling it', async () => {
		const { event, calls } = fakeEvent({ form: { ...fields, grace_seconds: '2m' } });
		expect(await actions.update(event)).toMatchObject({
			status: 400,
			data: { action: 'update', message: 'grace_seconds must be a whole number' }
		});
		expect(calls()).toEqual([]);
	});

	it("shows the engine's validation message", async () => {
		const { event } = fakeEvent({
			form: { ...fields, period_seconds: '5' },
			status: 400,
			body: '{"error":"period_seconds must be at least 30"}'
		});
		expect(await actions.update(event)).toMatchObject({
			status: 400,
			data: { action: 'update', message: 'period_seconds must be at least 30' }
		});
	});

	it("shows the engine's refusal for a viewer", async () => {
		const { event } = fakeEvent({ form: fields, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.update(event)).toMatchObject({ status: 403, data: { action: 'update', message: 'forbidden' } });
	});

	it('falls back to a fixed message when the engine body is not its error shape', async () => {
		const { event } = fakeEvent({ form: fields, status: 422, body: 'Failed to deserialize the JSON body' });
		expect(await actions.update(event)).toMatchObject({
			status: 422,
			data: { action: 'update', message: 'Could not update heartbeat.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: fields, status: 401, body: '{"error":"unauthorized"}' });
		await expect(actions.update(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { heartbeatId: hostile }, form: fields });
		await actions.update(event);
		expect(calls().map((call) => call.path)).toEqual([`${listPath}/${hostileSegment}`]);
	});
});

describe('heartbeat pause and resume actions', () => {
	it('pauses and resumes with a PATCH of the paused flag', async () => {
		const pause = fakeEvent();
		expect(await actions.pause(pause.event)).toEqual({ success: true });
		expect(pause.calls()).toEqual([{ method: 'PATCH', path: heartbeatPath, search: '', body: '{"paused":true}' }]);

		const resume = fakeEvent();
		expect(await actions.resume(resume.event)).toEqual({ success: true });
		expect(resume.calls()).toEqual([{ method: 'PATCH', path: heartbeatPath, search: '', body: '{"paused":false}' }]);
	});

	it("shows the engine's message when the engine refuses", async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.pause(event)).toMatchObject({ status: 403, data: { action: 'pause', message: 'forbidden' } });
	});

	it('survives an engine error that is not JSON', async () => {
		const { event } = fakeEvent({ status: 500, body: 'Internal Server Error' });
		expect(await actions.resume(event)).toMatchObject({
			status: 500,
			data: { action: 'resume', message: 'Could not update heartbeat.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(actions.pause(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

// Without JavaScript the browser shows no confirm() dialog, so a bare POST from
// the Delete button must not delete: the page asks first.
describe('heartbeat delete action', () => {
	it('asks for confirmation instead of deleting when the form carries none', async () => {
		const { event, calls } = fakeEvent({ form: {} });
		expect(await actions.delete(event)).toEqual({ confirmDelete: true });
		expect(calls()).toEqual([]);
	});

	it('accepts nothing but "yes" as the confirmation', async () => {
		for (const confirm of ['', 'true', '1', 'no', 'YES']) {
			const { event, calls } = fakeEvent({ form: { confirm } });
			expect(await actions.delete(event), confirm).toEqual({ confirmDelete: true });
			expect(calls()).toEqual([]);
		}
	});

	it('deletes once confirmed and returns to the list', async () => {
		const { event, calls } = fakeEvent({ form: { confirm: 'yes' } });
		await expect(actions.delete(event)).rejects.toMatchObject({ status: 303, location: listPath });
		expect(calls()).toEqual([{ method: 'DELETE', path: heartbeatPath, search: '', body: undefined }]);
	});

	it("shows the engine's message when the delete is refused", async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.delete(event)).toMatchObject({ status: 403, data: { action: 'delete', message: 'forbidden' } });
	});

	it('survives an engine error that is not JSON', async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 502, body: 'Bad Gateway' });
		expect(await actions.delete(event)).toMatchObject({
			status: 502,
			data: { action: 'delete', message: 'Could not delete heartbeat.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 401, body: '{"error":"unauthorized"}' });
		await expect(actions.delete(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment, in the request and the redirect', async () => {
		const { event, calls } = fakeEvent({ params: { id: hostile }, form: { confirm: 'yes' } });
		await expect(actions.delete(event)).rejects.toMatchObject({ location: `/projects/${hostileSegment}/heartbeats` });
		expect(calls().map((call) => call.path)).toEqual([`/projects/${hostileSegment}/heartbeats/${HEARTBEAT_ID}`]);
	});
});

// Rotating replaces the ping token and the old ping URL stops working at once,
// so it asks first, like delete does, and the question works without JavaScript.
describe('heartbeat rotate-token action', () => {
	const rotatePath = `${heartbeatPath}/rotate-token`;

	it('asks for confirmation instead of rotating when the form carries none', async () => {
		const { event, calls } = fakeEvent({ form: {} });
		expect(await actions.rotate(event)).toEqual({ confirmRotate: true });
		expect(calls()).toEqual([]);
	});

	it('accepts nothing but "yes" as the confirmation', async () => {
		for (const confirm of ['', 'true', '1', 'no', 'YES']) {
			const { event, calls } = fakeEvent({ form: { confirm } });
			expect(await actions.rotate(event), confirm).toEqual({ confirmRotate: true });
			expect(calls()).toEqual([]);
		}
	});

	// The new token is not echoed in the action's result: the page reads it
	// from the heartbeat that the load fetches afterwards.
	it('posts to the rotate-token endpoint once confirmed, and returns no token', async () => {
		const { event, calls } = fakeEvent({
			form: { confirm: 'yes' },
			body: JSON.stringify({ id: HEARTBEAT_ID, token: 'c'.repeat(64) })
		});
		expect(await actions.rotate(event)).toEqual({ rotated: true });
		// The engine takes only JSON bodies, so the body-less action sends `{}`.
		expect(calls()).toEqual([{ method: 'POST', path: rotatePath, search: '', body: '{}' }]);
	});

	it("shows the engine's refusal for a viewer", async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.rotate(event)).toMatchObject({ status: 403, data: { action: 'rotate', message: 'forbidden' } });
	});

	it("shows the engine's message for a heartbeat that is gone", async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 404, body: '{"error":"not found"}' });
		expect(await actions.rotate(event)).toMatchObject({ status: 404, data: { action: 'rotate', message: 'not found' } });
	});

	it('survives an engine error that is not JSON', async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 500, body: 'Internal Server Error' });
		expect(await actions.rotate(event)).toMatchObject({
			status: 500,
			data: { action: 'rotate', message: 'Could not rotate token.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 401, body: '{"error":"unauthorized"}' });
		await expect(actions.rotate(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { heartbeatId: hostile }, form: { confirm: 'yes' } });
		await actions.rotate(event);
		expect(calls().map((call) => call.path)).toEqual([`${listPath}/${hostileSegment}/rotate-token`]);
	});
});
