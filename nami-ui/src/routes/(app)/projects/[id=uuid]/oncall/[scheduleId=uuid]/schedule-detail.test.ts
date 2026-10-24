import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Member, Override, Schedule, Shift } from '#lib/incidents/oncall';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const SCHEDULE_ID = '5d2c7e1a-3b4f-4a6d-8e9c-1f2a3b4c5d6e';
const OVERRIDE_ID = '7e8f9a0b-1c2d-4e3f-8a5b-6c7d8e9f0a1b';
const USER_ID = '951106f5-21e1-4c23-9a60-9df4fc9e79ed';
const GONE_ID = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const params = { scheduleId: SCHEDULE_ID };
const base = `/projects/${PROJECT_ID}/schedules/${SCHEDULE_ID}`;

const members: Member[] = [{ user_id: USER_ID, email: 'ann@b.test', project_role: 'member' }];
const shifts: Shift[] = [
	{ user_id: USER_ID, email: 'ann@b.test', is_member: true, starts_at: '2026-10-19T07:00:00Z', ends_at: '2026-10-20T07:00:00Z', override_id: null },
	{ user_id: GONE_ID, email: 'gone@b.test', is_member: false, starts_at: '2026-10-20T07:00:00Z', ends_at: '2026-10-21T07:00:00Z', override_id: OVERRIDE_ID }
];
const overrides: Override[] = [
	{ id: OVERRIDE_ID, user_id: GONE_ID, email: 'gone@b.test', starts_at: '2026-10-20T07:00:00Z', ends_at: '2026-10-21T07:00:00Z' }
];

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
		restrictions: [{ weekdays: [1, 2, 3, 4, 5], from: '09:00', to: '17:00' }],
		participants: [
			{ user_id: USER_ID, email: 'ann@b.test', is_member: true },
			{ user_id: GONE_ID, email: 'gone@b.test', is_member: false }
		],
		on_call_now: { user_id: USER_ID, email: 'ann@b.test', until: '2026-10-20T07:00:00Z' },
		...overrides
	};
}

function answers(url: URL): Response {
	if (url.pathname.endsWith('/members')) return new Response(JSON.stringify(members));
	if (url.pathname.endsWith('/shifts')) return new Response(JSON.stringify(shifts));
	if (url.pathname.endsWith('/overrides')) return new Response(JSON.stringify(overrides));
	return new Response(JSON.stringify(schedule()));
}

/** A POST with repeated fields, which `fakeEvent({ form })` cannot send. */
function withForm(event: { request: Request }, fields: [string, string][]) {
	const body = new FormData();
	for (const [name, value] of fields) body.append(name, value);
	event.request = new Request('http://ui.test/', { method: 'POST', body });
}

describe('schedule detail load', () => {
	it('loads the schedule, its shifts, its overrides and the members', async () => {
		const { event, calls } = fakeEvent({ params, respond: answers });
		expect(await load(event)).toEqual({ schedule: schedule(), shifts, overrides, members });
		expect(calls().map((c) => c.path + c.search).sort()).toEqual(
			[base, `${base}/shifts`, `${base}/overrides`, `/projects/${PROJECT_ID}/members`].sort()
		);
	});

	it('404s for an unknown schedule', async () => {
		await expect(load(fakeEvent({ params, status: 404, body: '{"error":"not found"}' }).event)).rejects.toMatchObject({ status: 404 });
	});

	it('still opens when the members cannot be loaded', async () => {
		const { event } = fakeEvent({
			params,
			respond: (url) => (url.pathname.endsWith('/members') ? new Response('{"error":"internal error"}', { status: 500 }) : answers(url))
		});
		expect(await load(event)).toMatchObject({ members: [] });
	});

	it('keeps a hostile schedule id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { scheduleId: '../x?' }, respond: answers });
		await load(event);
		expect(calls().map((c) => c.path)).toContain(`/projects/${PROJECT_ID}/schedules/..%2Fx%3F`);
	});
});

