import { describe, expect, it } from 'vitest';
import { fakeEvent, HEARTBEAT_ID, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const incidentsPath = `/projects/${PROJECT_ID}/incidents`;
const EMPTY_PAGE = JSON.stringify({ incidents: [], next_cursor: null });
const NEW_ID = '3f0c8a52-7d1e-4b6a-9c2f-5e8d7a6b4c31';
const POLICY_ID = '7e6d5c4b-3a29-4180-9f7e-6d5c4b3a2918';
const SEVERITY_ID = '9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d';

/** The incident list requests only (the load also reads severities and policies). */
function listCalls(calls: () => { method: string; path: string; search: string; body?: string }[]) {
	return calls().filter((call) => call.path.endsWith('/incidents'));
}

describe('incidents load', () => {
	const incidents = [
		{ id: 'i1', monitor_id: MONITOR_ID, heartbeat_id: null, title: 'API is down', status: 'open' },
		{ id: 'i2', monitor_id: null, heartbeat_id: HEARTBEAT_ID, title: 'Backup missed its heartbeat', status: 'open' }
	];
	const severities = [{ id: SEVERITY_ID, name: 'high', rank: 2, is_default: true }];
	const policies = [{ id: POLICY_ID, name: 'Default', is_default: true, steps: [] }];

	it('lists the unresolved incidents by default, with the severities and policies for the form', async () => {
		const { event, calls } = fakeEvent({
			respond: (url) =>
				new Response(
					JSON.stringify(
						url.pathname.endsWith('/severities') ? severities : url.pathname.endsWith('/escalation-policies') ? policies : { incidents, next_cursor: 'c' }
					)
				)
		});
		expect(await load(event)).toEqual({
			incidents,
			nextCursor: 'c',
			query: { tab: 'unresolved', source: null, cursor: null },
			severities,
			policies
		});
		expect(calls().map((call) => call.path + call.search).sort()).toEqual(
			[`${incidentsPath}?status=unresolved`, `/projects/${PROJECT_ID}/severities`, `/projects/${PROJECT_ID}/escalation-policies`].sort()
		);
	});

	it.each([
		['?tab=resolved', '?status=resolved'],
		['?status=resolved', '?status=resolved'],
		['?tab=acknowledged', '?status=acknowledged'],
		['?tab=all', ''],
		['?tab=all&source=heartbeat&cursor=MjAy_-', '?source=heartbeat&cursor=MjAy_-']
	])('forwards %s as %j', async (query, search) => {
		const { event, calls } = fakeEvent({ url: `http://ui.test/${query}`, body: EMPTY_PAGE });
		await load(event);
		expect(listCalls(calls)).toEqual([{ method: 'GET', path: incidentsPath, search, body: undefined }]);
	});

	// The query is user input: only parsed values reach the engine.
	it.each([
		['?tab=bogus&source=x&cursor=%%'],
		['?status=open'],
		['?tab=resolved%26status%3Dopen'],
		['?tab=unresolved&source=monitor%26monitor_id%3D1'],
		['?cursor=..%2Fauth%2Fme%3F'],
		['?tab=%00&source=%00&cursor=%00']
	])('forwards only parsed query values, never %j', async (query) => {
		const { event, calls } = fakeEvent({ url: `http://ui.test/${query}`, body: EMPTY_PAGE });
		expect(await load(event)).toMatchObject({ incidents: [], query: { tab: 'unresolved', source: null, cursor: null } });
		expect(listCalls(calls)).toEqual([{ method: 'GET', path: incidentsPath, search: '?status=unresolved', body: undefined }]);
	});

	it('keeps the list when the severities or policies cannot be read', async () => {
		const { event } = fakeEvent({
			respond: (url) => (url.pathname.endsWith('/incidents') ? new Response(EMPTY_PAGE) : new Response('{"error":"x"}', { status: 500 }))
		});
		expect(await load(event)).toMatchObject({ incidents: [], severities: [], policies: [] });
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
		const { event } = fakeEvent({ status: 400, body: 'Failed to deserialize query string' });
		await expect(load(event)).rejects.toMatchObject({ status: 500, body: { message: 'Could not load incidents.' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, body: EMPTY_PAGE });
		await load(event);
		expect(calls().map((call) => call.path + call.search)).toContain('/projects/..%2Fauth%2Fme%3F/incidents?status=unresolved');
		expect(calls().every((call) => call.path.startsWith('/projects/..%2Fauth%2Fme%3F/'))).toBe(true);
	});

	// Incident details carry job output: never cached, whatever the engine says.
	it.each([
		['a listing', { body: EMPTY_PAGE }, null],
		['a caller the engine refuses', { status: 403, body: '{"error":"forbidden"}' }, { status: 403 }],
		['an engine failure', { status: 500, body: '{"error":"internal error"}' }, { status: 500 }],
		['a session that is gone', { status: 401, body: '{"error":"unauthorized"}' }, { status: 303 }]
	] satisfies [string, Parameters<typeof fakeEvent>[0], { status: number } | null][])('forbids caching for %s', async (_name, engine, rejection) => {
		const { event, setHeaders } = fakeEvent(engine);
		if (rejection) await expect(load(event)).rejects.toMatchObject(rejection);
		else await load(event);
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
	});
});

describe('create incident', () => {
	it('posts the filled fields only and redirects to the new incident', async () => {
		const { event, calls } = fakeEvent({ form: { title: ' DB slow ', description: '', severity_id: '', escalation_policy_id: '' }, status: 201, body: JSON.stringify({ id: NEW_ID }) });
		await expect(actions.create(event)).rejects.toMatchObject({ status: 303, location: `${incidentsPath}/${NEW_ID}` });
		expect(calls()).toEqual([{ method: 'POST', path: incidentsPath, search: '', body: '{"title":"DB slow"}' }]);
	});

	it('sends the description, severity and policy when given', async () => {
		const { event, calls } = fakeEvent({
			form: { title: 'DB slow', description: 'p99 > 2s\nsince 10:00', severity_id: SEVERITY_ID, escalation_policy_id: POLICY_ID },
			status: 201,
			body: JSON.stringify({ id: NEW_ID })
		});
		await expect(actions.create(event)).rejects.toMatchObject({ status: 303 });
		// Multipart form encoding turns the newline into CRLF; the engine
		// normalises it back to LF.
		expect(JSON.parse(calls()[0].body!)).toEqual({
			title: 'DB slow',
			description: 'p99 > 2s\r\nsince 10:00',
			severity_id: SEVERITY_ID,
			escalation_policy_id: POLICY_ID
		});
	});

	it.each([
		[{ title: 'x', escalation_policy_id: '../x' }],
		[{ title: 'x', severity_id: 'not-a-uuid' }],
		[{ title: '   ' }]
	])('refuses %j locally', async (form) => {
		const { event, calls } = fakeEvent({ form });
		expect(await actions.create(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});

	it("shows the engine's 403 as the form message", async () => {
		const { event } = fakeEvent({ form: { title: 'x' }, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.create(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});

	it('stays on the list when the engine answers an id that is not a UUID', async () => {
		const { event } = fakeEvent({ form: { title: 'x' }, status: 201, body: JSON.stringify({ id: '../../auth' }) });
		await expect(actions.create(event)).rejects.toMatchObject({ status: 303, location: incidentsPath });
	});
});
