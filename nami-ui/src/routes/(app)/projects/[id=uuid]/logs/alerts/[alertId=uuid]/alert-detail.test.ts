import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const ALERT_ID = '3d0c5b1e-8f7a-4c2d-9e6b-1a2b3c4d5e6f';
const PATH = `/projects/${PROJECT_ID}/log-alerts/${ALERT_ID}`;
const params = { alertId: ALERT_ID };
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const alert = { id: ALERT_ID, name: 'Checkout errors', threshold: 10, comparison: 'gt', state: 'ok' };

describe('log alert detail', () => {
	it('loads the alert and the sources; 404 is a 404', async () => {
		const { event, calls } = fakeEvent({
			params,
			respond: (url) => (url.pathname.endsWith('/sources') ? json([]) : json(alert))
		});
		expect(await load(event)).toEqual({ alert, sources: [] });
		expect(calls()[0]).toMatchObject({ method: 'GET', path: PATH });
		await expect(load(fakeEvent({ params, status: 404 }).event)).rejects.toMatchObject({ status: 404 });
	});

	it('update PATCHes the payload', async () => {
		const { event, calls } = fakeEvent({
			params,
			form: { name: 'Quiet', threshold: '1', comparison: 'lt', window_seconds: '3600', interval_seconds: '300' },
			respond: () => json(alert)
		});
		expect(await actions.update(event)).toEqual({ saved: true });
		expect(calls()[0]).toMatchObject({ method: 'PATCH', path: PATH });
		expect(JSON.parse(calls()[0].body!)).toEqual({
			name: 'Quiet',
			query: {},
			threshold: 1,
			comparison: 'lt',
			window_seconds: 3600,
			interval_seconds: 300,
			enabled: false
		});
	});

	it('delete asks first without confirm=yes', async () => {
		const { event, calls } = fakeEvent({ params, form: {} });
		expect(await actions.delete(event)).toEqual({ confirmDelete: true });
		expect(calls()).toEqual([]);
	});

	it('delete with confirm=yes redirects to the list; a non-admin gets the admin message', async () => {
		const ok = fakeEvent({ params, form: { confirm: 'yes' }, status: 200 });
		await expect(actions.delete(ok.event)).rejects.toMatchObject({
			status: 303,
			location: `/projects/${PROJECT_ID}/logs/alerts`
		});
		expect(ok.calls()).toEqual([{ method: 'DELETE', path: PATH, search: '', body: undefined }]);

		const denied = await actions.delete(fakeEvent({ params, form: { confirm: 'yes' }, status: 403 }).event);
		expect(denied).toMatchObject({ status: 403, data: { message: 'Only project admins can delete alerts.' } });
	});
});
