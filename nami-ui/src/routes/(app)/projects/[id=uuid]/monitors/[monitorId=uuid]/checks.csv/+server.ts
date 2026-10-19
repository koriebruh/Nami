import { error, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, enginePath } from '#lib/server/engine';
import { relayStream } from '#lib/server/relay-stream';
import { isUuid } from '#lib/uuid';
import type { RequestHandler } from './$types';

// An RFC 3339 timestamp or a plain date, nothing else: the values are parsed
// and sent on re-serialised, so no typed text reaches the engine URL.
const DATE_PARAM = /^\d{4}-\d{2}-\d{2}(T\d{2}:\d{2}(:\d{2}(\.\d{1,9})?)?(Z|[+-]\d{2}:\d{2}))?$/;

function dateParam(value: string | null): string | null {
	if (value === null || !DATE_PARAM.test(value)) return null;
	const ms = Date.parse(value);
	return Number.isFinite(ms) ? new Date(ms).toISOString() : null;
}

const plain = (status: number, text: string, extra: Record<string, string> = {}) =>
	new Response(text, {
		status,
		headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store', ...extra }
	});

/**
 * The check history of a monitor as a CSV download, streamed from the engine.
 * Only `from` and `to` are forwarded.
 */
export const GET: RequestHandler = async (event) => {
	const { id, monitorId } = event.params;
	if (!isUuid(id) || !isUuid(monitorId)) throw error(404, 'Not found');
	const from = dateParam(event.url.searchParams.get('from'));
	const to = dateParam(event.url.searchParams.get('to'));
	if (!from || !to) return plain(400, 'from and to must be dates (RFC 3339).');

	let response: Response;
	try {
		response = await callEngine(
			event,
			`${enginePath`/projects/${id}/monitors/${monitorId}/checks/export`}?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`,
			{ signal: event.request.signal }
		);
	} catch (cause) {
		// The browser leaving aborts the request: nothing to answer then.
		if (event.request.signal.aborted) throw cause;
		return plain(502, 'The export service is unreachable.');
	}

	if (response.status === 401) throw redirect(303, '/login');
	if (response.status === 503) {
		const wait = response.headers.get('retry-after');
		await response.body?.cancel();
		return plain(503, 'Too many exports are running. Try again in a few seconds.', wait && /^\d{1,4}$/.test(wait) ? { 'retry-after': wait } : {});
	}
	if (!response.ok || !response.body) {
		const message = response.ok ? 'The export returned no data.' : await engineErrorMessage(response, 'Could not export checks.');
		return plain(response.ok ? 502 : response.status, message);
	}

	return new Response(relayStream(response.body), {
		status: 200,
		headers: {
			'content-type': 'text/csv; charset=utf-8',
			'content-disposition': `attachment; filename="checks-${monitorId}.csv"`,
			'cache-control': 'no-store'
		}
	});
};
