import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/fake-event';
import { actions, load } from './+page.server';
import { actions as editActions, load as editLoad } from './[windowId=uuid]/+page.server';

const WINDOW_ID = '7d1e2f3a-4b5c-4d6e-8f70-8192a3b4c5d6';
const base = `/projects/${PROJECT_ID}/maintenance-windows`;
const onceForm = { title: ' DB upgrade ', kind: 'once', monitor_id: '', starts_at: '2026-10-13T22:00', ends_at: '2026-10-14T02:00' };

describe('maintenance list load', () => {
	it('returns windows with monitor names only', async () => {
		const windows = [{ id: WINDOW_ID, title: 'x' }];
		const { event, calls } = fakeEvent({
			respond: (url) =>
				new Response(url.pathname.endsWith('/monitors') ? '[{"id":"m1","name":"API","url":"secret"}]' : JSON.stringify(windows))
		});
		expect(await load(event)).toEqual({ windows, monitors: [{ id: 'm1', name: 'API' }] });
		expect(calls().map((c) => c.path).sort()).toEqual([base, `/projects/${PROJECT_ID}/monitors`]);
	});

	it('still lists windows when the monitors cannot be read', async () => {
		const { event } = fakeEvent({ respond: (url) => (url.pathname.endsWith('/monitors') ? new Response('{}', { status: 500 }) : new Response('[]')) });
		expect(await load(event)).toEqual({ windows: [], monitors: [] });
	});

	it('keeps a 403, turns other failures into a 500, sends a lost session to login', async () => {
		await expect(load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({ status: 403 });
		await expect(load(fakeEvent({ status: 500, body: '{"error":"boom"}' }).event)).rejects.toMatchObject({ status: 500, body: { message: 'boom' } });
		await expect(load(fakeEvent({ status: 401 }).event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

describe('maintenance create action', () => {
	it('posts the parsed payload as JSON', async () => {
		const { event, calls } = fakeEvent({ status: 201, form: onceForm });
		expect(await actions.create(event)).toEqual({ action: 'create', success: true, message: 'Maintenance window created.' });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: base,
				search: '',
				body: JSON.stringify({ title: 'DB upgrade', kind: 'once', starts_at: '2026-10-13T22:00:00Z', ends_at: '2026-10-14T02:00:00Z' })
			}
		]);
	});

	it('refuses a malformed form before calling the engine', async () => {
		const { event, calls } = fakeEvent({ form: { ...onceForm, ends_at: '' } });
		expect(await actions.create(event)).toMatchObject({ status: 400, data: { message: 'Enter a start and an end date and time' } });
		expect(calls()).toEqual([]);
	});

	it('shows the engine message, e.g. a bad time zone', async () => {
		const { event } = fakeEvent({
			status: 400,
			body: '{"error":"timezone must be an IANA name"}',
			form: { title: 'x', kind: 'weekly', days_of_week: '1', start_time: '01:00', end_time: '02:00', timezone: 'Nope' }
		});
		expect(await actions.create(event)).toMatchObject({
			status: 400,
			data: { action: 'create', success: false, message: 'timezone must be an IANA name' }
		});
	});
});

describe('maintenance delete action', () => {
	it('asks first without confirm=yes and deletes nothing', async () => {
		const { event, calls } = fakeEvent({ form: { window_id: WINDOW_ID } });
		expect(await actions.delete(event)).toEqual({ action: 'delete', confirmDelete: WINDOW_ID });
		expect(calls()).toEqual([]);
	});

	it('deletes with confirm=yes', async () => {
		const { event, calls } = fakeEvent({ status: 200, body: "{}", form: { window_id: WINDOW_ID, confirm: 'yes' } });
		expect(await actions.delete(event)).toMatchObject({ action: 'delete', success: true });
		expect(calls()).toEqual([{ method: 'DELETE', path: `${base}/${WINDOW_ID}`, search: '', body: undefined }]);
	});

	it('never lets a form value steer the engine path', async () => {
		for (const window_id of ['../auth/me?', `${WINDOW_ID}/..`, '']) {
			const { event, calls } = fakeEvent({ form: { window_id, confirm: 'yes' } });
			expect(await actions.delete(event), window_id).toMatchObject({ status: 400 });
			expect(calls()).toEqual([]);
		}
	});

	it("shows a viewer's 403", async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}', form: { window_id: WINDOW_ID, confirm: 'yes' } });
		expect(await actions.delete(event)).toMatchObject({ status: 403, data: { success: false, message: 'forbidden' } });
	});
});

describe('maintenance edit', () => {
	const params = { windowId: WINDOW_ID };

	it('is a 404 when the engine does not know the window (e.g. another project)', async () => {
		const { event } = fakeEvent({ params, status: 404, body: '{"error":"not found"}' });
		await expect(editLoad(event)).rejects.toMatchObject({ status: 404 });
	});

	it('patches a cleared scope as null and goes back to the list', async () => {
		const { event, calls } = fakeEvent({ params, form: onceForm });
		await expect(editActions.default(event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/maintenance` });
		expect(calls()[0]).toMatchObject({ method: 'PATCH', path: `${base}/${WINDOW_ID}` });
		expect(JSON.parse(calls()[0].body!)).toMatchObject({ monitor_id: null, title: 'DB upgrade' });
	});

	it('shows the engine message when the save is refused', async () => {
		const { event } = fakeEvent({ params, status: 400, body: '{"error":"ends_at must be after starts_at"}', form: onceForm });
		expect(await editActions.default(event)).toMatchObject({ status: 400, data: { message: 'ends_at must be after starts_at' } });
	});
});
