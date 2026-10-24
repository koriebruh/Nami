import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Member, Schedule } from '#lib/incidents/oncall';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const SCHEDULE_ID = '5d2c7e1a-3b4f-4a6d-8e9c-1f2a3b4c5d6e';
const USER_ID = '951106f5-21e1-4c23-9a60-9df4fc9e79ed';
const members: Member[] = [{ user_id: USER_ID, email: 'ann@b.test', project_role: 'member' }];

function schedule(overrides: Partial<Schedule> = {}): Schedule {
	return {
		id: SCHEDULE_ID,
		project_id: PROJECT_ID,
		name: 'Primary',
		timezone: 'Europe/Berlin',
		rotation: 'weekly',
		rotation_hours: null,
		handoff_time: '09:00',
		start_date: '2026-10-19',
		restrictions: [],
		participants: [{ user_id: USER_ID, email: 'ann@b.test', is_member: true }],
		on_call_now: { user_id: USER_ID, email: 'ann@b.test', until: '2026-10-26T08:00:00Z' },
		...overrides
	};
}

const createForm = {
	name: 'Primary',
	timezone: 'UTC',
	rotation: 'daily',
	handoff_time: '09:00',
	start_date: '2026-10-19',
	participant: USER_ID
};

describe('on-call list', () => {
	it('loads the schedules and the members', async () => {
		const { event, calls } = fakeEvent({
			respond: (url) => new Response(JSON.stringify(url.pathname.endsWith('/members') ? members : [schedule()]))
		});
		expect(await load(event)).toEqual({ schedules: [schedule()], members });
		expect(calls().map((c) => c.path).sort()).toEqual([`/projects/${PROJECT_ID}/members`, `/projects/${PROJECT_ID}/schedules`]);
	});

	it('still lists the schedules when the members cannot be loaded', async () => {
		const { event } = fakeEvent({
			respond: (url) =>
				url.pathname.endsWith('/members') ? new Response('{"error":"internal error"}', { status: 500 }) : new Response('[]')
		});
		expect(await load(event)).toEqual({ schedules: [], members: [] });
	});

	it('keeps a 403 a 403', async () => {
		await expect(load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({ status: 403 });
	});

	it('creates a schedule and opens it', async () => {
		const { event, calls } = fakeEvent({ form: createForm, status: 201, body: JSON.stringify(schedule()) });
		await expect(actions.create(event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/oncall/${SCHEDULE_ID}` });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: `/projects/${PROJECT_ID}/schedules`,
				search: '',
				body: JSON.stringify({
					name: 'Primary',
					timezone: 'UTC',
					rotation: 'daily',
					handoff_time: '09:00',
					start_date: '2026-10-19',
					participants: [USER_ID],
					restrictions: []
				})
			}
		]);
	});

	it('refuses a bad form without calling the engine, and shows the engine message', async () => {
		const bad = fakeEvent({ form: { ...createForm, participant: 'x' } });
		expect(await actions.create(bad.event)).toMatchObject({ status: 400, data: { action: 'create' } });
		expect(bad.calls()).toEqual([]);
		const refused = fakeEvent({ form: createForm, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.create(refused.event)).toMatchObject({ status: 403, data: { action: 'create', message: 'forbidden' } });
	});
});

function renderPage(role: 'admin' | 'member' | 'viewer', schedules: Schedule[] = [schedule()]) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role,
		schedules,
		members
	};
	return render(Page, { props: { params: { id: PROJECT_ID }, data, form: null } as never }).body;
}

describe('on-call list page', () => {
	it('shows who is on call until when, in the schedule zone with UTC in the tooltip', () => {
		const body = renderPage('viewer');
		expect(body).toContain(`href="/projects/${PROJECT_ID}/oncall/${SCHEDULE_ID}"`);
		expect(body).toContain('ann@b.test');
		expect(body).toContain('Mon 26 Oct 09:00');
		expect(body).toContain('title="2026-10-26 08:00:00 UTC"');
	});

	it('offers the create form to admins only', () => {
		expect(renderPage('viewer')).not.toContain('action="?/create"');
		expect(renderPage('member')).not.toContain('action="?/create"');
		expect(renderPage('admin')).toContain('action="?/create"');
	});

	it('says when nobody is on call and when there are no schedules', () => {
		expect(renderPage('viewer', [schedule({ on_call_now: null })])).toContain('Nobody');
		expect(renderPage('viewer', [])).toContain('No on-call schedules yet.');
	});
});
