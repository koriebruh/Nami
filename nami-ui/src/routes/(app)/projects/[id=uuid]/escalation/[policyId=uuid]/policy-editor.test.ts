import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { policyPayloadFromForm } from '#lib/server/incidents/policy-form';
import type { EditorState, PolicyView, TargetChoice } from '#lib/incidents/escalation';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const POLICY_ID = '5d2c7e1a-3b4f-4a6d-8e9c-1f2a3b4c5d6e';
const OTHER_ID = '7e8f9a0b-1c2d-4e3f-8a5b-6c7d8e9f0a1b';
const USER_ID = '951106f5-21e1-4c23-9a60-9df4fc9e79ed';
const SCHEDULE_ID = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const CHANNEL_ID = '1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d';
const params = { policyId: POLICY_ID };
const path = `/projects/${PROJECT_ID}/escalation-policies/${POLICY_ID}`;

function policy(overrides: Partial<PolicyView> = {}): PolicyView {
	return {
		id: POLICY_ID,
		project_id: PROJECT_ID,
		name: 'Business hours',
		repeat_count: 1,
		fallback_policy_id: OTHER_ID,
		is_default: false,
		steps: [
			{
				position: 0,
				delay_minutes: 0,
				condition: { weekdays: [1, 2, 3, 4, 5], from: '09:00', to: '17:00', timezone: 'Asia/Jakarta' },
				targets: [{ type: 'user', id: USER_ID, label: 'ann@b.test' }]
			},
			{ position: 1, delay_minutes: 15, condition: null, targets: [{ type: 'schedule', id: SCHEDULE_ID, label: 'Primary' }] }
		],
		updated_at: '2026-10-25T00:00:00Z',
		...overrides
	};
}
const other = policy({ id: OTHER_ID, name: 'Default', is_default: true, fallback_policy_id: null });
const members = [{ user_id: USER_ID, email: 'ann@b.test', project_role: 'member' }];
const schedules = [{ id: SCHEDULE_ID, name: 'Primary' }];
const channels = [{ id: CHANNEL_ID, name: 'Ops', kind: 'slack' }];

function answers(url: URL): Response {
	const p = url.pathname;
	if (p.endsWith('/members')) return new Response(JSON.stringify(members));
	if (p.endsWith('/schedules')) return new Response(JSON.stringify(schedules));
	if (p.endsWith('/notification-channels')) return new Response(JSON.stringify(channels));
	if (p.endsWith('/escalation-policies')) return new Response(JSON.stringify([policy(), other]));
	return new Response(JSON.stringify(policy()));
}

const saveForm = { name: 'Business hours', repeat_count: '1', fallback_policy_id: OTHER_ID, step_count: '1', step_0_delay: '5', step_0_target: `channel:${CHANNEL_ID}` };

