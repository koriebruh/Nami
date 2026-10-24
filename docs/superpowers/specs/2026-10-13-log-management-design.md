# Log Management

> Parity additions (2026-10-13): ingest pipeline rules (drop, remove fields, redact, rename, sample), MessagePack bodies, message pattern grouping + Patterns tab, `regex`/`not_regex`/numeric filter ops, shareable explorer URLs, streamed CSV/NDJSON export, column picker, `trace_id`/`span_id` columns, per-source ingestion stats + optional daily byte quota.

## Context

Third sub-project of Nami (roadmap §3), mirroring BetterStack Logs. Builds
on Foundation (auth/RBAC/projects), Uptime Phase 1 (ClickHouse client,
incidents, `alerts::notify_transition`, worker loop) and Uptime Phase 2
(`crypto.rs` AES-GCM with `NAMI_ENCRYPTION_KEY`). Starts after both uptime
phases merge.

## Goals

### Sources

- Per project: `name`, `platform` label (`http`, `syslog`, `vector`,
  `docker`, `other`), `retention_days` (1-365, default 30).
- Each source has an ingest token: 32 random bytes, base64url. Stored as
  SHA-256 (lookup) + AES-GCM ciphertext (so the UI can keep showing it, as
  BetterStack does). Rotate = new token, old one stops working
  immediately. Viewers can see a source but not its token; members and
  admins can.
- CRUD: member creates/edits/rotates, admin deletes (deleting a source
  stops ingestion; its rows age out by TTL).
- Optional `daily_byte_quota` (>= 1 MiB; null = unlimited) and an
  ordered `pipeline` of ingest rules — see "Ingest pipeline" and
  "Ingestion stats and quota".

### HTTP ingestion

- `POST /api/v1/logs`, `Authorization: Bearer <source token>`, no session.
- Body: a JSON object, a JSON array of objects, NDJSON
  (`application/x-ndjson`), or MessagePack (`application/msgpack` or
  `application/x-msgpack`: one map or an array of maps); optional
  `Content-Encoding: gzip`. MessagePack `bin` values become base64
  strings, timestamp extensions (type -1) become RFC 3339 strings, other
  extension types become `null`, and a non-string map key → 400.
  Nesting deeper than 128 levels → 400 for both JSON and MessagePack.
  Declared array/map/string lengths are never used to preallocate (the
  decompressed-body limit bounds them).
- Field mapping per event: timestamp from `dt` | `timestamp` | `time`
  (RFC 3339, or unix seconds / milliseconds) else receive time; message
  from `message` | `msg` | `log`; level from `level` | `severity` | `lvl`,
  normalized lowercase (unknown → `info`); `trace_id` / `span_id` from
  the keys of the same name when they are 32 / 16 hex chars and not all
  zeros (stored lowercase; otherwise the key stays in `fields`); every
  other key goes into `fields` (JSON object, kept as-is).
- Limits: 10 MiB compressed body, 50 MiB decompressed (gzip-bomb guard →
  413), 10,000 events per request (→ 413), message truncated at 64 KiB
  with a `…[truncated]` marker, `fields` capped at 256 KiB per event.
- Per-source rate limit (in-memory token bucket, keyed by source id):
  50 requests/s, burst 100 → 429 with the service's JSON error shape.
- Served by the public listener (`PUBLIC_PORT`) only, never by the private
  API port, so exposing the one public port to shippers exposes none of the
  API. A bearer token that is not exactly 43 base64url characters → 401
  without a database query.
