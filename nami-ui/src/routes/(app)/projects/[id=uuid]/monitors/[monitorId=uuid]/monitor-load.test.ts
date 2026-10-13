import { describe, expect, it } from 'vitest';
import { fakeEvent, MONITOR_ID, PROJECT_ID } from '#lib/server/fake-event';
import { actions as detailActions, load as loadDetail } from './+page.server';
import { actions as editActions, load as loadEdit } from './edit/+page.server';

const monitorPath = `/projects/${PROJECT_ID}/monitors/${MONITOR_ID}`;
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });

// The loads of the monitor's two pages. Every engine call answers with the
// same status here, so what is under test is how a failure becomes a page error.
describe.each([
	['detail', loadDetail],
	['edit', loadEdit]
] as const)('%s page load', (_name, load) => {
	it('is a 404 for a monitor the engine does not have', async () => {
		const { event } = fakeEvent({ status: 404, body: '{"error":"not found"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 404, body: { message: 'Monitor not found.' } });
	});

	it('keeps a 403 a 403', async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 403 });
	});

	it("turns any other engine failure into a 500 with the engine's message", async () => {
		const { event } = fakeEvent({ status: 500, body: '{"error":"internal error"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 500, body: { message: 'internal error' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

describe('detail page load', () => {
	const monitor = { id: MONITOR_ID, name: 'API' };
	const answers: Record<string, unknown> = {
		[monitorPath]: monitor,
		[`${monitorPath}/stats`]: { uptime_24h: 1, series: [] },
		[`${monitorPath}/checks`]: [{ checked_at: '2026-10-13T10:00:00Z' }],
		[`/projects/${PROJECT_ID}/incidents`]: [{ id: 'incident-1' }]
	};

	it('returns the monitor with its stats, recent checks and incidents', async () => {
		const { event, calls } = fakeEvent({ respond: (url) => json(answers[url.pathname]) });
		expect(await loadDetail(event)).toEqual({
			monitor,
			stats: answers[`${monitorPath}/stats`],
			checks: answers[`${monitorPath}/checks`],
			incidents: answers[`/projects/${PROJECT_ID}/incidents`]
		});
		expect(calls().map((call) => call.path + call.search)).toEqual([
			monitorPath,
			`${monitorPath}/stats`,
			`${monitorPath}/checks?limit=20`,
			`/projects/${PROJECT_ID}/incidents?monitor_id=${MONITOR_ID}`
		]);
	});

	// History lives in ClickHouse: without it the page still shows the monitor.
	it('still returns the monitor when its history cannot be read', async () => {
		const { event } = fakeEvent({
			respond: (url) => (url.pathname === monitorPath ? json(monitor) : json({ error: 'internal error' }, 500))
		});
		expect(await loadDetail(event)).toEqual({ monitor, stats: null, checks: [], incidents: [] });
	});

	it('keeps a hostile id inside one path segment of every engine path', async () => {
		const { event, calls } = fakeEvent({ params: { monitorId: '../auth/me?' } });
		await loadDetail(event);
		const id = '..%2Fauth%2Fme%3F';
		expect(calls().map((call) => call.path + call.search)).toEqual([
			`/projects/${PROJECT_ID}/monitors/${id}`,
			`/projects/${PROJECT_ID}/monitors/${id}/stats`,
			`/projects/${PROJECT_ID}/monitors/${id}/checks?limit=20`,
			`/projects/${PROJECT_ID}/incidents?monitor_id=${id}`
		]);
	});
});

describe('edit page load', () => {
	it('returns the monitor', async () => {
		const monitor = { id: MONITOR_ID, name: 'API' };
		const { event, calls } = fakeEvent({ body: JSON.stringify(monitor) });
		expect(await loadEdit(event)).toEqual({ monitor });
		expect(calls().map((call) => call.path)).toEqual([monitorPath]);
	});
});

describe('detail page actions', () => {
	it('pauses and resumes with a PATCH of the paused flag', async () => {
		const pause = fakeEvent();
		expect(await detailActions.pause(pause.event)).toEqual({ success: true });
		expect(pause.calls()).toEqual([{ method: 'PATCH', path: monitorPath, search: '', body: '{"paused":true}' }]);

		const resume = fakeEvent();
		expect(await detailActions.resume(resume.event)).toEqual({ success: true });
		expect(resume.calls()).toEqual([{ method: 'PATCH', path: monitorPath, search: '', body: '{"paused":false}' }]);
	});

	it("shows the engine's message when the engine refuses", async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		expect(await detailActions.pause(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(detailActions.pause(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	// Without JavaScript the browser shows no confirm() dialog, so a bare POST
	// from the Delete button must not delete: the page asks first.
	describe('delete', () => {
		it('asks for confirmation instead of deleting when the form carries none', async () => {
			const { event, calls } = fakeEvent({ form: {} });
			expect(await detailActions.delete(event)).toEqual({ confirmDelete: true });
			expect(calls()).toEqual([]);
		});

		it('accepts nothing but "yes" as the confirmation', async () => {
			for (const confirm of ['', 'true', '1', 'no', 'YES']) {
				const { event, calls } = fakeEvent({ form: { confirm } });
				expect(await detailActions.delete(event)).toEqual({ confirmDelete: true });
				expect(calls()).toEqual([]);
			}
		});

		it('deletes once confirmed and returns to the list', async () => {
			const { event, calls } = fakeEvent({ form: { confirm: 'yes' } });
			await expect(detailActions.delete(event)).rejects.toMatchObject({
				status: 303,
				location: `/projects/${PROJECT_ID}/monitors`
			});
			expect(calls()).toEqual([{ method: 'DELETE', path: monitorPath, search: '', body: undefined }]);
		});

		it("shows the engine's message when the delete is refused", async () => {
			const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 403, body: '{"error":"forbidden"}' });
			expect(await detailActions.delete(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
		});

		it('sends a visitor whose session is gone to the login page', async () => {
			const { event } = fakeEvent({ form: { confirm: 'yes' }, status: 401, body: '{"error":"unauthorized"}' });
			await expect(detailActions.delete(event)).rejects.toMatchObject({ status: 303, location: '/login' });
		});
	});
});

describe('edit page action', () => {
	const fields = { name: 'API', check_type: 'http', url: 'https://example.com' };

	it('patches the monitor and returns to its page', async () => {
		const { event, calls } = fakeEvent({ form: fields });
		await expect(editActions.default(event)).rejects.toMatchObject({ status: 303, location: monitorPath });
		expect(calls()).toEqual([
			{
				method: 'PATCH',
				path: monitorPath,
				search: '',
				body: '{"name":"API","check_type":"http","url":"https://example.com","http_method":"GET"}'
			}
		]);
	});

	it('stops at a number the engine would not parse, without calling it', async () => {
		const { event, calls } = fakeEvent({ form: { ...fields, interval_seconds: '1.5' } });
		expect(await editActions.default(event)).toMatchObject({
			status: 400,
			data: { message: 'interval_seconds must be a whole number' }
		});
		expect(calls()).toEqual([]);
	});

	it("shows the engine's validation message", async () => {
		const { event } = fakeEvent({
			form: fields,
			status: 400,
			body: '{"error":"timeout_seconds must be less than interval_seconds"}'
		});
		expect(await editActions.default(event)).toMatchObject({
			status: 400,
			data: { message: 'timeout_seconds must be less than interval_seconds' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: fields, status: 401, body: '{"error":"unauthorized"}' });
		await expect(editActions.default(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});
