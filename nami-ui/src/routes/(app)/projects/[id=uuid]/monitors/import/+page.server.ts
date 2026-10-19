import { fail, type RequestEvent } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import { importPayloadFromForm } from '#lib/server/import-form';
import type { ImportReport, MonitorGroup } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitor-groups`);
	redirectToLoginIfUnauthorized(response);
	// The group picker is optional: offer "No group" alone if this fails.
	const groups: MonitorGroup[] = response.ok ? await response.json() : [];
	return { groups };
};

const FRIENDLY: Record<number, string> = {
	403: 'Only members can import monitors.',
	409: 'Another import is already running in this project. Wait for it to finish, then try again.',
	413: 'The import is too large (the limit is 2 MiB). Split it into smaller files.'
};

async function run(event: RequestEvent, dryRun: boolean) {
	let form: FormData;
	try {
		form = await event.request.formData();
	} catch {
		return fail(400, { message: 'The form could not be read.' });
	}
	// Handed back with every failure too, so the pasted text is not lost.
	const format = String(form.get('format') ?? 'urls');
	const pasted = String(form.get('content') ?? '');
	const echo = { format, content: pasted };

	const parsed = await importPayloadFromForm(form, dryRun);
	if (!parsed.ok) return fail(400, { message: parsed.message, ...echo });

	const response = await callEngine(event, enginePath`/projects/${event.params.id}/monitors/import`, {
		method: 'POST',
		body: JSON.stringify(parsed.payload)
	});
	redirectToLoginIfUnauthorized(response);
	if (!response.ok) {
		const message =
			FRIENDLY[response.status] ?? (await engineErrorMessage(response, 'Could not import monitors.'));
		return fail(response.status, { message, ...echo });
	}
	const report: ImportReport = await response.json();
	return { report, ...echo };
}

export const actions: Actions = {
	preview: (event) => run(event, true),
	import: (event) => run(event, false)
};
