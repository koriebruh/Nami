import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { load } from './+page.server';
import { actions as newActions, load as newLoad } from './new/+page.server';

const ALERT_ID = '3d0c5b1e-8f7a-4c2d-9e6b-1a2b3c4d5e6f';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const alert = { id: ALERT_ID, name: 'Checkout errors', threshold: 10, comparison: 'gt', state: 'ok' };
const sources = [{ id: '7a1c3e5f-2b4d-4f60-8a9b-0c1d2e3f4a5b', name: 'api' }];
const form = {
	name: 'Checkout errors',
	threshold: '10',
	comparison: 'gt',
	window_seconds: '300',
	interval_seconds: '60',
	enabled: 'on',
	level: 'error'
};

describe('log alerts list', () => {
	it('loads the alerts and the sources', async () => {
		const { event, calls } = fakeEvent({
			respond: (url) => (url.pathname.endsWith('/sources') ? json(sources) : json([alert]))
		});
		expect(await load(event)).toEqual({ alerts: [alert], sources });
		expect(calls().map((c) => c.path)).toEqual([`/projects/${PROJECT_ID}/log-alerts`, `/projects/${PROJECT_ID}/sources`]);
	});

	it('maps 403 to 403 and 401 to login', async () => {
		await expect(load(fakeEvent({ status: 403 }).event)).rejects.toMatchObject({ status: 403 });
		await expect(load(fakeEvent({ status: 401 }).event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

describe('new log alert', () => {
	it('prefills the query from an explorer link', async () => {
		const { event } = fakeEvent({ url: 'http://ui.test/x?level=error&q=disk', respond: () => json(sources) });
		const data: any = await newLoad(event);
		expect(data.initial.query).toEqual({ q: 'disk', levels: ['error'] });
		expect(data.sources).toEqual(sources);
	});

	it('creates the alert and redirects to it', async () => {
		const { event, calls } = fakeEvent({ form, respond: () => json({ ...alert }, 201) });
		await expect(newActions.create(event)).rejects.toMatchObject({
			status: 303,
			location: `/projects/${PROJECT_ID}/logs/alerts/${ALERT_ID}`
		});
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/log-alerts` });
		expect(JSON.parse(calls()[0].body!)).toEqual({
			name: 'Checkout errors',
			query: { levels: ['error'] },
			threshold: 10,
			comparison: 'gt',
			window_seconds: 300,
			interval_seconds: 60,
			enabled: true
		});
	});

	it('refuses an invalid form without a call, and shows the engine message', async () => {
		const bad = fakeEvent({ form: { ...form, threshold: '-1' } });
		expect(await newActions.create(bad.event)).toMatchObject({ status: 400 });
		expect(bad.calls()).toHaveLength(0);

		const refused = fakeEvent({ form, respond: () => json({ error: 'lt needs a threshold of at least 1' }, 400) });
		expect(await newActions.create(refused.event)).toMatchObject({
			status: 400,
			data: { message: 'lt needs a threshold of at least 1' }
		});
	});
});

describe('new log alert, escalation policy and severity (NAMI-INC-14)', () => {
	it('omits "Project default" on create', async () => {
		const { event, calls } = fakeEvent({ form: { ...form, escalation_policy_id: '', severity_id: '' }, respond: () => json(alert, 201) });
		await expect(newActions.create(event)).rejects.toMatchObject({ status: 303 });
		expect(JSON.parse(calls()[0].body!)).not.toHaveProperty('escalation_policy_id');
		expect(JSON.parse(calls()[0].body!)).not.toHaveProperty('severity_id');
	});

	it('refuses a value that is not a UUID without calling the engine', async () => {
		const { event, calls } = fakeEvent({ form: { ...form, escalation_policy_id: '../auth/me' } });
		expect(await newActions.create(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});
});
