import { error, fail, redirect } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	publicEngineUrl,
	publicSyslogAddress,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { formText } from '#lib/server/uptime/monitor-form';
import { sourcePayloadFromForm } from '#lib/server/logs/source-form';
import type { DailyUsage, Source, SourceWithToken } from '#lib/logs/logs';
import type { Actions, PageServerLoad } from './$types';

function sourcePath(params: { id: string; sourceId: string }): string {
	return enginePath`/projects/${params.id}/sources/${params.sourceId}`;
}

export const load: PageServerLoad = async (event) => {
	// The page shows the token after a reveal or a rotate (a no-JavaScript form
	// post renders it from this load): no browser or proxy may keep it. Set
	// first and only here, since SvelteKit throws on a second set.
	event.setHeaders({ 'cache-control': 'private, no-store' });

	const path = sourcePath(event.params);
	const response = await callEngine(event, path);
	redirectToLoginIfUnauthorized(response);
	if (response.status === 404) throw error(404, 'Source not found.');
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load the source.')
		);
	}
	const source: Source = await response.json();
	// Usage is a nicety: the page works without it.
	// The engine answers oldest first and zero-filled, so the last day is today.
	const usageResponse = await callEngine(event, `${path}/usage?days=30`);
	const usage: DailyUsage[] = usageResponse.ok ? await usageResponse.json() : [];
	const today: DailyUsage | null = usage.at(-1) ?? null;
	return { source, today, usage, engineUrl: publicEngineUrl(), syslog: publicSyslogAddress() };
};

type ActionName = 'update' | 'reveal' | 'rotate' | 'delete';

/** `engineFailure` plus the action that failed, so the page shows it beside its control. */
async function failure(action: ActionName, response: Response, fallback: string) {
	const { status, data } = await engineFailure(response, fallback);
	return fail(status, { ...data, action });
}

/** The token goes to the page in the action result only, never into a URL. */
async function tokenResult(action: 'reveal' | 'rotate', response: Response, fallback: string) {
	if (response.status === 403) {
		return fail(403, {
			action,
			message: 'Only project members and admins can see or rotate the token.'
		});
	}
	if (!response.ok) return failure(action, response, fallback);
	const body: { token: string } | SourceWithToken = await response.json();
	return { token: body.token };
}

export const actions: Actions = {
	update: async (event) => {
		const parsed = sourcePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { action: 'update' as const, message: parsed.message });
		const response = await callEngine(event, sourcePath(event.params), {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (response.status === 403) {
			return fail(403, {
				action: 'update' as const,
				message: 'Only project members and admins can change a source.'
			});
		}
		if (!response.ok) return failure('update', response, 'Could not save the source.');
		return { saved: true as const };
	},
	reveal: async (event) => {
		const response = await callEngine(event, `${sourcePath(event.params)}/token`);
		return tokenResult('reveal', response, 'Could not reveal the token.');
	},
	rotate: async (event) => {
		// Rotating ends the old token at once, for every sender: confirmed like
		// a delete. The button asks with confirm() and then sends `confirm=yes`;
		// without JavaScript a bare POST sends the page back to ask first.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmRotate: true as const };
		const response = await callEngine(event, `${sourcePath(event.params)}/rotate`, {
			method: 'POST'
		});
		const result = await tokenResult('rotate', response, 'Could not rotate the token.');
		return 'token' in result ? { ...result, rotated: true as const } : result;
	},
	delete: async (event) => {
		// As rotate: a bare POST must not delete a source.
		const form = await event.request.formData();
		if (formText(form, 'confirm') !== 'yes') return { confirmDelete: true as const };
		const response = await callEngine(event, sourcePath(event.params), { method: 'DELETE' });
		if (response.status === 403) {
			return fail(403, { action: 'delete' as const, message: 'Only project admins can delete a source.' });
		}
		if (!response.ok) return failure('delete', response, 'Could not delete the source.');
		throw redirect(303, enginePath`/projects/${event.params.id}/logs/sources`);
	}
};
