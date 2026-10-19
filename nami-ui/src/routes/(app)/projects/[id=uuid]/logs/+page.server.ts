import { error } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/engine';
import { pickLogParams, SEARCH_PARAMS } from '#lib/server/log-proxy';
import {
	engineParams,
	PAGE_SIZE,
	parseExplorerState,
	resolveWindow,
	type Histogram,
	type LogEntry,
	type Source
} from '#lib/logs';
import type { PageServerLoad } from './$types';

/**
 * The explorer: its whole state is in the URL. A relative preset becomes an
 * absolute window here, once, so "Load more" pages through the same window.
 * The URL is parsed into a validated state first and only that state, through
 * the allow-list, becomes engine parameters: no arbitrary key is forwarded.
 */
export const load: PageServerLoad = async (event) => {
	const state = parseExplorerState(event.url.searchParams);
	const window = resolveWindow(state, Date.now());
	const filters = engineParams(state, window);
	const query = pickLogParams(filters, SEARCH_PARAMS);
	const histogramQuery = query;
	const base = enginePath`/projects/${event.params.id}`;
	const paged = new URLSearchParams(query.slice(1));
	paged.set('limit', String(PAGE_SIZE));
	const [searchResponse, histogramResponse, sourcesResponse] = await Promise.all([
		callEngine(event, `${base}/logs?${paged}`),
		callEngine(event, `${base}/logs/histogram${histogramQuery}`),
		callEngine(event, `${base}/sources`)
	]);
	for (const response of [searchResponse, histogramResponse, sourcesResponse]) {
		redirectToLoginIfUnauthorized(response);
	}
	if ([searchResponse, histogramResponse, sourcesResponse].some((response) => response.status === 403)) {
		throw error(403, 'You do not have access to this project.');
	}

	let logs: LogEntry[] = [];
	let nextCursor: string | null = null;
	let searchError: string | null = null;
	if (searchResponse.ok) {
		const page: { logs: LogEntry[]; next_cursor: string | null } = await searchResponse.json();
		logs = page.logs;
		nextCursor = page.next_cursor;
	} else {
		// A bad filter (e.g. an invalid regex) is shown on the page, not as an error page.
		searchError = await engineErrorMessage(searchResponse, 'Could not search the logs.');
	}
	const histogram: Histogram | null = histogramResponse.ok ? await histogramResponse.json() : null;
	const sources: Source[] = sourcesResponse.ok ? await sourcesResponse.json() : [];
	return { state, window, logs, nextCursor, searchError, histogram, sources };
};
