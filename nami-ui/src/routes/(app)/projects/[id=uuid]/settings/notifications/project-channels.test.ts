import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import type { Channel } from '#lib/incidents/channels';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const CHANNEL_ID = '6f1e2d3c-4b5a-4c9d-8e7f-0a1b2c3d4e5f';
const base = `/projects/${PROJECT_ID}/notification-channels`;
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

function channel(overrides: Partial<Channel> = {}): Channel {
	return {
		id: CHANNEL_ID,
		project_id: PROJECT_ID,
		user_id: null,
		name: 'Team list',
		kind: 'email',
		summary: 'oncall@example.com',
		enabled: true,
		created_at: '2026-10-20T10:00:00Z',
		updated_at: '2026-10-20T10:00:00Z',
		...overrides
	};
}

describe('project channels load', () => {
	it('lists the project channels with no-store', async () => {
		const { event, setHeaders, calls } = fakeEvent({ body: JSON.stringify([channel()]) });
		expect(await load(event)).toEqual({ channels: [channel()] });
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
		expect(calls()).toEqual([{ method: 'GET', path: base, search: '', body: undefined }]);
	});

	it('answers 403 for a non-member and sends a lost session to login', async () => {
		await expect(load(fakeEvent({ status: 403, body: '{"error":"forbidden"}' }).event)).rejects.toMatchObject({ status: 403 });
		await expect(load(fakeEvent({ status: 401 }).event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

describe('project channel actions', () => {
	it('creates an email channel (project scope) and a webhook with its secret', async () => {
		const email = fakeEvent({
			form: { name: 'Team list', kind: 'email', address: ' oncall@example.com ' },
			respond: () => json(channel(), 201)
		});
		expect(await actions.create(email.event)).toEqual({ action: 'create', created: true });
		expect(email.calls()).toEqual([
			{
				method: 'POST',
				path: base,
				search: '',
				body: JSON.stringify({ name: 'Team list', kind: 'email', config: { address: 'oncall@example.com' }, enabled: true })
			}
		]);

		const hook = fakeEvent({
			form: { name: 'Hook', kind: 'webhook', url: 'http://receiver.internal/nami' },
			respond: () => json({ ...channel({ kind: 'webhook' }), signing_secret: 's3cr3t' }, 201)
		});
		expect(await actions.create(hook.event)).toEqual({ action: 'create', created: true, signingSecret: 's3cr3t' });
	});

	it('surfaces an engine 403 on create as the form message', async () => {
		const { event } = fakeEvent({
			form: { name: 'S', kind: 'slack', webhook_url: 'https://hooks.slack.com/services/x' },
			status: 403,
			body: '{"error":"forbidden"}'
		});
		expect(await actions.create(event)).toMatchObject({
			status: 403,
			data: { action: 'create', message: 'Only project admins can change project channels.' }
		});
	});

	it('saves an edit that replaces the config whole', async () => {
		const { event, calls } = fakeEvent({
			form: { channel_id: CHANNEL_ID, name: 'Team', kind: 'email', address: 'team@example.com', enabled: 'on' },
			respond: () => json(channel())
		});
		expect(await actions.update(event)).toMatchObject({ action: 'update', channel: CHANNEL_ID });
		expect(JSON.parse(calls()[0].body!)).toEqual({
			name: 'Team',
			enabled: true,
			kind: 'email',
			config: { address: 'team@example.com' }
		});
		expect(calls()[0]).toMatchObject({ method: 'PATCH', path: `${base}/${CHANNEL_ID}` });
	});

	it('sends a test and shows the engine outcome', async () => {
		const ok = fakeEvent({ form: { channel_id: CHANNEL_ID }, respond: () => json({ ok: true }) });
		expect(await actions.test(ok.event)).toEqual({ action: 'test', tested: CHANNEL_ID, ok: true });
		expect(ok.calls()).toEqual([{ method: 'POST', path: `${base}/${CHANNEL_ID}/test`, search: '', body: '{}' }]);

		const failed = fakeEvent({ form: { channel_id: CHANNEL_ID }, status: 502, body: '{"error":"connection failed"}' });
		expect(await actions.test(failed.event)).toMatchObject({
			status: 502,
			data: { tested: CHANNEL_ID, message: 'connection failed' }
		});
	});

	it('asks before deleting, then deletes, and shows an in-use refusal', async () => {
		const first = fakeEvent({ form: { channel_id: CHANNEL_ID } });
		expect(await actions.delete(first.event)).toEqual({ action: 'delete', confirmDelete: CHANNEL_ID });
		expect(first.calls()).toEqual([]);

		const { event, calls } = fakeEvent({
			form: { channel_id: CHANNEL_ID, confirm: 'yes' },
			respond: () => new Response(null, { status: 204 })
		});
		expect(await actions.delete(event)).toEqual({ action: 'delete', deleted: true, message: 'Channel deleted.' });
		expect(calls()).toEqual([{ method: 'DELETE', path: `${base}/${CHANNEL_ID}`, search: '', body: undefined }]);

		const inUse = fakeEvent({
			form: { channel_id: CHANNEL_ID, confirm: 'yes' },
			status: 409,
			body: '{"error":"channel is used by escalation policy \\"Default\\""}'
		});
		expect(await actions.delete(inUse.event)).toMatchObject({
			status: 409,
			data: { message: 'channel is used by escalation policy "Default"' }
		});
	});

	it('never lets a form value steer the engine path', async () => {
		for (const action of [actions.update, actions.test, actions.delete]) {
			for (const channel_id of ['../auth/me?', '', `${CHANNEL_ID}/x`]) {
				const { event, calls } = fakeEvent({ form: { channel_id, name: 'x', kind: 'email', confirm: 'yes' } });
				expect(await action(event), channel_id).toMatchObject({ status: 400 });
				expect(calls()).toEqual([]);
			}
		}
	});
});

describe('project channels page', () => {
	const renderPage = (role: 'admin' | 'member' | 'viewer') =>
		render(Page, {
			props: {
				params: { id: PROJECT_ID },
				data: {
					user: { id: 'u', email: 'a@b.test', global_role: 'member' },
					project: { id: PROJECT_ID, name: 'Demo', slug: 'demo' },
					role,
					channels: [channel()]
				},
				form: null
			} as never
		});

	it('shows a viewer the list and no form', () => {
		const { body } = renderPage('viewer');
		expect(body).toContain('Team list');
		expect(body).toContain('oncall@example.com');
		expect(body).not.toContain('<form');
		expect(body).toContain('Only project admins can change project channels.');
	});

	it('shows an admin the forms, with the project email kind', () => {
		const { body } = renderPage('admin');
		const created = body.match(/<input[^>]*id="new-[^"]*"[^>]*>/g) ?? [];
		expect(created.map((tag) => /id="new-([^"]*)"/.exec(tag)![1])).toEqual(['address', 'url', 'webhook_url', 'bot_token', 'chat_id']);
		for (const tag of created) expect(tag).not.toContain('required');
		expect(created.find((tag) => tag.includes('name="chat_id"'))).toContain('type="text"');
		expect(created.find((tag) => tag.includes('name="address"'))).toContain('type="email"');
		expect(body).toContain('<form');
		expect(body).toContain('Send test');
		expect(body).toContain('<option value="email"');
		expect(body).not.toContain('Only project admins can change project channels.');
		for (const tag of body.match(/<input[^>]*name="(address|url|webhook_url|bot_token|chat_id)"[^>]*>/g) ?? []) {
			expect(tag).not.toContain('value=');
		}
	});
});
