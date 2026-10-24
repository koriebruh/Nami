// Notification channel forms → engine bodies, and the load and actions shared
// by the personal (/account/notifications) and project channel pages.
// Secrets are write-only: nothing here ever reads one back from the engine.
import { error, fail, type RequestEvent } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { isUuid } from '#lib/core/uuid';
import { CHANNEL_FIELDS, isChannelKind, type Channel } from '#lib/incidents/channels';

export { CHANNEL_KINDS } from '#lib/incidents/channels';

export type ChannelPayload = { name?: string; kind?: string; config?: Record<string, string>; enabled?: boolean };

const text = (form: FormData, name: string) => String(form.get(name) ?? '').trim();

/** mode 'create': every config field of the kind is required. mode 'edit': config only when every
 *  secret field of the kind is filled (all-or-nothing replace); blank = keep. */
export function channelPayloadFromForm(
	form: FormData,
	mode: 'create' | 'edit',
	scope: 'personal' | 'project'
): { ok: true; payload: ChannelPayload } | { ok: false; message: string } {
	const kind = text(form, 'kind');
	if (!isChannelKind(kind)) return { ok: false, message: 'Unknown channel kind.' };
	if (kind === 'email' && scope === 'personal') {
		return { ok: false, message: 'Email goes to your account address; pick another kind.' };
	}
	const name = text(form, 'name');
	if (name === '') return { ok: false, message: 'Name is required.' };

	// Only the kind's own keys: the engine refuses unknown config fields.
	const fields = CHANNEL_FIELDS[kind];
	const config = Object.fromEntries(fields.map((f) => [f.name, text(form, f.name)]));
	const filled = fields.filter((f) => config[f.name] !== '');

	if (mode === 'create') {
		if (filled.length < fields.length) {
			return { ok: false, message: `${fields.map((f) => f.label).join(' and ')} required.` };
		}
		return { ok: true, payload: { name, kind, config, enabled: true } };
	}

	const payload: ChannelPayload = { name, enabled: form.has('enabled') };
	if (filled.length === 0) return { ok: true, payload };
	if (filled.length < fields.length) {
		return {
			ok: false,
			message: `Fill in both ${fields.map((f) => f.label).join(' and ')} to replace them, or leave both blank to keep them.`
		};
	}
	return { ok: true, payload: { ...payload, kind, config } };
}

/** The engine path of a page's channel list, e.g. `/account/notification-channels`. */
type Base = (event: RequestEvent) => string;

export function channelLoad(base: Base) {
	return async (event: RequestEvent) => {
		// The page takes secrets and shows a signing secret once (the no-JS form
		// post renders it from this load): no browser or proxy may keep it.
		event.setHeaders({ 'cache-control': 'private, no-store' });
		const response = await callEngine(event, base(event));
		redirectToLoginIfUnauthorized(response);
		if (!response.ok) {
			throw error(
				response.status === 403 ? 403 : 500,
				await engineErrorMessage(response, 'Could not load notification channels.')
			);
		}
		const channels: Channel[] = await response.json();
		return { channels };
	};
}

const ADMIN_ONLY = 'Only project admins can change project channels.';

export function channelActions(base: Base, scope: 'personal' | 'project') {
	async function failure<A extends string>(action: A, response: Response, fallback: string, extra = {}) {
		const { status, data } = await engineFailure(response, fallback);
		const message = scope === 'project' && status === 403 ? ADMIN_ONLY : data.message;
		return fail(status, { action, message, ...extra });
	}

	/** The channel id from the form, only if it is a UUID (it goes into a path). */
	const channelId = (form: FormData) => {
		const id = form.get('channel_id');
		return isUuid(id) ? id : null;
	};
	const unknown = <A extends string>(action: A) => fail(400, { action, message: 'Unknown channel.' });

	return {
		// Returns the signing secret instead of redirecting: it is shown once and
		// never put in a URL.
		create: async (event: RequestEvent) => {
			const parsed = channelPayloadFromForm(await event.request.formData(), 'create', scope);
			if (!parsed.ok) return fail(400, { action: 'create' as const, message: parsed.message });
			const response = await callEngine(event, base(event), {
				method: 'POST',
				body: JSON.stringify(parsed.payload)
			});
			if (!response.ok) return failure('create' as const, response, 'Could not create the channel.');
			const created: Channel & { signing_secret?: string } = await response.json();
			return created.signing_secret
				? { action: 'create' as const, created: true, signingSecret: created.signing_secret }
				: { action: 'create' as const, created: true };
		},
		update: async (event: RequestEvent) => {
			const form = await event.request.formData();
			const id = channelId(form);
			if (!id) return unknown('update' as const);
			const parsed = channelPayloadFromForm(form, 'edit', scope);
			if (!parsed.ok) return fail(400, { action: 'update' as const, channel: id, message: parsed.message });
			const response = await callEngine(event, base(event) + enginePath`/${id}`, {
				method: 'PATCH',
				body: JSON.stringify(parsed.payload)
			});
			if (!response.ok) return failure('update' as const, response, 'Could not save the channel.', { channel: id });
			return { action: 'update' as const, channel: id, saved: true, message: 'Channel saved.' };
		},
		// 200 is a sent test; 429 (too soon) and 502 (the send failed) carry the
		// engine's reason, shown on the channel's row.
		test: async (event: RequestEvent) => {
			const id = channelId(await event.request.formData());
			if (!id) return unknown('test' as const);
			const response = await callEngine(event, base(event) + enginePath`/${id}/test`, { method: 'POST' });
			if (!response.ok) return failure('test' as const, response, 'The test could not be sent.', { tested: id });
			return { action: 'test' as const, tested: id, ok: true };
		},
		delete: async (event: RequestEvent) => {
			const form = await event.request.formData();
			const id = channelId(form);
			if (!id) return unknown('delete' as const);
			// The Delete button asks with confirm() and then sends `confirm=yes`.
			// Without JavaScript a bare POST must not delete: ask on the page first.
			if (text(form, 'confirm') !== 'yes') return { action: 'delete' as const, confirmDelete: id };
			const response = await callEngine(event, base(event) + enginePath`/${id}`, { method: 'DELETE' });
			if (!response.ok) return failure('delete' as const, response, 'Could not delete the channel.');
			return { action: 'delete' as const, deleted: true, message: 'Channel deleted.' };
		}
	};
}
