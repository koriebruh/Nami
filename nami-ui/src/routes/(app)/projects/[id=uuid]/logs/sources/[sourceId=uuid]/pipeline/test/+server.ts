import { error } from '@sveltejs/kit';
import { enginePath } from '#lib/server/core/engine';
import { relayJsonPost } from '#lib/server/core/json-relay';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

/** The pipeline editor's live preview: runs unsaved rules over sample events. Nothing is stored. */
export const POST: RequestHandler = async (event) => {
	const { id, sourceId } = event.params;
	if (!isUuid(id) || !isUuid(sourceId)) throw error(404, 'Not found');
	return relayJsonPost(event, enginePath`/projects/${id}/sources/${sourceId}/pipeline/test`);
};
