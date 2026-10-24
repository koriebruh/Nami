# Error Tracking (Sentry-compatible), Part 1 Implementation Plan

> **Status:** outline. Nothing is merged; NAMI-ERR-1 is next, once the Traces phase has merged.
> **Branch:** `feat/errors-part1` in `.worktrees/errors-part1/`.
> **Depends on:** Traces (sub-project 7) merged. The spec orders Error tracking after it (the `trace_id` links); Log Management's ingest helpers and `logs::query::pattern` are already on `main`.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R8.01 | Applications with DSNs; envelope + store endpoints (gzip/deflate/br, size limits, unknown items skipped) | NAMI-ERR-1, NAMI-ERR-2, NAMI-ERR-6 |
| R8.02 | Event normalization with data scrubbing | NAMI-ERR-3 |
| R8.03 | Grouping, Part 1 half: fingerprint, type + normalized value + top in-app frames on the frames as sent (symbolication-first is Part 2, NAMI-ERR-14) | NAMI-ERR-4 |
| R8.04 | Issues: first/last seen, counts, affected users, resolve, ignore (until N / until date), regression on a new release | NAMI-ERR-5, NAMI-ERR-7 |
| R8.06 | Spike protection per DSN (rate limit + daily cap) | NAMI-ERR-6 |
| R8.09 | ClickHouse `error_events` with retention | NAMI-ERR-4, NAMI-ERR-6 |
| R8.10 | UI, Part 1 half: applications, issue list, issue detail (stack trace, breadcrumbs); releases and alert rules are Part 2 (NAMI-ERR-17) | NAMI-ERR-8, NAMI-ERR-9, NAMI-ERR-10 |

The phase security review and fix wave is NAMI-ERR-11.

## Scope and split

Error tracking ships in two phases:

- **Part 1 (this plan, NAMI-ERR-1..11):** applications and DSNs, the
  envelope and store endpoints with their decoders and limits, spike
  protection, deduplication and usage counters, normalization and
  scrubbing, grouping, the `error_events` table, issues with their state
  machine (resolve, ignore, regression), the issues API, and the
  applications, issue list and issue detail pages.
- **Part 2 (`2026-10-24-error-tracking-part2.md`, NAMI-ERR-12..18):**
  release tracking, the upload token and source map artifacts,
  server-side symbolication, alert rules that open incidents, and the
  releases and alert rules pages.

Why two phases:

1. Part 1 is the smallest thing that is useful on its own. An application
   changes its DSN, and its errors show up grouped into issues that can be
   resolved and ignored. Every limit on the public endpoint ships with it.
   A public ingest endpoint whose key is public by design does not merge
   without spike protection (R8.06 is Part 1 for that reason).
2. Part 2 is the part with new dependencies and new trust boundaries. It
   adds the `sourcemap` and `zstd` crates and axum's `multipart` feature. It
   adds a second credential (the upload token) on the public listener, a
   parser for large untrusted JSON (source maps) on the ingest path, and a
   migration in the incidents domain. Each of these deserves its own
   security review instead of being one more item in an already long one.
3. Together the two would be 18 tasks, about twice the size of the phases
   that have merged so far.

What the split costs, recorded as rulings in Part 2: release rows begin
when Part 2 is deployed (no backfill), and JavaScript events stored before
their maps are uploaded stay minified and keep the group they had
(`grouping_version` stays 1). The spec already accepts the second. There
is a third: once Part 2 symbolicates, a JavaScript app whose maps are
uploaded groups on original frames, so each of its existing issues gets a
new twin once (Part 2, R-ERR26).

**Goal:** An application that uses a Sentry SDK changes only its DSN to
`https://{public_key}@{ingest host}/{dsn_id}`, and its errors arrive in
Nami. They are normalized, scrubbed and grouped into issues with counts,
affected users, first and last seen, and a resolve / ignore / regression
lifecycle, and members browse them on an issue list and an issue detail
page with the stack trace and breadcrumbs.

**Architecture:** A new engine domain `errors/`. The ingest endpoints
(`POST /api/{dsn_id}/envelope/`, `POST /api/{dsn_id}/store/`) are on the
public listener next to log ingest and heartbeat pings. One request goes
through these steps:

1. Take an engine-wide ingest place.
2. Find the key (header, query, else the envelope header after a capped
   first-line decode) and authenticate the app.
3. Check the CORS origin.
4. Read the body. Only then take one of the app's in-flight places, so a
   body that is still arriving never holds one.
5. Decode the body under the decode cap.
6. Frame the envelope, which is a pure function.
7. Check the duplicate set, the token bucket and the daily cap.
8. On a blocking thread: decode the event JSON under a budget, normalize
   and scrub it, then group it. All three steps are pure.
9. Look up or insert the issue, and apply the pure state machine to it.
10. Insert one row into `error_events` (async insert) and record the
    in-memory counters.

One background loop (`errors::worker`) flushes issue counters every 5 s
and usage counters every 10 s, and runs the minute sweep (ignore-until-date
reopen, usage prune). Issues live in Postgres and events in
ClickHouse. Every read of events goes through an issue id that Postgres has
already checked against the path's project.

**Tech Stack:** Rust (axum 0.8, sqlx 0.9 runtime queries, serde_json,
flate2, sha2, the existing `core::clickhouse::ClickHouse` client), one new
crate (`brotli-decompressor`, R-ERR25), SvelteKit 5 + Tailwind 4 with the
`nami-ui/src/lib/components/ui/` design system.

**Spec:** `docs/superpowers/specs/2026-10-13-error-tracking-design.md`
(Applications and DSNs, Ingestion, Normalization, Grouping, Issues,
Storage, UI, RBAC, Security requirements). Part 2 covers Releases, Source
maps and Alert rules.

**When to start:** after the last Traces phase PR has merged. Create
`.worktrees/errors-part1/` on `feat/errors-part1` from the latest
`origin/main`, re-read this plan against the then-current code, write the
step-level code task by task, and pick the migration number (the next free
`NNNN` in `nami-engine/migrations/`).

## Global Constraints

- Follow `CLAUDE.md`, `docs/STANDARDS.md` and
  `docs/superpowers/plan-conventions.md`. Engine code lives in
  `nami-engine/src/errors/`. `core/router.rs` mounts its routes and
  `core/state.rs` holds `AppState.errors: Arc<ErrorsState>`. `core` gains
  only `Config.ingest_url` (NAMI-ERR-1).
- Tests: a new target `nami-engine/tests/errors/main.rs` (mounts
  `#[path = "../common/mod.rs"] mod common;`), files
  `tests/errors/<file>.rs`, shared helpers in
  `nami-engine/tests/common/errors.rs`. Run one file with
  `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test errors <file>:: -- --test-threads=8`.
  Tests that touch ClickHouse create their own `nami_test_*` database with
  `ClickHouse::create_test_database()` and drop it.
- Migrations: one Postgres file for this phase, `NNNN_errors_<change>.sql`
  (suggested `NNNN_errors_apps_issues.sql`). NAMI-ERR-1 creates it, and
  NAMI-ERR-5 and NAMI-ERR-6 append to it. No other domain's tables change
  in Part 1. ClickHouse DDL is a new `// ---- Errors domain ----` group in
  `core/clickhouse.rs` `MIGRATIONS`.
- RBAC: viewer reads apps, issues and events. Member creates and edits
  apps, rotates the DSN key and changes issue status. Admin deletes apps.
  The first line of every project-scoped handler is
  `require_project_role(&state.db, &user, project_id, Role::X).await?`.
  Authorization failure is 403. An app or issue id outside the path's
  project is 404.
- Every limit below is a named `pub const` next to the type it bounds, and
  each has a test at the bound and one past it. Tests name the constant,
  never the literal.
- Telemetry is untrusted. No event text is formatted into SQL. ClickHouse
  SQL is built from `&'static str` fragments with bound parameters, and the
  project id is bound from the path. The UI renders event content only
  through Svelte text interpolation, never `{@html}`.
- CPU-heavy work (decompression, JSON decode, normalization, grouping)
  runs in `tokio::task::spawn_blocking`, behind a decode place. No lock is
  held across `.await`.
- Never log DSN keys, event payloads, user data or request bodies.
  `tracing` fields carry ids and counts only.
- No long dashes in code comments or docs written by this phase.

## Review Focus

1. Every path through ingest is bounded: wire bytes, decoded bytes, the
   first-line decode before auth, header lines, item count, event bytes,
   the JSON decode budget, nesting, frames, breadcrumbs, tags and contexts.
   A malformed envelope never panics (fuzz corpus, NAMI-ERR-2).
2. Auth: unknown key, another app's key and a key/path mismatch are each a
   401, with the same body. The origin check is a 403. Rate limit and cap
   are 429 with `Retry-After` and `X-Sentry-Rate-Limits`.
3. Scrubbing and privacy: sensitive keys arrive as `[Filtered]`, `Cookie`
   and `Authorization` are gone, and no IP is stored (raw or as a
   reversible hash) unless `store_user_ip`.
4. The issue state machine is one pure function. Each status write is
   guarded, so a concurrent regression is detected once.
5. Cross-project isolation for apps, issues and events (404), and escaped
   rendering of every stored string.

## Rulings (where the spec is silent)

- **R-ERR1 Two phases.** See "Scope and split".
- **R-ERR2 Public listener, uniform 401.** The envelope and store routes
  are served on `PUBLIC_PORT` (the API port is private to nami-ui), with
  and without the trailing slash. A `dsn_id` that is not a positive
  integer, an unknown `dsn_id`, an unknown key, another app's key and a
  key/path mismatch all answer `401 {"error": "invalid DSN"}`. Nothing
  tells a missing app from a wrong key.
- **R-ERR3 Where the key comes from.** The first source present decides,
  and a malformed one is a 401 with no fall-through:
  1. `X-Sentry-Auth`: scheme `Sentry` (case-insensitive), then
     comma-separated `name=value` pairs. The value of `sentry_key` is used.
     Other pairs (`sentry_version`, `sentry_client`, `sentry_secret`) are
     ignored.
  2. The `sentry_key` query parameter.
  3. The envelope header's `dsn`, parsed with `url::Url`. Its username is
     the key, and its last non-empty path segment must equal the path's
     `dsn_id`.

  A key is 32 lowercase hex characters (`PUBLIC_KEY_HEX_LEN`).
  `Authorization: Sentry ...` is not accepted. The stored key is compared
  in constant time.
