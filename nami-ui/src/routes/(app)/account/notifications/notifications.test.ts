import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import type { Channel } from '#lib/incidents/channels';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const CHANNEL_ID = '6f1e2d3c-4b5a-4c9d-8e7f-0a1b2c3d4e5f';
const base = '/account/notification-channels';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

function channel(overrides: Partial<Channel> = {}): Channel {
	return {
		id: CHANNEL_ID,
		project_id: null,
		user_id: 'u',
		name: 'My Slack',
		kind: 'slack',
		summary: 'hooks.slack.com',
		enabled: true,
		created_at: '2026-10-20T10:00:00Z',
		updated_at: '2026-10-20T10:00:00Z',
		...overrides
	};
}

describe('personal channels load', () => {
	it('lists the caller own channels with no-store', async () => {
		const { event, setHeaders, calls } = fakeEvent({ body: JSON.stringify([channel()]) });
		expect(await load(event)).toEqual({ channels: [channel()] });
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
		expect(calls()).toEqual([{ method: 'GET', path: base, search: '', body: undefined }]);
	});

	it('sends a lost session to the login page', async () => {
		await expect(load(fakeEvent({ status: 401 }).event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});
});

describe('personal channel actions', () => {
	it('creates a webhook and returns its signing secret once, in the result only', async () => {
		const { event, calls } = fakeEvent({
			form: { name: 'Hook', kind: 'webhook', url: 'https://hooks.example.com/nami' },
			respond: () => json({ ...channel({ kind: 'webhook' }), signing_secret: 's3cr3t' }, 201)
		});
		expect(await actions.create(event)).toEqual({ action: 'create', created: true, signingSecret: 's3cr3t' });
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: base,
				search: '',
				body: JSON.stringify({ name: 'Hook', kind: 'webhook', config: { url: 'https://hooks.example.com/nami' }, enabled: true })
			}
		]);
	});

	it('returns no signing secret for other kinds', async () => {
		const { event } = fakeEvent({
			form: { name: 'S', kind: 'slack', webhook_url: 'https://hooks.slack.com/services/x' },
			respond: () => json(channel(), 201)
		});
		expect(await actions.create(event)).toEqual({ action: 'create', created: true });
	});

	it('refuses a bad form without calling the engine and relays engine errors', async () => {
		const bad = fakeEvent({ form: { name: 'x', kind: 'email', address: 'a@b.test' } });
		expect(await actions.create(bad.event)).toMatchObject({ status: 400, data: { action: 'create' } });
		expect(bad.calls()).toEqual([]);
		const refused = fakeEvent({
			form: { name: 'x', kind: 'webhook', url: 'http://127.0.0.1/' },
			status: 400,
			body: '{"error":"personal channels must point at a public address"}'
		});
		expect(await actions.create(refused.event)).toMatchObject({
			status: 400,
			data: { message: 'personal channels must point at a public address' }
		});
	});

	// The no-JS create form posts every kind's fields; only the chosen kind's go.
	it('creates from the no-JS form, which carries every kind\'s fields', async () => {
		const { event, calls } = fakeEvent({
			form: { name: 'D', kind: 'discord', url: '', webhook_url: 'https://discord.com/api/webhooks/1/x', bot_token: '', chat_id: '' },
			respond: () => json(channel({ kind: 'discord' }), 201)
		});
		expect(await actions.create(event)).toEqual({ action: 'create', created: true });
		expect(JSON.parse(calls()[0].body!)).toEqual({
			name: 'D',
			kind: 'discord',
			config: { webhook_url: 'https://discord.com/api/webhooks/1/x' },
			enabled: true
		});
	});

	it('saves an edit with PATCH, keeping the secret when the field is blank', async () => {
		const { event, calls } = fakeEvent({
			form: { channel_id: CHANNEL_ID, name: 'Renamed', kind: 'slack', webhook_url: '' },
			respond: () => json(channel())
		});
		expect(await actions.update(event)).toEqual({ action: 'update', channel: CHANNEL_ID, saved: true, message: 'Channel saved.' });
		expect(calls()).toEqual([
			{ method: 'PATCH', path: `${base}/${CHANNEL_ID}`, search: '', body: JSON.stringify({ name: 'Renamed', enabled: false }) }
		]);
	});

	it('sends a test with an empty JSON body and shows the outcome', async () => {
		const ok = fakeEvent({ form: { channel_id: CHANNEL_ID }, respond: () => json({ ok: true }) });
		expect(await actions.test(ok.event)).toEqual({ action: 'test', tested: CHANNEL_ID, ok: true });
		expect(ok.calls()).toEqual([{ method: 'POST', path: `${base}/${CHANNEL_ID}/test`, search: '', body: '{}' }]);

		const failed = fakeEvent({ form: { channel_id: CHANNEL_ID }, status: 502, body: '{"error":"HTTP 404"}' });
		expect(await actions.test(failed.event)).toMatchObject({
			status: 502,
			data: { action: 'test', tested: CHANNEL_ID, message: 'HTTP 404' }
		});
		const limited = fakeEvent({
			form: { channel_id: CHANNEL_ID },
			status: 429,
			body: '{"error":"wait a few seconds before testing again"}'
		});
		expect(await actions.test(limited.event)).toMatchObject({
			status: 429,
			data: { tested: CHANNEL_ID, message: 'wait a few seconds before testing again' }
		});
	});

	it('asks before deleting, then deletes with confirm=yes', async () => {
		const first = fakeEvent({ form: { channel_id: CHANNEL_ID } });
		expect(await actions.delete(first.event)).toEqual({ action: 'delete', confirmDelete: CHANNEL_ID });
		expect(first.calls()).toEqual([]);

		const { event, calls } = fakeEvent({
			form: { channel_id: CHANNEL_ID, confirm: 'yes' },
			respond: () => new Response(null, { status: 204 })
		});
		expect(await actions.delete(event)).toEqual({ action: 'delete', deleted: true, message: 'Channel deleted.' });
		expect(calls()).toEqual([{ method: 'DELETE', path: `${base}/${CHANNEL_ID}`, search: '', body: undefined }]);
	});

	it('never lets a form value steer the engine path', async () => {
		for (const action of [actions.update, actions.test, actions.delete]) {
			for (const channel_id of ['../auth/me?', '', `${CHANNEL_ID}/x`]) {
				const { event, calls } = fakeEvent({ form: { channel_id, name: 'x', kind: 'slack', confirm: 'yes' } });
				expect(await action(event), channel_id).toMatchObject({ status: 400 });
				expect(calls()).toEqual([]);
			}
		}
	});
});

