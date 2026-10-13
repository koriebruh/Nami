import { error } from '@sveltejs/kit';
import { describe, expect, it } from 'vitest';
import { fakeEvent, MONITOR_ID, PROJECT_ID } from '#lib/server/fake-event';
import { load as loadLayout } from './+layout.server';
import { load as loadRoot } from './+page.server';
import { load as loadMonitors } from './monitors/+page.server';
import { actions as newActions } from './monitors/new/+page.server';
import { actions as settingsActions } from './settings/+page.server';

describe('project layout load', () => {
	const project = { id: PROJECT_ID, name: 'UP7 Demo', slug: 'up7-demo' };

	it('returns the project the engine found', async () => {
		const { event, calls } = fakeEvent({ body: JSON.stringify(project) });
		expect(await loadLayout(event)).toEqual({ project });
		expect(calls()).toEqual([{ method: 'GET', path: `/projects/${PROJECT_ID}`, search: '', body: undefined }]);
	});

	// The engine answers 403 for a project the caller may not see, whether or
	// not it exists; only a global admin gets the 404 of a project that is not there.
	it('is a 403 "no access" when the engine refuses', async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		await expect(loadLayout(event)).rejects.toMatchObject({
			status: 403,
			body: { message: 'You do not have access to this project.' }
		});
	});

	it('is a 404 when the engine has no such project', async () => {
		const { event } = fakeEvent({ status: 404, body: '{"error":"not found"}' });
		await expect(loadLayout(event)).rejects.toMatchObject({ status: 404, body: { message: 'Project not found.' } });
	});

	// A failing engine says nothing about access: it must not read as "no access".
	it.each([500, 502, 503, 429, 422])('is a 502 "engine unavailable", not an access message, for an engine %i', async (status) => {
		const { event } = fakeEvent({ status, body: '{"error":"internal error"}' });
		await expect(loadLayout(event)).rejects.toMatchObject({
			status: 502,
			body: { message: 'Engine unavailable, retrying' }
		});
	});

	it('is a 502 when the engine cannot be reached at all', async () => {
		const { event } = fakeEvent({ unreachable: true });
		await expect(loadLayout(event)).rejects.toMatchObject({
			status: 502,
			body: { message: 'Engine unavailable, retrying' }
		});
	});

	it('is a 502 when a 200 is not the project JSON', async () => {
		const { event } = fakeEvent({ body: '<html>bad gateway</html>' });
		await expect(loadLayout(event)).rejects.toMatchObject({ status: 502 });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(loadLayout(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, body: JSON.stringify(project) });
		await loadLayout(event);
		expect(calls().map((call) => call.path + call.search)).toEqual(['/projects/..%2Fauth%2Fme%3F']);
	});
});

describe('project root load', () => {
	it('redirects to the Monitors tab once the layouts have loaded', async () => {
		const steps: string[] = [];
		const { event } = fakeEvent();
		event.parent = async () => {
			steps.push('layouts loaded');
			return {};
		};
		await expect(loadRoot(event)).rejects.toMatchObject({ status: 307, location: `/projects/${PROJECT_ID}/monitors` });
		expect(steps).toEqual(['layouts loaded']);
	});

	// The layout is what knows the project cannot be opened: its error must win.
	it("passes on the layout's error instead of redirecting", async () => {
		const { event } = fakeEvent();
		event.parent = async () => {
			throw error(403, 'You do not have access to this project.');
		};
		await expect(loadRoot(event)).rejects.toMatchObject({ status: 403 });
	});

	it('keeps a hostile id inside one path segment of the location', async () => {
		const { event } = fakeEvent({ params: { id: '../auth/me?' } });
		await expect(loadRoot(event)).rejects.toMatchObject({ location: '/projects/..%2Fauth%2Fme%3F/monitors' });
	});
});

