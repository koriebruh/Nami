import { enginePath } from '#lib/server/core/engine';
import { channelActions, channelLoad } from '#lib/server/incidents/channel-form';
import type { Actions, PageServerLoad } from './$types';
import type { RequestEvent } from '@sveltejs/kit';

const base = (event: RequestEvent) => enginePath`/projects/${event.params.id}/notification-channels`;

export const load: PageServerLoad = channelLoad(base);
export const actions = channelActions(base, 'project') satisfies Actions;