- Load limits, so that no sender can exhaust the engine or the pool the API
  and the uptime worker share:
  - at most 32 ingest requests run at once per engine instance; one that
    finds every place taken → `503` with `Retry-After: 1`, answered before
    any database work. A source may have at most 4 requests in flight, and a
    project at most 8 whatever its number of sources → `429`
    `too_many_in_flight` (counted with the rate-limited requests), so neither
    one token nor one project can take every place; a project has at most
    `MAX_SOURCES_PER_PROJECT` sources (default 50, one more → 409) and at
    most 2 bodies decoding at once;
  - at most 8 Postgres statements at once (never more than half the pool),
    so the API and the worker always find connections free;
  - the body must arrive at a pace: 5 s to start, then 64 KiB/s, 60 s at the
    most → `408`, logged with the source (never the token). A request holds
    its place until its work ends, also when the client has hung up, and its
    events are stored and counted either way;
  - what a body decodes to and stores is capped, not only its bytes: the
    decoders stop at 192 MiB of estimated decoded size (a MessagePack `0` is
    one byte and a 32-byte value) → `413`, and so do the rows once they store
    more than 100 MiB, and the insert body once it would encode to more (a
    control character is one byte in a message and six in the JSON sent to
    ClickHouse). At most 4 bodies are decoded at once, 2 of one source; an
    insert body above 8 MiB keeps its decode place until ClickHouse has taken
    it, so only a few large ones are in memory at the same time. 50 MiB of zeros
    in one array costs a few hundred ms and under 200 MiB, not gigabytes;
  - at most one `Content-Encoding` line, a single `gzip` (or none) → else `415`.
- Response `202 {"accepted": N}`. Inserts use ClickHouse
  `async_insert=1, wait_for_async_insert=1` so the server batches small
  inserts and 202 still means durable.

### Syslog ingestion

- RFC 5424 over TCP (newline or octet-counting framing) and UDP on
  configurable ports (default 5140 TCP/UDP; `SYSLOG_ENABLED`).
- Auth: structured-data element `[nami token="<source token>"]`; frames
  without a valid token are dropped and counted (log line, no crash).
- Mapping: severity → level, `HOSTNAME`/`APP-NAME`/`PROCID`/`MSGID` and
  other SD params → `fields`.
- Malformed frames never crash the listener; TCP connections idle > 5 min
  are closed; max frame 64 KiB.

### Ingest pipeline (per source)

Every log event — HTTP (JSON, NDJSON, MessagePack), syslog, and OTLP logs
from the Traces sub-project — goes through one order: parse → field
mapping → truncation caps → the source's pipeline rules → pattern
extraction → quota accounting → insert.

