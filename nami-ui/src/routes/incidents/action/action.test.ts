import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const inspected = { action: 'acknowledge', incident: { id: 'i', project_id: 'p', project_name: 'Shop', title: 'db down',
	status: 'open', severity: 'high', started_at: '2026-10-19T00:00:00Z' }, user_email: 'a@b.test' };

describe('incident action page', () => {
	it('inspects without mutating and sets no-store and no-referrer once', async () => {
		const { event, calls, setHeaders } = fakeEvent({ url: 'http://ui.test/incidents/action?token=abc', body: JSON.stringify(inspected) });
		expect(await load(event)).toEqual({ inspected, token: 'abc' });
		expect(calls()).toEqual([{ method: 'POST', path: '/incident-actions/inspect', search: '', body: '{"token":"abc"}' }]);
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders.mock.calls[0][0]).toMatchObject({ 'cache-control': 'private, no-store', 'referrer-policy': 'no-referrer' });
	});

	it('shows the engine problem for used, expired or revoked links', async () => {
		const { event } = fakeEvent({ url: 'http://ui.test/incidents/action?token=abc', status: 410, body: '{"error":"this link was already used"}' });
		expect(await load(event)).toEqual({ problem: 'this link was already used' });
	});

	it('refuses a missing token', async () => {
		await expect(load(fakeEvent({ url: 'http://ui.test/incidents/action' }).event)).rejects.toMatchObject({ status: 400 });
	});

	it('confirms with the token from the form body only', async () => {
		const { event, calls } = fakeEvent({ url: 'http://ui.test/incidents/action?token=fromurl', form: { token: 'fromform' }, body: '{"status":"acknowledged"}' });
		expect(await actions.confirm(event)).toEqual({ done: true, status: 'acknowledged' });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: '/incident-actions/confirm', body: '{"token":"fromform"}' });
	});

	it('refuses an overlong token without calling the engine', async () => {
		const { event, calls } = fakeEvent({ url: `http://ui.test/incidents/action?token=${'a'.repeat(1025)}` });
		await expect(load(event)).rejects.toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
		const atCap = fakeEvent({ url: `http://ui.test/incidents/action?token=${'a'.repeat(1024)}`, body: JSON.stringify(inspected) });
		await expect(load(atCap.event)).resolves.toMatchObject({ inspected });
	});

	it('turns an unexpected engine status into a page error', async () => {
		const { event } = fakeEvent({ url: 'http://ui.test/incidents/action?token=abc', status: 500, body: '{"error":"internal error"}' });
		await expect(load(event)).rejects.toMatchObject({ status: 500 });
	});

	it('shows the engine message when a confirm is refused, with its status', async () => {
		const { event } = fakeEvent({ form: { token: 'abc' }, status: 409, body: '{"error":"the incident is already acknowledged"}' });
		expect(await actions.confirm(event)).toMatchObject({ status: 409, data: { message: 'the incident is already acknowledged' } });
	});

	it('does not call the engine for a confirm without a token', async () => {
		const { event, calls } = fakeEvent({ form: {} });
		expect(await actions.confirm(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});
});
