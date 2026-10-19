import { error } from "@sveltejs/kit";
import { callEngine, enginePath } from "#lib/server/engine";
import { pickLogParams, TAIL_PARAMS } from "#lib/server/log-proxy";
import { isUuid } from "#lib/uuid";
import type { RequestHandler } from "./$types";

/**
 * Live tail: relays nami-engine's Server-Sent Events to the browser as they
 * arrive. The engine body is handed through as a stream (never read here),
 * and the browser disconnecting aborts the engine request.
 *
 * An engine error (401 session gone, 403 no access, 400 bad filter, 429 too
 * many open tails) is passed through with its status and body: EventSource
 * does not reconnect on a non-200 answer, so the page sees the failure once
 * and decides (a 401 means "log in again"), instead of a redirect page being
 * mistaken for a stream.
 */
export const GET: RequestHandler = async (event) => {
  if (!isUuid(event.params.id)) throw error(404, "Not found");
  const query = pickLogParams(event.url.searchParams, TAIL_PARAMS);
  let response: Response;
  try {
    response = await callEngine(
      event,
      enginePath`/projects/${event.params.id}/logs/tail` + query,
      {
        headers: { accept: "text/event-stream" },
        signal: event.request.signal,
      },
    );
  } catch (cause) {
    // The browser leaving aborts the request: nothing to answer then.
    if (event.request.signal.aborted) throw cause;
    return new Response(
      JSON.stringify({ error: "the log service is unreachable" }),
      {
        status: 502,
        headers: {
          "content-type": "application/json",
          "cache-control": "no-store",
        },
      },
    );
  }
  if (!response.ok || !response.body) {
    return new Response(await response.text(), {
      status: response.ok ? 502 : response.status,
      headers: {
        "content-type":
          response.headers.get("content-type") ?? "application/json",
        "cache-control": "no-store",
      },
    });
  }
  return new Response(response.body, {
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-store, no-transform",
      "x-accel-buffering": "no",
    },
  });
};
