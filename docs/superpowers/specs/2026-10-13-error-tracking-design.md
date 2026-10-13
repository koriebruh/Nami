# Error Tracking (Sentry-compatible)

## Context

Eighth sub-project (roadmap §8), mirroring Better Stack Error tracking:
applications keep their Sentry SDKs and change only the DSN. Builds on
Logs (ClickHouse client, ingest limits, rate limiting, pattern
normalization), Incident Management (escalation policies, severities,
`alerts::notify_transition`), Traces (`trace_id` links) and `crypto.rs`.
Starts after Traces merges.

## Goals

### Applications and DSNs

- `error_apps` per project: `name`, `platform` label, numeric `dsn_id`
  (bigint identity — Sentry SDKs require an integer project id in the
  DSN), `public_key` (32 hex chars), `allowed_origins` (default `*`),
  `retention_days` (1-365, default 90), `rate_limit_per_minute` (default
  1,000), `daily_event_cap` (default 100,000), `store_user_ip` (default
  false), an upload token (see "Source maps").
- DSN: `https://{public_key}@{ingest host}/{dsn_id}`; the ingest host
  comes from `NAMI_INGEST_URL` and the reverse proxy routes
  `/api/{n}/envelope/` and `/api/{n}/store/` to the engine. Rotating the
  key invalidates the old DSN.
- Ingest auth, first match wins: `X-Sentry-Auth: Sentry sentry_key=…`
  header, `sentry_key` query parameter, or the envelope header's `dsn`.
  The key must belong to the app in the path, else 401.
- CORS for browser SDKs: preflight answered; `Origin` must match
  `allowed_origins`, else 403.

### Ingestion

- `POST /api/{dsn_id}/envelope/` — Sentry envelopes: a header line, then
  items, each an item-header line followed by a payload of `length`
  bytes (up to the next newline when `length` is absent). `event` items
  are processed; `transaction` and `attachment` items are read past and
  discarded; unknown item types are skipped. Response `200 {"id":
  "<event_id>"}`.
- `POST /api/{dsn_id}/store/` — one legacy event JSON body.
- `Content-Encoding`: none, `gzip`, `deflate`, or `br` (sentry-python
  uses brotli when installed); anything else → 415. Limits: 20 MiB body,
  compressed and decompressed (bomb guard → 413), event payload 1 MiB
  (→ 413), JSON nesting 128. Item `length` values are bounded by the body
  limit and never preallocated.
- A duplicate `event_id` within 1 h (per app, in-memory LRU per engine
  instance) is acknowledged and dropped.
- Spike protection per app: a token bucket at `rate_limit_per_minute` and
  `daily_event_cap` per UTC day; over either → 429 with `Retry-After` and
  `X-Sentry-Rate-Limits: <seconds>::key`, so SDKs back off. Daily counters
  in `error_app_usage_daily` (accepted, rate_limited, over_cap, invalid,
  duplicate).

### Normalization

Every event becomes one canonical JSON document:

- `event_id` (32 hex, generated if missing), `timestamp` (seconds or
  RFC 3339; more than 1 h in the future → receive time), `platform`,
  `level` (`fatal|error|warning|info|debug`, default `error`), `logger`,
  `transaction`, `server_name`, `release` (<= 200 chars), `environment`
  (<= 64, default `production`), `dist`, message (`logentry.formatted`
  or `message`), `sdk` (name, version).
- Exception chain `exception.values[]` (oldest first): `type`, `value`
  (<= 8 KiB), `module`, `mechanism` (`type`, `handled`), and
  `stacktrace.frames[]` (oldest first; at most 250 kept — the first and
  last 125): `filename`, `abs_path`, `function`, `module`, `lineno`,
  `colno`, `in_app`, `context_line`, `pre_context` / `post_context` (5
  lines each). Frame `vars` are dropped.