- `sources.pipeline` (jsonb): an ordered list of at most 50 rules,
  replaced as a whole by `PUT /projects/{id}/sources/{source_id}/pipeline`
  (member) and validated on write (400 naming the failing rule index).
  `POST .../pipeline/test` runs submitted rules over up to 100 sample
  events and returns each output event, or `dropped` / `sampled_out`,
  storing nothing (the editor's live preview).
- Match condition: `{"field": path, "equals": value}` (path syntax as
  search filters; `level` and `message` address the normalized level and
  message, which never remain in `fields`; non-string values compare by
  their JSON text) or `{"message_regex": pattern}`.
- Rule kinds, applied top to bottom; the first matching `drop` ends
  processing:
  - `drop` `{match}` — discard the event.
  - `remove_fields` `{paths: [...]}` — delete those keys from `fields`.
  - `redact` `{keys?: [...], regex?: pattern}` — every value under a key
    whose name matches one of `keys` (case-insensitive, any depth)
    becomes `"[REDACTED]"`; every `regex` match in `message` and in
    string values of `fields` is replaced with `"[REDACTED]"`.
  - `rename` `{from: path, to: path}` — move a value; an existing target
    is overwritten; a missing source is a no-op.
  - `sample` `{n: 2-10000, match?}` — keep 1 in `n` matching events
    with a deterministic per-rule counter per engine instance (the 1st,
    (n+1)th, … are kept); without `match` it applies to every event.
- Regexes use the `regex` crate (linear time, no backtracking), max 256
  chars, compiled once per rule-set version with a size limit (256 KiB
  program, 256 KiB lazy-DFA cache), at most 20 per pipeline; invalid
  patterns are rejected at write time.
- The compiled rule set is cached per source and reloaded when
  `sources.updated_at` changes (checked at most every 10 s), so edits
  reach every engine instance within ~10 s.
- Redaction runs before pattern extraction and storage: a redacted value
  never reaches ClickHouse, `pattern` included.

### Message patterns

- At ingest the first 1,024 chars of `message` are normalized into
  `pattern`, replacing in this order: quoted strings (`"…"`, `'…'`) →
  `<str>`, UUIDs → `<uuid>`, IPv4/IPv6 addresses → `<ip>`, `0x…` and
  standalone hex runs of >= 8 chars containing a digit and a letter →
  `<hex>`, standalone integers/decimals (optional sign) → `<num>`.
  Example: `user 42 logged in from 10.0.0.7` →
  `user <num> logged in from <ip>`.
- Stored per row as `pattern`, with `pattern_hash = cityHash64(pattern)`
  as a materialized column.
- `GET /projects/{id}/logs/patterns` (search filters and window) → the
  top 100 patterns by count: `{pattern_hash, pattern, count, first_seen,
  last_seen}`.

### Storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS logs (
    id UUID,                          -- uuid v7, halves swapped (below)
    project_id UUID,
    source_id UUID,
    dt DateTime64(3, 'UTC'),
    level LowCardinality(String),
    message String,
    fields String,                    -- JSON object
    trace_id String,                  -- 32 lowercase hex, or ''
    span_id String,                   -- 16 lowercase hex, or ''
    pattern String,
    pattern_hash UInt64 MATERIALIZED cityHash64(pattern),
    retention_days UInt16,
    INDEX idx_message message TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 4,
    INDEX idx_level level TYPE set(16) GRANULARITY 4,
    INDEX idx_trace trace_id TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_pattern pattern_hash TYPE bloom_filter(0.01) GRANULARITY 4
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, dt, id)
TTL toDateTime(dt) + toIntervalDay(retention_days);
```

Retention is per row (set from the source at ingest); changing a source's
retention affects new rows only (documented in the UI).

Row ids (ruling, final review of Part 1): an id is a UUIDv7 with its two
64-bit halves swapped (`store::new_row_id`). ClickHouse compares `UUID`
values by their second 8 bytes first, so a plain v7 would not sort in
creation order inside `ORDER BY (project_id, dt, id)`; swapped, the
timestamp and the high counter bits come first and ClickHouse's order is
the generation order. Ids are opaque: nothing reads one back as a UUIDv7
(a stored id does not parse as version 7 unless swapped back), they are
only unique and ordered.

### Search

`GET /projects/{id}/logs` with:

- `from`, `to` (required window, default last hour, max 31 days → 400),
- `source_id` (repeatable), `level` (repeatable),
- `q` — case-insensitive substring over `message` (and `fields` when
  `in_fields=true`),
- `filter` (repeatable JSON `{"path":"a.b","op":"eq|neq|contains|exists|
  regex|not_regex|gt|gte|lt|lte","value":"…"}`; max 10 filters; path
  segments match `^[A-Za-z0-9_-]{1,64}$`, max depth 5). `regex` /
  `not_regex` use ClickHouse `match` (RE2, linear time), pattern <= 256
  chars, invalid pattern → 400; `not_regex` also matches rows where the
  field is missing. `gt` / `gte` / `lt` / `lte` need a finite number
  (else 400) and compare numerically; a missing or non-numeric field
  never matches them,
- `trace_id` / `span_id` (exact; 32 / 16 hex chars, else 400) and
  `pattern_hash` (repeatable),
- `limit` (default 100, max 500), `cursor` (opaque `(dt, id)` keyset
  cursor, newest first).