describe('schedule detail actions', () => {
	it('save PUTs the schedule', async () => {
		const { event, calls } = fakeEvent({ params, body: JSON.stringify(schedule()) });
		withForm(event, [
			['name', 'Primary'],
			['timezone', 'Europe/Berlin'],
			['rotation', 'weekly'],
			['handoff_time', '09:00'],
			['start_date', '2026-10-19'],
			['participant', GONE_ID],
			['participant', USER_ID],
			['restriction_count', '0']
		]);
		expect(await actions.save(event)).toMatchObject({ action: 'save', saved: true });
		expect(calls()).toEqual([
			{
				method: 'PUT',
				path: base,
				search: '',
				body: JSON.stringify({
					name: 'Primary',
					timezone: 'Europe/Berlin',
					rotation: 'weekly',
					handoff_time: '09:00',
					start_date: '2026-10-19',
					participants: [GONE_ID, USER_ID],
					restrictions: []
				})
			}
		]);
	});

	it("save shows the engine's message", async () => {
		const { event } = fakeEvent({ params, status: 400, body: `{"error":"user ${GONE_ID} is not a member of this project"}` });
		withForm(event, [['name', 'P'], ['rotation', 'daily'], ['participant', GONE_ID]]);
		expect(await actions.save(event)).toMatchObject({ status: 400, data: { action: 'save', message: `user ${GONE_ID} is not a member of this project` } });
	});

	it('addOverride posts the UTC payload', async () => {
		const { event, calls } = fakeEvent({
			params,
			form: { user_id: USER_ID, timezone: 'Europe/Berlin', starts_at: '2026-10-19T09:00', ends_at: '2026-10-20T09:00' },
			status: 201
		});
		expect(await actions.addOverride(event)).toMatchObject({ action: 'addOverride', added: true });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: `${base}/overrides`,
				search: '',
				body: JSON.stringify({ user_id: USER_ID, starts_at: '2026-10-19T07:00:00.000Z', ends_at: '2026-10-20T07:00:00.000Z' })
			}
		]);
	});

	it('addOverride refuses a skipped hour without calling the engine', async () => {
		const { event, calls } = fakeEvent({
			params,
			form: { user_id: USER_ID, timezone: 'Europe/Berlin', starts_at: '2026-03-29T02:30', ends_at: '2026-03-29T09:00' }
		});
		expect(await actions.addOverride(event)).toMatchObject({
			status: 400,
			data: { message: 'That time does not exist in Europe/Berlin (daylight saving change)' }
		});
		expect(calls()).toEqual([]);
	});

	it('deleteOverride needs confirm=yes', async () => {
		const ask = fakeEvent({ params, form: { override_id: OVERRIDE_ID } });
		expect(await actions.deleteOverride(ask.event)).toEqual({ action: 'deleteOverride', confirmDeleteOverride: OVERRIDE_ID });
		expect(ask.calls()).toEqual([]);

		const yes = fakeEvent({ params, form: { override_id: OVERRIDE_ID, confirm: 'yes' }, respond: () => new Response(null, { status: 204 }) });
		expect(await actions.deleteOverride(yes.event)).toMatchObject({ action: 'deleteOverride', deleted: true });
		expect(yes.calls()).toEqual([{ method: 'DELETE', path: `${base}/overrides/${OVERRIDE_ID}`, search: '', body: undefined }]);

		const bad = fakeEvent({ params, form: { override_id: '../x', confirm: 'yes' } });
		expect(await actions.deleteOverride(bad.event)).toMatchObject({ status: 400 });
		expect(bad.calls()).toEqual([]);
	});

	it('delete needs confirm=yes and then returns to the list', async () => {
		const ask = fakeEvent({ params, form: {} });
		expect(await actions.delete(ask.event)).toEqual({ action: 'delete', confirmDelete: true });
		expect(ask.calls()).toEqual([]);

		const yes = fakeEvent({ params, form: { confirm: 'yes' }, respond: () => new Response(null, { status: 204 }) });
		await expect(actions.delete(yes.event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/oncall` });
		expect(yes.calls()).toEqual([{ method: 'DELETE', path: base, search: '', body: undefined }]);
	});

	it('delete shows a 409 with the engine message', async () => {
		const { event } = fakeEvent({ params, form: { confirm: 'yes' }, status: 409, body: '{"error":"schedule is used by escalation policy \\"Default\\""}' });
		expect(await actions.delete(event)).toMatchObject({
			status: 409,
			data: { action: 'delete', message: 'schedule is used by escalation policy "Default"' }
		});
	});

	it("shows the engine's 403 as the form message", async () => {
		const { event } = fakeEvent({ params, form: { override_id: OVERRIDE_ID, confirm: 'yes' }, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.deleteOverride(event)).toMatchObject({ status: 403, data: { message: 'forbidden' } });
	});
});

function renderPage(role: 'admin' | 'member' | 'viewer', form: unknown = null) {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role,
		schedule: schedule(),
		shifts,
		overrides,
		members
	};
	return render(Page, { props: { params: { id: PROJECT_ID, scheduleId: SCHEDULE_ID }, data, form } as never }).body;
}

describe('schedule detail page', () => {
	it('shows a viewer the schedule, shifts and overrides, but no form', () => {
		const body = renderPage('viewer');
		for (const action of ['save', 'addOverride', 'deleteOverride', 'delete']) expect(body).not.toContain(`action="?/${action}"`);
		expect(body).toContain('Primary');
		expect(body).toContain('Mon 19 Oct 09:00');
		expect(body).toContain('Override');
		expect(body).toContain('gone@b.test');
	});

	it('shows an admin every form', () => {
		const body = renderPage('admin');
		for (const action of ['save', 'addOverride', 'deleteOverride', 'delete']) expect(body).toContain(`action="?/${action}"`);
		// The zone of the override inputs is stated next to them.
		expect(body).toContain('(Europe/Berlin)');
		// Reordering is announced, and focus finds its way back to the buttons.
		expect(body).toMatch(/role="status" aria-live="polite"/);
		expect(body).toContain(`id="${SCHEDULE_ID}-down-`);
	});

	it('flags participants who are no longer project members', () => {
		expect(renderPage('viewer')).toContain('not a project member: never paged');
		expect(renderPage('admin')).toContain('not a project member: never paged');
	});

	it('asks before deleting, on the page, without JavaScript', () => {
		expect(renderPage('admin', { action: 'delete', confirmDelete: true })).toContain('Yes, delete');
		expect(renderPage('admin', { action: 'deleteOverride', confirmDeleteOverride: OVERRIDE_ID })).toContain('Yes, delete');
	});
});
