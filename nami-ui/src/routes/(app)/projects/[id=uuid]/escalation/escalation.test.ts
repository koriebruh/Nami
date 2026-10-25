import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import type { PolicyView } from '#lib/incidents/escalation';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const POLICY_ID = '5d2c7e1a-3b4f-4a6d-8e9c-1f2a3b4c5d6e';
const OTHER_ID = '7e8f9a0b-1c2d-4e3f-8a5b-6c7d8e9f0a1b';
const base = `/projects/${PROJECT_ID}/escalation-policies`;

function policy(overrides: Partial<PolicyView> = {}): PolicyView {
	return {
		id: POLICY_ID,
		project_id: PROJECT_ID,
		name: 'Default',
		repeat_count: 0,
		fallback_policy_id: null,
		is_default: true,
		steps: [{ position: 0, delay_minutes: 0, condition: null, targets: [{ type: 'all_members', label: 'All members' }] }],
		updated_at: '2026-10-25T00:00:00Z',
		...overrides
	};
}

describe('escalation list', () => {
	it('loads the policies', async () => {
		const { event, calls } = fakeEvent({ body: JSON.stringify([policy()]) });
		expect(await load(event)).toEqual({ policies: [policy()] });
		expect(calls().map((c) => c.path)).toEqual([base]);
	});

	it('keeps a 403 a 403', async () => {
		await expect(load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({ status: 403 });
	});

	it('creates a policy with one step to all members, then opens the editor', async () => {
		const { event, calls } = fakeEvent({
			form: { name: 'Night', step_count: '1', step_0_delay: '0', step_0_target: 'all_members' },
			status: 201,
			body: JSON.stringify(policy({ id: OTHER_ID }))
		});
		await expect(actions.create(event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/escalation/${OTHER_ID}` });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: base,
				search: '',
				body: JSON.stringify({
					name: 'Night',
					repeat_count: 0,
					fallback_policy_id: null,
					steps: [{ delay_minutes: 0, targets: [{ type: 'all_members' }], condition: null }]
				})
			}
		]);
	});

	it('refuses a bad form without calling the engine, and shows the engine message', async () => {
		const bad = fakeEvent({ form: { name: '', step_count: '1', step_0_delay: '0', step_0_target: 'all_members' } });
		expect(await actions.create(bad.event)).toMatchObject({ status: 400, data: { action: 'create' } });
		expect(bad.calls()).toEqual([]);
		const taken = fakeEvent({
			form: { name: 'Default', step_count: '1', step_0_delay: '0', step_0_target: 'all_members' },
			status: 409,
			body: '{"error":"a policy with this name already exists"}'
		});
		expect(await actions.create(taken.event)).toMatchObject({
			status: 409,
			data: { action: 'create', message: 'a policy with this name already exists' }
		});
	});

	it('makes a policy the default', async () => {
		const { event, calls } = fakeEvent({ form: { policy_id: OTHER_ID }, body: JSON.stringify(policy({ id: OTHER_ID })) });
		expect(await actions.makeDefault(event)).toMatchObject({ action: 'default' });
		expect(calls()).toEqual([{ method: 'POST', path: `${base}/${OTHER_ID}/default`, search: '', body: '{}' }]);
	});

	it('deletes in two steps and shows the 409 of the default policy', async () => {
		const ask = fakeEvent({ form: { policy_id: POLICY_ID } });
		expect(await actions.delete(ask.event)).toEqual({ action: 'delete', confirmDelete: POLICY_ID });
		expect(ask.calls()).toEqual([]);
		const message = 'the default policy cannot be deleted; make another policy the default first';
		const refused = fakeEvent({ form: { policy_id: POLICY_ID, confirm: 'yes' }, status: 409, body: JSON.stringify({ error: message }) });
		expect(await actions.delete(refused.event)).toMatchObject({ status: 409, data: { action: 'delete', message } });
		expect(refused.calls()).toEqual([{ method: 'DELETE', path: `${base}/${POLICY_ID}`, search: '', body: undefined }]);
	});

	it('refuses a policy id that is not a UUID', async () => {
		const { event, calls } = fakeEvent({ form: { policy_id: '../auth/me', confirm: 'yes' } });
		expect(await actions.delete(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});
});

function renderPage(role: 'admin' | 'member' | 'viewer') {
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role,
		policies: [policy(), policy({ id: OTHER_ID, name: 'Night', is_default: false })]
	};
	return render(Page, { props: { params: { id: PROJECT_ID }, data, form: null } as never }).body;
}

describe('escalation list page', () => {
	it('lists the policies with the default badge and links to the editor', () => {
		const body = renderPage('viewer');
		expect(body).toContain(`href="/projects/${PROJECT_ID}/escalation/${OTHER_ID}"`);
		expect(body).toContain('Night');
		expect(body).toContain('Default</span>');
	});

	it('offers create, make default and delete to admins only', () => {
		for (const role of ['viewer', 'member'] as const) {
			const body = renderPage(role);
			expect(body).not.toContain('action="?/create"');
			expect(body).not.toContain('action="?/makeDefault"');
			expect(body).not.toContain('action="?/delete"');
		}
		const admin = renderPage('admin');
		expect(admin).toContain('action="?/create"');
		expect(admin).toContain('action="?/makeDefault"');
		expect(admin).toContain('action="?/delete"');
	});
});