describe('monitor list load', () => {
	it('returns the monitors', async () => {
		const monitors = [{ id: MONITOR_ID, name: 'API', uptime_24h: null }];
		const { event, calls } = fakeEvent({ body: JSON.stringify(monitors) });
		expect(await loadMonitors(event)).toEqual({ monitors });
		expect(calls().map((call) => call.path)).toEqual([`/projects/${PROJECT_ID}/monitors`]);
	});

	it('keeps a 403 a 403', async () => {
		const { event } = fakeEvent({ status: 403, body: '{"error":"forbidden"}' });
		await expect(loadMonitors(event)).rejects.toMatchObject({ status: 403, body: { message: 'forbidden' } });
	});

	it("turns any other engine failure into a 500 with the engine's message", async () => {
		const { event } = fakeEvent({ status: 500, body: '{"error":"internal error"}' });
		await expect(loadMonitors(event)).rejects.toMatchObject({ status: 500, body: { message: 'internal error' } });
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ status: 401, body: '{"error":"unauthorized"}' });
		await expect(loadMonitors(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, body: '[]' });
		await loadMonitors(event);
		expect(calls().map((call) => call.path + call.search)).toEqual(['/projects/..%2Fauth%2Fme%3F/monitors']);
	});
});

describe('new monitor action', () => {
	const fields = { name: 'API', check_type: 'http', url: 'https://example.com' };

	it('creates the monitor and opens its page', async () => {
		const { event, calls } = fakeEvent({ form: fields, status: 201, body: JSON.stringify({ id: MONITOR_ID }) });
		await expect(newActions.default(event)).rejects.toMatchObject({
			status: 303,
			location: `/projects/${PROJECT_ID}/monitors/${MONITOR_ID}`
		});
		expect(calls()).toEqual([
			{
				method: 'POST',
				path: `/projects/${PROJECT_ID}/monitors`,
				search: '',
				body: '{"name":"API","check_type":"http","url":"https://example.com","http_method":"GET"}'
			}
		]);
	});

	it("shows the engine's validation message", async () => {
		const { event } = fakeEvent({ form: fields, status: 400, body: '{"error":"url must be a valid http(s) URL"}' });
		expect(await newActions.default(event)).toMatchObject({
			status: 400,
			data: { message: 'url must be a valid http(s) URL' }
		});
	});

	it('falls back to a fixed message when the engine body is not its error shape', async () => {
		const { event } = fakeEvent({ form: fields, status: 422, body: 'Failed to deserialize the JSON body' });
		expect(await newActions.default(event)).toMatchObject({
			status: 422,
			data: { message: 'Could not create monitor.' }
		});
	});

	it('sends a visitor whose session is gone to the login page', async () => {
		const { event } = fakeEvent({ form: fields, status: 401, body: '{"error":"unauthorized"}' });
		await expect(newActions.default(event)).rejects.toMatchObject({ status: 303, location: '/login' });
	});

	it('keeps a hostile id inside one path segment', async () => {
		const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, form: fields, body: JSON.stringify({ id: MONITOR_ID }) });
		await expect(newActions.default(event)).rejects.toMatchObject({
			location: `/projects/..%2Fauth%2Fme%3F/monitors/${MONITOR_ID}`
		});
		expect(calls().map((call) => call.path)).toEqual(['/projects/..%2Fauth%2Fme%3F/monitors']);
	});
});