List rows (search pages and the live tail) are bounded whatever senders
stored: the message is cut to 4096 characters and `fields` over 8 KiB are
replaced by a marker object, with `truncated: true` on the row.
`GET /projects/{id}/logs/{log_id}?dt=<RFC 3339>` (viewer, project-confined,
404 when absent) returns one whole row for the explorer's expanded view.
Log reads run with `max_result_bytes` (result_overflow_mode=throw) and the
ClickHouse client refuses any response body over 32 MiB while reading it;
one user may have at most 4 searches and histograms, and 8 entry reads, running
(→ 429).

The project id is always bound from the path. Every value — including
JSON path segments — is a ClickHouse query parameter; the SQL text is
assembled only from fixed fragments with numbered placeholders.

Also `GET /projects/{id}/logs/histogram` (same filters) → counts per
bucket (auto bucket size targeting ~60 buckets) for the explorer's volume
chart.

### Export

`GET /projects/{id}/logs/export?format=csv|ndjson` (viewer; same filters
and window as search) streams newest-first up to 100,000 rows, then
stops. The cap is sent up front in `X-Nami-Export-Limit`; the UI compares
it with the histogram total and warns before a truncated export. NDJSON
rows carry every stored column; CSV has `dt, level, source, message,
trace_id` plus the caller's `columns` (max 10 field paths, extracted
server-side). CSV follows RFC 4180, and a cell starting with `=`, `+`,
`-`, `@`, tab or CR gets a leading `'` (formula-injection guard).
ClickHouse `max_execution_time` 120 s; max 2 concurrent exports per user
(→ 429) and 8 per engine instance (→ 503), both with `Retry-After: 5`
(the uptime check export uses the same convention). A `source_id` that is
not one of the project's matches nothing, as in search. The export task ends at its 150 s
deadline even when the client stops reading. A file cut short (size cap,
row too large, a ClickHouse error mid-stream, the deadline) ends with an
explicit trailer: NDJSON `{"_nami_export_error":"<reason>"}`, CSV a last
record `# export stopped early: <reason>`; when even the trailer cannot be
delivered, the connection is cut instead of closed cleanly.

### Live tail

`GET /projects/{id}/logs/tail` (same filters minus window/cursor) →
Server-Sent Events. The engine polls ClickHouse every 1 s for the ids of
the newest rows (at most 200) in a 60 s look-back, reads the list rows of
only the ids it has not sent yet (late rows included), emits them as
`event: log`, sends a comment
keep-alive every 15 s, re-checks the session every 60 s (ends the stream
if the session became invalid), and closes after 30 min (client
reconnects). nami-ui proxies the stream through a `+server.ts` endpoint
without buffering.

### Saved views

Postgres `log_views` (project, name, query JSON with the search
parameters and picked columns, created_by). Members create/edit/delete;
viewers read.

### Ingestion stats and quota

- Per source and UTC day: `events` and `bytes` accepted (bytes = UTF-8
  length of the stored `message` + `fields`), `events_dropped`
  (pipeline), `events_sampled_out`, `requests_rate_limited`,
  `requests_over_quota`. Counted in memory, flushed every 10 s into
  Postgres `source_usage_daily` (additive upsert), kept 400 days.
  `GET /projects/{id}/sources/{source_id}/usage?days=30` (viewer) feeds
  the source page's daily chart.
- `daily_byte_quota`: once the day's flushed bytes plus this instance's
  unflushed delta reach it, HTTP and OTLP log requests for the source
  get 429 with error code `quota_exceeded` (distinct from the rate-limit
  429) and `Retry-After` = seconds until the next UTC midnight; syslog
  frames are dropped. Every rejection is counted. A request that crosses
  the quota is accepted whole, so overshoot is bounded by one request
  plus ~10 s of traffic per engine instance (documented in the UI).

### Log alerts

