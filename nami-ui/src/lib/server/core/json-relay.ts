import type { RequestEvent } from '@sveltejs/kit';
import { callEngine } from '#lib/server/core/engine';

export const MAX_RELAY_BYTES = 1024 * 1024;

const reply = (status: number, error: string) =>
	new Response(JSON.stringify({ error }), {
		status,
		headers: { 'content-type': 'application/json', 'cache-control': 'no-store' }
	});

/** The body as text, or null once it passes `limit` bytes (a missing or false content-length included). */
async function boundedText(request: Request, limit: number): Promise<string | null> {
	if (!request.body) return '';
	const reader = request.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > limit) {
			await reader.cancel();
			return null;
		}
		chunks.push(value);
	}
	const bytes = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return new TextDecoder().decode(bytes);
}

/**
 * A small same-origin JSON POST (an editor's preview) relayed to nami-engine.
 * Only `application/json` is accepted: browsers cannot send that cross-site
 * without a CORS preflight, which this app never answers, and an `Origin`
 * from elsewhere is refused outright. The body is read once, bounded, and
 * checked to be JSON before the engine sees it. The engine's status and JSON
 * are passed through (a 401 tells the page to go to the login page). The
 * browser hanging up aborts the engine request too.
 */
export async function relayJsonPost(event: RequestEvent, path: string): Promise<Response> {
	const type = event.request.headers.get('content-type') ?? '';
	if (!type.toLowerCase().startsWith('application/json')) return reply(415, 'expected a JSON body');
	const origin = event.request.headers.get('origin');
	if (origin && origin !== event.url.origin) return reply(403, 'cross-origin requests are not allowed');
	const declared = Number(event.request.headers.get('content-length') ?? '0');
	if (declared > MAX_RELAY_BYTES) return reply(413, 'the request is too large');
	const body = await boundedText(event.request, MAX_RELAY_BYTES);
	if (body === null) return reply(413, 'the request is too large');
	try {
		JSON.parse(body);
	} catch {
		return reply(400, 'the request body is not valid JSON');
	}
	let response: Response;
	try {
		response = await callEngine(event, path, { method: 'POST', body, signal: event.request.signal });
	} catch (cause) {
		// The browser leaving aborts the request: nothing to answer then.
		if (event.request.signal.aborted) throw cause;
		return reply(502, 'the log service is unreachable');
	}
	return new Response(await response.text(), {
		status: response.status,
		headers: {
			'content-type': response.headers.get('content-type') ?? 'application/json',
			'cache-control': 'no-store'
		}
	});
}
