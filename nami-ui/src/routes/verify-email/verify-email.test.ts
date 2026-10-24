import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { load } from './+page.server';

const TOKEN = 'a'.repeat(64);

describe('verify email load', () => {
	it('verifies the token with the engine and reports success', async () => {
		const { event, calls } = fakeEvent({ url: `http://ui.test/verify-email?token=${TOKEN}`, status: 200 });
		await expect(load(event)).resolves.toEqual({ verified: true });
		expect(calls()).toEqual([{ method: 'GET', path: '/auth/verify-email', search: `?token=${TOKEN}`, body: undefined }]);
	});

	it('shows the generic failure for a token the engine rejects', async () => {
		const { event } = fakeEvent({
			url: 'http://ui.test/verify-email?token=used',
			status: 400,
			body: '{"error":"invalid or expired token"}'
		});
		await expect(load(event)).resolves.toEqual({ verified: false });
	});

	it('shows the generic failure when the engine is unreachable', async () => {
		const { event } = fakeEvent({ url: `http://ui.test/verify-email?token=${TOKEN}`, unreachable: true });
		await expect(load(event)).resolves.toEqual({ verified: false });
	});

	it('keeps a token with query characters inside the one query value', async () => {
		const { event, calls } = fakeEvent({ url: 'http://ui.test/verify-email?token=a%26b%3Dc', status: 400 });
		await load(event);
		expect(calls()[0].search).toBe('?token=a%26b%3Dc');
	});

	it('does not call the engine without a token', async () => {
		for (const url of ['http://ui.test/verify-email', 'http://ui.test/verify-email?token=']) {
			const { event, calls } = fakeEvent({ url });
			await expect(load(event)).resolves.toEqual({ verified: false });
			expect(calls()).toEqual([]);
		}
	});
});