Postgres `log_alerts`: project, name, query JSON (same shape as a view),
`threshold` (count), `comparison` (`gt` | `lt`), `window_seconds`
(60-86400), `interval_seconds` (60-86400), `enabled`, `state`
(`ok` | `alerting`), `last_evaluated_at`, `last_count`, `last_error`,
`last_error_at`, `next_evaluation_at`.
The worker evaluates due alerts (atomic claim, same pattern as monitors):
condition met while `ok` → `alerting`, opens an incident
(`incidents.log_alert_id`), notifies through `alerts::notify_transition`;
condition clears while `alerting` → `ok`, resolves the incident.
While a project-wide maintenance window (one with no monitor) is in
effect, the alert's state machine stands still, as for heartbeats: the
count is recorded, but the state does not change, no incident opens or
resolves and nothing is sent. A monitor-scoped window does not hold it.
The first evaluation after the window applies the transition. A failed
count (ClickHouse unavailable, or a query error) changes nothing except
`last_error` / `last_error_at`, a fixed, user-safe text (never
ClickHouse's own message), so an alert whose query keeps failing is
visible instead of silently never evaluating. The next successful
evaluation clears `last_error`.
No-data handling, confirmation/recovery periods and per-series incidents
arrive with the Metrics sub-project's shared alert evaluator.

### UI

Logs section in the project nav: Sources (list, create, token reveal +
copy, rotate, delete, setup snippets for curl / Vector / syslog, pipeline
rule editor with live preview over pasted sample events, daily
events/bytes chart, quota setting), Explorer (search bar, level chips,
source picker, time-range presets 15 m / 1 h / 6 h / 24 h / 7 d + custom,
volume histogram, results table with level colors and expandable
pretty-printed fields, column picker for up to 10 field paths, load
more, CSV/NDJSON export, "View trace" on rows with a `trace_id` once
Traces ships), Patterns tab (top patterns with counts; a click filters
the explorer by `pattern_hash`), Live tail toggle, Saved views, Alerts
(list, create/edit with a live preview count).

All explorer state — window (relative presets stay relative), sources,
levels, `q`, filters, columns, active tab — lives in the URL query
string, so a copied link reopens the same view for anyone with access to
the project. Tokens never appear in it.

## Security requirements (tested)

- Missing/invalid/rotated/deleted-source token → 401; tokens never logged.
- Search, histogram and tail are always confined to the path's project;
  a `source_id` from another project yields nothing (never other
  projects' rows).
- Injection: `q`, filter values and paths containing quotes, backslashes,
  `'; DROP TABLE logs; --`, ClickHouse function syntax, and newlines are
  matched literally or rejected — never executed.
- Oversized body, gzip bomb, too many events → 413; rate limit → 429.
- Syslog fuzz: truncated frames, huge octet counts, invalid UTF-8, missing
  SD — listener survives and keeps serving.
- SSE endpoint requires a session and stops when it is invalidated.
- Pipeline: a catastrophic-backtracking pattern (`(a+)+$`) over a 64 KiB
  message finishes in linear time; invalid or > 256-char patterns → 400;
  a redacted value is absent from the stored row (`message`, `fields`,
  `pattern`), including nested keys and mixed-case key names.
- New filter ops: regex and numeric values are bound as parameters;
  a non-numeric value for a numeric op → 400.
- MessagePack fuzz: truncated input, wrong types, lengths declaring 2^32
  elements, non-string keys, nesting > 128 → 4xx, no panic, no large
  allocation.
- Export: confined to the path's project; the 100,000-row cap holds;
  formula cells (`=cmd|' /C calc'!A0`, `+1`, `@SUM(1)`) are prefixed;
  a third concurrent export → 429.
- Quota: over quota → 429 `quota_exceeded`, counted; the project's other
  sources keep ingesting.

## Non-goals

SQL query console, log-to-metrics transforms, anomaly detection, archive to
S3, Fluentd/Logstash-specific endpoints (the HTTP endpoint covers them),
VRL or other scripted transforms and event splitting (declarative rules
only).