- **R-ERR4 Little work before auth.** When the key comes from a header or
  the query, the app is authenticated before the body is read. Only an
  envelope that carries its key in its own header is read first, at most
  `MAX_BODY_BYTES`. Then only its first line goes through the streaming
  decoder, capped at `MAX_HEADER_LINE_BYTES`, and the rest is decoded
  after auth. The store endpoint needs a header or query key.
- **R-ERR5 Content-Type is not checked, Content-Encoding is.** The
  browser SDK sends envelopes as `text/plain` to avoid a CORS preflight,
  so any `Content-Type` is accepted. `Content-Encoding` must be absent or
  exactly one of these: `identity`, `gzip`, `x-gzip`, `deflate` (zlib,
  RFC 1950, as HTTP defines it) or `br`. A list, a second header line or
  any other coding is 415. A raw-deflate body under `deflate` fails to
  decode and is 400.
- **R-ERR6 Envelope framing.**
  - A framing error is a 400 for the whole request, because nothing after
    it can be trusted to be aligned. Framing errors are: an invalid
    envelope header, an invalid item header or one without a string
    `type`, a `length` past the end of the body, and a length-delimited
    payload not followed by `\n` or the end of the body.
  - Whitespace after the last item is ignored.
  - At most `MAX_ENVELOPE_ITEMS = 100` items, and at most one `event`
    item (`MAX_EVENT_ITEMS = 1`). A second event item is a 400, as in
    Sentry Relay.
  - The reply is `200 {"id": "<event_id>"}` for an envelope with an event.
    Without one, it is `{"id": "<envelope header event_id>"}` when that is
    a valid id, else `{}`.
- **R-ERR7 Event decode budget.** An event payload over `MAX_EVENT_BYTES`
  (1 MiB) is a 413. The payload is decoded with
  `logs::ingest::parse_json_events_within` and a
  `Budget::new(MAX_EVENT_DECODED_BYTES)` of 32 MiB, so the `Value` it builds
  is bounded. Over the budget is a 413. Anything other than exactly one
  JSON object is a 400: the first non-whitespace byte must be `{` (that
  decoder also accepts `[{..}]` and several objects), and exactly one value
  must be decoded. The nesting limit of 128 (`MAX_JSON_DEPTH`) is
  serde_json's own recursion limit. Tests at depth 128 (accepted) and 129
  (400) pin it, and if serde_json's boundary is off by one from the spec, a
  depth check in the budgeted decoder enforces 128 exactly.
- **R-ERR8 CORS.**
  - `OPTIONS` on either route answers `204` for any origin without a
    database lookup:
    - `Access-Control-Allow-Origin: <Origin>`;
    - `Access-Control-Allow-Methods: POST, OPTIONS`;
    - `Access-Control-Allow-Headers: content-type, content-encoding, x-sentry-auth, sentry-trace, baggage`;
    - `Access-Control-Max-Age: 3600`.

    The POST then enforces the app's `allowed_origins`, so a preflight
    that passes grants nothing.
  - A request without `Origin` is a server SDK and is not checked.
    `Origin: null` passes only with `*`.
  - An entry is `*` or an exact origin `scheme://host[:port]`, lowercase,
    with no path. At most `MAX_ALLOWED_ORIGINS = 50` entries, each at most
    `MAX_ORIGIN_CHARS = 253`.
  - Every POST reply to an allowed origin carries
    `Access-Control-Allow-Origin: <Origin>`, `Vary: Origin` and
    `Access-Control-Expose-Headers: x-sentry-rate-limits, x-sentry-error, retry-after`.
    That includes 4xx and 429, so a browser SDK can back off. A 403 for
    the origin carries no CORS headers.
- **R-ERR9 Spike protection numbers.**
  - Per app, a token bucket with capacity `rate_limit_per_minute` that
    refills at `rate_limit_per_minute / 60` tokens per second. A new
    bucket starts full.
  - One token per event item. An envelope without an event (sessions,
    client reports) takes no token and does not count toward the cap: it
    is cheap once framed, and a 429 on it would make the SDK back off for
    errors too. The per-app in-flight cap and the engine-wide places
    bound it.
  - The daily cap counts accepted events per UTC day: the flushed count
    from `error_app_usage_daily` (cached `FLUSHED_CACHE_TTL` = 10 s) plus
    this instance's pending count.
  - With several engine instances, the bucket is per instance, and the cap
    can overshoot by what the other instances accepted since their last
    flush (at most `USAGE_FLUSH_INTERVAL` + `FLUSHED_CACHE_TTL` of their
    traffic). This is documented and accepted, as for log quotas.
  - Over the bucket: 429 with `Retry-After: s` and
    `X-Sentry-Rate-Limits: s::key`, where `s = ceil(60 / rate_limit_per_minute)`
    and at least 1. Over the cap: the same headers, with `s` = seconds
    until the next UTC midnight (`logs::ingest::seconds_until_utc_midnight`).
  - Concurrency:
    - `MAX_IN_FLIGHT_PER_APP = 8` requests per app, as for a logs project.
      The place is taken only after the body has been read, so many slow
      browser clients do not hold it. It covers decode and store, which
      take tens of milliseconds, so 8 places carry far more than the
      default 1,000 events per minute. It keeps one app from taking every
      engine place.
    - `MAX_ERROR_INGEST_IN_FLIGHT = 32` engine-wide.
    - `MAX_ERROR_DECODES_IN_FLIGHT = 4` decodes engine-wide (wait).

    Both caps answer 429 (STANDARDS §3) with `Retry-After: 1` and
    `X-Sentry-Rate-Limits: 1::key`. A bare 429 would make an SDK back off
    for 60 s.
  - `rate_limit_per_minute` is 1..=100,000. `daily_event_cap` is
    1..=10,000,000.
- **R-ERR10 Duplicates.** The "LRU" is a two-generation set of
  `(app_id, event_id)` (`errors::cache::TwoGen`):
  - A generation rotates after one hour or at `MAX_DEDUP_IDS / 2`
    entries, so an id is remembered for 1 to 2 h. Rotating early under a
    flood shortens that window, which is documented.
  - `MAX_DEDUP_IDS = 200_000` per instance, about 10 MiB.
  - The id is inserted when the event has passed the limits. It is removed
    again if storing fails, so the SDK's retry is accepted.
  - A duplicate is answered `200 {"id"}` and counted as `duplicate`.
- **R-ERR11 Normalization limits the spec does not give.** See the table
  in NAMI-ERR-3. Strings are cut at a character boundary, and byte limits
  are UTF-8 bytes. A field that fails its format is dropped, never
  rejected, except `release` and `environment`: an invalid release is
  dropped (a cut one would merge distinct releases), and an invalid
  environment becomes `production`.
- **R-ERR12 Scrubbing rule.**
  - A key matches when, lowercased with `-` read as `_`, it *contains* one
    of the spec's words (so `X-Api-Key`, `session_id` and `x-csrf-token`
    match). The whole value under it, object or array included, becomes
    the string `[Filtered]`.
  - Query strings (`request.query` as a string, and the query part of
    `request.url`) are parsed with `form_urlencoded` and scrubbed per
    parameter.
  - `request.data`, `request.cookies` and `request.env` are always
    dropped: the spec keeps only url, method, query and headers.
  - Without `store_user_ip`, `user.ip_address` is dropped. So are the
    headers `X-Forwarded-For`, `X-Real-Ip`, `Forwarded`,
    `CF-Connecting-IP` and `True-Client-IP`.
- **R-ERR13 Affected users without a reversible IP.**
  - `user_key` is `id:<id>`, else `email:<email>`, else
    `username:<username>`, else `ip:<h>`.
  - `h` is the first 32 hex characters of
    SHA-256(`cipher.derive_subkey("errors.user_key")` || app id || IP).
    A plain SHA-256 of an IPv4 address is reversible by enumeration.
  - The IP is `user.ip_address` when it is a literal IP. When it is
    `{{auto}}`, the peer address (`ConnectInfo`) is used. Trusting
    `X-Forwarded-For` waits for the trusted-proxy work of the hardening
    sub-project.
  - With no IP and no user, `user_key` is empty and the event is not
    counted as a user.
- **R-ERR14 Grouping details.** See NAMI-ERR-4. Components are hashed as
  their JSON array encoding (`serde_json::to_vec(&components)`), so
  `["ab","c"]` and `["a","bc"]` differ. Beyond the spec's stripping of
  query strings and content hashes, a URL filename also loses its scheme
  and host. The JavaScript SDK reports full URLs, and the same bundle
  served from two CDN hosts, or from a preview and a production domain,
  must not split into two groups.
- **R-ERR15 Issue state machine choices.**
  - An event without a release on an issue resolved in release `r`
    "differs" and regresses it. An event from an older release also
    differs. The spec compares versions, and Nami does not order versions.
  - Ignore-until-count compares `event_count + this instance's pending
    count` with `ignored_at_count + ignore_until_count`, so it can be late
    by what other instances have not flushed yet.
  - Reopening an ignored issue (count or date reached) makes it
    `unresolved` without `regressed_at`.
  - The date is applied by the minute sweep only (at most 60 s late).
  - Resolve, unresolve and ignore are allowed from every status.
    Repeating the current status is a 200 that changes nothing.
- **R-ERR16 Counters.** Issue counters, `last_seen` and the latest title,
  culprit, level and release accumulate in memory per instance and flush
  every `ISSUE_FLUSH_INTERVAL` (5 s) in one `unnest` update. A failed flush
  keeps its deltas for the next one. A crash loses at most 5 s of counts
  (the events themselves are in ClickHouse). A new issue is inserted with
  `event_count = 0`, and its first event is counted like any other.
- **R-ERR17 Issue list counts.**
  1. Postgres selects up to `MAX_LIST_CANDIDATES = 1000` candidate issues
     by project, app, status and title search, with `last_seen >=` the
     range start, newest first.
  2. One ClickHouse query aggregates those ids over the range with the
     environment, release and level filters.
  3. `events` and `users` in the list are counts within the range (as in
     Sentry). `event_count`, `first_seen` and `last_seen` are lifetime
     values.
  4. Sort by `last_seen` or `first_seen` uses the lifetime columns. Sort
     by `events` or `users` uses the range counts.
  5. Pagination is by page over the sorted candidates. When the 1000 cap
     was hit, the reply says `truncated: true`.
- **R-ERR18 Issues are kept.** An issue row stays until its app is
  deleted, also after its events have expired. The list hides stale issues
  through `last_seen >=` the range start, and the detail page says "no
  stored events" when ClickHouse has none. Deleting issues automatically
  would also delete the incidents that Part 2 links to them, and with them
  the incident history.