- `breadcrumbs` (last 100; message <= 1 KiB, data <= 4 KiB), `tags`
  (<= 100; key <= 32, value <= 200 chars), `contexts` (<= 64 KiB; the
  `trace` context gives `trace_id`), `user` (`id`, `username`, `email`;
  `ip_address` only with `store_user_ip`), `request` (url, method, query,
  headers minus `Cookie` and `Authorization`), `extra` (<= 64 KiB).
- Server-side scrubbing: values under keys matching `password`,
  `passwd`, `secret`, `token`, `api_key`, `apikey`, `authorization`,
  `cookie`, `session`, `csrf`, `credit_card`, `card_number`, `ssn`
  (case-insensitive, any depth, in `request`, `extra`, `contexts` and
  breadcrumb data) become `[Filtered]`.

### Grouping

- Symbolication (see "Source maps") runs first.
- With an SDK `fingerprint`: components = its strings, with
  `{{ default }}` expanding to the default components below.
- Else, with an exception: the `type` of every exception in the chain,
  the normalized `value` of the last one, and its up to 5 most recent
  in-app frames (most recent frames when none is in-app), each as
  `module` or `filename` (query string and content hashes stripped) plus
  `function`. Line and column numbers are excluded, so a new release
  does not split groups.
- Else: the normalized message.
- Value normalization = the Logs pattern rules (numbers, UUIDs, IPs, hex,
  quoted strings → placeholders). `grouping_hash` = the first 32 hex
  chars of SHA-256 over the components; issues record
  `grouping_version = 1` so the algorithm can change later.

### Issues

- Postgres `error_issues`: unique `(app_id, grouping_hash)`, title (type
  and latest value, or the message), culprit (top in-app frame), level,
  `first_seen`, `last_seen`, `event_count`, `first_release`,
  `last_release`, status `unresolved | resolved | ignored`,
  `resolved_in_release`, `ignore_until`, `ignore_until_count`,
  `ignored_at_count`, `regressed_at`.
- The first event inserts the issue (`on conflict do nothing`, so
  concurrent first events create one issue); counters and `last_seen`
  accumulate in memory and flush every 5 s (no row lock per event).
- Affected users = distinct `user_key` (user id, else email, else
  username, else hashed IP) counted in ClickHouse.
- Resolve (member) records `resolved_in_release` = the issue's latest
  release. A new event reopens the issue as a regression (`regressed_at`,
  regression alert) when its release differs from
  `resolved_in_release`, or on any new event when the issue has never
  had a release.
- Ignore: indefinitely, until N more events (`ignore_until_count`,
  counted from `ignored_at_count`), or until a date (`ignore_until`; a
  sweep reopens it then). Ignored issues keep storing and counting events
  but do not alert.

### Releases

`error_releases` (app, version, first/last seen, events, new issues,
regressions) are created by the first event naming them or through the
upload API; the release list links to that release's issues.

### Source maps (JavaScript)

