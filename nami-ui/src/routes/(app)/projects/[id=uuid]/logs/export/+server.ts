import { error } from '@sveltejs/kit';
import { callEngine, enginePath } from '#lib/server/core/engine';
import { EXPORT_PARAMS, pickLogParams } from '#lib/server/logs/log-proxy';
import { relayStream } from '#lib/server/core/relay-stream';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

const TYPES = new Set(['text/csv; charset=utf-8', 'application/x-ndjson']);
const DISPOSITION = /^attachment; filename="nami-logs-\d{8}T\d{6}Z\.(csv|ndjson)"$/;

/**
 * Export download: relays nami-engine's streamed CSV/NDJSON body as it
 * arrives, with only the headers a download needs. The body is relayed
 * chunk by chunk (`relayStream`: never read whole or buffered here), so:
 * - the engine's backpressure reaches the browser (the adapter writes a
 *   chunk only once the previous one drained);
 * - an engine stream that breaks (the engine cut its connection because
 *   the export could not end properly) errors this body too, which the
 *   adapter turns into a cut connection, never a clean end of file;
 * - the browser leaving aborts the engine request, which ends the export
 *   task and the ClickHouse query.
 *
 * An engine error before the file starts (400, 401, 403, 429 the user's
 * own 2 exports, 503 the engine's export slots all taken, 500) is passed
 * through with its status, JSON body and `Retry-After`.
 */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id)) throw error(404, 'Not found');
	const query = pickLogParams(event.url.searchParams, EXPORT_PARAMS);
	let response: Response;
	try {
		response = await callEngine(
			event,
			enginePath`/projects/${event.params.id}/logs/export` + query,
			{ signal: event.request.signal }
		);
	} catch (cause) {
		// The browser leaving aborts the request: nothing to answer then.
		if (event.request.signal.aborted) throw cause;
		return json502('the log service is unreachable');
	}
	if (!response.ok || !response.body) {
		return new Response(await response.text(), {
			status: response.ok ? 502 : response.status,
			headers: errorHeaders(response.headers.get('content-type') ?? 'application/json', response)
		});
	}
	const type = response.headers.get('content-type') ?? '';
	if (!TYPES.has(type)) {
		await response.body.cancel();
		return json502('unexpected export response');
	}
	const disposition = response.headers.get('content-disposition') ?? '';
	const extension = type === 'application/x-ndjson' ? 'ndjson' : 'csv';
	const headers: Record<string, string> = {
		'content-type': type,
		'content-disposition': DISPOSITION.test(disposition)
			? disposition
			: `attachment; filename="nami-logs.${extension}"`,
		'cache-control': 'no-store',
		'x-content-type-options': 'nosniff',
		'x-accel-buffering': 'no'
	};
	const limit = response.headers.get('x-nami-export-limit');
	if (limit && /^\d{1,9}$/.test(limit)) headers['x-nami-export-limit'] = limit;
	return new Response(relayStream(response.body), { headers });
};

/**
 * Headers of an answer that is not the file: never cached, never sniffed
 * (the body may be the engine's text), and the engine's `Retry-After` on a
 * 429/503 when it is a plain number.
 */
function errorHeaders(type: string, engine?: Response): Record<string, string> {
	const headers: Record<string, string> = {
		'content-type': type,
		'cache-control': 'no-store',
		'x-content-type-options': 'nosniff'
	};
	const wait = engine?.headers.get('retry-after');
	if (wait && /^\d{1,4}$/.test(wait)) headers['retry-after'] = wait;
	return headers;
}

function json502(message: string): Response {
	return new Response(JSON.stringify({ error: message }), {
		status: 502,
		headers: errorHeaders('application/json')
	});
}
