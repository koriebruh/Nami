import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const TOKEN = 'a'.repeat(64);
const submit = (event: unknown) => (actions.default as (e: any) => Promise<unknown>)(event);

describe('verify email load', () => {
	it('never calls the engine, with or without a token', async () => {
		for (const url of [
			`http://ui.test/verify-email?token=${TOKEN}`,
			'http://ui.test/verify-email?token=used',
			'http://ui.test/verify-email'
		]) {
			const { event, calls } = fakeEvent({ url });
			await load(event);
			expect(calls()).toEqual([]);
		}
	});

	it('reports only whether a token is present', async () => {
		const withToken = fakeEvent({ url: `http://ui.test/verify-email?token=${TOKEN}` });
		await expect(load(withToken.event)).resolves.toEqual({ hasToken: true });
		for (const url of ['http://ui.test/verify-email', 'http://ui.test/verify-email?token=']) {
			await expect(load(fakeEvent({ url }).event)).resolves.toEqual({ hasToken: false });
		}
	});
});

describe('verify email action', () => {
	it('verifies the token with the engine and reports success', async () => {
		const { event, calls } = fakeEvent({ status: 200, form: { token: TOKEN } });
		await expect(submit(event)).resolves.toEqual({ verified: true });
		expect(calls()).toEqual([{ method: 'GET', path: '/auth/verify-email', search: `?token=${TOKEN}`, body: undefined }]);
	});

	it('shows the generic failure for a token the engine rejects', async () => {
		const { event } = fakeEvent({ status: 400, body: '{"error":"invalid or expired token"}', form: { token: 'used' } });
		await expect(submit(event)).resolves.toEqual({ verified: false });
	});

	it('shows the generic failure when the engine is unreachable', async () => {
		const { event } = fakeEvent({ unreachable: true, form: { token: TOKEN } });
		await expect(submit(event)).resolves.toEqual({ verified: false });
	});

	it('keeps a token with query characters inside the one query value', async () => {
		const { event, calls } = fakeEvent({ status: 400, form: { token: 'a&b=c' } });
		await submit(event);
		expect(calls()[0].search).toBe('?token=a%26b%3Dc');
	});

	it('does not call the engine without a token', async () => {
		for (const form of [{} as Record<string, string>, { token: '' }]) {
			const { event, calls } = fakeEvent({ form });
			await expect(submit(event)).resolves.toEqual({ verified: false });
			expect(calls()).toEqual([]);
		}
	});
});