- **R-ERR19 Timestamps.** More than 1 h in the future
  (`MAX_FUTURE_SKEW`), or more than 30 days in the past (`MAX_EVENT_AGE`,
  which keeps partitions bounded), becomes the receive time. A string
  without an offset is read as UTC.
- **R-ERR20 Deleting an app.** Its issues and usage rows cascade in
  Postgres. Its ClickHouse rows are left to their TTL. They cannot be
  reached, because every event read takes an issue id that Postgres has
  checked.
- **R-ERR21 Apps per project.** At most `MAX_APPS_PER_PROJECT = 50`,
  checked inside the insert transaction (409 past it). Names are unique per
  project (409).
- **R-ERR22 Ingest URL.** `NAMI_INGEST_URL` (`Config.ingest_url`) is an
  absolute `http`/`https` URL with no userinfo, query or fragment. The
  default is `http://localhost:{PUBLIC_PORT}`. The DSN is
  `{scheme}://{public_key}@{host[:port]}{path}/{dsn_id}`, with the path's
  trailing slash removed, and SDKs append `/api/{dsn_id}/envelope/`
  themselves.
- **R-ERR23 RUM seam.** `rum_session_id` is written empty by the public
  endpoints. RUM (sub-project 9) passes it through the internal entry
  `errors::ingest::accept_event` that the routes call.
- **R-ERR24 One loop.** `errors::worker::run_forever` is spawned once in
  `main.rs`. Every iteration logs a failure and continues.
- **R-ERR25 Crate pin.** Brotli bodies are decoded with
  `brotli-decompressor` (pure Rust, decode-only, MIT/BSD-3, depends only on
  the `alloc-no-stdlib` crates). It is pinned with
  `cargo add brotli-decompressor@6.0.1`: a release about a month old at
  writing (6.1.x was days old). At phase start, re-pin to the newest
  release at least 14 days old. `flate2` (installed) covers gzip and zlib.

## Tasks

### Task 1 (NAMI-ERR-1): Create the errors domain, applications, DSNs and the ingest URL setting

**Covers:** R8.01 (applications and DSNs)  **Depends on:** none (first task)

**Files:**
- Create: `nami-engine/migrations/NNNN_errors_apps_issues.sql`,
  `nami-engine/src/errors/mod.rs` (`ErrorsState`, module list),
  `nami-engine/src/errors/app/mod.rs` (validation, DSN building, key
  generation), `nami-engine/src/errors/app/model.rs`,
  `nami-engine/src/errors/app/routes.rs`,
  `nami-engine/tests/errors/main.rs`, `nami-engine/tests/errors/apps.rs`,
  `nami-engine/tests/common/errors.rs` (`create_app`, `dsn_of`).
- Modify: `nami-engine/src/lib.rs` (`pub mod errors;`),
  `nami-engine/src/core/config.rs` (`ingest_url`),
  `nami-engine/src/core/state.rs` (`errors: Arc<ErrorsState>`, also in
  `for_test`), `nami-engine/src/core/router.rs` (mount the apps router in
  `build_router`), `nami-engine/src/main.rs` (state field),
  `nami-engine/tests/common/mod.rs` (`pub mod errors;`),
  `nami-engine/tests/core/migrations.rs` (the new tables exist).

**Data shape:**

```sql
create table error_apps (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    platform text not null default 'other',
    dsn_id bigint generated always as identity,
    public_key text not null,
    allowed_origins text[] not null default '{*}',
    retention_days int not null default 90,
    rate_limit_per_minute int not null default 1000,
    daily_event_cap int not null default 100000,
    store_user_ip boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint error_apps_name_unique unique (project_id, name),
    constraint error_apps_dsn_id_unique unique (dsn_id),
    constraint error_apps_public_key_unique unique (public_key),
    constraint error_apps_public_key_format check (public_key ~ '^[0-9a-f]{32}$'),
    constraint error_apps_platform_check check (platform in
        ('javascript', 'node', 'python', 'go', 'ruby', 'java', 'dotnet', 'php', 'other')),
    constraint error_apps_retention_check check (retention_days between 1 and 365),
    constraint error_apps_rate_check check (rate_limit_per_minute between 1 and 100000),
    constraint error_apps_cap_check check (daily_event_cap between 1 and 10000000),
    constraint error_apps_origins_count check (cardinality(allowed_origins) between 1 and 50)
);
create index error_apps_project_idx on error_apps (project_id, name);
```

- Rust:
  - `pub enum Platform { Javascript, Node, Python, Go, Ruby, Java, Dotnet, Php, Other }`
    (serde `snake_case`), with `PLATFORMS` as the one list.
  - `pub enum AllowedOrigin { Any, Exact(String) }`, with
    `pub struct AllowedOrigins(Vec<AllowedOrigin>)` (non-empty, at most
    `MAX_ALLOWED_ORIGINS`).
  - Constants: `DEFAULT_RETENTION_DAYS = 90`, `MAX_RETENTION_DAYS = 365`,
    `DEFAULT_RATE_LIMIT_PER_MINUTE = 1000`, `MAX_RATE_LIMIT_PER_MINUTE = 100_000`,
    `DEFAULT_DAILY_EVENT_CAP = 100_000`, `MAX_DAILY_EVENT_CAP = 10_000_000`,
    `MAX_APPS_PER_PROJECT = 50`, `PUBLIC_KEY_HEX_LEN = 32`.
  - `pub struct ErrorApp { id, project_id, name, platform, dsn_id: i64, public_key, dsn: String, allowed_origins, retention_days, rate_limit_per_minute, daily_event_cap, store_user_ip, created_at, updated_at }`.
    `dsn` is built per response from `Config.ingest_url`.
  - `pub struct IngestApp { id, project_id, public_key, allowed_origins, retention_days, rate_limit_per_minute, daily_event_cap, store_user_ip }`
    is what ingest loads (`find_for_ingest(pool, dsn_id)`).
- JSON:
  - Create: `{"name", "platform"?, "allowed_origins"?: [..], "retention_days"?, "rate_limit_per_minute"?, "daily_event_cap"?, "store_user_ip"?}`
    with `deny_unknown_fields`.
  - Patch: the same fields, all optional.
  - Every app reply is the `ErrorApp` JSON.
  - `GET` one app adds `"usage_today": {accepted, rate_limited, over_cap, invalid, duplicate}`
    (zeros until NAMI-ERR-6 fills the table).

**API** (private API, session):

| Method and path | Min role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/errors/apps` | viewer | `200 {"apps": [..]}` by name | 403 |
| `POST /projects/{id}/errors/apps` | member | `201 ErrorApp` | 400 validation, 403, 409 name taken or `MAX_APPS_PER_PROJECT` |
| `GET /projects/{id}/errors/apps/{app_id}` | viewer | `200 ErrorApp + usage_today` | 403, 404 |
| `PATCH /projects/{id}/errors/apps/{app_id}` | member | `200 ErrorApp` | 400, 403, 404, 409 |
| `POST /projects/{id}/errors/apps/{app_id}/rotate-key` | member | `200 ErrorApp` (new key and DSN) | 403, 404 |
| `DELETE /projects/{id}/errors/apps/{app_id}` | admin | `204` | 403, 404 |

**Behaviour:**
1. The name goes through `core::validate::validate_name`.
2. The public key is 16 bytes from `OsRng`, hex-encoded. Rotating it
   replaces it in one update, and the old DSN is rejected from that moment
   (NAMI-ERR-6 test).
3. An origin entry is validated by `parse_allowed_origin`. It is trimmed
   and lowercased. `*` is allowed, or `http`/`https` with a host, an
   optional port and no path, query, fragment or userinfo, at most
   `MAX_ORIGIN_CHARS`. A trailing `/` is removed. Duplicates are removed.
   An empty list is a 400.
4. `Config.ingest_url` is read from `NAMI_INGEST_URL` (R-ERR22). An invalid
   value panics at start with a message naming the variable. The test
   config uses `http://localhost:{public_port}`.
5. The app count check and the insert run in one transaction that locks
   the project row (`select ... for update`), as `uptime::quota` does.
6. Deleting an app cascades its issues and usage rows (R-ERR20).

**Tests** (`tests/errors/apps.rs`):
- `a_member_creates_an_app_with_the_defaults_and_a_dsn`
- `the_dsn_uses_the_ingest_url_key_and_numeric_id`
- `a_viewer_lists_apps_but_cannot_create_one`
- `a_member_cannot_delete_an_app_and_an_admin_can`
- `rotating_the_key_changes_the_dsn`
- `retention_rate_and_cap_are_checked_at_and_past_their_bounds`
- `allowed_origins_accept_star_and_exact_origins_only`
- `too_many_allowed_origins_is_a_400`
- `a_duplicate_name_is_a_409`
- `the_app_after_max_apps_per_project_is_a_409`
- `an_app_of_another_project_is_a_404`
- `unknown_fields_in_the_body_are_a_400`
- Unit tests in `errors/app/mod.rs`: `parse_allowed_origin` cases (path,
  userinfo, port, uppercase, trailing slash) and DSN building with an
  ingest URL that has a path prefix.
- `tests/core/migrations.rs`: `error_apps` exists with its named
  constraints.

**Acceptance:** `cargo test --test errors apps::` and
`cargo test --test core migrations::` pass. `cargo clippy --all-targets`
is clean.

### Task 2 (NAMI-ERR-2): Write the body decoders and the envelope and store framing (pure, fuzzed)

**Covers:** R8.01 (gzip/deflate/br, size limits, unknown items skipped)  **Depends on:** NAMI-ERR-1

**Files:**
- Create: `nami-engine/src/errors/ingest/mod.rs` (module list and
  `IngestError`), `nami-engine/src/errors/ingest/decode.rs`,
  `nami-engine/src/errors/ingest/envelope.rs`,
  `nami-engine/tests/fixtures/errors/envelopes/` (the corpus: one file per
  case, listed below).
- Modify: `nami-engine/Cargo.toml` (`brotli-decompressor = "6.0.1"`,
  R-ERR25), `nami-engine/src/logs/ingest/mod.rs`: `read_body` becomes
  `read_body_within(body, headers, pace, max_bytes)`, and `read_body` keeps
  its signature and calls it with `MAX_BODY_BYTES`. The logs tests stay
  green unchanged.

**Data shape:**
- Constants:

  | Constant | Value |
  |---|---|
  | `MAX_BODY_BYTES` | 20 MiB, as sent |
  | `MAX_DECOMPRESSED_BYTES` | 20 MiB |
  | `MAX_HEADER_LINE_BYTES` | 64 KiB |
  | `MAX_ENVELOPE_ITEMS` | 100 |
  | `MAX_EVENT_ITEMS` | 1 |
  | `MAX_EVENT_BYTES` | 1 MiB |
  | `MAX_EVENT_DECODED_BYTES` | 32 MiB |
  | `MAX_JSON_DEPTH` | 128 |

