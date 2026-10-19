// Query parameters the BFF forwards to nami-engine's log endpoints. Only
// these keys pass (repeats kept, in order), so a crafted URL cannot smuggle
// anything else into the engine request.

/** Filters shared by search, histogram and live tail. */
export const FILTER_PARAMS = [
  "source_id",
  "level",
  "q",
  "in_fields",
  "filter",
  "trace_id",
  "span_id",
  "pattern_hash",
];
/** Live tail: the filters only (no window, cursor or limit). */
export const TAIL_PARAMS = FILTER_PARAMS;
/** Search: the filters plus the window and pagination. */
export const SEARCH_PARAMS = [
  ...FILTER_PARAMS,
  "from",
  "to",
  "limit",
  "cursor",
];

/** `?key=value&…` with only `keys`, or `''` when none are present. */
export function pickLogParams(
  params: URLSearchParams,
  keys: readonly string[],
): string {
  const picked = new URLSearchParams();
  for (const [key, value] of params) {
    if (keys.includes(key)) picked.append(key, value);
  }
  const text = picked.toString();
  return text ? `?${text}` : "";
}