- Upload with the app's upload token (`Authorization: Bearer
  nami_upl_…`; 32 random bytes, stored SHA-256, shown once, rotatable):
  `POST /api/v1/error-apps/{app_id}/releases/{version}/artifacts`
  (multipart; one file per part, `name` = the URL it is served from, full
  or `~/path`). Files — minified sources and `.map` files — are kept per
  (release, name), zstd-compressed in Postgres `error_artifacts`. Limits:
  50 MiB per file, 1 GiB per release, 5 GiB per app (→ 413). Deleting a
  release deletes its artifacts.
- Symbolication at ingest for `javascript` / `node` events: each frame's
  `abs_path` is matched to an uploaded minified file (full URL, then
  `~/path`); its `sourceMappingURL` is resolved only against uploaded
  artifacts (inline `data:` maps are decoded; anything else is ignored,
  never fetched); the position is mapped with the `sourcemap` crate
  (filename, function, line, column, context lines from
  `sourcesContent`), and the minified position stays on the frame.
  Parsing runs in `spawn_blocking` with a 2 s budget per event; parsed
  maps are cached (LRU keyed by SHA-256, 256 MiB). Events that arrive
  before their maps stay minified (documented: upload before deploying).

### Alert rules

- `error_alert_rules` per project: optional app filter, optional
  environment and minimum-level filters, condition `new_issue` |
  `regression` | `frequency` (N events of one issue within
  `window_seconds`, evaluated every minute), `escalation_policy_id`,
  `severity_id`, `enabled`.
- A match opens an incident (`source = 'error_alert'`,
  `incidents.error_issue_id`, metadata `app`, `environment`, `release`),
  at most one unresolved incident per (rule, issue). Resolving or
  ignoring the issue resolves its open incidents.

### Storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS error_events (
    project_id UUID,
    app_id UUID,
    issue_id UUID,
    event_id String,                 -- 32 hex
    dt DateTime64(3, 'UTC'),
    level LowCardinality(String),
    release LowCardinality(String),
    environment LowCardinality(String),
    platform LowCardinality(String),
    exception_type String,
    message String,
    user_key String,
    tags Map(LowCardinality(String), String),
    trace_id String,
    rum_session_id String,
    payload String CODEC(ZSTD(3)),   -- normalized event JSON
    retention_days UInt16,
    INDEX idx_event event_id TYPE bloom_filter(0.01) GRANULARITY 4
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, issue_id, dt)
TTL toDateTime(dt) + toIntervalDay(retention_days);
```

Postgres: `error_apps`, `error_issues`, `error_releases`,
`error_artifacts`, `error_alert_rules`, `error_app_usage_daily`;
`incidents` gains `error_issue_id` and the `error_alert` source.

### UI

Errors section in the project nav:

- Applications: list, create, DSN copy, setup snippets (JavaScript,
  Python, Go, Ruby, Java, .NET, PHP), settings, upload-token rotate.
- Issues: filters (app, status, environment, release, level, time range,
  title search), sort (last seen, first seen, events, users), 24 h
  sparkline.
- Issue detail: status actions incl. ignore-until, counts, first/last
  seen with releases, top tag values, latest event (exception chain,
  frames with in-app highlighting, collapsible library frames, context
  lines, breadcrumbs, contexts, user, request, SDK), older/newer event
  navigation, "View trace" / "View logs" by `trace_id`.
- Releases, Alert rules.

### RBAC

Viewers read; members create/edit apps, rotate keys and tokens, change
issue status, manage releases and alert rules; admins delete apps. The
upload token is shown only at creation and rotation.

## Security requirements (tested)

- DSN keys are public but bounded: unknown key, another app's key, or a
  key/path mismatch → 401; an origin outside `allowed_origins` → 403;
  rate limit and daily cap → 429 with Sentry rate-limit headers.
- Payload limits: oversized or bomb bodies → 413; malformed envelopes
  (bad item length, missing newline, invalid header JSON, truncated
  payload) → 400 or the item skipped, never a panic (fuzz corpus); deep
  nesting → 400.
- Scrubbing: sensitive keys arrive as `[Filtered]`; `Cookie` and
  `Authorization` headers are gone; no IP stored unless enabled.
- No SSRF: an event or map referencing `http(s)://…` source maps causes
  no outbound request (the test asserts zero connections).
- Upload token: required, hashed at rest; a DSN key cannot upload; size
  limits enforced; uploads to another app → 403.
- Stored content renders escaped: exception values, breadcrumbs, tags
  and source context containing HTML/script show as text.
- Source map robustness: malformed or huge maps leave frames
  unsymbolicated and never crash the ingest path.
- Cross-project isolation for apps, issues, events and artifacts → 404.

## Non-goals

Performance monitoring (transactions are dropped), attachments and
minidumps, native symbolication (dSYM, PDB, ProGuard), release health
(sessions), profiling, user feedback, Sentry replays (RUM records
replays), the `sentry-cli` / bundler-plugin upload protocol (Nami's
upload API with a documented curl script instead), frame local
variables, custom grouping rules (SDK fingerprints only), issue merging,
AI fix prompts (AI SRE's "Explain with AI" covers issues). Tickets and
chat notifications come through incidents, which already have them.