- `pub enum Coding { Identity, Gzip, Zlib, Brotli }`, and
  `pub fn coding(headers: &HeaderMap) -> Result<Coding, IngestError>`
  (R-ERR5).
- `pub struct Decoder` wraps `BufReader<Box<dyn Read + Send>>` over the
  body bytes, with the output capped at `MAX_DECOMPRESSED_BYTES`
  (`Read::take(cap + 1)`). It has `first_line(&mut self) -> Result<&[u8], IngestError>`
  (at most `MAX_HEADER_LINE_BYTES`) and
  `rest(self) -> Result<Vec<u8>, IngestError>`. Both run on a blocking
  thread.
- The envelope types borrow from the decoded body, so no payload is copied
  or preallocated:

  ```rust
  pub struct EnvelopeHeader { pub event_id: Option<String>, pub dsn: Option<String> }
  pub enum ItemKind { Event, Transaction, Attachment, Other }
  pub struct Item<'a> { pub kind: ItemKind, pub payload: &'a [u8] }
  pub struct Envelope<'a> { pub header: EnvelopeHeader, pub event: Option<&'a [u8]>, pub items: usize }
  pub fn parse_envelope(body: &[u8]) -> Result<Envelope<'_>, IngestError>;
  pub fn parse_envelope_header(line: &[u8]) -> Result<EnvelopeHeader, IngestError>;
  ```

  Headers are deserialized into typed structs (`#[derive(Deserialize)]`,
  unknown fields ignored, `type` required on items, `length: Option<u64>`),
  never into a `Value`.
- `pub fn decode_event(payload: &[u8]) -> Result<serde_json::Map<String, Value>, IngestError>`:
  - over `MAX_EVENT_BYTES` is a 413;
  - decoded with `logs::ingest::parse_json_events_within` under
    `Budget::new(MAX_EVENT_DECODED_BYTES)`, and over the budget is a 413;
  - a payload whose first non-whitespace byte is not `{`, or that holds
    more than one value, is a 400;
  - deeper than `MAX_JSON_DEPTH` is a 400 (R-ERR7).
- `IngestError` variants, each with its status: `InvalidDsn` (401),
  `OriginNotAllowed` (403), `BadRequest(String)` (400),
  `PayloadTooLarge(String)` (413), `UnsupportedEncoding` (415),
  `RateLimited { retry_after_secs }` (429), `TooManyInFlight` (429),
  `Busy` (429), `RequestTimeout` (408), `Unavailable` (503, the store
  failed). `TooManyInFlight` and `Busy` carry `Retry-After: 1` and
  `X-Sentry-Rate-Limits: 1::key` (R-ERR9). The body is
  `{"error": "<message>"}`, with a constant message and no echo of the
  input beyond 200 bytes.

**Behaviour:**
1. Framing follows the spec and R-ERR6:
   - the envelope header line, then items;
   - an item header line, then `length` bytes followed by `\n` or the end,
     or with no `length`, up to the next `\n` or the end.
2. `length` is checked against what is left (`remaining.len()`) before it
   is used, so an `i64::MAX` length is a 400 with no allocation.
3. `event` items are returned. `transaction` and `attachment` items are
   counted and read past. Every other type is skipped. Ids are not checked
   here.
4. An empty body is a 400. A body that is only an envelope header (with
   or without a final `\n`) is a valid envelope with no items.
5. A trailing `\r` before `\n` on a header line is accepted (JSON
   whitespace).
6. The store body is one event JSON: `decode_event` over the whole decoded
   body.
7. A decode that passes `MAX_DECOMPRESSED_BYTES` stops at the cap and is a
   413. A corrupt stream is a 400.

**Tests** (unit tests in `envelope.rs` and `decode.rs`):
- `an_envelope_with_an_event_and_an_attachment_returns_the_event`
- `an_item_without_length_ends_at_the_next_newline`
- `a_length_past_the_end_is_a_400`
- `a_payload_not_followed_by_a_newline_is_a_400`
- `an_invalid_envelope_header_is_a_400`
- `an_item_header_without_type_is_a_400`
- `unknown_item_types_are_skipped`
- `a_second_event_item_is_a_400`
- `max_envelope_items_is_accepted_and_one_more_is_a_400`
- `a_header_line_at_max_header_line_bytes_is_accepted_and_one_byte_more_is_a_400`
- `an_event_of_max_event_bytes_is_accepted_and_one_byte_more_is_a_413`
- `a_small_event_that_decodes_past_the_budget_is_a_413`
- `nesting_of_max_json_depth_is_accepted_and_one_more_is_a_400`
- `gzip_zlib_and_brotli_bodies_decode`
- `a_list_of_codings_or_an_unknown_coding_is_a_415`
- `a_gzip_bomb_stops_at_max_decompressed_bytes_with_a_413` (and a brotli
  bomb, and a zlib bomb)
- `the_first_line_is_decoded_without_inflating_the_rest`: a gzip body whose
  first line is small and whose rest inflates past the cap returns the
  first line.
- `the_fixture_corpus_never_panics`: every file in
  `tests/fixtures/errors/envelopes/` is parsed and gives `Ok` or a 4xx
  `IngestError`.
- `seeded_mutations_of_the_corpus_never_panic`: a fixed-seed `StdRng`
  flips, inserts, deletes and duplicates bytes and length digits, 20,000
  cases, under `catch_unwind`.
- Corpus files: `event_only`, `event_attachment_transaction`,
  `no_length_items`, `session_only`, `header_only`, `crlf_lines`,
  `bad_length`, `negative_length`, `huge_length`, `missing_newline`,
  `bad_header_json`, `bad_item_json`, `truncated_payload`, `two_events`,
  `deep_nesting`, `binary_attachment`.

**Acceptance:** `cargo test --lib errors::ingest` passes. The logs tests
(`cargo test --test logs logs_ingest::`) pass unchanged.

### Task 3 (NAMI-ERR-3): Normalize events and scrub sensitive data (pure)

**Covers:** R8.02  **Depends on:** NAMI-ERR-2

**Files:**
- Create: `nami-engine/src/errors/normalize/mod.rs` (`NormalizedEvent` and
  `normalize`), `nami-engine/src/errors/normalize/scrub.rs`,
  `nami-engine/tests/fixtures/errors/events/` (SDK samples: sentry-javascript
  browser, sentry-python, sentry-go, a legacy store body).

**Data shape:**
- The canonical document (`#[derive(Serialize, Deserialize)]`). An empty
  optional field is left out (`skip_serializing_if`):

  ```rust
  pub enum Level { Fatal, Error, Warning, Info, Debug }      // snake_case; default Error
  pub struct NormalizedEvent {
      pub event_id: String,                 // 32 lowercase hex
      pub timestamp: DateTime<Utc>, pub received: DateTime<Utc>,
      pub platform: String, pub level: Level,
      pub logger: Option<String>, pub transaction: Option<String>, pub server_name: Option<String>,
      pub release: Option<String>, pub environment: String, pub dist: Option<String>,
      pub message: Option<String>,
      pub sdk: Option<Sdk>,                 // name, version
      pub exception: Vec<Exception>,        // oldest first
      pub breadcrumbs: Vec<Breadcrumb>,
      pub tags: BTreeMap<String, String>,
      pub contexts: Map<String, Value>,
      pub user: Option<User>,               // id, username, email, ip_address
      pub request: Option<Request>,         // url, method, query, headers
      pub extra: Map<String, Value>,
      pub fingerprint: Vec<String>,
      pub trace_id: Option<String>,         // 32 hex, from contexts.trace.trace_id
  }
  pub struct Exception { pub r#type: Option<String>, pub value: Option<String>, pub module: Option<String>,
      pub mechanism: Option<Mechanism>, pub frames: Vec<Frame> }       // mechanism: type, handled: Option<bool>
  pub struct Frame { pub filename, abs_path, function, module: Option<String>, pub lineno, colno: Option<u32>,
      pub in_app: Option<bool>, pub context_line: Option<String>, pub pre_context, post_context: Vec<String> }
  pub struct NormalizeInput<'a> { pub app: &'a IngestApp, pub received: DateTime<Utc>, pub peer_ip: IpAddr, pub user_key_secret: &'a [u8; 32] }
  pub fn normalize(raw: Map<String, Value>, input: &NormalizeInput) -> (NormalizedEvent, String /* user_key */);
  ```

- Limits (`pub const` in `normalize/mod.rs`). The spec gives the first
  group; the rest are rulings (R-ERR11):

  | Field | Limit |
  |---|---|
  | `release` | 200 chars, Sentry's rules: no `/`, `\`, newline or tab, not `.`, `..` or blank, else dropped |
  | `environment` | 64 chars, no `/` or control chars, else `production` |
  | exception `value` | 8 KiB |
  | frames per exception | 250: the first 125 and the last 125 (`MAX_FRAMES`, `KEEP_HEAD_FRAMES`, `KEEP_TAIL_FRAMES`) |
  | `pre_context` / `post_context` | 5 lines each, the 5 nearest the context line |
  | breadcrumbs | the last 100; `message` 1 KiB; `data` 4 KiB as JSON (over: `data` dropped) |
  | tags | 100; key 32 chars of `[A-Za-z0-9_.:-]`; value 200 chars, no control chars |
  | `contexts` | 64 KiB as JSON (over: only `trace` is kept, and if that is still over, nothing) |
  | `extra` | 64 KiB as JSON (over: `{"_nami_truncated": true}`) |
  | `platform` | `[a-z0-9_-]{1,32}`, else `other` |
  | `logger`, `dist`, sdk `name` / `version` | 64 chars |
  | `transaction`, `server_name` | 200 chars |
  | `message` | 8 KiB |
  | exceptions per chain | 25, the most recent (`MAX_EXCEPTIONS`) |
  | exception `type`, `module`, frame `function`, frame `module` | 256 chars |
  | mechanism `type` | 64 chars |
  | frame `filename`, `abs_path` | 1024 chars |
  | context line | 512 chars; a longer line keeps the 512 around `colno` when known (minified bundles) |
  | `user.id`, `user.username` | 128 chars; `user.email` 256 chars |
  | `request.url` | 2048 chars; `method` 16 chars of `[A-Z]`; `query` 4 KiB; `headers` 100 entries, value 1 KiB |
  | `fingerprint` | 32 entries of 256 chars; strings, numbers and booleans are kept as strings, others dropped |

