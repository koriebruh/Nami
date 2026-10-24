import { error } from '@sveltejs/kit';
import { enginePath } from '#lib/server/core/engine';
import { relayJsonPost } from '#lib/server/core/json-relay';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

/** The alert form's live count: how many events the unsaved query matches in the window now. */
export const POST: RequestHandler = async (event) => {
	const { id } = event.params;
	if (!isUuid(id)) throw error(404, 'Not found');
	return relayJsonPost(event, enginePath`/projects/${id}/log-alerts/preview`);
};
