import { error, fail } from '@sveltejs/kit';
import {
	callEngine,
	engineErrorMessage,
	engineFailure,
	enginePath,
	redirectToLoginIfUnauthorized
} from '#lib/server/core/engine';
import { PATTERN_PARAMS, pickLogParams, SEARCH_PARAMS } from '#lib/server/logs/log-proxy';
import { formText } from '#lib/server/uptime/monitor-form';
import { isUuid } from '#lib/core/uuid';
import {
	engineParams,
	PAGE_SIZE,
	parseExplorerState,
	resolveWindow,
	viewQueryFromState,
	type Histogram,
	type LogEntry,
	type LogView,
	type PatternCount,
	type Source
} from '#lib/logs/logs';
import type { Actions, PageServerLoad } from './$types';

/**
 * The explorer: its whole state is in the URL. A relative preset becomes an
 * absolute window here, once, so "Load more" pages through the same window.
 * The URL is parsed into a validated state first and only that state, through
 * the allow-list, becomes engine parameters: no arbitrary key is forwarded.
 * The Patterns tab asks for the window's top patterns instead of a page of
 * results.
 */
export const load: PageServerLoad = async (event) => {
	const state = parseExplorerState(event.url.searchParams);
	const window = resolveWindow(state, Date.now());
	const filters = engineParams(state, window);
	const query = pickLogParams(filters, SEARCH_PARAMS);
	const histogramQuery = query;
	const base = enginePath`/projects/${event.params.id}`;
	const patternsTab = state.tab === 'patterns';
	const paged = new URLSearchParams(query.slice(1));
	paged.set('limit', String(PAGE_SIZE));
	const main = patternsTab
		? `${base}/logs/patterns${pickLogParams(filters, PATTERN_PARAMS)}`
		: `${base}/logs?${paged}`;
	// Saved views last: call order is array order (tests read the first three).
	const [mainResponse, histogramResponse, sourcesResponse, viewsResponse] = await Promise.all([
		callEngine(event, main),
		callEngine(event, `${base}/logs/histogram${histogramQuery}`),
		callEngine(event, `${base}/sources`),
		callEngine(event, `${base}/log-views`)
	]);
	for (const response of [mainResponse, histogramResponse, sourcesResponse]) {
		redirectToLoginIfUnauthorized(response);
	}
	if ([mainResponse, histogramResponse, sourcesResponse].some((response) => response.status === 403)) {
		throw error(403, 'You do not have access to this project.');
	}

	let logs: LogEntry[] = [];
	let nextCursor: string | null = null;
	let searchError: string | null = null;
	let patterns: PatternCount[] = [];
	let patternsError: string | null = null;
	if (patternsTab) {
		if (mainResponse.ok) patterns = ((await mainResponse.json()) as { patterns: PatternCount[] }).patterns;
		else patternsError = await engineErrorMessage(mainResponse, 'Could not load the patterns.');
	} else if (mainResponse.ok) {
		const page: { logs: LogEntry[]; next_cursor: string | null } = await mainResponse.json();
		logs = page.logs;
		nextCursor = page.next_cursor;
	} else {
		// A bad filter (e.g. an invalid regex) is shown on the page, not as an error page.
		searchError = await engineErrorMessage(mainResponse, 'Could not search the logs.');
	}
	const histogram: Histogram | null = histogramResponse.ok ? await histogramResponse.json() : null;
	const histogramError = histogramResponse.ok
		? null
		: await engineErrorMessage(histogramResponse, 'Could not load the log volume chart.');
	const sources: Source[] = sourcesResponse.ok ? await sourcesResponse.json() : [];
	const sourcesError = sourcesResponse.ok
		? null
		: await engineErrorMessage(sourcesResponse, 'Could not load the sources.');
	// Saved views are a convenience: without them the explorer still works.
	const views: LogView[] = viewsResponse.ok ? await viewsResponse.json().catch(() => []) : [];
	return {
		state,
		window,
		logs,
		nextCursor,
		searchError,
		patterns,
		patternsError,
		histogram,
		histogramError,
		sources,
		sourcesError,
		views
	};
};

/** The explorer query string a view form sent, as a view query (never live or tab). */
function viewQuery(form: FormData) {
	return viewQueryFromState(parseExplorerState(new URLSearchParams(formText(form, 'state'))));
}

export const actions: Actions = {
	saveView: async (event) => {
		const form = await event.request.formData();
		const name = formText(form, 'name').trim();
		if (!name) return fail(400, { message: 'Enter a name for the view.' });
		const response = await callEngine(event, enginePath`/projects/${event.params.id}/log-views`, {
			method: 'POST',
			body: JSON.stringify({ name, query: viewQuery(form) })
		});
		if (!response.ok) return engineFailure(response, 'Could not save the view.');
		return { savedView: name };
	},
	updateView: async (event) => {
		const form = await event.request.formData();
		const viewId = form.get('view_id');
		if (!isUuid(viewId)) return fail(400, { message: 'Unknown view.' });
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/log-views/${viewId}`,
			{ method: 'PATCH', body: JSON.stringify({ query: viewQuery(form) }) }
		);
		if (!response.ok) return engineFailure(response, 'Could not update the view.');
		return { updatedView: true as const };
	},
	deleteView: async (event) => {
		const form = await event.request.formData();
		const viewId = form.get('view_id');
		if (!isUuid(viewId)) return fail(400, { message: 'Unknown view.' });
		// The Delete button asks with confirm() and then sends `confirm=yes`;
		// a bare POST (no JavaScript) asks on the page first.
		if (formText(form, 'confirm') !== 'yes') return { confirmDeleteView: viewId };
		const response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/log-views/${viewId}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) return engineFailure(response, 'Could not delete the view.');
		return { deletedView: true as const };
	}
};