**Behaviour:**
1. `event_id`: 32 hex characters, or a hyphenated UUID, becomes lowercase
   32 hex. Anything else is replaced by a fresh `Uuid::new_v4().simple()`.
2. `timestamp`: seconds (integer or float) or an RFC 3339 string, with
   R-ERR19. Missing or invalid becomes the receive time.
3. `level`: `fatal|error|warning|info|debug`, plus `warn` to warning,
   `critical` to fatal and `log` to info (case-insensitive). Anything else
   is `error`.
4. `message`: `logentry.formatted`, else `logentry.message`, else a string
   `message`, else `message.formatted` / `message.message` (legacy). Params
   are not substituted.
5. `exception`: `exception.values[]`, or a bare array (legacy), keeping the
   most recent `MAX_EXCEPTIONS`. Frames come from `stacktrace.frames[]`
   (oldest first). `vars` and every other frame key are dropped.
   Top-level `stacktrace` and `threads` are dropped (native and thread
   dumps are a non-goal).
6. `breadcrumbs`: `breadcrumbs.values[]` or a bare array. Fields
   `timestamp`, `type`, `category`, `level`, `message` and `data`.
7. `tags`: an object, or an array of `[key, value]` pairs. Scalar values
   become strings. Invalid pairs are dropped. Order is by key
   (`BTreeMap`), and the first `MAX_TAGS` are kept.
8. `contexts.trace.trace_id` becomes `trace_id` when
   `logs::ingest::normalize::is_hex_id(id, 32)`.
9. `user`: `id` (a string or a number), `username`, `email`. For
   `ip_address`, see R-ERR12 and R-ERR13 (kept only with `store_user_ip`;
   `{{auto}}` is the peer IP). Other user keys are dropped.
10. `request`: `url`, `method`, `query` (a string, an object or pairs,
    stored as a query string), and `headers` (an object or pairs). `Cookie`
    and `Authorization` are always removed (case-insensitive), as are
    `request.data`, `request.cookies` and `request.env` (R-ERR12).
