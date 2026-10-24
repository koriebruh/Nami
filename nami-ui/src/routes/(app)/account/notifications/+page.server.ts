import { enginePath } from '#lib/server/core/engine';
import { channelActions, channelLoad } from '#lib/server/incidents/channel-form';
import type { Actions, PageServerLoad } from './$types';

// The caller's own channels; the engine scopes them to the session user.
const base = () => enginePath`/account/notification-channels`;

export const load: PageServerLoad = channelLoad(base);
export const actions = channelActions(base, 'personal') satisfies Actions;