describe('settings actions', () => {
	const userId = '951106f5-21e1-4c23-9a60-9df4fc9e79ed';
	const hostile = ['', '   ', '../../auth/logout', `${userId}/../../monitors`, `${userId}?x=1`, 'not-a-uuid'];

	describe('addMember', () => {
		it('adds the member with the chosen role', async () => {
			const { event, calls } = fakeEvent({ form: { user_id: userId, project_role: 'viewer' } });
			expect(await settingsActions.addMember(event)).toEqual({ action: 'add', success: true, message: 'Member added.' });
			expect(calls()).toEqual([
				{
					method: 'POST',
					path: `/projects/${PROJECT_ID}/members`,
					search: '',
					body: JSON.stringify({ user_id: userId, project_role: 'viewer' })
				}
			]);
		});

		it('ignores whitespace around the user id', async () => {
			const { event, calls } = fakeEvent({ form: { user_id: `  ${userId} `, project_role: 'member' } });
			await settingsActions.addMember(event);
			expect(JSON.parse(calls()[0].body as string).user_id).toBe(userId);
		});

		it('refuses a user id that is not a UUID, without calling the engine', async () => {
			for (const user_id of hostile) {
				const { event, calls } = fakeEvent({ form: { user_id, project_role: 'member' } });
				expect(await settingsActions.addMember(event), JSON.stringify(user_id)).toMatchObject({
					status: 400,
					data: { action: 'add', success: false, message: 'User ID must be a UUID.' }
				});
				expect(calls()).toEqual([]);
			}
		});

		it("shows the engine's message", async () => {
			const { event } = fakeEvent({ form: { user_id: userId, project_role: 'member' }, status: 404, body: '{"error":"not found"}' });
			expect(await settingsActions.addMember(event)).toEqual({ action: 'add', success: false, message: 'not found' });
		});

		it('survives an engine error that is not JSON', async () => {
			const { event } = fakeEvent({
				form: { user_id: userId, project_role: 'nobody' },
				status: 422,
				body: 'Failed to deserialize the JSON body'
			});
			expect(await settingsActions.addMember(event)).toEqual({ action: 'add', success: false, message: 'could not add member' });
		});

		it('sends a visitor whose session is gone to the login page', async () => {
			const { event } = fakeEvent({ form: { user_id: userId, project_role: 'member' }, status: 401, body: '{"error":"unauthorized"}' });
			await expect(settingsActions.addMember(event)).rejects.toMatchObject({ status: 303, location: '/login' });
		});
	});

	describe('removeMember', () => {
		it('removes the member', async () => {
			const { event, calls } = fakeEvent({ form: { user_id: userId } });
			expect(await settingsActions.removeMember(event)).toEqual({ action: 'remove', success: true, message: 'Member removed.' });
			expect(calls()).toEqual([
				{ method: 'DELETE', path: `/projects/${PROJECT_ID}/members/${userId}`, search: '', body: undefined }
			]);
		});

		it('refuses a user id that is not a UUID, without calling the engine', async () => {
			for (const user_id of hostile) {
				const { event, calls } = fakeEvent({ form: { user_id } });
				expect(await settingsActions.removeMember(event), JSON.stringify(user_id)).toMatchObject({
					status: 400,
					data: { action: 'remove', success: false, message: 'User ID must be a UUID.' }
				});
				expect(calls()).toEqual([]);
			}
		});

		it("shows the engine's message", async () => {
			const { event } = fakeEvent({ form: { user_id: userId }, status: 403, body: '{"error":"forbidden"}' });
			expect(await settingsActions.removeMember(event)).toEqual({ action: 'remove', success: false, message: 'forbidden' });
		});

		it('survives an engine error that is not JSON', async () => {
			const { event } = fakeEvent({ form: { user_id: userId }, status: 400, body: 'Invalid URL: invalid character' });
			expect(await settingsActions.removeMember(event)).toEqual({
				action: 'remove',
				success: false,
				message: 'could not remove member'
			});
		});

		it('sends a visitor whose session is gone to the login page', async () => {
			const { event } = fakeEvent({ form: { user_id: userId }, status: 401, body: '{"error":"unauthorized"}' });
			await expect(settingsActions.removeMember(event)).rejects.toMatchObject({ status: 303, location: '/login' });
		});

		it('keeps a hostile project id inside one path segment', async () => {
			const { event, calls } = fakeEvent({ params: { id: '../auth/me?' }, form: { user_id: userId } });
			await settingsActions.removeMember(event);
			expect(calls().map((call) => call.path)).toEqual([`/projects/..%2Fauth%2Fme%3F/members/${userId}`]);
		});
	});
});