11. Scrubbing (`scrub.rs`, R-ERR12) runs over `request` (headers and query
    parameters, the url's query included), `extra`, `contexts` and every
    breadcrumb's `data`, at any depth. The words are `SENSITIVE_KEY_PARTS`:
    `password`, `passwd`, `secret`, `token`, `api_key`, `apikey`,
    `authorization`, `cookie`, `session`, `csrf`, `credit_card`,
    `card_number`, `ssn`. The replacement is `FILTERED = "[Filtered]"`.
12. Unknown top-level keys are dropped. The result is what `error_events.payload`
    stores, and what the issue detail shows.

**Tests** (unit tests in `normalize/mod.rs` and `scrub.rs`):
- `each_sdk_fixture_normalizes_to_its_expected_document` (golden JSON
  files next to the fixtures)
- `a_hyphenated_event_id_is_lowercased_and_an_invalid_one_is_replaced`
- `a_timestamp_more_than_max_future_skew_ahead_becomes_the_receive_time`
  (at the bound and past it), and the same for `MAX_EVENT_AGE`
- `level_synonyms_map_and_unknown_levels_are_error`
- `a_release_at_200_chars_is_kept_and_201_or_a_slash_drops_it`
- `an_invalid_environment_becomes_production`
- `frames_past_max_frames_keep_the_first_and_last_125`
- `context_lines_keep_the_five_nearest`
- `a_long_minified_context_line_keeps_the_window_around_colno`
- `frame_vars_are_dropped`
- `the_last_100_breadcrumbs_are_kept`
- `breadcrumb_data_over_4_kib_is_dropped`
- `tags_from_pairs_and_objects_are_capped_and_invalid_keys_dropped`
- `contexts_over_64_kib_keep_only_trace`
- `extra_over_64_kib_is_replaced_by_the_truncation_marker`
- `sensitive_keys_at_any_depth_become_filtered` (one case per word in
  `SENSITIVE_KEY_PARTS`, plus `X-Api-Key` and a nested array)
- `query_parameters_in_the_url_and_query_are_scrubbed`
- `cookie_and_authorization_headers_are_removed`
- `request_data_cookies_and_env_are_dropped`
- `without_store_user_ip_no_ip_is_kept_anywhere`: `user.ip_address`, the
  forwarding headers and `{{auto}}` are absent from the serialized
  document.
- `with_store_user_ip_auto_becomes_the_peer_ip`
- `user_key_prefers_id_then_email_then_username_then_keyed_ip_hash`
- `the_ip_hash_depends_on_the_secret_and_the_app`
- `the_trace_id_comes_from_the_trace_context`

**Acceptance:** `cargo test --lib errors::normalize` passes. A reviewer can
check every limit in the table against a named constant and a test.

### Task 4 (NAMI-ERR-4): Group events and add the `error_events` table

**Covers:** R8.03 (Part 1 half), R8.09  **Depends on:** NAMI-ERR-3

**Files:**
- Create: `nami-engine/src/errors/grouping.rs`,
  `nami-engine/src/errors/store.rs` (row type, insert, the event reads that
  NAMI-ERR-7 uses), `nami-engine/tests/errors/store.rs`.
- Modify: `nami-engine/src/core/clickhouse.rs` (`MIGRATIONS` gains the
  `error_events` statement under `// ---- Errors domain ----`; the array
  length grows by one), `nami-engine/tests/core/clickhouse.rs` (the table
  exists after `migrate`).

**Data shape:**
- ClickHouse, exactly as the spec gives it:

  ```sql
  CREATE TABLE IF NOT EXISTS error_events (
      project_id UUID,
      app_id UUID,
      issue_id UUID,
      event_id String,
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
      payload String CODEC(ZSTD(3)),
      retention_days UInt16,
      INDEX idx_event event_id TYPE bloom_filter(0.01) GRANULARITY 4
  ) ENGINE = MergeTree
  PARTITION BY toYYYYMMDD(dt)
  ORDER BY (project_id, issue_id, dt)
  TTL toDateTime(dt) + toIntervalDay(retention_days)
  ```

- `pub struct ErrorEventRow` (Serialize) has one field per column.
  - `release` is `""` when the event has none.
  - `exception_type` is the type of the last exception, else `""`.
  - `message` is the last exception's value, else the message, at most
    8 KiB.
  - `retention_days` is copied from the app at ingest, so a change applies
    to new events only.
  - `pub async fn insert_event(ch, &ErrorEventRow)` uses
    `logs::ingest::store::INSERT_SETTINGS` (async insert, wait).
- Grouping:

  ```rust
  pub const GROUPING_VERSION: i16 = 1;
  pub const MAX_GROUPING_FRAMES: usize = 5;
  pub struct Grouping { pub components: Vec<String>, pub hash: String /* 32 hex */, pub title: String, pub culprit: String }
  pub fn group(event: &NormalizedEvent) -> Grouping;
  ```

**Behaviour:**
1. With a non-empty `fingerprint`: the components are its entries in
   order. An entry matching `^\{\{\s*default\s*\}\}$` (case-insensitive)
   expands to the default components below.
2. Else, with at least one exception, the default components are:
   - the `type` (or `""`) of every exception, oldest first;
   - the normalized `value` of the last exception
     (`logs::query::pattern::normalize_message`);
   - up to `MAX_GROUPING_FRAMES` frames of the last exception: the most
     recent frames with `in_app == Some(true)`, or the most recent frames
     when none is in-app. Each is `"<location>|<function or ?>"`.
3. Else: the normalized message (`""` when there is none).
4. A frame's location is its `module` when set. Otherwise it is its
   `filename` (or `abs_path` when `filename` is empty), cleaned like this:
   - for a URL (`scheme://host`), only its path is kept;
   - `?...` and `#...` are removed;
   - content hashes in the last path segment are removed: every match of
     `(?i)[.\-_][0-9a-f]{8,}(?=[.\-_]|$)`, so `main.3f9a8c1b.js` becomes
     `main.js` and `app-5d41402abc4b2a76.js` becomes `app.js`.

   Line and column numbers are never used.
5. `hash` = the first 32 hex characters of SHA-256 over
   `serde_json::to_vec(&components)` (R-ERR14).
6. `title`:
   - `"<type>: <first line of value>"`, using the last exception;
   - `type` alone when the value is empty, or the value alone when the
     type is empty;
   - else the first line of the message;
   - else `<unlabeled event>`.

   Cut at `MAX_TITLE_CHARS = 200`.
7. `culprit`: `"<function> (<location>)"` of the most recent in-app frame
   (or the most recent frame), else `transaction`, else `""`. At most 200
   chars.

**Tests:**
- Unit tests in `grouping.rs`:
  - `the_same_error_on_different_lines_groups_together`
  - `a_different_exception_type_splits_the_group`
  - `numbers_and_uuids_in_the_value_do_not_split_the_group`
  - `only_the_five_most_recent_in_app_frames_count`
  - `without_in_app_frames_the_most_recent_frames_count`
  - `content_hashes_and_query_strings_are_stripped_from_filenames`
  - `a_url_filename_groups_across_cdn_hosts`
  - `a_fingerprint_replaces_the_default_and_default_expands_inside_it`
    (with `{{default}}` and `{{ default }}`)
  - `component_boundaries_are_part_of_the_hash` (`["ab","c"]` vs
    `["a","bc"]`)
  - `a_message_event_groups_by_its_normalized_message`
  - `the_title_and_culprit_follow_the_rules`
  - `the_hash_of_a_fixed_event_is_stable`: a golden value, so a change to
    the algorithm cannot pass unnoticed while `GROUPING_VERSION` stays 1.
- `tests/errors/store.rs`:
  - `an_inserted_event_reads_back_with_its_payload`
  - `the_row_carries_the_app_retention`
  - `events_are_stored_per_project_and_issue`
- `tests/core/clickhouse.rs`: `migrate_creates_error_events` (and running
  `migrate` twice is a no-op).

**Acceptance:** `cargo test --lib errors::grouping`,
`cargo test --test errors store::` and `cargo test --test core clickhouse::`
pass.

### Task 5 (NAMI-ERR-5): Add issues with their state machine, counters and the sweep

**Covers:** R8.04 (issue records, lifecycle, regression)  **Depends on:** NAMI-ERR-4

**Files:**
- Create: `nami-engine/src/errors/issue/mod.rs` (the state machine, pure),
  `nami-engine/src/errors/issue/model.rs` (SQL),
  `nami-engine/src/errors/issue/counters.rs` (pending deltas and the
  flush), `nami-engine/src/errors/worker.rs` (the loop, R-ERR24),
  `nami-engine/tests/errors/issues.rs`.
- Modify: `nami-engine/migrations/NNNN_errors_apps_issues.sql` (append),
  `nami-engine/src/errors/mod.rs` (`ErrorsState.counters`),
  `nami-engine/src/main.rs` (spawn `errors::worker::run_forever`),
  `nami-engine/tests/core/migrations.rs`.

**Data shape:**

```sql
create table error_issues (
    id uuid primary key,                       -- Uuid::now_v7(), set by the engine
    project_id uuid not null references projects(id) on delete cascade,
    app_id uuid not null references error_apps(id) on delete cascade,
    grouping_hash text not null,
    grouping_version smallint not null default 1,
    title text not null,
    culprit text not null default '',
    level text not null,
    first_seen timestamptz not null,
    last_seen timestamptz not null,
    event_count bigint not null default 0,
    first_release text,
    last_release text,
    status text not null default 'unresolved',
    resolved_in_release text,
    resolved_at timestamptz,
    resolved_by uuid references users(id) on delete set null,
    ignore_until timestamptz,
    ignore_until_count bigint,
    ignored_at_count bigint,
    regressed_at timestamptz,
    constraint error_issues_hash_unique unique (app_id, grouping_hash),
    constraint error_issues_hash_format check (grouping_hash ~ '^[0-9a-f]{32}$'),
    constraint error_issues_level_check check (level in ('fatal', 'error', 'warning', 'info', 'debug')),
    constraint error_issues_count_check check (event_count >= 0),
    constraint error_issues_status_check check (status in ('unresolved', 'resolved', 'ignored')),
    constraint error_issues_resolved_shape check ((status = 'resolved') = (resolved_at is not null)),
    constraint error_issues_release_shape check (status = 'resolved' or resolved_in_release is null),
    constraint error_issues_ignore_shape check (status = 'ignored'
        or (ignore_until is null and ignore_until_count is null and ignored_at_count is null)),
    constraint error_issues_ignore_one_rule check (ignore_until is null or ignore_until_count is null),
    constraint error_issues_ignore_count_base check ((ignore_until_count is null) = (ignored_at_count is null)),
    constraint error_issues_ignore_count_positive check (ignore_until_count is null or ignore_until_count > 0)
);
create index error_issues_project_last_seen_idx on error_issues (project_id, last_seen desc);
create index error_issues_app_last_seen_idx on error_issues (app_id, last_seen desc);
create index error_issues_ignore_until_idx on error_issues (ignore_until)
    where status = 'ignored' and ignore_until is not null;
```

The state machine (`errors/issue/mod.rs`, no I/O):

```rust
pub enum IssueStatus { Unresolved, Resolved { in_release: Option<String> }, Ignored(Ignore) }
pub enum Ignore { Forever, UntilCount { at_count: i64, more: i64 }, UntilDate(DateTime<Utc>) }
pub enum Command { Resolve, Unresolve, Ignore(IgnoreRequest) }
pub enum IgnoreRequest { Forever, Count(i64), Until(DateTime<Utc>) }
pub enum Effect { Regressed, Reopened, Closed }   // Closed = resolved or ignored by a member
pub struct Transition { pub next: IssueStatus, pub effect: Option<Effect> }
pub struct Context<'a> { pub latest_release: Option<&'a str>, pub event_count: i64, pub now: DateTime<Utc> }

pub fn on_command(current: &IssueStatus, command: Command, ctx: &Context) -> Result<Transition, String>;
pub fn on_event(current: &IssueStatus, release: Option<&str>, count_after: i64) -> Option<Transition>;
pub fn on_sweep(current: &IssueStatus, now: DateTime<Utc>) -> Option<Transition>;
pub const MAX_IGNORE_COUNT: i64 = 1_000_000;
pub const MAX_IGNORE_DAYS: i64 = 365;
```

`IssueStatus` maps to and from the columns in one place
(`model::status_columns` / `model::row_status`).

**Behaviour:**
1. Transitions (R-ERR15):

   | From | Trigger | To | Effect |
   |---|---|---|---|
   | any | `Resolve` | `Resolved { in_release: ctx.latest_release }` | `Closed` (none if already resolved) |
   | any | `Unresolve` | `Unresolved` | none |
   | any | `Ignore(Forever)` | `Ignored(Forever)` | `Closed` |
   | any | `Ignore(Count(n))`, 1 <= n <= `MAX_IGNORE_COUNT` | `Ignored(UntilCount { at_count: ctx.event_count, more: n })` | `Closed` |
   | any | `Ignore(Until(t))`, now < t <= now + `MAX_IGNORE_DAYS` | `Ignored(UntilDate(t))` | `Closed` |
   | `Resolved { in_release: r }` | event with release `e`, where `r` is `None` or `e != r` (a missing `e` differs) | `Unresolved` with `regressed_at` | `Regressed` |
   | `Resolved { in_release: Some(r) }` | event with release `r` | unchanged | none |
   | `Ignored(UntilCount { at_count, more })` | event with `count_after >= at_count + more` | `Unresolved` | `Reopened` |
   | `Ignored(UntilDate(t))` | sweep at `now >= t` | `Unresolved` | `Reopened` |
   | `Unresolved`, `Ignored(Forever)`, other cases | event | unchanged | none |

   An out-of-range `n` or `t` is `Err` (400 at the API). Effects are what
   Part 2 hooks into: a `Regressed` alert, and incidents resolved on
   `Closed`. Part 1 only records them.
2. First event:
   `insert ... on conflict (app_id, grouping_hash) do nothing returning id`
   with `event_count = 0`, `first_seen = last_seen = event time`,
   `first_release = last_release = event release`. When the insert loses
   the race, the row is selected. Concurrent first events make one issue
   (test). `upsert_issue` returns `(IssueRow, created: bool)` with no row
   lock.
3. Each later event reads the row by `(app_id, grouping_hash)` (one index
   lookup) and calls `on_event` with
   `count_after = event_count + pending + 1`. A transition is written with
   a guarded update. For a regression:
   `where id = $1 and status = 'resolved' and resolved_in_release is not distinct from $2`.
   For a count reopen: `where id = $1 and status = 'ignored' and ignored_at_count = $2`.
   Only the request whose update returns the row reports the effect, so a
   regression is detected once across requests and instances.
4. Counters (`counters.rs`, R-ERR16):
   - `PendingIssues` is a `std::sync::Mutex<HashMap<Uuid, IssueDelta>>`.
     `IssueDelta` holds `count`, `last_seen`, and the latest event's
     `title`, `culprit`, `level` and `release`.
   - `record(issue_id, &delta)` is called after the ClickHouse insert has
     succeeded.
   - `pending(issue_id) -> i64` is read by rule 3.
   - `flush(pool)` swaps the map out under the lock, then runs one
     `update error_issues set ... from unnest($1::uuid[], $2::bigint[], ...)`.
     The latest fields are set only where the delta's `last_seen >=` the
     stored one, and `first_release` only when it is null. On error the
     deltas are merged back.
   - `ISSUE_FLUSH_INTERVAL = 5 s`.
5. Worker (`worker.rs`), every `ISSUE_FLUSH_INTERVAL`:
   - flush issue counters;
   - every `USAGE_FLUSH_INTERVAL` (10 s, added in NAMI-ERR-6), flush usage;
   - every `SWEEP_INTERVAL` (60 s):
     - reopen at most `SWEEP_BATCH` ignored issues whose `ignore_until <= now()`,
       through `on_sweep` and a guarded update;
     - prune usage rows older than `USAGE_RETENTION_DAYS = 400` (NAMI-ERR-6).

   A failing step logs at `error` and the loop continues.

**Tests:**
- Unit tests in `errors/issue/mod.rs`: one test per table row, plus
  `ignore_count_of_zero_or_past_max_ignore_count_is_rejected`,
  `ignore_until_now_or_past_max_ignore_days_is_rejected`,
  `resolving_records_the_latest_release`,
  `an_issue_without_releases_regresses_on_any_event`,
  `an_event_without_a_release_regresses_an_issue_resolved_in_a_release`.
- `tests/errors/issues.rs`:
  - `the_first_event_creates_one_issue_even_when_two_race`
  - `counters_flush_into_event_count_and_last_seen`
  - `a_failed_flush_keeps_its_deltas` (the pool closed for one flush)
  - `the_latest_event_sets_title_culprit_level_and_last_release`
  - `a_regression_is_reported_once_for_concurrent_events`
  - `ignore_until_count_reopens_after_n_more_events`
  - `the_sweep_reopens_an_issue_whose_ignore_date_passed`
  - `the_status_shape_constraints_reject_inconsistent_rows`
  - `deleting_an_app_deletes_its_issues`

**Acceptance:** `cargo test --lib errors::issue` and
`cargo test --test errors issues::` pass.

### Task 6 (NAMI-ERR-6): Serve the ingest endpoints with auth, CORS, spike protection, dedup and usage

**Covers:** R8.01 (endpoints), R8.06, R8.09 (rows written with their retention)  **Depends on:** NAMI-ERR-5

**Files:**
- Create: `nami-engine/src/errors/ingest/auth.rs` (key extraction, pure),
  `nami-engine/src/errors/ingest/cors.rs` (origin check and headers,
  pure), `nami-engine/src/errors/ingest/limits.rs` (`ErrorLimits`: buckets,
  in-flight, dedup), `nami-engine/src/errors/ingest/routes.rs` (the public
  router), `nami-engine/src/errors/cache.rs` (`TwoGen`, R-ERR10),
  `nami-engine/src/errors/usage.rs`, `nami-engine/tests/errors/ingest.rs`,
  `nami-engine/tests/errors/ingest_limits.rs`.
- Modify: `nami-engine/migrations/NNNN_errors_apps_issues.sql` (append
  `error_app_usage_daily`), `nami-engine/src/errors/ingest/mod.rs`
  (`accept_event`, the pipeline),
  `nami-engine/src/logs/ingest/rate_limit.rs`
  (`TokenBuckets::try_take_with(key, amount, rate, burst, now)`, which
  `try_take_at` calls with its own rate and burst; the logs tests stay
  green), `nami-engine/src/core/router.rs` (merge the errors public router
  into `build_public_router_with_limits`), `nami-engine/src/errors/worker.rs`
  (usage flush and prune), `nami-engine/src/errors/app/model.rs`
  (`usage_today`), `nami-engine/tests/common/errors.rs` (`envelope(..)`,
  `post_envelope(..)`, gzip, zlib and brotli helpers).

**Data shape:**

```sql
create table error_app_usage_daily (
    app_id uuid not null references error_apps(id) on delete cascade,
    day date not null,
    accepted bigint not null default 0,
    rate_limited bigint not null default 0,
    over_cap bigint not null default 0,
    invalid bigint not null default 0,
    duplicate bigint not null default 0,
    constraint error_app_usage_daily_pk primary key (app_id, day)
);
```

- `pub struct ErrorLimits { places: Arc<Semaphore>, decodes: Arc<Semaphore>, buckets: TokenBuckets, in_flight: InFlight, seen: TwoGen<(Uuid, u128)> }`.
  `buckets` and `in_flight` reuse `logs::ingest::rate_limit`.
- Constants: `MAX_ERROR_INGEST_IN_FLIGHT = 32`,
  `MAX_ERROR_DECODES_IN_FLIGHT = 4`, `MAX_IN_FLIGHT_PER_APP = 4`,
  `MAX_DEDUP_IDS = 200_000`, `DEDUP_GENERATION = 1 h`,
  `USAGE_FLUSH_INTERVAL = 10 s`, `FLUSHED_CACHE_TTL = 10 s`,
  `USAGE_RETENTION_DAYS = 400`. The body pace reuses
  `logs::ingest::BodyPace::DEFAULT`.
- `pub async fn accept_event(state: &AppState, app: &IngestApp, raw: Map<String, Value>, received: DateTime<Utc>, peer: IpAddr) -> Result<String /* event_id */, IngestError>`
  covers steps 7 to 10 of the Architecture. It is the RUM seam (R-ERR23).

**API** (public listener, no session):

| Method and path | Auth | Success | Errors |
|---|---|---|---|
| `POST /api/{dsn_id}/envelope/` (and without `/`) | DSN key (R-ERR3) | `200 {"id": ".."}` or `{}` | 400 framing or JSON, 401 DSN, 403 origin, 408 slow body, 413 size, 415 coding, 429 limits or busy, 503 store down |
| `POST /api/{dsn_id}/store/` (and without `/`) | DSN key, header or query only | `200 {"id": ".."}` | as above |
| `OPTIONS` on both | none | `204` with the R-ERR8 headers | none |

**Behaviour** (the order is fixed, and each step's failure is the status
shown):
1. Take an engine-wide place, or 429 `Busy`.
2. Take the key from the header or query (R-ERR3). Load
   `IngestApp` by `dsn_id` and compare the key in constant time, or 401.
   With no key, do this after step 5's first-line decode (R-ERR4).
3. Check `Origin` against `allowed_origins` (R-ERR8), or 403.
4. `read_body_within(.., MAX_BODY_BYTES)` (408 or 413).
5. Take one of the app's in-flight places
   (`InFlight::try_enter(app.id)`), or 429 `TooManyInFlight`. Then decode
   under a decode place (415, 400 or 413).
6. `parse_envelope`, or the store body. A 400 or 413 after auth counts
   as `invalid`.
7. With no event item: reply now (R-ERR6), and count nothing.
8. `event_id` (from the payload, else the envelope header, else
   generated). If it is in `seen`: reply 200 and count `duplicate`.
9. Take one bucket token with rate `rate_limit_per_minute / 60` and burst
   `rate_limit_per_minute`, or 429 counted as `rate_limited` (R-ERR9).
10. Check the daily cap: `flushed_today (cached) + pending >= daily_event_cap`
    is a 429 counted as `over_cap`.
11. Insert into `seen`. On a blocking thread: `decode_event`, `normalize`,
    `group`.
12. Upsert the issue and apply `on_event` (NAMI-ERR-5).
13. Insert the `ErrorEventRow`. On failure, remove the id from `seen` and
    answer 503.
14. Record the issue counters, count `accepted`, and reply
    `200 {"id": event_id}`.

The 429 replies carry `Retry-After` and `X-Sentry-Rate-Limits: <s>::key`.
Every reply to an allowed origin carries the CORS headers (R-ERR8).
`ConnectInfo` gives the peer IP for R-ERR13. `usage.rs` mirrors
`logs::ingest::usage` (in-memory per `(app, day)`, flushed with
`insert ... on conflict do update set x = x + excluded.x`). The cached
flushed count is refreshed after `FLUSHED_CACHE_TTL`.

**Tests:**
- `tests/errors/ingest.rs`:
  - `a_python_sdk_envelope_creates_an_issue_and_an_event`
  - `a_browser_sdk_envelope_with_the_query_key_and_text_plain_is_accepted`
  - `the_store_endpoint_accepts_a_legacy_event`
  - `gzip_deflate_and_brotli_envelopes_are_accepted`
  - `the_key_in_the_envelope_header_dsn_is_accepted`
  - `an_unknown_key_is_a_401`
  - `another_apps_key_is_a_401`
  - `a_key_for_another_dsn_id_in_the_envelope_header_is_a_401`
  - `a_rotated_key_is_a_401`
  - `a_non_numeric_dsn_id_is_a_401`
  - `the_401_bodies_are_identical` (all of the above)
  - `an_origin_outside_allowed_origins_is_a_403_without_cors_headers`
  - `an_allowed_origin_gets_cors_headers_also_on_a_429`
  - `a_request_without_origin_is_not_origin_checked`
  - `a_preflight_is_answered_without_a_database_lookup`
  - `a_session_only_envelope_is_acknowledged_and_stores_nothing`
  - `transactions_and_attachments_are_dropped`
  - `a_duplicate_event_id_is_acknowledged_and_stored_once`
  - `an_event_whose_store_fails_is_accepted_when_retried`
  - `events_link_their_trace_id`
  - `a_body_over_max_body_bytes_is_a_413_before_it_is_read` (by declared
    length)
  - `a_bomb_body_is_a_413`
  - `malformed_envelopes_are_400_and_counted_invalid`
  - `rows_carry_the_apps_retention_days`
- `tests/errors/ingest_limits.rs`:
  - `the_bucket_allows_rate_limit_per_minute_events_then_429s_with_sentry_headers`
  - `the_retry_after_of_the_bucket_is_at_least_one_second`
  - `the_daily_cap_is_accepted_up_to_the_cap_then_429_until_midnight`
  - `the_cap_counts_flushed_and_pending_events`
  - `envelopes_without_events_take_no_token`
  - `a_request_past_max_in_flight_per_app_is_a_429_with_sentry_headers`
  - `a_slow_body_does_not_hold_a_place_of_its_app`
  - `a_full_endpoint_is_a_429_with_a_one_second_retry` (`MAX_ERROR_INGEST_IN_FLIGHT`)
  - `usage_counters_flush_per_app_and_day`
  - `usage_rows_older_than_retention_are_pruned`
- Unit tests:
  - `auth.rs`: header parsing (spacing, case, missing `sentry_key`, a
    second scheme), first-source-wins.
  - `cors.rs`: origin matching (`*`, exact, `null`, port, case).
  - `cache.rs`: `TwoGen` rotation by age and by size, the 1 to 2 h window.
  - `rate_limit.rs`: `try_take_with` with a per-call rate.

**Acceptance:** `cargo test --test errors ingest::`,
`cargo test --test errors ingest_limits::` and
`cargo test --test logs logs_ingest::` pass. A manual check with
`sentry-cli send-event` or a small `sentry-sdk` script against a local
engine shows an issue.

### Task 7 (NAMI-ERR-7): Build the issues and events API: list, detail, event navigation, status

**Covers:** R8.04 (counts, affected users, resolve, ignore through the API)  **Depends on:** NAMI-ERR-6

**Files:**
- Create: `nami-engine/src/errors/issue/routes.rs`,
  `nami-engine/src/errors/issue/list.rs` (the R-ERR17 query, with
  ClickHouse SQL from static fragments), `nami-engine/tests/errors/issue_api.rs`.
- Modify: `nami-engine/src/errors/issue/model.rs`,
  `nami-engine/src/errors/store.rs` (event reads, top tags, sparklines,
  user counts), `nami-engine/src/core/router.rs` (mount the issues router).

**Data shape:**
- List query, parsed into `IssueQuery` (unknown parameters are a 400):
  - `app_id`;
  - `status` = `unresolved` (the default), `resolved`, `ignored` or `all`;
  - `environment` and `release` (validated like the event fields);
  - `level`;
  - `q`: a title search of at most 200 chars, matched as
    `strpos(lower(title), lower($q)) > 0`, so no LIKE escaping is needed;
  - `range` = `24h`, `7d`, `14d` (the default), `30d` or `90d`, or `from`
    and `to` (RFC 3339, `from < to`, at most 90 days);
  - `sort` = `last_seen` (the default), `first_seen`, `events` or `users`;
  - `page` (from 1) and `limit` (`DEFAULT_PAGE_SIZE = 25`,
    `MAX_PAGE_SIZE = 100`).
- `IssueListItem`: `{id, app_id, app_name, title, culprit, level, status, first_seen, last_seen, first_release, last_release, event_count, regressed_at, events, users, sparkline: [u64; 24]}`.
  `events` and `users` are range counts (R-ERR17). `sparkline` is hourly
  over the last 24 h.
- List reply: `{"issues": [..], "page", "limit", "truncated": bool}`.
- `IssueDetail`: the issue's columns, plus:
  - `users`: lifetime `uniqIf(user_key, user_key != '')`;
  - `top_tags`: `[{key, values: [{value, count}]}]` over the last 14 days,
    the 5 most frequent values for up to 20 keys, with `release` and
    `environment` included as keys;
  - `latest_event`: `{event_id, dt, event: NormalizedEvent, older_event_id, newer_event_id}`.
- Status body (`deny_unknown_fields`, tagged by `action`):
  `{"action": "resolve"}`, `{"action": "unresolve"}`, `{"action": "ignore"}`,
  `{"action": "ignore", "until_count": n}`, or
  `{"action": "ignore", "until": "<rfc3339>"}`. Both `until_count` and
  `until` at once is a 400.

**API:**

| Method and path | Min role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/errors/issues?...` | viewer | `200` list | 400 query, 403, 404 foreign `app_id` |
| `GET /projects/{id}/errors/issues/{issue_id}` | viewer | `200 IssueDetail` | 403, 404 |
| `GET /projects/{id}/errors/issues/{issue_id}/events/{event_id}` | viewer | `200 {event_id, dt, event, older_event_id, newer_event_id}` | 400 id format, 403, 404 |
| `POST /projects/{id}/errors/issues/{issue_id}/status` | member | `200 IssueDetail` | 400 body or range, 403, 404 |

**Behaviour:**
1. Every handler checks the role first, then loads the issue with
   `where id = $1 and project_id = $2` (404 otherwise). Every ClickHouse
   read binds that project id and issue id.
2. The list follows R-ERR17:
   - Postgres candidates, at most `MAX_LIST_CANDIDATES`;
   - one ClickHouse aggregate over `issue_id IN {ids:Array(UUID)}` with
     `dt` in the range and the optional filters;
   - sort and page;
   - one hourly sparkline query for the page's ids.

   With an environment, release or level filter, issues without a
   matching event in the range are left out.
3. Reads use `logs::query::search::READ_SETTINGS` (`max_result_bytes`, time
   cap) and `logs::query::search::QuerySlots` (per-user concurrency), as
   log searches do.
4. Event navigation orders by `(dt, event_id)`. `older_event_id` is the
   next row before it, `newer_event_id` the next row after it, and either
   is `null` at the end. An `event_id` that is not 32 hex is a 400.
5. A status change runs `on_command` with
   `latest_release = last_release` and `event_count` = the stored count
   plus pending. It writes the columns in one guarded update and records
   `resolved_by` / `resolved_at` for resolve. It returns the effect for
   Part 2 (unused here).

**Tests** (`tests/errors/issue_api.rs`):
- `a_viewer_lists_and_reads_issues_but_cannot_change_status`
- `a_member_resolves_ignores_and_unresolves_an_issue`
- `resolving_records_the_issues_latest_release`
- `ignore_until_count_and_until_date_are_validated_at_their_bounds`
- `ignore_with_both_until_count_and_until_is_a_400`
- `the_default_list_shows_unresolved_issues_of_the_last_14_days`
- `filters_by_app_environment_release_level_and_title`
- `sorting_by_events_and_users_uses_range_counts`
- `the_list_reports_truncated_past_max_list_candidates`
- `page_size_past_max_page_size_is_a_400`
- `a_range_over_90_days_is_a_400`
- `affected_users_count_distinct_user_keys`
- `top_tags_include_release_and_environment`
- `event_navigation_walks_older_and_newer`
- `an_issue_of_another_project_is_a_404`
- `an_event_of_another_issue_is_a_404`
- `an_app_id_of_another_project_in_the_filter_is_a_404`
- `filter_values_with_sql_characters_are_bound_not_interpolated`

**Acceptance:** `cargo test --test errors issue_api::` passes.

### Task 8 (NAMI-ERR-8): Add the Errors section and the applications pages (UI)

**Covers:** R8.10 (applications)  **Depends on:** NAMI-ERR-1 (API), runs after NAMI-ERR-7 on the branch

**Files:**
- Create:
  - `nami-ui/src/lib/errors/errors.ts` (types mirroring the engine JSON,
    plus `setupSnippet(platform, dsn)`) and `nami-ui/src/lib/errors/errors.test.ts`;
  - `nami-ui/src/lib/server/errors/app-form.ts` and
    `nami-ui/src/lib/server/errors/app-form.test.ts`;
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/+layout.svelte`
    (sub-tabs: Issues, Applications);
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/apps/+page.server.ts`,
    `+page.svelte` and `apps-page.test.ts`;
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/apps/[appId=uuid]/+page.server.ts`,
    `+page.svelte` and `app-page.test.ts`.
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` (an
  "Errors" tab to `/projects/{id}/errors`).

**Behaviour:**
1. The list shows a `Card` per app (name, platform `Badge`, DSN with
   `CopyButton`, today's accepted and limited counts). An `EmptyState`
   links to the create form.
2. Create form (member): name, platform `Select`, allowed origins (one
   per line), retention days, rate limit per minute, daily cap,
   "store user IP" checkbox. Bounds are mirrored for messages. The
   engine's 400 text is shown in an `.alert-danger`.
3. App page:
   - the DSN with `CopyButton`;
   - setup snippets in `SegmentedTabs` (button mode) for JavaScript,
     Python, Go, Ruby, Java, .NET and PHP, built by `setupSnippet` with the
     DSN inserted as text;
   - the settings form;
   - "Rotate key", as a confirm step that warns the old DSN stops working;
   - "Delete" (admin), as a two-step delete with `confirm=yes`.
4. Viewers see the DSN and snippets with the forms disabled. Every engine
   call goes through `callEngine` with `enginePath`, and the `appId`
   param uses the `uuid` matcher.

**Tests** (vitest):
- `errors.test.ts`: `setupSnippet` gives the DSN for each of the seven
  platforms, and a DSN with quotes is escaped for the language.
- `app-form.test.ts`: parsing, bounds, origins split per line, unknown
  platform refused.
- `apps-page.test.ts`: a viewer sees no create form, and a failed create
  shows the engine's message.
- `app-page.test.ts`: rotate posts to `rotate-key`, delete needs
  `confirm=yes`, a member sees no delete button.

**Acceptance:** `npm run check` (0 errors, 0 warnings) and
`npx vitest run` pass. The pages use only design tokens and `ui/`
components.

### Task 9 (NAMI-ERR-9): Build the issue list page (UI)

**Covers:** R8.10 (issue list)  **Depends on:** NAMI-ERR-7, NAMI-ERR-8

**Files:**
- Create: `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/+page.server.ts`,
  `+page.svelte` and `issues-page.test.ts`.
- Modify: `nami-ui/src/lib/errors/errors.ts` (`parseIssueQuery`,
  `issueQueryParams`, which mirror the engine's defaults and bounds and
  drop anything invalid, like `parseExplorerState`).

**Behaviour:**
1. Filters live in the URL: app, status (`SegmentedTabs`: Unresolved,
   Resolved, Ignored, All), environment, release, level, time range, title
   search. Sort is last seen, first seen, events or users.
2. Each row shows the level `Badge`, title (a link to the detail page),
   culprit, app, events and users in the range, a `Sparkline` of the 24
   hourly counts, first and last seen in UTC, and "regressed" when
   `regressed_at` is set.
3. Pagination has previous and next links. A `truncated` reply shows a
   note that only the newest 1000 matching issues are listed.
4. Titles and culprits are text interpolations, so `<script>` shows as
   text.

**Tests** (vitest): `parseIssueQuery` round-trips and drops invalid values;
the loader passes the query to the engine path and 400s become a message;
a server render with a title of `<img src=x onerror=alert(1)>` contains the
escaped text and no `<img` element.

**Acceptance:** `npm run check` and `npx vitest run` pass.

### Task 10 (NAMI-ERR-10): Build the issue detail page with stack trace and breadcrumbs (UI)

**Covers:** R8.10 (issue detail: stack trace, breadcrumbs)  **Depends on:** NAMI-ERR-9

**Files:**
- Create:
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/issues/[issueId=uuid]/+page.server.ts`,
    `+page.svelte` and `issue-page.test.ts`;
  - `nami-ui/src/lib/components/StackTrace.svelte`,
    `nami-ui/src/lib/components/EventDetail.svelte` and
    `nami-ui/src/lib/components/event-detail.test.ts`.
- Modify: `nami-ui/src/lib/errors/errors.ts` (`frameGroups(frames)`, which
  collapses runs of non-in-app frames; `isEventId`).

**Behaviour:**
1. Header: title, culprit, level, status, and the status actions for
   members. These are resolve, unresolve and ignore, with ignore offering
   forever, "until N more events" (number input,
   1..`MAX_IGNORE_COUNT`) and "until date" (`<input type="datetime-local">`,
   sent in UTC).
2. `StatStrip`: events (lifetime), users, first seen with its first
   release, last seen with its last release.
3. Top tag values: a `Card` per key with its values and counts.
4. The event comes from `?event=<id>` (checked with `isEventId`), else the
   latest. It shows older and newer links.
5. `EventDetail` shows:
   - the exception chain, the outermost exception first;
   - frames most recent first, in-app frames highlighted, runs of
     library frames collapsed into a native `<details>` ("N library
     frames");
   - context lines with line numbers, the error line marked;
   - breadcrumbs (time UTC, category, level, message, data as formatted
     JSON text);
   - contexts, user, request (url, method, query, headers) and SDK.
6. "View trace" links to the Traces trace page
   (`/projects/{id}/traces/{trace_id}`) when `trace_id` is set. "View logs"
   links to the logs explorer filtered by `trace_id` over the event time
   +/- 5 min. That uses the explorer's `trace_id` filter, which the Traces
   phase (roadmap §7) adds. If that filter has not shipped,
   this task adds a `trace_id` URL parameter to `parseExplorerState` and
   `explorerParams`.
7. All event content is text interpolation. No `{@html}` anywhere in the
   errors pages.

**Tests** (vitest):
- `frameGroups` keeps in-app frames and collapses library runs;
- the loader rejects a bad `event` parameter by falling back to the latest
  event;
- a server render of `EventDetail` with `<script>` in an exception value, a
  breadcrumb message, a tag value and a context line shows escaped text and
  no `<script>` element;
- status actions post the right `action` bodies;
- a viewer sees no actions.

**Acceptance:** `npm run check` and `npx vitest run` pass. A manual check
against a local engine with a sentry-javascript and a sentry-python event
shows the trace, breadcrumbs and request.

### Task 11 (NAMI-ERR-11): Run the Part 1 security review and fix wave

**Covers:** the phase security review (no roadmap item of its own)  **Depends on:** NAMI-ERR-1..10

**Files:** fixes on the branch as found, plus
`docs/superpowers/decisions/<date>-errors-part1.md` (the rulings above as
applied, the limits, and what is deferred) and the roadmap ticks for the
items this phase finishes (R8.01, R8.02, R8.04, R8.06, R8.09; R8.03 and
R8.10 stay unticked until Part 2).

**Behaviour:** a review of the whole phase diff against the spec's
"Security requirements (tested)", with one test each where it is missing:
1. DSN bounds: the 401 cases, the 403 origin, the 429 headers.
2. Payload limits: every bound in NAMI-ERR-2, bombs for all three codings,
   and the fuzz corpus run with a new seed.
3. Scrubbing and IP: no IP or forwarding header in any stored payload
   without `store_user_ip`. The user key hash is keyed.
4. Isolation: apps, issues and events of another project are 404 on
   every route.
5. Escaping: the server-render tests of NAMI-ERR-9 and NAMI-ERR-10.
6. Bounded resources: memory per request (the decode budget, one event
   per envelope), connection and place limits, no blocking work on the
   runtime, no lock across `.await`.
7. Logging: no key, payload or user data in `tracing` fields.

A fix wave follows on the same branch, then a re-review until clean.

**Acceptance:** every finding is fixed or recorded as deferred in the
decisions doc. `cargo test`, `cargo clippy --all-targets`, `npm run check`
and `npx vitest run` pass. The phase PR lists the local runs (no CI yet)
and is auto-merged once the review is clean.
