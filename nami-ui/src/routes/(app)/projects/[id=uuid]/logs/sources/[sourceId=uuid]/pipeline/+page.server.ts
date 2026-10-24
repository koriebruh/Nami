import { error, fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { formText } from '#lib/server/uptime/monitor-form';
import type { Source } from '#lib/logs/logs';
import type { Rule } from '#lib/logs/pipeline';
import type { Actions, PageServerLoad } from './$types';

function pipelinePath(params: { id: string; sourceId: string }): string {
	return enginePath`/projects/${params.id}/sources/${params.sourceId}/pipeline`;
}

async function loaded<T>(response: Response, what: string): Promise<T> {
	redirectToLoginIfUnauthorized(response);
	if (response.status === 404) throw error(404, 'Source not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, `Could not load the ${what}.`));
	}
	return (await response.json()) as T;
}

export const load: PageServerLoad = async (event) => {
	const sourcePath = enginePath`/projects/${event.params.id}/sources/${event.params.sourceId}`;
	const source = await loaded<Source>(await callEngine(event, sourcePath), 'source');
	const pipeline = await loaded<{ rules: Rule[]; updated_at: string }>(
		await callEngine(event, pipelinePath(event.params)),
		'pipeline'
	);
	return { source, rules: pipeline.rules, updatedAt: pipeline.updated_at };
};

export const actions: Actions = {
	save: async (event) => {
		let rules: unknown;
		try {
			rules = JSON.parse(formText(await event.request.formData(), 'rules'));
		} catch {
			rules = undefined;
		}
		if (!Array.isArray(rules)) return fail(400, { message: 'The rules are not valid JSON.' });
		const response = await callEngine(event, pipelinePath(event.params), {
			method: 'PUT',
			body: JSON.stringify({ rules })
		});
		if (response.status === 403) {
			return fail(403, { message: 'Only project members and admins can change the pipeline.' });
		}
		if (!response.ok) return engineFailure(response, 'Could not save the pipeline.');
		return { saved: true as const };
	}
};
