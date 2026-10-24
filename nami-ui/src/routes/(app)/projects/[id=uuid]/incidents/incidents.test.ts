import { describe, expect, it } from 'vitest';
import { fakeEvent, HEARTBEAT_ID, MONITOR_ID, PROJECT_ID } from '#lib/server/core/fake-event';
import { load } from './+page.server';

const incidentsPath = `/projects/${PROJECT_ID}/incidents`;
const EMPTY_PAGE = JSON.stringify({ incidents: [], next_cursor: null });

describe('incidents load', () => {
	const incidents = [
		{ id: 'i1', monitor_id: MONITOR_ID, heartbeat_id: null, title: 'API is down', status: 'open' },
		{ id: 'i2', monitor_id: null, heartbeat_id: HEARTBEAT_ID, title: 'Backup missed its heartbeat', status: 'open' }
	];

	it('lists the unresolved incidents on the open tab by default', async () => {
		const { event, calls } = fakeEvent({ body: JSON.stringify({ incidents, next_cursor: 'c' }) });
		expect(await load(event)).toEqual({ incidents, status: 'open' });
		expect(calls()).toEqual([{ method: 'GET', path: incidentsPath, search: '?status=unresolved', body: undefined }]);
	});

	it('lists the resolved incidents for ?status=resolved', async () => {
		const { event, calls } = fakeEvent({ url: 'http://ui.test/?status=resolved', body: EMPTY_PAGE });
		expect(await load(event)).toEqual({ incidents: [], status: 'resolved' });
		expect(calls()).toEqual([{ method: 'GET', path: incidentsPath, search: '?status=resolved', body: undefined }]);
	});

	// The query value is user input: whatever else it holds, the engine is
	// asked for the unresolved ones and the raw value never reaches the request.
	it.each([
		['all'],
		[''],
		['OPEN'],
		['Resolved'],
		[' resolved'],
		['resolved '],
		['open&monitor_id=00000000-0000-0000-0000-000000000000'],
		['resolved&status=open'],
		['resolved#x'],
		['../auth/me?'],
		['%00']
	])('forwards only "unresolved" or "resolved", never %j', async (value) => {
		const { event, calls } = fakeEvent({
			url: `http://ui.test/?status=${encodeURIComponent(value)}`,
			body: EMPTY_PAGE
		});
		expect(await load(event)).toEqual({ incidents: [], status: 'open' });
		expect(calls()).toEqual([{ method: 'GET', path: incidentsPath, search: '?status=unresolved', body: undefined }]);
	});

	it('shows the open tab when the query has no status at all', async () => {
		const { event, calls } = fakeEvent({ url: 'http://ui.test/?other=1', body: EMPTY_PAGE });
		await load(event);
		expect(calls().map((call) => call.search)).toEqual(['?status=unresolved']);
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
		expect(calls().map((call) => call.path + call.search)).toEqual(['/projects/..%2Fauth%2Fme%3F/incidents?status=unresolved']);
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
