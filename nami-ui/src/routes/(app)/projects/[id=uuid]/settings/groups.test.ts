import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/fake-event';
import { actions, load } from './+page.server';

const GROUP_ID = '5a4b3c2d-1e0f-4a9b-8c7d-6e5f4a3b2c1d';
const base = `/projects/${PROJECT_ID}/monitor-groups`;

describe('settings load', () => {
	it('returns the groups', async () => {
		const groups = [{ id: GROUP_ID, name: 'APIs', position: 1 }];
		expect(await load(fakeEvent({ body: JSON.stringify(groups) }).event)).toEqual({ groups, groupsError: null });
	});

	it('keeps the page (member forms) when the groups fail to load', async () => {
		expect(await load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).toEqual({ groups: [], groupsError: 'forbidden' });
	});
});

describe('group actions', () => {
	it('creates a group at position 0 by default, as JSON', async () => {
		const { event, calls } = fakeEvent({ status: 201, form: { name: ' APIs ', position: '' } });
		expect(await actions.createGroup(event)).toEqual({ action: 'group', success: true, message: 'Group created.' });
		expect(calls()).toEqual([{ method: 'POST', path: base, search: '', body: JSON.stringify({ name: 'APIs', position: 0 }) }]);
	});

	it('refuses a position that is not a whole number in range', async () => {
		for (const position of ['1.5', 'abc', '-1', '1000001']) {
			const { event, calls } = fakeEvent({ form: { name: 'x', position } });
			expect(await actions.createGroup(event), position).toMatchObject({ status: 400, data: { success: false } });
			expect(calls()).toEqual([]);
		}
	});

	it('renames and repositions with PATCH', async () => {
		const { event, calls } = fakeEvent({ form: { group_id: GROUP_ID, name: 'Core', position: '3' } });
		expect(await actions.updateGroup(event)).toMatchObject({ success: true, message: 'Group saved.' });
		expect(calls()).toEqual([{ method: 'PATCH', path: `${base}/${GROUP_ID}`, search: '', body: JSON.stringify({ name: 'Core', position: 3 }) }]);
	});

	it('keeps the current position when the field is cleared on edit', async () => {
		const { event, calls } = fakeEvent({ status: 200, body: '{}', form: { group_id: GROUP_ID, name: 'Core', position: '' } });
		expect(await actions.updateGroup(event)).toMatchObject({ success: true });
		expect(calls()[0].body).toBe(JSON.stringify({ name: 'Core' }));
	});

	it('shows the engine message (duplicate name)', async () => {
		const { event } = fakeEvent({ status: 409, body: '{"error":"group name already used"}', form: { group_id: GROUP_ID, name: 'x' } });
		expect(await actions.updateGroup(event)).toMatchObject({ status: 409, data: { message: 'group name already used' } });
	});

	it('asks before deleting, then deletes with confirm=yes', async () => {
		const first = fakeEvent({ form: { group_id: GROUP_ID } });
		expect(await actions.deleteGroup(first.event)).toEqual({ action: 'group', confirmDelete: GROUP_ID });
		expect(first.calls()).toEqual([]);

		const { event, calls } = fakeEvent({ status: 200, body: "{}", form: { group_id: GROUP_ID, confirm: 'yes' } });
		expect(await actions.deleteGroup(event)).toMatchObject({ success: true, message: 'Group deleted; its monitors are now ungrouped.' });
		expect(calls()).toEqual([{ method: 'DELETE', path: `${base}/${GROUP_ID}`, search: '', body: undefined }]);
	});

	it('never lets a form value steer the engine path', async () => {
		for (const action of [actions.updateGroup, actions.deleteGroup]) {
			for (const group_id of ['../auth/me?', '', `${GROUP_ID}/x`]) {
				const { event, calls } = fakeEvent({ form: { group_id, name: 'x', confirm: 'yes' } });
				expect(await action(event), group_id).toMatchObject({ status: 400 });
				expect(calls()).toEqual([]);
			}
		}
	});
});
