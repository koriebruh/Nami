import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	publicEngineUrl,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { loadAvailability } from '#lib/server/uptime/availability';
import { heartbeatPayloadFromForm } from '#lib/server/uptime/heartbeat-form';
import { formText } from '#lib/server/uptime/monitor-form';
import type { Heartbeat, HeartbeatPing, MonitorGroup } from '#lib/uptime/uptime';
import type { Actions, PageServerLoad, RequestEvent } from './$types';

async function jsonArray<T>(response: Response): Promise<T[]> {
	if (!response.ok) return [];
	try {
		const body: unknown = await response.json();
		return Array.isArray(body) ? (body as T[]) : [];
	} catch {
		return [];
	}
}

function heartbeatPath(params: { id: string; heartbeatId: string }): string {
	return enginePath`/projects/${params.id}/heartbeats/${params.heartbeatId}`;
}

export const load: PageServerLoad = async (event) => {
	// The page carries the ping token, in its markup and in the data that goes
	// with it: no browser or proxy may keep either. Set first, so that every
	// response of this load has it (a viewer's, an error page, a redirect, and
	// the page a form without JavaScript gets back), and only here: SvelteKit
	// throws when a header is set twice.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const [response, pingsResponse, groupsResponse, availability] = await Promise.all([
		callEngine(event, heartbeatPath(event.params)),
		callEngine(event, `${heartbeatPath(event.params)}/pings?limit=50`),
		callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`),
		loadAvailability(event, `${heartbeatPath(event.params)}/availability`)
	]);
	redirectToLoginIfUnauthorized(response);
	if (response.status === 404) throw error(404, 'Heartbeat not found.');
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load heartbeat.')
		);
	}
	// `token` is null for a viewer (see the list load): the page shows no ping
	// URL then.
	const heartbeat: Heartbeat = await response.json();
	// Pings and groups are extras: the page renders without them.
	const pings = await jsonArray<HeartbeatPing>(pingsResponse);
	const groups = await jsonArray<MonitorGroup>(groupsResponse);
	return { heartbeat, pings, groups, engineUrl: publicEngineUrl(), availability };
};

type ActionName = 'update' | 'pause' | 'resume' | 'rotate' | 'delete';

/**
 * `engineFailure` plus the name of the action that failed. The page shows the
 * message beside the control that was used (the settings form is far from the
 * pause button), and every action's failure arrives in the same `form`.
 */
async function failure(action: ActionName, response: Response, fallback: string) {
	const { status, data } = await engineFailure(response, fallback);
	return fail(status, { ...data, action });
}

function patchHeartbeat(event: RequestEvent, body: Record<string, unknown>): Promise<Response> {
	return callEngine(event, heartbeatPath(event.params), { method: 'PATCH', body: JSON.stringify(body) });
}

async function setPaused(event: RequestEvent, paused: boolean) {
	const response = await patchHeartbeat(event, { paused });
	if (!response.ok) return failure(paused ? 'pause' : 'resume', response, 'Could not update heartbeat.');
	return { success: true as const };
}

export const actions: Actions = {
	update: async (event) => {
		const parsed = heartbeatPayloadFromForm(await event.request.formData(), 'edit');
		if (!parsed.ok) return fail(400, { action: 'update' as const, message: parsed.message });
		const response = await patchHeartbeat(event, parsed.payload);
		if (!response.ok) return failure('update', response, 'Could not update heartbeat.');
		return { saved: true as const };
	},
	pause: (event) => setPaused(event, true),
	resume: (event) => setPaused(event, false),
	rotate: async (event) => {
		// Rotating ends the old ping URL at once, so it is confirmed like a
		// delete: the button asks with confirm() and then sends `confirm=yes`;
		// without JavaScript a bare POST sends the page back to ask first.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmRotate: true as const };

		const response = await callEngine(event, `${heartbeatPath(event.params)}/rotate-token`, {
			method: 'POST'
		});
		if (!response.ok) return failure('rotate', response, 'Could not rotate token.');
		// The new token is not passed on here: the page reads it from the
		// heartbeat that its load fetches once this action is done.
		return { rotated: true as const };
	},
	delete: async (event) => {
		// The Delete button asks with confirm() and then sends `confirm=yes`.
		// Without JavaScript there is no dialog, and a bare POST must not delete
		// a heartbeat and its incidents: send the page back to ask first.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmDelete: true as const };

		const response = await callEngine(event, heartbeatPath(event.params), { method: 'DELETE' });
		if (!response.ok) return failure('delete', response, 'Could not delete heartbeat.');
		throw redirect(303, enginePath`/projects/${event.params.id}/heartbeats`);
	}
};