describe('policy editor load', () => {
	it('loads the policy, schedules, project channels, members and policies', async () => {
		const { event, calls } = fakeEvent({ params, respond: answers });
		const data = (await load(event)) as { policy: PolicyView; editor: EditorState; choices: TargetChoice[]; fallbacks: unknown[] };
		expect(calls().map((c) => c.path).sort()).toEqual(
			[
				path,
				`/projects/${PROJECT_ID}/escalation-policies`,
				`/projects/${PROJECT_ID}/members`,
				`/projects/${PROJECT_ID}/notification-channels`,
				`/projects/${PROJECT_ID}/schedules`
			].sort()
		);
		expect(data.policy).toEqual(policy());
		expect(data.editor.steps.map((s) => s.targets[0].value)).toEqual([`user:${USER_ID}`, `schedule:${SCHEDULE_ID}`]);
		expect(data.choices.map((c) => c.value)).toEqual(['all_members', `user:${USER_ID}`, `schedule:${SCHEDULE_ID}`, `channel:${CHANNEL_ID}`]);
		// The fallback can be any other policy, never the policy itself.
		expect(data.fallbacks).toEqual([{ id: OTHER_ID, name: 'Default' }]);
	});

	it('keeps an id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { policyId: '../x' }, respond: answers });
		await load(event);
		expect(calls().some((c) => c.path === `/projects/${PROJECT_ID}/escalation-policies/..%2Fx`)).toBe(true);
	});

	it('is a 404 for a policy the engine does not have, and keeps a 403 a 403', async () => {
		await expect(load(fakeEvent({ params, status: 404, body: '{"error":"not found"}' }).event)).rejects.toMatchObject({ status: 404 });
		await expect(load(fakeEvent({ params, status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({ status: 403 });
	});
});

describe('policy editor actions', () => {
	it('saves with a PUT of the whole policy', async () => {
		const { event, calls } = fakeEvent({ params, form: saveForm, body: JSON.stringify(policy()) });
		expect(await actions.save(event)).toMatchObject({ action: 'save', saved: true });
		expect(calls()).toEqual([
			{
				method: 'PUT',
				path,
				search: '',
				body: JSON.stringify({
					name: 'Business hours',
					repeat_count: 1,
					fallback_policy_id: OTHER_ID,
					steps: [{ delay_minutes: 5, targets: [{ type: 'project_channel', id: CHANNEL_ID }], condition: null }]
				})
			}
		]);
	});

	it('shows the engine message of a 400, and refuses a bad form without calling it', async () => {
		const refused = fakeEvent({ params, form: saveForm, status: 400, body: '{"error":"fallback policy not found"}' });
		expect(await actions.save(refused.event)).toMatchObject({ status: 400, data: { action: 'save', message: 'fallback policy not found' } });
		const bad = fakeEvent({ params, form: { ...saveForm, step_0_target: 'user:../../auth/me' } });
		expect(await actions.save(bad.event)).toMatchObject({ status: 400, data: { action: 'save' } });
		expect(bad.calls()).toEqual([]);
	});

	it('shows an engine 403 as the form message', async () => {
		const { event } = fakeEvent({ params, form: saveForm, status: 403, body: '{"error":"forbidden"}' });
		expect(await actions.save(event)).toMatchObject({ status: 403, data: { action: 'save', message: 'forbidden' } });
	});

	it('makes the policy the default with a POST of {}', async () => {
		const { event, calls } = fakeEvent({ params, form: {}, body: JSON.stringify(policy({ is_default: true })) });
		expect(await actions.default(event)).toMatchObject({ action: 'default' });
		expect(calls()).toEqual([{ method: 'POST', path: `${path}/default`, search: '', body: '{}' }]);
	});

	it('deletes only with confirm=yes, shows a 409, and goes back to the list', async () => {
		const ask = fakeEvent({ params, form: {} });
		expect(await actions.delete(ask.event)).toEqual({ action: 'delete', confirmDelete: POLICY_ID });
		expect(ask.calls()).toEqual([]);
		const message = 'the default policy cannot be deleted; make another policy the default first';
		const refused = fakeEvent({ params, form: { confirm: 'yes' }, status: 409, body: JSON.stringify({ error: message }) });
		expect(await actions.delete(refused.event)).toMatchObject({ status: 409, data: { action: 'delete', message } });
		const ok = fakeEvent({ params, form: { confirm: 'yes' }, body: '{"deleted":true}' });
		await expect(actions.delete(ok.event)).rejects.toMatchObject({ status: 303, location: `/projects/${PROJECT_ID}/escalation` });
		expect(ok.calls()).toEqual([{ method: 'DELETE', path, search: '', body: undefined }]);
	});
});

async function renderPage(role: 'admin' | 'member' | 'viewer') {
	const { event } = fakeEvent({ params, respond: answers });
	const data = {
		user: { id: 'u', email: 'a@b.test', global_role: 'member' },
		project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
		role,
		...(await load(event))
	};
	return render(Page, { props: { params: { id: PROJECT_ID, policyId: POLICY_ID }, data, form: null } as never }).body;
}

describe('policy editor page', () => {
	it('shows viewers and members the steps read-only', async () => {
		for (const role of ['viewer', 'member'] as const) {
			const body = await renderPage(role);
			expect(body).not.toContain('action="?/save"');
			expect(body).not.toContain('action="?/delete"');
			expect(body).toContain('ann@b.test');
			expect(body).toContain('Primary');
			expect(body).toContain('09:00–17:00 (Asia/Jakarta)');
			expect(body).toContain('Default');
		}
	});

	it('gives admins the editor with the stored values', async () => {
		const body = await renderPage('admin');
		expect(body).toContain('action="?/save"');
		expect(body).toContain('action="?/default"');
		expect(body).toContain('action="?/delete"');
		expect(body).toContain('name="step_count" value="2"');
		expect(body).toContain(`name="step_0_target" value="user:${USER_ID}"`);
		expect(body).toContain(`name="step_1_target" value="schedule:${SCHEDULE_ID}"`);
		expect(body).toMatch(/name="step_0_condition"[^>]*checked/);
		// The fallback select offers the other policy, never this one.
		expect(body).toMatch(new RegExp(`<option value="${OTHER_ID}"[^>]*selected`));
		expect(body).not.toContain(`<option value="${POLICY_ID}"`);
	});

	// What the browser would submit for the rendered form, unchanged: inputs
	// with a name (checkboxes only when checked) and each select's selected
	// option (else its first).
	function submitted(body: string): FormData {
		const html = body.slice(body.indexOf('action="?/save"'), body.indexOf('</form>', body.indexOf('action="?/save"')));
		const attr = (tag: string, name: string) => new RegExp(`\\s${name}="([^"]*)"`).exec(tag)?.[1];
		const form = new FormData();
		for (const [tag] of html.matchAll(/<input\b[^>]*>/g)) {
			const name = attr(tag, 'name');
			if (!name) continue;
			if (attr(tag, 'type') === 'checkbox' && !/\schecked/.test(tag)) continue;
			form.append(name, attr(tag, 'value') ?? 'on');
		}
		for (const [, open, options] of html.matchAll(/<select\b([^>]*)>([\s\S]*?)<\/select>/g)) {
			const name = attr(open, 'name');
			if (!name) continue;
			const all = [...options.matchAll(/<option\b([^>]*)>/g)].map((m) => m[1]);
			form.append(name, attr(all.find((o) => /\sselected/.test(o)) ?? all[0], 'value') ?? '');
		}
		return form;
	}

	it('submits the stored policy unchanged when saved without edits', async () => {
		const stored = policy();
		expect(policyPayloadFromForm(submitted(await renderPage('admin')))).toEqual({
			ok: true,
			payload: {
				name: stored.name,
				repeat_count: stored.repeat_count,
				fallback_policy_id: stored.fallback_policy_id,
				steps: stored.steps.map(({ delay_minutes, condition, targets }) => ({
					delay_minutes,
					condition,
					targets: targets.map(({ label: _, ...target }) => target)
				}))
			}
		});
	});

	it('gives step and target controls stable ids and a live region for moves', async () => {
		const body = await renderPage('admin');
		expect(body).toMatch(/<p class="sr-only" role="status" aria-live="polite">/);
		for (const control of ['up', 'down', 'remove', 'delay', 'pick', 't0']) {
			expect(body).toMatch(new RegExp(`id="policy-s\\d+-${control}"`));
		}
	});
});