describe('personal channels page', () => {
	const user = { id: 'u', email: 'a@b.test', global_role: 'viewer' };
	const renderPage = (form: unknown = null, channels = [channel()]) =>
		render(Page, { props: { params: {}, data: { user, channels }, form } as never });

	it('never renders a secret value, and offers no personal email kind', () => {
		const { body } = renderPage();
		expect(body).toContain('My Slack');
		expect(body).toContain('hooks.slack.com');
		const secretInputs = body.match(/<input[^>]*name="webhook_url"[^>]*>/g) ?? [];
		expect(secretInputs.length).toBeGreaterThan(0);
		for (const tag of secretInputs) {
			expect(tag).not.toContain('value=');
			expect(tag).toContain('type="password"');
			expect(tag).toContain('autocomplete="off"');
		}
		expect(body).toContain('Leave blank to keep the current value');
		expect(body).not.toContain('<option value="email"');
		expect(body).toContain('Send test');
	});

	// svelte/server runs no onMount: this is the markup a browser without JavaScript gets.
	it('gives a no-JS create form every personal kind\'s fields, once each and none required', () => {
		const { body } = renderPage(null, []);
		const inputs = body.match(/<input[^>]*id="new-[^"]*"[^>]*>/g) ?? [];
		expect(inputs.map((tag) => /id="new-([^"]*)"/.exec(tag)![1])).toEqual(['url', 'webhook_url', 'bot_token', 'chat_id']);
		for (const tag of inputs) {
			expect(tag).not.toContain('required');
			expect(tag).not.toContain('value=');
			expect(tag).not.toContain('type="email"');
		}
		expect(body).toContain('Webhook URL (Slack, Microsoft Teams, Discord)');
	});

	it('shows the signing secret once with the receiver guidance', () => {
		const { body } = renderPage({ action: 'create', created: true, signingSecret: 's3cr3t' });
		expect(body).toContain('s3cr3t');
		expect(body).toContain('Shown once. Store it now.');
		expect(body).toContain('X-Nami-Signature');
		expect(body).toContain('X-Nami-Delivery');
	});

	it('shows the test outcome on its channel and the second delete step', () => {
		expect(renderPage({ action: 'test', tested: CHANNEL_ID, message: 'HTTP 404' }).body).toContain('HTTP 404');
		expect(renderPage({ action: 'test', tested: CHANNEL_ID, ok: true }).body).toContain('Test sent.');
		expect(renderPage({ action: 'delete', confirmDelete: CHANNEL_ID }).body).toContain('Yes, delete');
	});
});
