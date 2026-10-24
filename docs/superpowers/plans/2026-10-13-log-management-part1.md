# Log Management — Part 1 Implementation Plan

> **Status:** done. Historical record of a merged phase. Paths, module names and migration numbers below predate the 2026-10-19 restructure; translate them with `docs/superpowers/decisions/2026-10-19-domain-restructure.md`. Do not execute this plan again.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Nami BetterStack-style log management, part 1: per-project log sources with ingest tokens, HTTP ingestion (JSON, NDJSON, MessagePack, gzip) and RFC 5424 syslog ingestion into ClickHouse, per-source rate limits, usage stats and daily quotas, structured search with a volume histogram, live tail over Server-Sent Events, and the nami-ui sources pages and explorer.

**Architecture:** Postgres holds `sources` (token as SHA-256 for lookup plus an AES-GCM copy for reveal, via the existing `crypto::Cipher`) and `source_usage_daily`; ClickHouse holds one `logs` table with the spec's columns from the start (`trace_id`, `span_id`, `pattern`, materialized `pattern_hash`). Every ingest path builds a `logs::normalize::LogEvent` (field mapping, truncation caps), then `logs::store::build_row` adds the message pattern and inserts with waited async inserts; part 2's pipeline rules slot in between those two calls. Search, histogram and tail share one parameter parser and one SQL builder (`logs::query::Sql`) that only appends fixed fragments and numbered placeholders, so no user input ever reaches SQL text. nami-ui proxies the SSE stream and "load more" through allow-listing `+server.ts` endpoints; all explorer state lives in the URL.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13), new crates `flate2 1.1.10`, `http-body-util 0.1.5`, `form_urlencoded 1.2.2`, `tokio-stream 0.1.19`, `regex 1.13.1` (already in the tree, now direct), `uuid` feature `v7` (dev: `rmp-serde 1.3.1`), Postgres 17, ClickHouse 26.9, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-log-management-design.md`, including its "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §3.

**When to start:** only after every Uptime Phase 2 task (NAMI-UP-9 .. NAMI-UP-19) is merged to `main`. This plan was written against the complete Phase 2 end state (Phase 1 + Phase 2 as verified on 2026-10-13); review fixes landing in the meantime may shift a line, a name or a test count. Implementers adapt such drift minimally (same behaviour, same tests) and note it in the PR; they never redesign around it.

**Part 2 follows** (a separate plan, NAMI-LOG-9 onward): ingest pipeline rules (drop, remove fields, redact, rename, sample) with the pipeline editor, the Patterns tab and `/logs/patterns`, saved views, CSV/NDJSON export, log alerts, and the usage chart page. Part 1 leaves room for them: the pipeline goes between `normalize::from_json`/`SyslogMessage::into_event` and `store::build_row`; `source_usage_daily` already has the `events_dropped` / `events_sampled_out` columns (zero until then); `sources.updated_at` is bumped on every change (the pipeline cache key); the explorer reserves the URL keys of its future tabs. Postgres migrations 0040-0059 are reserved for Logs: part 1 uses 0040 and 0041, part 2 continues at 0042.

**Verification status (2026-10-13):** every task below was applied, in order, to a scratch copy of the Phase 2 end state, one commit per task. After each engine task the full engine suite was green against Postgres 17, ClickHouse 26.9 and Mailpit (334 tests after Task 6; Phase 2 alone had 216), and the new code was clippy-clean. After each UI task `npm run check` reported 0 errors / 0 warnings and vitest was green (88 tests after Task 8; Phase 2 alone had 63). A live walkthrough (engine with `SYSLOG_ENABLED=true` + nami-ui dev server + browser) created, revealed, rotated, edited and deleted sources as admin and as a viewer, ingested over HTTP and syslog, and used every explorer control including live tail with a late-arriving row. Code blocks below are generated from those commits. Still run every step — the scratch copy is not the repo.

**Execution:** per CLAUDE.md — one worktree per task under `.worktrees/NAMI-LOG-<n>/`, branch from the latest `origin/main`, the branch named under each task heading, Conventional Commits, task review, PR, and auto-merge on a clean review (pre-authorized for sub-projects 2-11). Tasks are sequential: each consumes the interfaces of the ones before it. How changes are written down: new files are given in full; a modified file is given as a unified diff against the previous task's state (apply it with `git apply` from the repo root, or by hand), except files a task mostly rewrites, which are given in full. `Cargo.toml`/`Cargo.lock` changes come from the `cargo add` commands shown and are committed with the task. Engine tests need `docker compose up -d postgres clickhouse mailpit`; building with `CARGO_PROFILE_DEV_DEBUG=0` keeps `target/` several GB smaller.

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**, ClickHouse 8123, Mailpit 1025/8025), never native installs. Engine tests run with `DATABASE_URL=postgres://nami:nami@localhost:5433/nami`.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`), no compile-time macros, no ORM; Postgres SQL strings are literals.
- ClickHouse: every value is a bound query parameter. SQL text is assembled only from fixed fragments with numbered placeholders (spec: "the SQL text is assembled only from fixed fragments with numbered placeholders") — in code, only through `logs::query::Sql`, whose `push` takes `&'static str`. The project id is always bound from the path.
- axum 0.8 path syntax `{param}`. Authorization failure is 403, never 404; an id of another project inside an accessible project path is 404; a `source_id` filter of another project matches nothing.
- RBAC (spec "Sources"): viewers read sources but never their token; members create, edit, reveal and rotate; admins delete. Search, histogram and live tail: viewer and up. Ingestion: the source token only, no session.
- Sources (spec): `platform` one of `http`, `syslog`, `vector`, `docker`, `other`; `retention_days` 1-365, default 30; `daily_byte_quota` >= 1 MiB or null (unlimited). Token: 32 random bytes, base64url; stored as SHA-256 (lookup) + AES-GCM ciphertext; rotate = new token, the old one stops working immediately.
- HTTP ingestion (spec): `POST /api/v1/logs`, `Authorization: Bearer <source token>`; JSON object / JSON array / NDJSON / MessagePack (`application/msgpack`, `application/x-msgpack`), optional `Content-Encoding: gzip`; 10 MiB compressed body, 50 MiB decompressed (gzip-bomb guard) → 413; more than 10,000 events → 413; message truncated at 64 KiB with a `…[truncated]` marker; `fields` capped at 256 KiB per event; 50 requests/s, burst 100 per source → 429 in the service's JSON error shape; inserts with `async_insert=1, wait_for_async_insert=1`; response `202 {"accepted": N}`. MessagePack: `bin` → base64 string, timestamp extension (type -1) → RFC 3339 string, other extensions → `null`, a non-string map key → 400, declared lengths never preallocated.
- Field mapping (spec): timestamp from `dt` | `timestamp` | `time` (RFC 3339, or unix seconds / milliseconds) else receive time; message from `message` | `msg` | `log`; level from `level` | `severity` | `lvl`, normalized lowercase (unknown → `info`); `trace_id` / `span_id` when 32 / 16 hex chars and not all zeros (stored lowercase, otherwise the key stays in `fields`); every other key into `fields`.
- Patterns (spec): the first 1,024 chars of `message`, replacing in order quoted strings → `<str>`, UUIDs → `<uuid>`, IPv4/IPv6 → `<ip>`, `0x…` and standalone hex runs >= 8 chars containing a digit and a letter → `<hex>`, standalone integers/decimals (optional sign) → `<num>`; `pattern_hash = cityHash64(pattern)` materialized.
- Syslog (spec): RFC 5424 over TCP (newline or octet-counting framing) and UDP, default port 5140, `SYSLOG_ENABLED`; auth `[nami token="<source token>"]`; frames without a valid token dropped and counted; severity → level; `HOSTNAME`/`APP-NAME`/`PROCID`/`MSGID` and other SD params → `fields`; max frame 64 KiB; TCP connections idle > 5 min closed; malformed frames never crash the listener.
- Search (spec): window required (default last hour), at most 31 days → 400; `source_id`, `level`, `pattern_hash` repeatable; `q` case-insensitive substring over `message` (and `fields` with `in_fields=true`); `filter` JSON `{"path","op","value"}`, ops `eq|neq|contains|exists|regex|not_regex|gt|gte|lt|lte`, at most 10, path segments `^[A-Za-z0-9_-]{1,64}$`, depth <= 5; regex via ClickHouse `match` (RE2), <= 256 chars, invalid → 400, `not_regex` also matches a missing field; numeric ops need a finite number (else 400), a missing or non-numeric field never matches; `trace_id`/`span_id` exact (32/16 hex, else 400); `limit` default 100, max 500; opaque `(dt, id)` cursor, newest first. Histogram: same filters, about 60 buckets.
- Live tail (spec): `GET /projects/{id}/logs/tail`, SSE `event: log`, poll every 1 s, comment keep-alive every 15 s, session re-check every 60 s (end when invalid), close after 30 min; nami-ui proxies it through a `+server.ts` without buffering.
- Usage and quota (spec): per source and UTC day, `events` and `bytes` (UTF-8 length of the stored `message` + `fields`) plus rejection counters, counted in memory, flushed every 10 s (additive upsert), kept 400 days; over `daily_byte_quota` (flushed + unflushed bytes) → 429 with error code `quota_exceeded` and `Retry-After` = seconds to the next UTC midnight; syslog frames dropped; every rejection counted; the request that crosses the quota is accepted whole.
- Security (spec "Security requirements"): missing/invalid/rotated/deleted-source token → 401, tokens never logged; injection attempts in `q`, filter values and paths are matched literally or rejected; oversized body, gzip bomb, too many events → 413; syslog fuzz never stops the listener; the SSE endpoint requires a session and stops when it is invalidated; MessagePack fuzz → 4xx, no panic, no large allocation.
- nami-ui: every engine call goes through `callEngine`; SvelteKit 3 conventions (`#lib` alias, `process.env` in server code, runes, `page` from `$app/state`, `goto(..., { reset: false })`); ids from params or forms are checked with `isUuid` before they go into an engine path; modules under `#lib/server` are never imported by components; initial input values through `defaultValue`/`defaultChecked`; dark theme classes copied from the existing pages; times shown in UTC; tokens never in a URL.
- Tests: test binaries run one after another (`cargo test`), never a cross-binary parallel runner; ClickHouse tests use their own `nami_test_*` database and drop it.
- Commits: Conventional Commits whose message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- A row that arrives late — its `dt` older than rows the live tail already showed, as with a batching shipper (Vector, Fluent Bit) next to a direct `curl` source — must still appear in live tail. A strict "newer than the last `(dt, id)` sent" cursor drops it silently → Task 6 (`tail_streams_new_matching_rows_including_late_ones`).
- Shippers that send NDJSON (or several JSON documents) with `Content-Type: application/json` must be accepted, not answered with a JSON parse error that the shipper retries forever → Task 3 (`json_objects_arrays_and_ndjson_are_stored`, "Shippers that send NDJSON as application/json are accepted too").
- Paging through rows that share one timestamp (shippers that stamp seconds, or a batch with one `dt`) must return every row exactly once — no duplicate and no gap at a page boundary → Task 5 (`pagination_returns_every_row_exactly_once`).
- A backfill whose timestamps span more than 100 days must be stored, not fail the whole request on ClickHouse's 100-partitions-per-insert limit → Task 2 (`a_batch_spanning_more_than_100_daily_partitions_is_accepted`).
- An event whose timestamp is unusable — microseconds or nanoseconds, decades off, or not a time at all — must neither fail the batch (a `DateTime64` overflow) nor vanish into a partition no one searches: it is stored at receive time and the original value stays in `fields` → Task 2 (`missing_unparsable_or_out_of_range_timestamps_fall_back_to_the_receive_time`).

## Decisions the spec leaves open

- **Names:** Postgres table `sources` (the spec's `sources.pipeline`/`sources.updated_at`), migrations `0040_create_sources.sql` and `0041_create_source_usage_daily.sql`; engine modules under `src/logs/`; routes `routes/sources.rs`, `routes/logs_ingest.rs`, `routes/logs.rs`.
- **Token reveal:** create and rotate return the token once (`SourceWithToken`); `GET /projects/{id}/sources/{source_id}/token` (member+) returns `{"token"}`; viewers get 403. A token sealed under another key (NAMI_ENCRYPTION_KEY changed) is a 409 with "rotate it" — ingestion keeps working, it only needs the hash. The UI shows a token only after a click (a form action result), never in the HTML before that and never in a URL.
- **Token lookup is not cached:** every HTTP request looks its token hash up in Postgres (indexed, unique), so a rotation or deletion takes effect at once on every engine instance. Syslog resolves all tokens of a batch in one `token_hash = any($1)` query.
- **Body formats:** JSON content types (`application/json`, `text/json`, `text/plain`, `application/x-www-form-urlencoded` as sent by `curl -d`, the NDJSON/JSON Lines types, or none) all use one parser: a whitespace-separated sequence of objects or arrays of objects, which covers a single object, an array, NDJSON and concatenated JSON. MessagePack types (`application/msgpack`, `application/x-msgpack`, `application/vnd.msgpack`) use a hand-written decoder (one map or an array of maps, no trailing bytes). Any other `Content-Type`, or a `Content-Encoding` other than `gzip`/`x-gzip`/`identity` → 415. A blank body → 400; `[]` → 202 `{"accepted": 0}`.
- **Nesting limit:** serde_json's built-in limit — at most 127 nested arrays/objects counting the outermost, so a value sits at most 128 levels deep; the 128th nested container → 400. MessagePack applies the identical count.
- **Order of checks** on `POST /api/v1/logs`: token (401) → rate limit (429 `rate_limited`, `Retry-After: 1`) → quota (429 `quota_exceeded`, `Retry-After` to UTC midnight) → content type (415) → body size (413) → decompress/parse (413/400) → store → 202. Nothing is read from an unauthenticated request. Both 429 bodies carry `{"error": "...", "code": "..."}`.
- **Timestamps:** usable only within [receive time − 365 days, receive time + 1 day] (no source keeps rows longer, and a stray timestamp would create partitions TTL drops at once); otherwise the receive time is used and the original key stays in `fields`. Numbers below 10^11 are unix seconds (fractions allowed), larger ones milliseconds; `dt` is stored at millisecond precision. ClickHouse also drops rows older than their source's retention on insert.
- **Levels:** stored levels are `trace, debug, info, notice, warn, error, critical, alert, emergency, fatal`. Aliases (`warning`, `err`, `crit`, `emerg`, `panic`, `dbg`, `trc`, `information`) are folded in; numbers map syslog severities 0-7 and pino/bunyan levels 10/20/30/40/50/60; anything else is `info`. The first present of `level`/`severity`/`lvl` is consumed; later ones stay in `fields`. Search's `level` parameter accepts only stored levels (400 otherwise).
- **Messages and fields:** a non-string message is stored as its JSON text, `null`/absent as empty. Over 256 KiB, `fields` drops its largest top-level keys first and gains `"_nami_truncated": "<n> field(s) dropped: event fields are capped at 256 KiB"`.
- **Patterns:** a single quote opens a quoted string only after a non-alphanumeric character and closes only before one, so "can't" and "user's" stay; IPv6 is replaced only when the run parses as an address and contains a digit (so `std::io` and `a::b` stay); `256.1.1.1` becomes `<num>.<num>` (decimals).
- **Usage:** counted per instance in memory and flushed every 10 s with one `UNNEST` upsert per flush; counters of deleted sources are dropped; a failed flush keeps every counter for the next one (counters of the last ≤ 10 s are lost on a crash). The quota check caches the flushed bytes for at most 10 s per source. Pruning keeps the last 400 days including today (hourly). `GET /projects/{id}/sources/{source_id}/usage?days=N` (viewer, 1-400, default 30, zero-filled, oldest first) ships in part 1 because the stats need a read path; the chart page is part 2.
- **Rate limiter:** a hand-written token bucket per source id (`logs::rate_limit::TokenBuckets`, testable with an injected clock), local to the instance; syslog is not rate limited (it is bounded by its queue and quota).
- **Syslog:** off by default (`SYSLOG_ENABLED=false`), ports `SYSLOG_TCP_PORT`/`SYSLOG_UDP_PORT` (default 5140), bound on all interfaces. Only RFC 5424 is accepted (BSD/RFC 3164 frames count as malformed); the header is strict (printable ASCII, RFC field lengths, RFC 3339 or `-` timestamp). SD elements other than `nami` become `fields.<SD-ID>` objects (a repeated param becomes an array); `facility`, `hostname`, `app_name`, `procid`, `msgid` are added as fields and win over a colliding SD-ID; the `nami` element is never stored. A TCP connection must complete a frame every 5 min (slow-loris safe); a framing error (bad or oversized octet count, a line over 64 KiB, a truncated frame) closes the connection. At most 1,024 TCP connections; accepted frames queue (10,000) to a batcher that stores up to 1,000 rows or every 1 s; a full queue drops and counts. Drops log one warning per 10 s at most, never with frame content.
- **Search details:** unknown query keys are ignored; list parameters take at most 100 values; `q` (<= 1,024 chars) is a literal substring (`positionCaseInsensitiveUTF8`, no wildcards). `eq`/`neq` compare strings unquoted and other JSON values by their JSON text; `contains` is case-insensitive; `neq` and `not_regex` also match a missing field; numeric ops accept JSON numbers and numeric strings. Regexes are syntax-checked with the `regex` crate; one RE2 still rejects maps ClickHouse's `CANNOT_COMPILE_REGEXP` to 400, and a query over its 30 s `max_execution_time` is a 400 telling the user to narrow it. `trace_id`, `span_id` and `pattern_hash` filters ship now (the columns exist). The window is inclusive at both ends. The response is `{"logs": [...], "next_cursor": "..." | null}` (one extra row is fetched to know whether a next page exists); `pattern_hash` is a decimal string (a UInt64 does not fit a JS number).
- **Histogram:** the smallest step from 1 s … 1 day (1, 2, 5, 10, 15, 30 s, 1, 2, 5, 10, 15, 30 min, 1, 2, 3, 6, 12 h, 1 d) giving at most 60 buckets; every bucket is returned (zeros included) with `count`, `errors` (error and above) and `warnings`.
- **Live tail (refines the spec's cursor):** each poll reads the newest rows (at most 1,000) with `dt` in the last 60 s and sends those not sent yet, oldest first; rows present when the tail starts are remembered, not sent. This keeps the spec's poll/keep-alive/re-check/30-minute behaviour while delivering late rows (see Review Focus); rows more than 60 s late are not tailed. The session re-check reloads the session from the store (`Session::load`) and re-runs the same check as the `CurrentUser` extractor (extracted as `rbac::user_from_session`) plus the project role, so logout, a password change or removal from the project all end the stream with `event: end` `{"reason":"session"}`; the 30-minute end sends `{"reason":"timeout"}` and EventSource reconnects. Responses carry `X-Accel-Buffering: no`.
- **BFF:** the tail and "load more" proxies forward only allow-listed query keys (`pickLogParams`), check the project id with `isUuid`, and abort the engine request when the browser disconnects (`signal`). `PUBLIC_SYSLOG_ADDRESS` ("host:port", default the public engine host on 5140) feeds the syslog snippet.
- **UI:** a "Logs" project tab with Explorer and Sources sub-tabs. Setup snippets: curl, Vector, rsyslog (RFC 5424 template with the token in structured data) and Docker (a Vector container reading the Docker socket — Docker's own syslog driver cannot add the token element). The explorer URL keys are `range` (`15m|1h|6h|24h|7d|custom`, default `1h`), `from`/`to` (custom only), `q`, `in_fields=1`, `level`*, `source`*, `filter`* (JSON), `col`* (at most 10 field paths), `live=1`; invalid values are dropped. A relative preset becomes an absolute window once per load, so "load more" pages through the same window. Live rows are prepended and capped at 1,000. The volume chart uses the dataviz reference status steps (critical `#d03b3b`, warning `#fab219`) and neutral `#737373` for other levels; a click on a bar zooms into its bucket.

## File Structure

```
nami-engine/
  Cargo.toml (+ Cargo.lock)          T2 regex, uuid/v7; T3 flate2, http-body-util, dev rmp-serde; T5 form_urlencoded;
                                     T6 tokio-stream
  .env.example                       T4 SYSLOG_ENABLED, SYSLOG_TCP_PORT, SYSLOG_UDP_PORT
  migrations/
    0040_create_sources.sql          T1 sources (token_hash unique, token_enc)
    0041_create_source_usage_daily.sql T3 per source and UTC day counters
  src/
    clickhouse.rs                    T2 logs DDL; insert_rows_with / query_rows_with (extra settings)
    config.rs                        T4 syslog_enabled, syslog_tcp_port, syslog_udp_port
    state.rs, main.rs                T3 AppState.logs (+ usage flusher); T4 syslog listeners
    lib.rs                           T2 mod logs
    router.rs, routes/mod.rs         T1, T3, T5 merge the new routers
    middleware/rbac.rs               T6 user_from_session (extracted from the CurrentUser extractor)
    models/source.rs (+ mod.rs)      T1 Source, tokens, CRUD, find_by_token; T4 find_by_token_hashes
    routes/sources.rs                T1 CRUD, token, rotate; T3 usage
    routes/logs_ingest.rs            T3 POST /api/v1/logs
    routes/logs.rs                   T5 search, histogram; T6 tail
    logs/
      mod.rs                         T2..T6 module list; T3 LogsState (T6 adds tail timings)
      normalize.rs                   T2 LogEvent, field mapping, levels, caps
      pattern.rs                     T2 message patterns
      store.rs                       T2 LogRow, build_row, insert_logs
      msgpack.rs                     T3 MessagePack decoder
      ingest.rs                      T3 IngestError, content negotiation, limits, gzip, JSON events
      rate_limit.rs                  T3 per-source token buckets
      usage.rs                       T3 UsageTracker, flush, prune, daily usage
      syslog/mod.rs, parse.rs, framing.rs  T4 listeners, batcher, RFC 5424 parser, TCP framing
      query.rs                       T5 parameters, filters, Sql builder
      search.rs                      T5 search, histogram
      tail.rs                        T6 live tail loop
  tests/
    common/mod.rs, common/logs.rs    T3 log helpers (seed_source, post_logs, stored_rows, ...)
    sources_api.rs                   T1
    logs_store.rs, clickhouse.rs     T2 (clickhouse.rs: table lists now include logs)
    logs_ingest.rs, logs_quota.rs    T3
    logs_syslog.rs                   T4
    logs_search.rs                   T5
    logs_tail.rs                     T6
    migrations.rs                    T1, T3 new tables

nami-ui/src/
  lib/logs.ts (+ .test.ts)           T7 sources types, formatting, setup snippets; T8 explorer URL state and row helpers
  lib/server/log-proxy.ts (+ .test)  T6 allow-listed query forwarding
  lib/server/source-form.ts (+ .test) T7 source form → engine body
  lib/server/engine.ts (+ .test)     T7 publicSyslogAddress
  lib/components/LogVolumeChart.svelte T8 stacked volume histogram
  routes/(app)/projects/[id]/
    +layout.svelte                   T7 Logs tab
    logs/+layout.svelte              T7 sub-tabs (T8 adds Explorer)
    logs/+page.server.ts             T7 redirect to sources; T8 explorer load
    logs/+page.svelte                T8 explorer
    logs/tail/+server.ts (+ test)    T6 SSE proxy
    logs/search/+server.ts (+ test)  T8 "load more" proxy
    logs/sources/+page.*             T7 list + create
    logs/sources/[sourceId]/+page.*  T7 detail, token, snippets, settings, delete
```

---

### Task 1 (NAMI-LOG-1): Log sources with hashed and sealed ingest tokens

Branch `feat/NAMI-LOG-1-log-sources`, worktree `.worktrees/NAMI-LOG-1/`.

**Files:**
- Create: `nami-engine/migrations/0040_create_sources.sql`, `nami-engine/src/models/source.rs`, `nami-engine/src/routes/sources.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/sources_api.rs` (+ unit tests in `src/models/source.rs`)

**Interfaces:**
- Consumes: `crypto::Cipher::{encrypt(&self, context: &str, plaintext: &[u8]) -> Vec<u8>, decrypt(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, DecryptError>}` and `AppState.cipher` (Phase 2); `auth::token::hash_token(raw: &str) -> String` (SHA-256 hex); `models::{nullable, validate_name}`; `middleware::rbac::{require_project_role, CurrentUser}`; test helpers `common::{call, seed_project, test_app, test_cipher, user_with_role}`.
- Produces (`nami_engine::models::source`):
  - consts `PLATFORMS: [&str; 5]`, `DEFAULT_PLATFORM = "http"`, `DEFAULT_RETENTION_DAYS: i32 = 30`, `MAX_RETENTION_DAYS: i32 = 365`, `MIN_DAILY_BYTE_QUOTA: i64 = 1_048_576`, `TOKEN_CONTEXT = "sources.token"`.
  - `Source { id: Uuid, project_id: Uuid, name: String, platform: String, retention_days: i32, daily_byte_quota: Option<i64>, created_at: DateTime<Utc>, updated_at: DateTime<Utc> }` (`FromRow`, `Serialize`; never carries token columns).
  - `SourceWithToken { #[serde(flatten)] source: Source, token: String }`.
  - `IngestSource { id: Uuid, project_id: Uuid, retention_days: i32, daily_byte_quota: Option<i64> }` (`FromRow`, `Clone`, `PartialEq`).
  - `SourceConfig { name, platform, retention_days, daily_byte_quota }`, `CreateSourceRequest::into_config(self) -> SourceConfig`, `PatchSourceRequest::apply_to(self, SourceConfig) -> SourceConfig` (`daily_byte_quota: null` clears), `validate(SourceConfig) -> Result<SourceConfig, String>`.
  - `new_token() -> String`; `insert_source(&PgPool, &Cipher, project_id: Uuid, &SourceConfig) -> Result<SourceWithToken, sqlx::Error>`; `list_for_project(&PgPool, Uuid) -> Result<Vec<Source>, sqlx::Error>`; `find_in_project(&PgPool, project_id, source_id) -> Result<Option<Source>, sqlx::Error>`; `update_in_project(&PgPool, project_id, source_id, PatchSourceRequest) -> Result<Option<Source>, AppError>`; `delete_in_project(&PgPool, project_id, source_id) -> Result<bool, sqlx::Error>`; `reveal_token(&PgPool, &Cipher, project_id, source_id) -> Result<Option<String>, AppError>`; `rotate_token(&PgPool, &Cipher, project_id, source_id) -> Result<Option<SourceWithToken>, sqlx::Error>`; `find_by_token(&PgPool, token: &str) -> Result<Option<IngestSource>, sqlx::Error>`.
  - Routes: `GET|POST /projects/{id}/sources`, `GET|PATCH|DELETE /projects/{id}/sources/{source_id}`, `GET /projects/{id}/sources/{source_id}/token`, `POST /projects/{id}/sources/{source_id}/rotate`.

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/sources_api.rs`:

```rust
// Log sources and their ingest tokens. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_project, test_app, test_cipher, user_with_role};
use nami_engine::{
    auth::token::hash_token,
    models::source::{self, TOKEN_CONTEXT},
};
use serde_json::{json, Value};
use uuid::Uuid;

async fn create(app: &axum::Router, project: Uuid, cookie: &axum::http::HeaderValue, body: Value) -> Value {
    let (status, created) = call(app, "POST", &format!("/projects/{project}/sources"), Some(cookie), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

fn timestamp(value: &Value) -> chrono::DateTime<chrono::Utc> {
    value.as_str().unwrap().parse().unwrap()
}

/// No response may carry the stored token columns, under any name.
fn assert_no_stored_token_columns(body: &Value) {
    let text = body.to_string();
    assert!(!text.contains("token_hash") && !text.contains("token_enc"), "{text}");
}

#[sqlx::test]
async fn members_create_sources_whose_token_is_stored_hashed_and_sealed(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let created = create(&app, project, &member, json!({ "name": "  api  " })).await;
    assert_eq!(created["name"], "api");
    assert_eq!(created["platform"], "http");
    assert_eq!(created["retention_days"], 30);
    assert_eq!(created["daily_byte_quota"], Value::Null);
    assert_no_stored_token_columns(&created);
    let token = created["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 43, "32 random bytes, base64url without padding");
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{token}");

    let (stored_hash, sealed): (String, Vec<u8>) = sqlx::query_as("select token_hash, token_enc from sources where id = $1")
        .bind(Uuid::parse_str(created["id"].as_str().unwrap()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_hash, hash_token(&token));
    assert!(!sealed.windows(token.len()).any(|w| w == token.as_bytes()), "the token is not stored in the clear");
    assert_eq!(test_cipher().decrypt(TOKEN_CONTEXT, &sealed).unwrap(), token.as_bytes());

    let full = create(
        &app,
        project,
        &member,
        json!({ "name": "edge", "platform": "syslog", "retention_days": 7, "daily_byte_quota": 1_048_576 }),
    )
    .await;
    assert_eq!((full["platform"].clone(), full["retention_days"].clone()), (json!("syslog"), json!(7)));
    assert_eq!(full["daily_byte_quota"], 1_048_576);
}

#[sqlx::test]
async fn viewers_read_sources_but_never_their_tokens(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let collection = format!("/projects/{project}/sources");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (status, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, one) = call(&app, "GET", &item, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    for body in [&list, &one] {
        assert!(!body.to_string().contains("token"), "viewers never see a token: {body}");
    }

    assert_eq!(call(&app, "GET", &format!("{item}/token"), Some(&viewer), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "POST", &format!("{item}/rotate"), Some(&viewer), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        call(&app, "PATCH", &item, Some(&viewer), Some(json!({ "name": "x" }))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", &collection, Some(&viewer), Some(json!({ "name": "x" }))).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn members_reveal_and_rotate_and_a_rotated_token_stops_resolving_at_once(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let old_token = created["token"].as_str().unwrap().to_string();
    let source_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let item = format!("/projects/{project}/sources/{source_id}");

    let (status, revealed) = call(&app, "GET", &format!("{item}/token"), Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(revealed, json!({ "token": old_token }));
    assert_eq!(source::find_by_token(&pool, &old_token).await.unwrap().unwrap().id, source_id);

    let (status, rotated) = call(&app, "POST", &format!("{item}/rotate"), Some(&member), None).await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    assert_no_stored_token_columns(&rotated);
    let new_token = rotated["token"].as_str().unwrap().to_string();
    assert_ne!(new_token, old_token);
    assert!(timestamp(&rotated["updated_at"]) > timestamp(&created["updated_at"]));

    assert_eq!(source::find_by_token(&pool, &old_token).await.unwrap(), None, "the old token is dead at once");
    let found = source::find_by_token(&pool, &new_token).await.unwrap().unwrap();
    assert_eq!((found.id, found.project_id, found.retention_days), (source_id, project, 30));
    let (_, revealed) = call(&app, "GET", &format!("{item}/token"), Some(&member), None).await;
    assert_eq!(revealed["token"], new_token.as_str());
}

#[sqlx::test]
async fn admins_delete_sources_and_a_deleted_source_token_resolves_nothing(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let token = created["token"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/sources/{}", created["id"].as_str().unwrap());

    assert_eq!(call(&app, "DELETE", &item, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    let (status, deleted) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!((status, deleted), (StatusCode::OK, json!({ "deleted": true })));
    assert_eq!(call(&app, "GET", &item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "DELETE", &item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(source::find_by_token(&pool, &token).await.unwrap(), None);
}

#[sqlx::test]
async fn outsiders_get_403_everywhere(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let collection = format!("/projects/{project}/sources");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    for (method, uri, body) in [
        ("GET", collection.clone(), None),
        ("POST", collection.clone(), Some(json!({ "name": "x" }))),
        ("GET", item.clone(), None),
        ("PATCH", item.clone(), Some(json!({ "name": "x" }))),
        ("DELETE", item.clone(), None),
        ("GET", format!("{item}/token"), None),
        ("POST", format!("{item}/rotate"), None),
    ] {
        assert_eq!(call(&app, method, &uri, Some(&outsider), body).await.0, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    assert_eq!(call(&app, "GET", &collection, None, None).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn invalid_sources_are_rejected(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (json!({ "name": " " }), "name is required"),
        (json!({ "name": "a\nb" }), "control characters"),
        (json!({ "name": "x", "platform": "fluentd" }), "platform must be one of http, syslog, vector, docker, other"),
        (json!({ "name": "x", "retention_days": 0 }), "retention_days must be between 1 and 365"),
        (json!({ "name": "x", "retention_days": 366 }), "retention_days must be between 1 and 365"),
        (json!({ "name": "x", "daily_byte_quota": 1_048_575 }), "daily_byte_quota must be at least 1048576 bytes"),
    ] {
        let (status, json) =
            call(&app, "POST", &format!("/projects/{project}/sources"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        assert!(json["error"].as_str().unwrap().contains(fragment), "{body}: {json}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from sources").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn patch_merges_fields_and_null_clears_the_quota(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api", "daily_byte_quota": 5_000_000 })).await;
    let item = format!("/projects/{project}/sources/{}", created["id"].as_str().unwrap());

    let (status, patched) =
        call(&app, "PATCH", &item, Some(&member), Some(json!({ "retention_days": 90, "platform": "vector" }))).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!((patched["name"].clone(), patched["retention_days"].clone()), (json!("api"), json!(90)));
    assert_eq!(patched["platform"], "vector");
    assert_eq!(patched["daily_byte_quota"], 5_000_000, "omitted fields are kept");
    assert!(timestamp(&patched["updated_at"]) > timestamp(&created["updated_at"]));

    let (_, cleared) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "daily_byte_quota": null }))).await;
    assert_eq!(cleared["daily_byte_quota"], Value::Null);
    let (status, json) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "retention_days": 0 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
}

/// Security: a source of another project, addressed through a project the
/// caller can manage, is a 404 for every route and nothing changes.
#[sqlx::test]
async fn sources_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let foreign = create(&app, other, &other_member, json!({ "name": "theirs" })).await;
    let item = format!("/projects/{project}/sources/{}", foreign["id"].as_str().unwrap());

    for (method, uri, body) in [
        ("GET", item.clone(), None),
        ("PATCH", item.clone(), Some(json!({ "name": "mine" }))),
        ("GET", format!("{item}/token"), None),
        ("POST", format!("{item}/rotate"), None),
        ("DELETE", item.clone(), None),
    ] {
        assert_eq!(call(&app, method, &uri, Some(&admin), body).await.0, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let token = foreign["token"].as_str().unwrap();
    let found = source::find_by_token(&pool, token).await.unwrap().unwrap();
    assert_eq!(found.project_id, other, "the foreign source and its token are untouched");
    let name: String = sqlx::query_scalar("select name from sources where id = $1")
        .bind(found.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "theirs");
}

#[sqlx::test]
async fn an_undecryptable_token_cannot_be_revealed_but_still_authenticates(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let token = created["token"].as_str().unwrap();
    let source_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    // Sealed under another key, as after a NAMI_ENCRYPTION_KEY change.
    let foreign = nami_engine::crypto::Cipher::new(&[9u8; 32]).encrypt(TOKEN_CONTEXT, token.as_bytes());
    sqlx::query("update sources set token_enc = $2 where id = $1")
        .bind(source_id)
        .bind(foreign)
        .execute(&pool)
        .await
        .unwrap();

    let (status, json) =
        call(&app, "GET", &format!("/projects/{project}/sources/{source_id}/token"), Some(&member), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(json["error"].as_str().unwrap().contains("rotate it"), "{json}");
    assert_eq!(source::find_by_token(&pool, token).await.unwrap().unwrap().id, source_id);
}

#[sqlx::test]
async fn find_by_token_ignores_empty_and_oversized_tokens(pool: sqlx::PgPool) {
    assert_eq!(source::find_by_token(&pool, "").await.unwrap(), None);
    assert_eq!(source::find_by_token(&pool, &"a".repeat(129)).await.unwrap(), None);
}
```

Add the table to the migrations test:

```diff
diff --git a/nami-engine/tests/migrations.rs b/nami-engine/tests/migrations.rs
--- a/nami-engine/tests/migrations.rs
+++ b/nami-engine/tests/migrations.rs
@@ -20,6 +20,7 @@ async fn all_tables_exist(pool: sqlx::PgPool) {
         "heartbeats",
         "maintenance_windows",
         "monitor_groups",
+        "sources",
     ] {
         assert!(names.contains(&expected.to_string()), "missing table {expected}");
     }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && cargo test --test sources_api --test migrations`
Expected: FAIL to compile — `error[E0432]: unresolved import nami_engine::models::source`.

- [ ] **Step 3: Add the migration**

`nami-engine/migrations/0040_create_sources.sql`:

```sql
-- Log sources (Logs sub-project; migrations 0040-0059 are reserved for it).
create table sources (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    platform text not null default 'http'
        check (platform in ('http', 'syslog', 'vector', 'docker', 'other')),
    -- Copied onto every ingested row (ClickHouse TTL): a change affects new
    -- rows only.
    retention_days int not null default 30 check (retention_days between 1 and 365),
    -- null = unlimited.
    daily_byte_quota bigint check (daily_byte_quota >= 1048576),
    -- SHA-256 (hex) of the ingest token: the ingest paths look sources up by
    -- it, so a rotated token stops working the moment this row changes.
    token_hash text not null unique,
    -- The same token sealed with AES-256-GCM (crypto::Cipher, associated
    -- data "sources.token") so members can see it again.
    token_enc bytea not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index sources_project_idx on sources (project_id);
```

- [ ] **Step 4: Implement the model**

`nami-engine/src/models/source.rs`:

```rust
//! Log sources: a project's named ingestion endpoints, each with its own
//! ingest token. A token is 32 random bytes, base64url without padding (43
//! characters), stored twice: as its SHA-256 (hex, `token_hash`), which the
//! ingest paths look sources up by, and sealed with AES-256-GCM
//! (`token_enc`, associated data [`TOKEN_CONTEXT`]) so members can see it
//! again. Neither column is ever selected into [`Source`], so no response
//! can carry them by accident.

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::{nullable, validate_name};
use crate::{auth::token::hash_token, crypto::Cipher, error::AppError};

pub const PLATFORMS: [&str; 5] = ["http", "syslog", "vector", "docker", "other"];
pub const DEFAULT_PLATFORM: &str = "http";
pub const DEFAULT_RETENTION_DAYS: i32 = 30;
pub const MAX_RETENTION_DAYS: i32 = 365;
/// The smallest daily byte quota a source may have (1 MiB).
pub const MIN_DAILY_BYTE_QUOTA: i64 = 1024 * 1024;
/// AES-GCM associated data for `sources.token_enc`.
pub const TOKEN_CONTEXT: &str = "sources.token";
/// Generated tokens are 43 characters; anything longer is never looked up.
const MAX_TOKEN_LEN: usize = 128;

/// A source as every API response shows it: never its token.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct Source {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub platform: String,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A source with its plaintext token: the response to create and rotate,
/// which only members and admins can call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceWithToken {
    #[serde(flatten)]
    pub source: Source,
    pub token: String,
}

/// The slice of a source the ingest paths need, found by token.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct IngestSource {
    pub id: Uuid,
    pub project_id: Uuid,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
}

/// The user-editable fields of a source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub name: String,
    pub platform: String,
    pub retention_days: i32,
    pub daily_byte_quota: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSourceRequest {
    pub name: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: i32,
    #[serde(default)]
    pub daily_byte_quota: Option<i64>,
}

fn default_platform() -> String {
    DEFAULT_PLATFORM.to_string()
}

fn default_retention_days() -> i32 {
    DEFAULT_RETENTION_DAYS
}

impl CreateSourceRequest {
    pub fn into_config(self) -> SourceConfig {
        SourceConfig {
            name: self.name,
            platform: self.platform,
            retention_days: self.retention_days,
            daily_byte_quota: self.daily_byte_quota,
        }
    }
}

/// Omitted fields are kept; `daily_byte_quota: null` removes the quota.
#[derive(Debug, Default, Deserialize)]
pub struct PatchSourceRequest {
    pub name: Option<String>,
    pub platform: Option<String>,
    pub retention_days: Option<i32>,
    #[serde(default, deserialize_with = "nullable")]
    pub daily_byte_quota: Option<Option<i64>>,
}

impl PatchSourceRequest {
    pub fn apply_to(self, base: SourceConfig) -> SourceConfig {
        SourceConfig {
            name: self.name.unwrap_or(base.name),
            platform: self.platform.unwrap_or(base.platform),
            retention_days: self.retention_days.unwrap_or(base.retention_days),
            daily_byte_quota: self.daily_byte_quota.unwrap_or(base.daily_byte_quota),
        }
    }
}

pub fn validate(mut c: SourceConfig) -> Result<SourceConfig, String> {
    c.name = validate_name(&c.name)?;
    if !PLATFORMS.contains(&c.platform.as_str()) {
        return Err(format!("platform must be one of {}", PLATFORMS.join(", ")));
    }
    if !(1..=MAX_RETENTION_DAYS).contains(&c.retention_days) {
        return Err(format!("retention_days must be between 1 and {MAX_RETENTION_DAYS}"));
    }
    if matches!(c.daily_byte_quota, Some(quota) if quota < MIN_DAILY_BYTE_QUOTA) {
        return Err(format!("daily_byte_quota must be at least {MIN_DAILY_BYTE_QUOTA} bytes (1 MiB), or null"));
    }
    Ok(c)
}

/// A fresh ingest token: 32 bytes from the OS RNG, base64url, no padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub async fn insert_source(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    c: &SourceConfig,
) -> Result<SourceWithToken, sqlx::Error> {
    let token = new_token();
    let source: Source = sqlx::query_as(
        "insert into sources (project_id, name, platform, retention_days, daily_byte_quota, token_hash, token_enc)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning *",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.platform)
    .bind(c.retention_days)
    .bind(c.daily_byte_quota)
    .bind(hash_token(&token))
    .bind(cipher.encrypt(TOKEN_CONTEXT, token.as_bytes()))
    .fetch_one(pool)
    .await?;
    Ok(SourceWithToken { source, token })
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Source>, sqlx::Error> {
    sqlx::query_as("select * from sources where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by both ids: another project's source is not found (404).
pub async fn find_in_project(pool: &PgPool, project_id: Uuid, source_id: Uuid) -> Result<Option<Source>, sqlx::Error> {
    sqlx::query_as("select * from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write under the row's lock; bumps `updated_at`.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Uuid,
    patch: PatchSourceRequest,
) -> Result<Option<Source>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Source> = sqlx::query_as("select * from sources where id = $1 and project_id = $2 for update")
        .bind(source_id)
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let base = SourceConfig {
        name: current.name,
        platform: current.platform,
        retention_days: current.retention_days,
        daily_byte_quota: current.daily_byte_quota,
    };
    let c = validate(patch.apply_to(base)).map_err(AppError::Validation)?;
    let updated: Source = sqlx::query_as(
        "update sources
         set name = $3, platform = $4, retention_days = $5, daily_byte_quota = $6, updated_at = now()
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(source_id)
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.platform)
    .bind(c.retention_days)
    .bind(c.daily_byte_quota)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Deleting a source stops its ingestion at once; its rows age out by TTL.
pub async fn delete_in_project(pool: &PgPool, project_id: Uuid, source_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The plaintext token, for members and admins. A token sealed under
/// another key (NAMI_ENCRYPTION_KEY changed) cannot be shown, but ingestion
/// keeps working — it only needs the hash — and rotating issues a new one.
pub async fn reveal_token(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<String>, AppError> {
    let sealed: Option<Vec<u8>> = sqlx::query_scalar("select token_enc from sources where id = $1 and project_id = $2")
        .bind(source_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await?;
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    let undecryptable = || {
        AppError::Conflict(
            "the stored token could not be decrypted (was NAMI_ENCRYPTION_KEY changed?); rotate it to get a new one"
                .to_string(),
        )
    };
    let plaintext = cipher.decrypt(TOKEN_CONTEXT, &sealed).map_err(|_| undecryptable())?;
    String::from_utf8(plaintext).map(Some).map_err(|_| undecryptable())
}

/// Replaces the token: the old one stops working as soon as this commits.
pub async fn rotate_token(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    source_id: Uuid,
) -> Result<Option<SourceWithToken>, sqlx::Error> {
    let token = new_token();
    let source: Option<Source> = sqlx::query_as(
        "update sources set token_hash = $3, token_enc = $4, updated_at = now()
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(source_id)
    .bind(project_id)
    .bind(hash_token(&token))
    .bind(cipher.encrypt(TOKEN_CONTEXT, token.as_bytes()))
    .fetch_optional(pool)
    .await?;
    Ok(source.map(|source| SourceWithToken { source, token }))
}

/// The source a presented ingest token belongs to, if any.
pub async fn find_by_token(pool: &PgPool, token: &str) -> Result<Option<IngestSource>, sqlx::Error> {
    if token.is_empty() || token.len() > MAX_TOKEN_LEN {
        return Ok(None);
    }
    sqlx::query_as("select id, project_id, retention_days, daily_byte_quota from sources where token_hash = $1")
        .bind(hash_token(token))
        .fetch_optional(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SourceConfig {
        SourceConfig {
            name: "api".to_string(),
            platform: "http".to_string(),
            retention_days: 30,
            daily_byte_quota: None,
        }
    }

    #[test]
    fn tokens_are_43_base64url_characters_and_never_repeat() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{a}");
        assert_ne!(a, b);
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&a).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn validate_accepts_the_bounds() {
        for retention_days in [1, 365] {
            assert!(validate(SourceConfig { retention_days, ..config() }).is_ok());
        }
        for platform in PLATFORMS {
            assert!(validate(SourceConfig { platform: platform.to_string(), ..config() }).is_ok());
        }
        let quota = SourceConfig { daily_byte_quota: Some(MIN_DAILY_BYTE_QUOTA), ..config() };
        assert_eq!(validate(quota).unwrap().daily_byte_quota, Some(1_048_576));
    }

    #[test]
    fn validate_trims_the_name() {
        assert_eq!(validate(SourceConfig { name: "  api  ".to_string(), ..config() }).unwrap().name, "api");
    }
}
```

```diff
diff --git a/nami-engine/src/models/mod.rs b/nami-engine/src/models/mod.rs
--- a/nami-engine/src/models/mod.rs
+++ b/nami-engine/src/models/mod.rs
@@ -8,6 +8,7 @@ pub mod target;
 pub mod incident;
 pub mod heartbeat;
 pub mod maintenance;
+pub mod source;
 
 use serde::{Deserialize, Deserializer};
 
```

- [ ] **Step 5: Add the routes**

`nami-engine/src/routes/sources.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::source::{self, CreateSourceRequest, PatchSourceRequest, Source, SourceWithToken},
    state::AppState,
};

// Viewers read sources but never their tokens; members create, edit, reveal
// and rotate; admins delete.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/sources", get(list_sources).post(create_source))
        .route(
            "/projects/{id}/sources/{source_id}",
            get(get_source).patch(patch_source).delete(delete_source),
        )
        .route("/projects/{id}/sources/{source_id}/token", get(reveal_token))
        .route("/projects/{id}/sources/{source_id}/rotate", post(rotate_token))
}

async fn list_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Source>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(source::list_for_project(&state.db, project_id).await?))
}

async fn create_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateSourceRequest>,
) -> Result<(StatusCode, Json<SourceWithToken>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = source::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = source::insert_source(&state.db, &state.cipher, project_id, &config)
        .await
        .map_err(|e| match &e {
            // Global admins bypass membership: the project may not exist.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => AppError::NotFound,
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = source::find_in_project(&state.db, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchSourceRequest>,
) -> Result<Json<Source>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = source::update_in_project(&state.db, project_id, source_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !source::delete_in_project(&state.db, project_id, source_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn reveal_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let token = source::reveal_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({ "token": token })))
}

async fn rotate_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<SourceWithToken>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let rotated = source::rotate_token(&state.db, &state.cipher, project_id, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(rotated))
}
```

```diff
diff --git a/nami-engine/src/routes/mod.rs b/nami-engine/src/routes/mod.rs
--- a/nami-engine/src/routes/mod.rs
+++ b/nami-engine/src/routes/mod.rs
@@ -6,3 +6,4 @@ pub mod incidents;
 pub mod heartbeats;
 pub mod maintenance;
 pub mod monitor_groups;
+pub mod sources;
```

```diff
diff --git a/nami-engine/src/router.rs b/nami-engine/src/router.rs
--- a/nami-engine/src/router.rs
+++ b/nami-engine/src/router.rs
@@ -62,6 +62,7 @@ pub fn build_router(state: AppState) -> Router {
         .merge(routes::heartbeats::public_router())
         .merge(routes::maintenance::router())
         .merge(routes::monitor_groups::router())
+        .merge(routes::sources::router())
         .with_state(state)
 }
 
```

- [ ] **Step 6: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/sources_api.rs` 10 passed, the 3 new `models::source` unit tests pass, every existing binary still green (229 tests in total on the verified base).

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/0040_create_sources.sql nami-engine/src/models/source.rs nami-engine/src/models/mod.rs \
  nami-engine/src/routes/sources.rs nami-engine/src/routes/mod.rs nami-engine/src/router.rs \
  nami-engine/tests/sources_api.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): log sources with hashed and sealed ingest tokens (NAMI-LOG-1)

Sources per project (platform, retention 1-365 days, optional daily byte
quota). The ingest token is 32 random bytes, base64url, stored as SHA-256
for lookup and AES-GCM sealed for reveal; rotating replaces both at once.
Viewers read sources without tokens, members create, edit, reveal and
rotate, admins delete.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2 (NAMI-LOG-2): ClickHouse `logs` table and event normalization

Branch `feat/NAMI-LOG-2-logs-table-normalization`, worktree `.worktrees/NAMI-LOG-2/`.

**Files:**
- Create: `nami-engine/src/logs/mod.rs`, `nami-engine/src/logs/normalize.rs`, `nami-engine/src/logs/pattern.rs`, `nami-engine/src/logs/store.rs`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/clickhouse.rs`, `nami-engine/src/lib.rs`, `nami-engine/tests/clickhouse.rs`
- Test: `nami-engine/tests/logs_store.rs` (+ unit tests in `normalize.rs`, `pattern.rs`, `clickhouse.rs`)

**Interfaces:**
- Consumes: `ClickHouse::{create_test_database, drop_database, insert_rows, query_rows}` (Phase 1); `models::source::IngestSource` (Task 1).
- Produces:
  - `clickhouse::MIGRATIONS` gains the spec's `logs` table. `ClickHouse::insert_rows_with<T: Serialize>(&self, table: &str, rows: &[T], settings: &[(&'static str, &'static str)]) -> Result<(), ClickHouseError>` and `ClickHouse::query_rows_with<T: DeserializeOwned>(&self, sql: &str, params: &[(&str, String)], settings: &[(&'static str, &'static str)]) -> Result<Vec<T>, ClickHouseError>` (panic on a setting the client owns: `query`, `database`, `readonly`, `default_format`, `wait_end_of_query`, `input_format_skip_unknown_fields`, `param_*`). `insert_rows`/`query_rows` delegate with no extra settings.
  - `logs::normalize`: `LogEvent { dt: DateTime<Utc>, level: String, message: String, fields: Map<String, Value>, trace_id: String, span_id: String }`; `LogEvent::new(dt, level: &str, message: String, fields: Map<String, Value>, trace_id: String, span_id: String) -> LogEvent` (applies the caps and ms precision); `from_json(object: Map<String, Value>, received_at: DateTime<Utc>) -> LogEvent`; `parse_timestamp(&Value) -> Option<DateTime<Utc>>`; `in_accepted_range(dt, received_at) -> bool`; `normalize_level(&Value) -> &'static str`; `level_from_name(&str) -> &'static str`; `level_from_number(u64) -> &'static str`; `is_hex_id(&str, len: usize) -> bool`; `truncate_message(String) -> String`; `cap_fields(Map<String, Value>) -> Map<String, Value>`; consts `MAX_MESSAGE_BYTES`, `TRUNCATION_MARKER`, `MAX_FIELDS_BYTES`, `FIELDS_TRUNCATED_KEY`, `TIMESTAMP_KEYS`, `MESSAGE_KEYS`, `LEVEL_KEYS`, `LEVELS: [&str; 10]`, `DEFAULT_LEVEL`, `MAX_PAST`, `MAX_FUTURE`.
  - `logs::pattern::normalize_message(message: &str) -> String`, `PATTERN_SOURCE_CHARS = 1024`.
  - `logs::store`: `LOGS_TABLE = "logs"`; `INSERT_SETTINGS` (`async_insert=1`, `wait_for_async_insert=1`, `throw_on_max_partitions_per_insert_block=0`); `LogRow { id: Uuid, project_id: Uuid, source_id: Uuid, dt: DateTime<Utc>, level: String, message: String, fields: String, trace_id: String, span_id: String, pattern: String, retention_days: u16 }` (`Serialize`, `Deserialize`); `LogRow::stored_bytes(&self) -> u64`; `build_row(event: LogEvent, source: &IngestSource) -> LogRow` (UUIDv7 id); `insert_logs(&ClickHouse, &[LogRow]) -> Result<(), ClickHouseError>`.

- [ ] **Step 1: Add the crates**

```bash
cd nami-engine
cargo add regex@1.13.1
cargo add uuid@1.28.0 --features v7
```

- [ ] **Step 2: Write the failing tests**

`nami-engine/tests/logs_store.rs`:

```rust
// The ClickHouse `logs` table and row building. Requires
// `docker compose up -d clickhouse`.
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    clickhouse::ClickHouse,
    logs::{
        normalize,
        store::{self, LogRow},
    },
    models::source::IngestSource,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

fn source() -> IngestSource {
    IngestSource { id: Uuid::new_v4(), project_id: Uuid::new_v4(), retention_days: 14, daily_byte_quota: None }
}

fn row(source: &IngestSource, value: Value, received_at: DateTime<Utc>) -> LogRow {
    store::build_row(normalize::from_json(value.as_object().unwrap().clone(), received_at), source)
}

fn now_millis() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

#[test]
fn build_row_carries_the_source_and_the_pattern() {
    let source = source();
    let received = now_millis();
    let built = row(
        &source,
        json!({ "message": "user 42 logged in from 10.0.0.7", "level": "error", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "k": [1, 2] }),
        received,
    );
    assert_eq!((built.project_id, built.source_id, built.retention_days), (source.project_id, source.id, 14));
    assert_eq!(built.dt, received);
    assert_eq!(built.level, "error");
    assert_eq!(built.pattern, "user <num> logged in from <ip>");
    assert_eq!(built.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(built.fields, r#"{"k":[1,2]}"#);
    assert_eq!(built.stored_bytes(), (built.message.len() + built.fields.len()) as u64);
    assert_eq!(built.id.get_version_num(), 7, "time-ordered UUIDv7 ids");
    assert_ne!(built.id, row(&source, json!({}), received).id);
}

#[derive(Debug, Deserialize)]
struct StoredRow {
    #[serde(flatten)]
    row: LogRow,
    pattern_hash: u64,
    expected_hash: u64,
}

#[tokio::test]
async fn rows_round_trip_and_equal_patterns_share_a_hash() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = source();
    let received = now_millis();
    let rows = vec![
        row(&source, json!({ "dt": (received - Duration::seconds(2)).to_rfc3339(), "message": "user 42 logged in", "level": "info", "user": { "name": "O'Brien \\ \"x\"" } }), received),
        row(&source, json!({ "dt": (received - Duration::seconds(1)).to_rfc3339(), "message": "user 7 logged in", "level": "warn" }), received),
        row(&source, json!({ "message": "disk full on /var\nretrying", "level": "error", "span_id": "00f067aa0ba902b7" }), received),
    ];
    store::insert_logs(&ch, &rows).await.unwrap();

    let stored: Vec<StoredRow> = ch
        .query_rows(
            "SELECT id, project_id, source_id, dt, level, message, fields, trace_id, span_id, pattern,
                    retention_days, pattern_hash, cityHash64(pattern) AS expected_hash
             FROM logs WHERE project_id = {project_id:UUID} ORDER BY dt, id",
            &[("project_id", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(stored.iter().map(|s| s.row.clone()).collect::<Vec<_>>(), rows);
    assert!(stored.iter().all(|s| s.pattern_hash == s.expected_hash), "pattern_hash = cityHash64(pattern)");
    assert_eq!(stored[0].pattern_hash, stored[1].pattern_hash, "same pattern, same hash");
    assert_ne!(stored[0].pattern_hash, stored[2].pattern_hash);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn a_batch_spanning_more_than_100_daily_partitions_is_accepted() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    // Rows older than their retention are dropped by the TTL on insert.
    let source = IngestSource { retention_days: 365, ..source() };
    let received = now_millis();
    let rows: Vec<LogRow> = (0..150)
        .map(|day| row(&source, json!({ "dt": (received - Duration::days(day)).to_rfc3339(), "message": "m" }), received))
        .collect();
    store::insert_logs(&ch, &rows).await.unwrap();
    let count: Vec<Value> = ch
        .query_rows(
            "SELECT count() AS n, uniqExact(toYYYYMMDD(dt)) AS days FROM logs WHERE project_id = {p:UUID}",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(count, vec![json!({ "n": 150, "days": 150 })]);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn the_logs_table_has_the_spec_schema() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let db = ("db", ch.database().to_string());
    let columns: Vec<Value> = ch
        .query_rows(
            "SELECT name, type, default_kind FROM system.columns
             WHERE database = {db:String} AND table = 'logs' ORDER BY position",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    let expected = [
        ("id", "UUID", ""),
        ("project_id", "UUID", ""),
        ("source_id", "UUID", ""),
        ("dt", "DateTime64(3, 'UTC')", ""),
        ("level", "LowCardinality(String)", ""),
        ("message", "String", ""),
        ("fields", "String", ""),
        ("trace_id", "String", ""),
        ("span_id", "String", ""),
        ("pattern", "String", ""),
        ("pattern_hash", "UInt64", "MATERIALIZED"),
        ("retention_days", "UInt16", ""),
    ];
    assert_eq!(
        columns,
        expected
            .iter()
            .map(|(name, ty, kind)| json!({ "name": name, "type": ty, "default_kind": kind }))
            .collect::<Vec<_>>()
    );
    let indexes: Vec<Value> = ch
        .query_rows(
            "SELECT name, type_full FROM system.data_skipping_indices
             WHERE database = {db:String} AND table = 'logs' ORDER BY name",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    assert_eq!(
        indexes,
        vec![
            json!({ "name": "idx_level", "type_full": "set(16)" }),
            json!({ "name": "idx_message", "type_full": "tokenbf_v1(32768, 3, 0)" }),
            json!({ "name": "idx_pattern", "type_full": "bloom_filter(0.01)" }),
            json!({ "name": "idx_trace", "type_full": "bloom_filter(0.01)" }),
        ]
    );
    let table: Vec<Value> = ch
        .query_rows(
            "SELECT partition_key, sorting_key, create_table_query FROM system.tables
             WHERE database = {db:String} AND name = 'logs'",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    assert_eq!(table[0]["partition_key"], "toYYYYMMDD(dt)");
    assert_eq!(table[0]["sorting_key"], "project_id, dt, id");
    let ddl = table[0]["create_table_query"].as_str().unwrap();
    assert!(ddl.contains("TTL toDateTime(dt) + toIntervalDay(retention_days)"), "{ddl}");
    ch.drop_database().await.unwrap();
}

/// The insert asks for an async insert that waits for the flush (so 202
/// still means stored) and tolerates wide backfills.
#[tokio::test]
async fn inserts_use_waited_async_inserts() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_logs");
    store::insert_logs(&ch, &[row(&source(), json!({ "message": "m" }), now_millis())]).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let params: Vec<(String, String)> = requests[0].url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    for expected in [
        ("async_insert", "1"),
        ("wait_for_async_insert", "1"),
        ("throw_on_max_partitions_per_insert_block", "0"),
        ("input_format_skip_unknown_fields", "0"),
    ] {
        assert!(
            params.iter().any(|(k, v)| k == expected.0 && v == expected.1),
            "missing {expected:?} in {params:?}"
        );
    }
    assert!(params.iter().any(|(k, v)| k == "param_table" && v == "logs"), "{params:?}");
}
```

The two table-list assertions in `tests/clickhouse.rs` now expect `logs` too:

```diff
diff --git a/nami-engine/tests/clickhouse.rs b/nami-engine/tests/clickhouse.rs
--- a/nami-engine/tests/clickhouse.rs
+++ b/nami-engine/tests/clickhouse.rs
@@ -150,12 +150,15 @@ async fn migrate_is_idempotent() {
     ch.migrate().await.unwrap(); // second run must be a no-op, not an error
     let tables: Vec<serde_json::Value> = ch
         .query_rows(
-            "SELECT name FROM system.tables WHERE database = {db:String}",
+            "SELECT name FROM system.tables WHERE database = {db:String} ORDER BY name",
             &[("db", ch.database().to_string())],
         )
         .await
         .unwrap();
-    assert_eq!(tables, vec![serde_json::json!({ "name": "uptime_checks" })]);
+    assert_eq!(
+        tables,
+        vec![serde_json::json!({ "name": "logs" }), serde_json::json!({ "name": "uptime_checks" })]
+    );
     ch.drop_database().await.unwrap();
 }
 
@@ -223,12 +226,15 @@ async fn query_rows_is_read_only_so_ddl_and_writes_fail_on_the_read_path() {
     // Nothing above took effect, and ordinary reads still work.
     let tables: Vec<serde_json::Value> = ch
         .query_rows(
-            "SELECT name FROM system.tables WHERE database = {db:String}",
+            "SELECT name FROM system.tables WHERE database = {db:String} ORDER BY name",
             &[("db", ch.database().to_string())],
         )
         .await
         .unwrap();
-    assert_eq!(tables, vec![serde_json::json!({ "name": "uptime_checks" })]);
+    assert_eq!(
+        tables,
+        vec![serde_json::json!({ "name": "logs" }), serde_json::json!({ "name": "uptime_checks" })]
+    );
     let rows: Vec<Count> = ch
         .query_rows("SELECT count() AS n FROM uptime_checks", &[])
         .await
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test logs_store`
Expected: FAIL to compile — `error[E0432]: unresolved import nami_engine::logs`.

- [ ] **Step 4: Add the table and the settings-aware calls to the ClickHouse client**

```diff
diff --git a/nami-engine/src/clickhouse.rs b/nami-engine/src/clickhouse.rs
--- a/nami-engine/src/clickhouse.rs
+++ b/nami-engine/src/clickhouse.rs
@@ -37,7 +37,8 @@ const BASE_SETTINGS: [(&str, &str); 3] = [
 
 /// Idempotent DDL applied by [`ClickHouse::migrate`], in order. Table names
 /// are unqualified because every request carries the `database` setting.
-const MIGRATIONS: [&str; 1] = ["CREATE TABLE IF NOT EXISTS uptime_checks (
+const MIGRATIONS: [&str; 2] = [
+    "CREATE TABLE IF NOT EXISTS uptime_checks (
     monitor_id UUID,
     project_id UUID,
     checked_at DateTime64(3, 'UTC'),
@@ -47,7 +48,44 @@ const MIGRATIONS: [&str; 1] = ["CREATE TABLE IF NOT EXISTS uptime_checks (
     error Nullable(String)
 ) ENGINE = MergeTree
 ORDER BY (monitor_id, checked_at)
-TTL toDateTime(checked_at) + INTERVAL 180 DAY"];
+TTL toDateTime(checked_at) + INTERVAL 180 DAY",
+    // Log events (see `logs::store`). `retention_days` is copied from the
+    // source onto each row at ingest, so the TTL is per row; `pattern_hash`
+    // is derived by ClickHouse and never inserted.
+    "CREATE TABLE IF NOT EXISTS logs (
+    id UUID,
+    project_id UUID,
+    source_id UUID,
+    dt DateTime64(3, 'UTC'),
+    level LowCardinality(String),
+    message String,
+    fields String,
+    trace_id String,
+    span_id String,
+    pattern String,
+    pattern_hash UInt64 MATERIALIZED cityHash64(pattern),
+    retention_days UInt16,
+    INDEX idx_message message TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 4,
+    INDEX idx_level level TYPE set(16) GRANULARITY 4,
+    INDEX idx_trace trace_id TYPE bloom_filter(0.01) GRANULARITY 4,
+    INDEX idx_pattern pattern_hash TYPE bloom_filter(0.01) GRANULARITY 4
+) ENGINE = MergeTree
+PARTITION BY toYYYYMMDD(dt)
+ORDER BY (project_id, dt, id)
+TTL toDateTime(dt) + toIntervalDay(retention_days)",
+];
+
+/// Settings a caller may never pass as extra settings: they are owned by
+/// this client (the query text, its parameters, the output format and the
+/// read-only guard on [`ClickHouse::query_rows_with`]).
+const RESERVED_SETTINGS: [&str; 6] = [
+    "query",
+    "database",
+    "readonly",
+    "default_format",
+    "wait_end_of_query",
+    "input_format_skip_unknown_fields",
+];
 
 /// Prefix of the throwaway databases created by
 /// [`ClickHouse::create_test_database`].
@@ -133,8 +171,21 @@ impl ClickHouse {
         &self,
         table: &str,
         rows: &[T],
+    ) -> Result<(), ClickHouseError> {
+        self.insert_rows_with(table, rows, &[]).await
+    }
+
+    /// [`Self::insert_rows`] plus extra ClickHouse settings, e.g.
+    /// `async_insert`. Settings are fixed names and values from code, never
+    /// user input; panics on one this client owns (see `RESERVED_SETTINGS`).
+    pub async fn insert_rows_with<T: Serialize>(
+        &self,
+        table: &str,
+        rows: &[T],
+        settings: &[(&'static str, &'static str)],
     ) -> Result<(), ClickHouseError> {
         let mut pairs = self.settings_with_database()?;
+        push_extra_settings(&mut pairs, settings);
         pairs.push((
             "input_format_skip_unknown_fields".to_string(),
             "0".to_string(),
@@ -169,8 +220,22 @@ impl ClickHouse {
         &self,
         sql: &str,
         params: &[(&str, String)],
+    ) -> Result<Vec<T>, ClickHouseError> {
+        self.query_rows_with(sql, params, &[]).await
+    }
+
+    /// [`Self::query_rows`] plus extra ClickHouse settings, e.g.
+    /// `max_execution_time`. Settings are fixed names and values from code,
+    /// never user input; panics on one this client owns (see
+    /// `RESERVED_SETTINGS`), so the read-only guard cannot be lifted.
+    pub async fn query_rows_with<T: DeserializeOwned>(
+        &self,
+        sql: &str,
+        params: &[(&str, String)],
+        settings: &[(&'static str, &'static str)],
     ) -> Result<Vec<T>, ClickHouseError> {
         let mut pairs = self.settings_with_database()?;
+        push_extra_settings(&mut pairs, settings);
         pairs.push(("default_format".to_string(), "JSONEachRow".to_string()));
         // Buffer the whole result server-side, so an error raised mid-query
         // becomes a non-2xx status instead of trailing text in a 200 body.
@@ -326,6 +391,18 @@ fn escape_param_value(value: &str) -> String {
     escaped
 }
 
+/// Appends caller-supplied settings, refusing the ones this client owns and
+/// anything that would smuggle in a query parameter.
+fn push_extra_settings(pairs: &mut Vec<(String, String)>, settings: &[(&'static str, &'static str)]) {
+    for (name, value) in settings {
+        assert!(
+            !RESERVED_SETTINGS.contains(name) && !name.starts_with("param_"),
+            "the ClickHouse setting {name:?} is managed by the client and cannot be overridden"
+        );
+        pairs.push((name.to_string(), value.to_string()));
+    }
+}
+
 /// The `param_<name>=<value>` pair for a `{name:Identifier}` placeholder.
 /// Unlike typed values it is not TSV-escaped (see [`escape_param_value`]):
 /// the value is validated and sent raw.
@@ -351,7 +428,32 @@ fn validate_identifier(name: &str) -> Result<&str, ClickHouseError> {
 
 #[cfg(test)]
 mod tests {
-    use super::{escape_param_value, validate_identifier, ClickHouseError};
+    use super::{escape_param_value, push_extra_settings, validate_identifier, ClickHouseError};
+
+    #[test]
+    fn extra_settings_are_appended() {
+        let mut pairs = Vec::new();
+        push_extra_settings(&mut pairs, &[("async_insert", "1"), ("max_execution_time", "30")]);
+        assert_eq!(
+            pairs,
+            vec![
+                ("async_insert".to_string(), "1".to_string()),
+                ("max_execution_time".to_string(), "30".to_string())
+            ]
+        );
+    }
+
+    #[test]
+    #[should_panic(expected = "managed by the client")]
+    fn extra_settings_cannot_lift_the_read_only_guard() {
+        push_extra_settings(&mut Vec::new(), &[("readonly", "0")]);
+    }
+
+    #[test]
+    #[should_panic(expected = "managed by the client")]
+    fn extra_settings_cannot_inject_query_parameters() {
+        push_extra_settings(&mut Vec::new(), &[("param_project_id", "x")]);
+    }
 
     #[test]
     fn escapes_exactly_the_tsv_special_characters() {
```

- [ ] **Step 5: Implement normalization**

`nami-engine/src/logs/normalize.rs` (its unit tests pin every mapping rule, the caps and the timestamp range):

```rust
//! Field mapping and size caps for one ingested event. Every ingest path
//! (HTTP JSON, NDJSON and MessagePack, syslog, and later OTLP logs) ends in
//! a [`LogEvent`] built here; `store::build_row` then turns it into a row.
//!
//! Mapping of a JSON object (`from_json`):
//! - timestamp from the first of `dt` | `timestamp` | `time` that parses
//!   (RFC 3339, or unix seconds / milliseconds) and lies within
//!   [`MAX_PAST`] / [`MAX_FUTURE`] of the receive time, else the receive
//!   time (and the unusable key stays in `fields`);
//! - message from the first present of `message` | `msg` | `log`;
//! - level from the first present of `level` | `severity` | `lvl`,
//!   normalized to one of [`LEVELS`] (unknown -> `info`);
//! - `trace_id` / `span_id` when they are 32 / 16 hex characters and not
//!   all zeros (stored lowercase; otherwise they stay in `fields`);
//! - every other key goes into `fields`.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Map, Value};

/// Messages are cut to at most this many bytes, marker included.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const TRUNCATION_MARKER: &str = "…[truncated]";
/// `fields`, serialized, never exceeds this many bytes.
pub const MAX_FIELDS_BYTES: usize = 256 * 1024;
/// Added to `fields` when keys had to be dropped to respect the cap.
pub const FIELDS_TRUNCATED_KEY: &str = "_nami_truncated";
/// Room kept for the [`FIELDS_TRUNCATED_KEY`] entry when dropping keys.
const MARKER_RESERVE: usize = 128;

pub const TIMESTAMP_KEYS: [&str; 3] = ["dt", "timestamp", "time"];
pub const MESSAGE_KEYS: [&str; 3] = ["message", "msg", "log"];
pub const LEVEL_KEYS: [&str; 3] = ["level", "severity", "lvl"];

/// Every level a stored row can have.
pub const LEVELS: [&str; 10] = [
    "trace", "debug", "info", "notice", "warn", "error", "critical", "alert", "emergency", "fatal",
];
pub const DEFAULT_LEVEL: &str = "info";

/// No source keeps rows longer than 365 days; an older timestamp is
/// treated as unusable instead of creating partitions TTL drops at once.
pub const MAX_PAST: Duration = Duration::days(365);
/// Clock skew allowance for timestamps ahead of the receive time.
pub const MAX_FUTURE: Duration = Duration::days(1);

#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub fields: Map<String, Value>,
    /// 32 lowercase hex characters, or empty.
    pub trace_id: String,
    /// 16 lowercase hex characters, or empty.
    pub span_id: String,
}

impl LogEvent {
    /// Applies the size caps (message truncation, fields cap) and the
    /// millisecond precision of the `dt` column. Every way of building an
    /// event goes through here.
    pub fn new(
        dt: DateTime<Utc>,
        level: &str,
        message: String,
        fields: Map<String, Value>,
        trace_id: String,
        span_id: String,
    ) -> LogEvent {
        LogEvent {
            dt: truncate_to_millis(dt),
            level: level.to_string(),
            message: truncate_message(message),
            fields: cap_fields(fields),
            trace_id,
            span_id,
        }
    }
}

/// Maps one ingested JSON object (see the module docs).
pub fn from_json(mut object: Map<String, Value>, received_at: DateTime<Utc>) -> LogEvent {
    let dt = take_timestamp(&mut object, received_at).unwrap_or(received_at);
    let message = take_first(&mut object, &MESSAGE_KEYS).map(message_text).unwrap_or_default();
    let level = take_first(&mut object, &LEVEL_KEYS)
        .map(|value| normalize_level(&value))
        .unwrap_or(DEFAULT_LEVEL);
    let trace_id = take_hex_id(&mut object, "trace_id", 32);
    let span_id = take_hex_id(&mut object, "span_id", 16);
    LogEvent::new(dt, level, message, object, trace_id, span_id)
}

/// Removes and returns the value of the first of `keys` present.
fn take_first(object: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    let key = keys.iter().find(|key| object.contains_key(**key))?;
    object.remove(*key)
}

fn take_timestamp(object: &mut Map<String, Value>, received_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    for key in TIMESTAMP_KEYS {
        let Some(value) = object.get(key) else { continue };
        if let Some(dt) = parse_timestamp(value).filter(|dt| in_accepted_range(*dt, received_at)) {
            object.remove(key);
            return Some(dt);
        }
    }
    None
}

/// RFC 3339 strings, and numbers as unix seconds (below 10^11, fractions
/// allowed) or milliseconds.
pub fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text.trim()).ok().map(|dt| dt.with_timezone(&Utc)),
        Value::Number(number) => {
            let raw = number.as_f64().filter(|v| v.is_finite())?;
            let millis = if raw.abs() < 1e11 { raw * 1000.0 } else { raw };
            // Far outside any accepted range; also keeps the cast exact.
            if millis.abs() > 1e15 {
                return None;
            }
            Utc.timestamp_millis_opt(millis.round() as i64).single()
        }
        _ => None,
    }
}

pub fn in_accepted_range(dt: DateTime<Utc>, received_at: DateTime<Utc>) -> bool {
    dt >= received_at - MAX_PAST && dt <= received_at + MAX_FUTURE
}

fn truncate_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(dt.timestamp_millis()).unwrap_or(dt)
}

/// Strings as they are; `null` as empty; anything else as its JSON text.
fn message_text(value: Value) -> String {
    match value {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Level names (case-insensitive, common aliases) and numbers (syslog
/// severities 0-7, pino/bunyan levels 10-60); anything else is `info`.
pub fn normalize_level(value: &Value) -> &'static str {
    match value {
        Value::String(name) => level_from_name(name),
        Value::Number(number) => number.as_u64().map(level_from_number).unwrap_or(DEFAULT_LEVEL),
        _ => DEFAULT_LEVEL,
    }
}

pub fn level_from_name(name: &str) -> &'static str {
    match name.trim().to_ascii_lowercase().as_str() {
        "trace" | "trc" => "trace",
        "debug" | "dbg" => "debug",
        "info" | "information" | "informational" => "info",
        "notice" => "notice",
        "warn" | "warning" => "warn",
        "error" | "err" => "error",
        "critical" | "crit" => "critical",
        "alert" => "alert",
        "emergency" | "emerg" => "emergency",
        "fatal" | "panic" => "fatal",
        _ => DEFAULT_LEVEL,
    }
}

/// Syslog severities 0-7 (RFC 5424) and pino/bunyan levels 10-60.
pub fn level_from_number(number: u64) -> &'static str {
    match number {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warn",
        5 => "notice",
        6 => "info",
        7 => "debug",
        10 => "trace",
        20 => "debug",
        30 => "info",
        40 => "warn",
        50 => "error",
        60 => "fatal",
        _ => DEFAULT_LEVEL,
    }
}

/// `len` hex characters, not all zeros (W3C trace context rules).
pub fn is_hex_id(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit()) && text.bytes().any(|b| b != b'0')
}

fn take_hex_id(object: &mut Map<String, Value>, key: &str, len: usize) -> String {
    let valid = matches!(object.get(key), Some(Value::String(text)) if is_hex_id(text, len));
    if !valid {
        return String::new();
    }
    match object.remove(key) {
        Some(Value::String(text)) => text.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Cuts the message to [`MAX_MESSAGE_BYTES`] (on a character boundary),
/// ending in [`TRUNCATION_MARKER`].
pub fn truncate_message(message: String) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    let cut = message.floor_char_boundary(MAX_MESSAGE_BYTES - TRUNCATION_MARKER.len());
    let mut truncated = message;
    truncated.truncate(cut);
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

/// Keeps serialized `fields` within [`MAX_FIELDS_BYTES`]: when over, the
/// largest top-level keys are dropped first and [`FIELDS_TRUNCATED_KEY`]
/// says how many went.
pub fn cap_fields(mut fields: Map<String, Value>) -> Map<String, Value> {
    let total = json_len(&fields);
    if total <= MAX_FIELDS_BYTES {
        return fields;
    }
    // Each entry costs `"key":value` plus one separator.
    let mut entries: Vec<(usize, String)> = fields
        .iter()
        .map(|(key, value)| (json_len(key) + 1 + json_len(value) + 1, key.clone()))
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut size = total;
    let mut dropped = 0usize;
    for (entry_len, key) in entries {
        if size + MARKER_RESERVE <= MAX_FIELDS_BYTES {
            break;
        }
        fields.remove(&key);
        size = size.saturating_sub(entry_len);
        dropped += 1;
    }
    fields.insert(
        FIELDS_TRUNCATED_KEY.to_string(),
        Value::String(format!("{dropped} field(s) dropped: event fields are capped at 256 KiB")),
    );
    fields
}

fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value).map(|bytes| bytes.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn received() -> DateTime<Utc> {
        "2026-10-13T12:00:00Z".parse().unwrap()
    }

    fn event(value: Value) -> LogEvent {
        from_json(value.as_object().unwrap().clone(), received())
    }

    #[test]
    fn maps_the_standard_keys_and_keeps_everything_else_in_fields() {
        let e = event(json!({
            "dt": "2026-10-13T11:59:58.250+02:00",
            "message": "user 42 logged in",
            "level": "WARNING",
            "user": { "id": 42 },
            "status": 200
        }));
        assert_eq!(e.dt, "2026-10-13T09:59:58.250Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(e.message, "user 42 logged in");
        assert_eq!(e.level, "warn");
        assert_eq!(Value::Object(e.fields), json!({ "user": { "id": 42 }, "status": 200 }));
        assert_eq!((e.trace_id.as_str(), e.span_id.as_str()), ("", ""));
    }

    #[test]
    fn timestamp_keys_are_tried_in_order_and_unix_seconds_or_millis_are_accepted() {
        assert_eq!(event(json!({ "timestamp": 1_791_892_799 })).dt, "2026-10-13T11:59:59Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(
            event(json!({ "time": 1_791_892_799_123_u64 })).dt,
            "2026-10-13T11:59:59.123Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            event(json!({ "time": 1_791_892_799.5 })).dt,
            "2026-10-13T11:59:59.500Z".parse::<DateTime<Utc>>().unwrap()
        );
        let both = event(json!({ "dt": "not a time", "timestamp": "2026-10-13T11:00:00Z" }));
        assert_eq!(both.dt, "2026-10-13T11:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(Value::Object(both.fields), json!({ "dt": "not a time" }), "the unusable key stays");
    }

    #[test]
    fn missing_unparsable_or_out_of_range_timestamps_fall_back_to_the_receive_time() {
        for value in [
            json!({}),
            json!({ "dt": "yesterday" }),
            json!({ "dt": true }),
            json!({ "dt": "2024-01-01T00:00:00Z" }),
            json!({ "dt": "2026-10-15T00:00:00Z" }),
            json!({ "time": 1e300 }),
            json!({ "time": -5 }),
        ] {
            let e = event(value.clone());
            assert_eq!(e.dt, received(), "{value}");
            if let Some(key) = value.as_object().unwrap().keys().next() {
                assert!(e.fields.contains_key(key), "{value}: the original stays in fields");
            }
        }
    }

    #[test]
    fn dt_is_kept_at_millisecond_precision() {
        let e = event(json!({ "dt": "2026-10-13T11:59:59.123456789Z" }));
        assert_eq!(e.dt.timestamp_subsec_nanos(), 123_000_000);
    }

    #[test]
    fn message_keys_are_tried_in_order_and_non_strings_become_json_text() {
        assert_eq!(event(json!({ "msg": "a" })).message, "a");
        assert_eq!(event(json!({ "log": "b\n" })).message, "b\n");
        let both = event(json!({ "msg": "second", "message": "first" }));
        assert_eq!(both.message, "first");
        assert_eq!(Value::Object(both.fields), json!({ "msg": "second" }));
        assert_eq!(event(json!({ "message": { "a": 1 } })).message, r#"{"a":1}"#);
        assert_eq!(event(json!({ "message": 7 })).message, "7");
        assert_eq!(event(json!({ "message": null })).message, "");
        assert_eq!(event(json!({ "other": 1 })).message, "");
    }

    #[test]
    fn levels_are_normalized_and_unknown_levels_are_info() {
        for (raw, expected) in [
            (json!("TRACE"), "trace"),
            (json!("dbg"), "debug"),
            (json!(" Info "), "info"),
            (json!("notice"), "notice"),
            (json!("Warning"), "warn"),
            (json!("ERR"), "error"),
            (json!("crit"), "critical"),
            (json!("alert"), "alert"),
            (json!("emerg"), "emergency"),
            (json!("panic"), "fatal"),
            (json!("verbose"), "info"),
            (json!(3), "error"),
            (json!(50), "error"),
            (json!(60), "fatal"),
            (json!(35), "info"),
            (json!(-1), "info"),
            (json!(true), "info"),
            (json!(null), "info"),
        ] {
            assert_eq!(event(json!({ "level": raw.clone() })).level, expected, "{raw}");
        }
        let e = event(json!({ "severity": "error", "lvl": "debug" }));
        assert_eq!(e.level, "error");
        assert_eq!(Value::Object(e.fields), json!({ "lvl": "debug" }));
        assert_eq!(event(json!({})).level, "info");
        for level in LEVELS {
            assert_eq!(level_from_name(level), level, "every stored level maps to itself");
        }
    }

    #[test]
    fn trace_and_span_ids_are_taken_when_valid_and_lowercased() {
        let e = event(json!({
            "trace_id": "4BF92F3577B34DA6A3CE929D0E0E4736",
            "span_id": "00f067aa0ba902b7"
        }));
        assert_eq!(e.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(e.span_id, "00f067aa0ba902b7");
        assert!(e.fields.is_empty());
        for (trace_id, span_id) in [
            (json!("00000000000000000000000000000000"), json!("0000000000000000")),
            (json!("4bf92f3577b34da6a3ce929d0e0e473"), json!("00f067aa0ba902b")),
            (json!("4bf92f3577b34da6a3ce929d0e0e473g"), json!("00f067aa0ba902bz")),
            (json!(42), json!(null)),
        ] {
            let e = event(json!({ "trace_id": trace_id.clone(), "span_id": span_id.clone() }));
            assert_eq!((e.trace_id.as_str(), e.span_id.as_str()), ("", ""), "{trace_id} {span_id}");
            assert_eq!(e.fields.get("trace_id"), Some(&trace_id), "invalid ids stay in fields");
            assert_eq!(e.fields.get("span_id"), Some(&span_id));
        }
    }

    #[test]
    fn long_messages_are_truncated_on_a_char_boundary_with_a_marker() {
        let exact = "a".repeat(MAX_MESSAGE_BYTES);
        assert_eq!(truncate_message(exact.clone()), exact, "64 KiB fits");
        let long = "é".repeat(MAX_MESSAGE_BYTES);
        let truncated = event(json!({ "message": long })).message;
        assert!(truncated.len() <= MAX_MESSAGE_BYTES, "{}", truncated.len());
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        assert!(truncated.trim_end_matches(TRUNCATION_MARKER).chars().all(|c| c == 'é'));
    }

    #[test]
    fn oversized_fields_drop_their_largest_keys_and_say_so() {
        let e = event(json!({
            "message": "m",
            "stack": "x".repeat(300 * 1024),
            "user_id": 42,
            "path": "/api"
        }));
        assert!(!e.fields.contains_key("stack"));
        assert_eq!(e.fields["user_id"], 42);
        assert_eq!(e.fields["path"], "/api");
        assert_eq!(e.fields[FIELDS_TRUNCATED_KEY], "1 field(s) dropped: event fields are capped at 256 KiB");
        assert!(serde_json::to_vec(&e.fields).unwrap().len() <= MAX_FIELDS_BYTES);
    }

    #[test]
    fn many_small_fields_are_capped_too() {
        let mut object = Map::new();
        for i in 0..40_000 {
            object.insert(format!("key{i:05}"), json!(i));
        }
        let e = from_json(object, received());
        let len = serde_json::to_vec(&e.fields).unwrap().len();
        assert!(len <= MAX_FIELDS_BYTES, "{len}");
        assert!(len > MAX_FIELDS_BYTES - 1024, "keeps as much as fits: {len}");
        assert!(e.fields.contains_key(FIELDS_TRUNCATED_KEY));
    }

    #[test]
    fn fields_under_the_cap_are_untouched() {
        let e = event(json!({ "big": "x".repeat(200 * 1024) }));
        assert_eq!(e.fields.len(), 1);
        assert!(!e.fields.contains_key(FIELDS_TRUNCATED_KEY));
    }
}
```

- [ ] **Step 6: Implement message patterns**

`nami-engine/src/logs/pattern.rs`:

```rust
//! Message patterns: the first 1,024 characters of a message with its
//! variable parts replaced, in this order: quoted strings -> `<str>`, UUIDs
//! -> `<uuid>`, IPv4/IPv6 addresses -> `<ip>`, `0x…` and standalone hex runs
//! of at least 8 characters with a digit and a letter -> `<hex>`, standalone
//! integers/decimals (optional sign) -> `<num>`. ClickHouse derives
//! `pattern_hash = cityHash64(pattern)` from the result, so equal patterns
//! always share a hash.

use std::{net::Ipv6Addr, sync::LazyLock};

use regex::{Captures, Regex};

/// Only this many leading characters of a message form its pattern.
pub const PATTERN_SOURCE_CHARS: usize = 1024;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap()
});
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\b")
        .unwrap()
});
/// Runs of address-like characters; the ones that parse as IPv6 are replaced.
static IPV6_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z_.:]+").unwrap());
static HEX_PREFIXED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b0[xX][0-9A-Fa-f]+\b").unwrap());
static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9A-Fa-f]{8,}\b").unwrap());
/// A number is standalone when no word character touches it; the captured
/// prefix (start of text or a non-word character) is kept.
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\W)[-+]?[0-9]+(?:\.[0-9]+)?\b").unwrap());

pub fn normalize_message(message: &str) -> String {
    let head = match message.char_indices().nth(PATTERN_SOURCE_CHARS) {
        Some((cut, _)) => &message[..cut],
        None => message,
    };
    let text = replace_quoted(head);
    let text = UUID.replace_all(&text, "<uuid>");
    let text = IPV4.replace_all(&text, "<ip>");
    let text = IPV6_CANDIDATE.replace_all(&text, |caps: &Captures| replace_ipv6(&caps[0]));
    let text = HEX_PREFIXED.replace_all(&text, "<hex>");
    let text = HEX_RUN.replace_all(&text, |caps: &Captures| {
        let run = &caps[0];
        let has_digit = run.bytes().any(|b| b.is_ascii_digit());
        let has_letter = run.bytes().any(|b| b.is_ascii_alphabetic());
        if has_digit && has_letter { "<hex>".to_string() } else { run.to_string() }
    });
    let text = NUMBER.replace_all(&text, "${1}<num>");
    text.into_owned()
}

/// `"…"` always, and `'…'` when the opening quote does not follow a letter
/// or digit and the closing one is not followed by one, so apostrophes in
/// words ("can't", "user's") stay as they are. Backslash escapes are
/// skipped; an unterminated quote is left alone.
fn replace_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let opens = c == '"' || (c == '\'' && (i == 0 || !chars[i - 1].is_alphanumeric()));
        if opens && let Some(end) = closing_quote(&chars, i) {
            out.push_str("<str>");
            i = end + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn closing_quote(chars: &[char], open: usize) -> Option<usize> {
    let quote = chars[open];
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => {
                let ends = quote == '"' || chars.get(i + 1).is_none_or(|next| !next.is_alphanumeric());
                if ends {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// `<ip>` when the run (minus trailing `.`/`:` punctuation) is an IPv6
/// address with at least one digit — so `std::io` or `a::b` stay.
fn replace_ipv6(run: &str) -> String {
    let core = run.trim_end_matches(['.', ':']);
    let is_address =
        core.contains(':') && core.bytes().any(|b| b.is_ascii_digit()) && core.parse::<Ipv6Addr>().is_ok();
    if is_address {
        format!("<ip>{}", &run[core.len()..])
    } else {
        run.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_message;

    #[test]
    fn the_spec_example() {
        assert_eq!(normalize_message("user 42 logged in from 10.0.0.7"), "user <num> logged in from <ip>");
    }

    #[test]
    fn quoted_strings_go_first_so_their_contents_never_leak() {
        assert_eq!(normalize_message(r#"GET "/api/users/42" took 12ms"#), "GET <str> took 12ms");
        assert_eq!(normalize_message(r#"say "a \"quoted\" 7" now"#), "say <str> now");
        assert_eq!(normalize_message("can't reach 'db-1' (user's pool)"), "can't reach <str> (user's pool)");
        assert_eq!(normalize_message("'it's fine' said 2"), "<str> said <num>");
        assert_eq!(normalize_message(r#"unterminated "quote 42"#), r#"unterminated "quote <num>"#);
    }

    #[test]
    fn uuids_ips_hex_and_numbers() {
        assert_eq!(normalize_message("order 3F2A9C1E-0B6F-4C1E-9B7A-2F1D3C4B5A69 paid"), "order <uuid> paid");
        assert_eq!(normalize_message("peer 192.168.1.255:8443 closed"), "peer <ip>:<num> closed");
        assert_eq!(normalize_message("from fe80::1%eth0 and [2001:db8::8a2e:370:7334]:80"), "from <ip>%eth0 and [<ip>]:<num>");
        assert_eq!(normalize_message("ping ::1."), "ping <ip>.");
        assert_eq!(normalize_message("ptr 0x7ffd5e8c and 0XAB"), "ptr <hex> and <hex>");
        assert_eq!(normalize_message("commit a1b2c3d4e5 on deadbeef"), "commit <hex> on deadbeef");
        assert_eq!(normalize_message("ids 12345678 -5 +3.25 x=-7 1,2"), "ids <num> <num> <num> x=<num> <num>,<num>");
    }

    #[test]
    fn numbers_inside_words_and_code_paths_are_left_alone() {
        assert_eq!(normalize_message("v2 abc123 123abc snake_42 took 150ms"), "v2 abc123 123abc snake_42 took 150ms");
        assert_eq!(normalize_message("std::io::Error at a::b"), "std::io::Error at a::b");
        assert_eq!(normalize_message("at 10:00:00 retry 3/5"), "at <num>:<num>:<num> retry <num>/<num>");
        assert_eq!(normalize_message("256.1.1.1 is not an address"), "<num>.<num> is not an address");
    }

    #[test]
    fn only_the_first_1024_characters_count() {
        let message = format!("{}{}", "é".repeat(1024), " 42 tail");
        assert_eq!(normalize_message(&message), "é".repeat(1024));
        let same_head = format!("{} different tail", "x".repeat(1024));
        assert_eq!(normalize_message(&same_head), normalize_message(&"x".repeat(1024)));
    }

    #[test]
    fn equal_shapes_give_equal_patterns() {
        let a = normalize_message("user 42 logged in from 10.0.0.7 as \"alice\"");
        let b = normalize_message("user 7 logged in from 172.16.0.1 as \"bob\"");
        assert_eq!(a, b);
        assert_ne!(a, normalize_message("user 7 logged out"));
    }

    #[test]
    fn empty_and_placeholder_like_text_is_stable() {
        assert_eq!(normalize_message(""), "");
        assert_eq!(normalize_message("<num> <ip>"), "<num> <ip>");
    }
}
```

- [ ] **Step 7: Implement rows and inserts**

`nami-engine/src/logs/store.rs`:

```rust
//! Log rows in ClickHouse (`logs`, see `clickhouse::MIGRATIONS`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{normalize::LogEvent, pattern};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    models::source::IngestSource,
};

pub const LOGS_TABLE: &str = "logs";

/// Sent with every insert into `logs`: `async_insert` lets ClickHouse batch
/// the many small ingest requests server-side, waiting for the flush keeps
/// a 202 meaning "stored", and a batch whose rows span more than 100 daily
/// partitions (a backfill) is accepted instead of failing.
pub const INSERT_SETTINGS: [(&str, &str); 3] = [
    ("async_insert", "1"),
    ("wait_for_async_insert", "1"),
    ("throw_on_max_partitions_per_insert_block", "0"),
];

/// One row of `logs` as inserted (`pattern_hash` is derived by ClickHouse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    /// UUIDv7 generated at ingest: unique, the `(dt, id)` tiebreaker.
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Uuid,
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    /// A JSON object.
    pub fields: String,
    pub trace_id: String,
    pub span_id: String,
    pub pattern: String,
    pub retention_days: u16,
}

impl LogRow {
    /// What a row counts against usage and quotas: the UTF-8 length of the
    /// stored message plus fields.
    pub fn stored_bytes(&self) -> u64 {
        (self.message.len() + self.fields.len()) as u64
    }
}

/// The row for `event` from `source`: a fresh id, the message pattern, and
/// the source's current retention.
pub fn build_row(event: LogEvent, source: &IngestSource) -> LogRow {
    let pattern = pattern::normalize_message(&event.message);
    LogRow {
        id: Uuid::now_v7(),
        project_id: source.project_id,
        source_id: source.id,
        dt: event.dt,
        level: event.level,
        fields: serde_json::Value::Object(event.fields).to_string(),
        message: event.message,
        trace_id: event.trace_id,
        span_id: event.span_id,
        pattern,
        retention_days: u16::try_from(source.retention_days).unwrap_or(u16::MAX),
    }
}

pub async fn insert_logs(ch: &ClickHouse, rows: &[LogRow]) -> Result<(), ClickHouseError> {
    ch.insert_rows_with(LOGS_TABLE, rows, &INSERT_SETTINGS).await
}
```

`nami-engine/src/logs/mod.rs`:

```rust
//! Log management: turning ingested events into rows (`normalize`,
//! `pattern`) and storing them in ClickHouse (`store`).
pub mod normalize;
pub mod pattern;
pub mod store;
```

```diff
diff --git a/nami-engine/src/lib.rs b/nami-engine/src/lib.rs
--- a/nami-engine/src/lib.rs
+++ b/nami-engine/src/lib.rs
@@ -5,6 +5,7 @@ pub mod crypto;
 pub mod db;
 pub mod email;
 pub mod error;
+pub mod logs;
 pub mod middleware;
 pub mod state;
 pub mod routes;
```

- [ ] **Step 8: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/logs_store.rs` 5 passed, `tests/clickhouse.rs` 11 passed, the new unit tests pass (`logs::normalize` 11, `logs::pattern` 7, `clickhouse` 3 more), 255 tests in total on the verified base.

- [ ] **Step 9: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/src/clickhouse.rs nami-engine/src/lib.rs \
  nami-engine/src/logs nami-engine/tests/logs_store.rs nami-engine/tests/clickhouse.rs
git commit -m "feat(engine): ClickHouse logs table and event normalization (NAMI-LOG-2)

The spec's logs table with trace_id, span_id, pattern and a materialized
pattern_hash from the start. Field mapping, level normalization, message
truncation at 64 KiB, the 256 KiB fields cap and message patterns are pure
functions; rows insert with waited async inserts and tolerate backfills
wider than 100 daily partitions.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3 (NAMI-LOG-3): HTTP ingestion with limits, rate limit, usage stats and quota

Branch `feat/NAMI-LOG-3-http-ingest`, worktree `.worktrees/NAMI-LOG-3/`.

**Files:**
- Create: `nami-engine/migrations/0041_create_source_usage_daily.sql`, `nami-engine/src/logs/msgpack.rs`, `nami-engine/src/logs/ingest.rs`, `nami-engine/src/logs/rate_limit.rs`, `nami-engine/src/logs/usage.rs`, `nami-engine/src/routes/logs_ingest.rs`, `nami-engine/tests/common/logs.rs`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/logs/mod.rs`, `nami-engine/src/state.rs`, `nami-engine/src/main.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/src/routes/sources.rs`, `nami-engine/tests/common/mod.rs`, `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/logs_ingest.rs`, `nami-engine/tests/logs_quota.rs` (+ unit tests in `msgpack.rs`, `ingest.rs`, `rate_limit.rs`)

**Interfaces:**
- Consumes: `models::source::{find_by_token, find_in_project, insert_source, rotate_token, delete_in_project, IngestSource, SourceConfig}` (Task 1); `logs::normalize::from_json`, `logs::store::{build_row, insert_logs, LogRow}` (Task 2); `AppError` (its `Database`/`ClickHouse` variants log and answer a generic 500); `common::{app, state_with_clickhouse, test_app, test_cipher, user_with_role, call, seed_project}`.
- Produces:
  - `logs::LogsState { rate_limiter: rate_limit::TokenBuckets, usage: usage::UsageTracker }` (`Default`) and `AppState.logs: Arc<LogsState>` (`AppState::for_test` builds a fresh one; tests swap it with struct update syntax).
  - `logs::rate_limit`: `REQUESTS_PER_SECOND = 50.0`, `BURST = 100.0`, `TokenBuckets::new(rate: f64, burst: f64)`, `try_acquire(&self, key: Uuid) -> bool`, `try_acquire_at(&self, key: Uuid, now: Instant) -> bool`.
  - `logs::usage`: `FLUSH_INTERVAL` (10 s), `RETENTION_DAYS = 400`, `FLUSHED_CACHE_TTL` (10 s); `UsageCounters { events, bytes, events_dropped, events_sampled_out, requests_rate_limited, requests_over_quota: i64 }`; `DailyUsage { day: NaiveDate, #[serde(flatten)] counters: UsageCounters }`; `UsageTracker::{record(&self, Uuid, NaiveDate, UsageCounters), record_accepted(&self, Uuid, NaiveDate, events: u64, bytes: u64), record_rate_limited(&self, Uuid, NaiveDate), record_over_quota(&self, Uuid, NaiveDate, rejected: u64), pending(&self, Uuid, NaiveDate) -> UsageCounters, bytes_today(&self, &PgPool, Uuid, NaiveDate) -> Result<i64, sqlx::Error>, flush(&self, &PgPool) -> Result<usize, sqlx::Error>}`; `prune(&PgPool, today: NaiveDate) -> Result<u64, sqlx::Error>`; `daily_usage(&PgPool, source_id: Uuid, days: i64, today: NaiveDate) -> Result<Vec<DailyUsage>, sqlx::Error>`; `run_forever(AppState)`.
  - `logs::msgpack`: `MAX_NESTING = 127`, `MsgpackError` (`UnexpectedEnd`, `ReservedMarker`, `NonStringKey`, `TooDeep`, `InvalidUtf8`, `TrailingBytes`, `NotAnObject`, `TooManyEvents`), `decode_value(&[u8]) -> Result<Value, MsgpackError>`, `decode_events(&[u8], max_events: usize) -> Result<Vec<Map<String, Value>>, MsgpackError>`.
  - `logs::ingest`: `MAX_BODY_BYTES` (10 MiB), `MAX_DECOMPRESSED_BYTES` (50 MiB), `MAX_EVENTS = 10_000`; `IngestError` (`Unauthorized`, `RateLimited`, `QuotaExceeded { retry_after_seconds: u64 }`, `PayloadTooLarge(String)`, `UnsupportedMediaType(String)`, `BadRequest(String)`, `Internal`, `App(AppError)`; `IntoResponse`); `BodyFormat::{Json, MessagePack}`, `BodyEncoding { format, gzip }`; `bearer_token(&HeaderMap) -> Option<&str>`; `body_encoding(&HeaderMap) -> Result<BodyEncoding, IngestError>`; `read_body(Body, &HeaderMap) -> Result<Bytes, IngestError>`; `decode_events(Bytes, BodyEncoding) -> Result<Vec<Map<String, Value>>, IngestError>`; `gunzip(&[u8]) -> Result<Vec<u8>, IngestError>`; `parse_json_events(&[u8]) -> Result<Vec<Map<String, Value>>, IngestError>`; `seconds_until_utc_midnight(DateTime<Utc>) -> u64`.
  - Routes: `POST /api/v1/logs` (`routes::logs_ingest::public_router()`), `GET /projects/{id}/sources/{source_id}/usage?days=N`.
  - Test helpers (`tests/common/logs.rs`): `source_config(&str) -> SourceConfig`, `seed_source(&PgPool, project: Uuid, name: &str) -> (Uuid, String)`, `seed_source_with(&PgPool, Uuid, SourceConfig) -> (Uuid, String)`, `post_logs(&Router, token: Option<&str>, headers: &[(&str, &str)], body: impl Into<Body>) -> (StatusCode, HeaderMap, Value)`, `post_json(&Router, token: &str, Value) -> (StatusCode, Value)`, `stored_rows(&ClickHouse, project: Uuid) -> Vec<Value>`, `messages(&[Value]) -> Vec<String>`.

- [ ] **Step 1: Add the crates**

```bash
cd nami-engine
cargo add flate2@1.1.10 http-body-util@0.1.5
cargo add --dev rmp-serde@1.3.1
```

- [ ] **Step 2: Add the shared test helpers**

`nami-engine/tests/common/logs.rs`:

```rust
//! Helpers for the log management tests.

use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    Router,
};
use nami_engine::{
    clickhouse::ClickHouse,
    models::source::{self, SourceConfig},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use super::test_cipher;

pub fn source_config(name: &str) -> SourceConfig {
    SourceConfig { name: name.to_string(), platform: "http".to_string(), retention_days: 30, daily_byte_quota: None }
}

/// Inserts a source directly (bypassing the API); returns its id and its
/// plaintext ingest token.
pub async fn seed_source(pool: &sqlx::PgPool, project_id: Uuid, name: &str) -> (Uuid, String) {
    seed_source_with(pool, project_id, source_config(name)).await
}

pub async fn seed_source_with(pool: &sqlx::PgPool, project_id: Uuid, config: SourceConfig) -> (Uuid, String) {
    let created = source::insert_source(pool, &test_cipher(), project_id, &config).await.unwrap();
    (created.source.id, created.token)
}

/// `POST /api/v1/logs` with `token` as a Bearer token (none when `None`)
/// and the extra request headers. Returns the status, the response headers
/// and the JSON body (`Value::Null` when not JSON).
pub async fn post_logs(
    app: &Router,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method("POST").uri("/api/v1/logs");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app.clone().oneshot(builder.body(body.into()).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// `POST /api/v1/logs` with a JSON body; returns the status and JSON body.
pub async fn post_json(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, json) = post_logs(app, Some(token), &[("content-type", "application/json")], body.to_string()).await;
    (status, json)
}

/// Every stored row of `project_id`, oldest first (`fields` as stored text).
pub async fn stored_rows(ch: &ClickHouse, project_id: Uuid) -> Vec<Value> {
    ch.query_rows(
        "SELECT toString(id) AS id, toString(source_id) AS source_id, dt, level, message, fields,
                trace_id, span_id, pattern, toString(pattern_hash) AS pattern_hash, retention_days
         FROM logs WHERE project_id = {project_id:UUID} ORDER BY dt, id",
        &[("project_id", project_id.to_string())],
    )
    .await
    .unwrap()
}

/// The messages of `rows`, sorted (for order-independent assertions).
pub fn messages(rows: &[Value]) -> Vec<String> {
    let mut messages: Vec<String> = rows.iter().map(|row| row["message"].as_str().unwrap().to_string()).collect();
    messages.sort();
    messages
}
```

```diff
diff --git a/nami-engine/tests/common/mod.rs b/nami-engine/tests/common/mod.rs
--- a/nami-engine/tests/common/mod.rs
+++ b/nami-engine/tests/common/mod.rs
@@ -1,8 +1,9 @@
-//! Helpers shared by the uptime-monitoring integration tests. Every test
+//! Helpers shared by the integration tests. Every test
 //! binary compiles this module separately and uses a different subset of
 //! it, hence the blanket `dead_code` allowance.
 #![allow(dead_code)]
 
+pub mod logs;
 pub mod tls;
 
 use std::sync::OnceLock;
```

- [ ] **Step 3: Write the failing tests**

`nami-engine/tests/logs_ingest.rs` (formats, gzip, 401s, 413s, 400/415, the rate limit, caps, and that no token reaches a log line):

```rust
// HTTP log ingestion (`POST /api/v1/logs`). Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::StatusCode;
use common::{
    app,
    logs::{messages, post_json, post_logs, seed_source, stored_rows},
    seed_project, state_with_clickhouse, test_app, test_cipher,
};
use nami_engine::{
    logs::{
        normalize::{FIELDS_TRUNCATED_KEY, MAX_MESSAGE_BYTES, TRUNCATION_MARKER},
        rate_limit::TokenBuckets,
        LogsState,
    },
    models::source,
    state::AppState,
};
use serde_json::{json, Value};

const JSON: (&str, &str) = ("content-type", "application/json");
const NDJSON: (&str, &str) = ("content-type", "application/x-ndjson");
const MSGPACK: (&str, &str) = ("content-type", "application/msgpack");
const GZIP: (&str, &str) = ("content-encoding", "gzip");

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn msgpack(value: &Value) -> Vec<u8> {
    rmp_serde::to_vec(value).unwrap()
}

#[sqlx::test]
async fn json_objects_arrays_and_ndjson_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;

    let object = json!({
        "dt": chrono::Utc::now().to_rfc3339(),
        "message": "one",
        "level": "WARNING",
        "trace_id": "4BF92F3577B34DA6A3CE929D0E0E4736",
        "user": { "id": 1 }
    });
    assert_eq!(post_json(&app, &token, object).await, (StatusCode::ACCEPTED, json!({ "accepted": 1 })));
    let array = json!([{ "msg": "two" }, { "log": "three", "severity": 3 }]);
    assert_eq!(post_json(&app, &token, array).await, (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    let ndjson = "{\"message\":\"four\"}\n\n{\"message\":\"five\"}\r\n";
    let (status, _, body) = post_logs(&app, Some(&token), &[NDJSON], ndjson).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    // Shippers that send NDJSON as application/json are accepted too.
    let (status, _, body) = post_logs(&app, Some(&token), &[JSON], "{\"message\":\"six\"}\n{\"message\":\"seven\"}").await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    assert_eq!(post_json(&app, &token, json!([])).await, (StatusCode::ACCEPTED, json!({ "accepted": 0 })));

    let rows = stored_rows(&ch, project).await;
    assert_eq!(messages(&rows), ["five", "four", "one", "seven", "six", "three", "two"]);
    let one = rows.iter().find(|r| r["message"] == "one").unwrap();
    assert_eq!(one["level"], "warn");
    assert_eq!(one["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(one["fields"], r#"{"user":{"id":1}}"#);
    assert_eq!(one["source_id"], source_id.to_string());
    assert_eq!(one["retention_days"], 30);
    assert_eq!(rows.iter().find(|r| r["message"] == "three").unwrap()["level"], "error");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn messagepack_bodies_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let (status, _, body) =
        post_logs(&app, Some(&token), &[MSGPACK], msgpack(&json!({ "message": "mp one", "level": 50 }))).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 1 })));
    let array = msgpack(&json!([{ "message": "mp two" }, { "message": "mp three" }]));
    let (status, _, body) = post_logs(&app, Some(&token), &[("content-type", "application/x-msgpack")], array).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    // { "message": "mp bin", "b": bin8 [1, 2, 3], "t": timestamp32(1_700_000_000) }
    let mut raw = vec![0x83, 0xa7];
    raw.extend(b"message");
    raw.extend([0xa6]);
    raw.extend(b"mp bin");
    raw.extend([0xa1, b'b', 0xc4, 3, 1, 2, 3, 0xa1, b't', 0xd6, 0xff]);
    raw.extend(1_700_000_000u32.to_be_bytes());
    let (status, _, body) = post_logs(&app, Some(&token), &[MSGPACK], raw).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 1 })));

    let rows = stored_rows(&ch, project).await;
    assert_eq!(messages(&rows), ["mp bin", "mp one", "mp three", "mp two"]);
    let bin = rows.iter().find(|r| r["message"] == "mp bin").unwrap();
    assert_eq!(bin["fields"], r#"{"b":"AQID","t":"2023-11-14T22:13:20Z"}"#);
    assert_eq!(rows.iter().find(|r| r["message"] == "mp one").unwrap()["level"], "error");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn gzip_bodies_are_decompressed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let body = gzip(json!([{ "message": "gz one" }, { "message": "gz two" }]).to_string().as_bytes());
    let (status, _, json) = post_logs(&app, Some(&token), &[JSON, GZIP], body).await;
    assert_eq!((status, json), (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    let body = gzip(&msgpack(&json!({ "message": "gz three" })));
    let (status, _, json) = post_logs(&app, Some(&token), &[MSGPACK, ("content-encoding", "x-gzip")], body).await;
    assert_eq!((status, json), (StatusCode::ACCEPTED, json!({ "accepted": 1 })));
    let (status, _, json) = post_logs(&app, Some(&token), &[JSON, GZIP], "{\"message\":\"not gzip\"}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(json["error"].as_str().unwrap().starts_with("invalid gzip body"), "{json}");
    assert_eq!(messages(&stored_rows(&ch, project).await), ["gz one", "gz three", "gz two"]);
    ch.drop_database().await.unwrap();
}

/// Security: every token that is not a live source's is a generic 401 and
/// nothing is stored.
#[sqlx::test]
async fn missing_invalid_rotated_and_deleted_source_tokens_are_401(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (rotated_id, rotated_old) = seed_source(&pool, project, "rotated").await;
    let rotated_new = source::rotate_token(&pool, &test_cipher(), project, rotated_id).await.unwrap().unwrap().token;
    let (deleted_id, deleted_token) = seed_source(&pool, project, "deleted").await;
    assert!(source::delete_in_project(&pool, project, deleted_id).await.unwrap());
    let body = json!({ "message": "should not be stored" }).to_string();

    let (status, _, json) = post_logs(&app, None, &[JSON], body.clone()).await;
    assert_eq!((status, json), (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" })));
    for authorization in [format!("Basic {rotated_new}"), "Bearer".to_string(), format!("Bearer {rotated_new}x")] {
        let (status, _, json) =
            post_logs(&app, None, &[JSON, ("authorization", authorization.as_str())], body.clone()).await;
        assert_eq!((status, json), (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" })), "{authorization}");
    }
    for token in [rotated_old.as_str(), deleted_token.as_str(), "not-a-token"] {
        let (status, _, json) = post_logs(&app, Some(token), &[JSON], body.clone()).await;
        assert_eq!((status, json), (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" })), "{token}");
    }
    assert!(stored_rows(&ch, project).await.is_empty());
    assert_eq!(post_json(&app, &rotated_new, json!({ "message": "ok" })).await.0, StatusCode::ACCEPTED);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn oversized_bodies_gzip_bombs_and_too_many_events_are_413(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let rejected = |json: &Value, fragment: &str| {
        assert!(json["error"].as_str().unwrap().contains(fragment), "{json} should mention {fragment:?}");
    };

    let (status, _, json) = post_logs(&app, Some(&token), &[JSON], vec![b' '; 10 * 1024 * 1024 + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "request body exceeds 10 MiB");
    let (status, _, json) =
        post_logs(&app, Some(&token), &[JSON, ("content-length", "20971520")], "{\"message\":\"x\"}").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "a declared length is refused before reading");
    rejected(&json, "request body exceeds 10 MiB");

    // 51 gzip members of 1 MiB of zeros: ~50 KiB on the wire, 51 MiB inflated.
    let bomb = gzip(&vec![0u8; 1024 * 1024]).repeat(51);
    let (status, _, json) = post_logs(&app, Some(&token), &[JSON, GZIP], bomb).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "decompressed request body exceeds 50 MiB");

    let too_many = Value::Array(vec![json!({ "message": "m" }); 10_001]);
    let (status, json) = post_json(&app, &token, too_many).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "too many events: at most 10000 per request");
    let (status, _, json) = post_logs(&app, Some(&token), &[NDJSON], "{}\n".repeat(10_001)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");
    let declared_too_many = [0xdd, 0x00, 0x00, 0x27, 0x11];
    let (status, _, json) = post_logs(&app, Some(&token), &[MSGPACK], declared_too_many.to_vec()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");
    assert!(stored_rows(&ch, project).await.is_empty());

    let exactly = Value::Array(vec![json!({ "message": "m" }); 10_000]);
    assert_eq!(post_json(&app, &token, exactly).await, (StatusCode::ACCEPTED, json!({ "accepted": 10_000 })));
    assert_eq!(stored_rows(&ch, project).await.len(), 10_000);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn malformed_bodies_are_400_and_unsupported_formats_415(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let nested_json = |depth: usize| format!("{{\"a\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1));
    let nested_msgpack = |depth: usize| {
        let mut raw = vec![0x81, 0xa1, b'a'];
        raw.extend(vec![0x91; depth - 2]);
        raw.push(0x90);
        raw
    };

    for (headers, body, fragment) in [
        (vec![JSON], b"{\"message\":".to_vec(), "invalid JSON body"),
        (vec![JSON], b"\"just a string\"".to_vec(), "expected a JSON object or an array of objects"),
        (vec![JSON], b"[{\"a\":1}, 2]".to_vec(), "invalid JSON body"),
        (vec![NDJSON], b"{\"a\":1}\nnot json\n".to_vec(), "line 2"),
        (vec![JSON], b"   ".to_vec(), "request body is empty"),
        (vec![JSON], nested_json(128).into_bytes(), "recursion limit exceeded"),
        (vec![MSGPACK], nested_msgpack(128), "nesting deeper than 127"),
        (vec![MSGPACK], vec![0x81, 0x01, 0x02], "map keys must be strings"),
        (vec![MSGPACK], vec![0x82, 0xa1, b'a'], "unexpected end of input"),
    ] {
        let (status, _, json) = post_logs(&app, Some(&token), &headers, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}: {json}", String::from_utf8_lossy(&body));
        assert!(json["error"].as_str().unwrap().contains(fragment), "{json} should mention {fragment:?}");
    }
    let (status, _, json) = post_logs(&app, Some(&token), &[JSON], nested_json(127)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "127 levels are fine: {json}");
    let (status, _, json) = post_logs(&app, Some(&token), &[MSGPACK], nested_msgpack(127)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");

    for headers in [vec![("content-type", "application/x-protobuf")], vec![JSON, ("content-encoding", "br")]] {
        let (status, _, json) = post_logs(&app, Some(&token), &headers, "{}").await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{headers:?}: {json}");
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_rate_limit_is_per_source_and_answers_429_json(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    // A bucket of 3 that practically never refills, so the test is not timing-sensitive.
    let logs = Arc::new(LogsState { rate_limiter: TokenBuckets::new(0.0001, 3.0), ..LogsState::default() });
    let state = AppState { logs: logs.clone(), ..state };
    let app = app(state);
    let project = seed_project(&pool).await;
    let (limited, token) = seed_source(&pool, project, "busy").await;
    let (_, other_token) = seed_source(&pool, project, "quiet").await;

    for _ in 0..3 {
        assert_eq!(post_json(&app, &token, json!({ "message": "m" })).await.0, StatusCode::ACCEPTED);
    }
    let (status, headers, json) = post_logs(&app, Some(&token), &[JSON], "{\"message\":\"m\"}").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["code"], "rate_limited");
    assert!(json["error"].as_str().unwrap().contains("rate limit"), "{json}");
    assert_eq!(headers["retry-after"], "1");
    assert_eq!(post_json(&app, &other_token, json!({ "message": "m" })).await.0, StatusCode::ACCEPTED);

    logs.usage.flush(&pool).await.unwrap();
    let rate_limited: i64 =
        sqlx::query_scalar("select requests_rate_limited from source_usage_daily where source_id = $1")
            .bind(limited)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rate_limited, 1);
    assert_eq!(stored_rows(&ch, project).await.len(), 4);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn long_messages_and_large_fields_are_capped_on_ingest(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let event = json!({ "message": "m".repeat(70 * 1024), "stack": "s".repeat(300 * 1024), "user_id": 7 });
    assert_eq!(post_json(&app, &token, event).await.0, StatusCode::ACCEPTED);

    let row = &stored_rows(&ch, project).await[0];
    let message = row["message"].as_str().unwrap();
    assert!(message.len() <= MAX_MESSAGE_BYTES && message.ends_with(TRUNCATION_MARKER), "{}", message.len());
    let fields: Value = serde_json::from_str(row["fields"].as_str().unwrap()).unwrap();
    assert_eq!(fields["user_id"], 7);
    assert!(fields.get("stack").is_none());
    assert!(fields.get(FIELDS_TRUNCATED_KEY).is_some());
    ch.drop_database().await.unwrap();
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

/// Security: tokens never reach a log line — accepted, rejected, or when
/// storing fails and the error is logged.
#[sqlx::test]
async fn tokens_never_appear_in_log_output(pool: sqlx::PgPool) {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let working = app(state);
    assert_eq!(post_json(&working, &token, json!({ "message": "fine" })).await.0, StatusCode::ACCEPTED);
    let wrong = format!("{token}-wrong");
    assert_eq!(post_json(&working, &wrong, json!({ "message": "no" })).await.0, StatusCode::UNAUTHORIZED);
    // `test_app` points ClickHouse at a database that does not exist: the
    // insert fails and the error is logged.
    let broken = test_app(pool.clone());
    let (status, json) = post_json(&broken, &token, json!({ "message": "lost" })).await;
    assert_eq!((status, json), (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "internal error" })));

    let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("clickhouse error"), "the capture works: {output}");
    assert!(!output.contains(&token), "a token leaked into the logs:\n{output}");
    assert!(!output.contains(&wrong), "a rejected token leaked into the logs:\n{output}");
    ch.drop_database().await.unwrap();
}
```

`nami-engine/tests/logs_quota.rs` (usage counting, the usage endpoint, the quota, flush failure and pruning):

```rust
// Per-source ingestion stats (`source_usage_daily`) and the daily byte
// quota. Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use chrono::Utc;
use common::{
    app, call,
    logs::{post_json, post_logs, seed_source, seed_source_with, source_config, stored_rows},
    seed_project, state_with_clickhouse, user_with_role,
};
use nami_engine::{
    logs::{
        usage::{self, UsageCounters},
        LogsState,
    },
    models::source::SourceConfig,
    state::AppState,
};
use serde_json::{json, Value};
use uuid::Uuid;

async fn usage_row(pool: &sqlx::PgPool, source_id: Uuid) -> UsageCounters {
    sqlx::query_as(
        "select events, bytes, events_dropped, events_sampled_out, requests_rate_limited, requests_over_quota
         from source_usage_daily where source_id = $1 and day = $2",
    )
    .bind(source_id)
    .bind(Utc::now().date_naive())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Bytes as ClickHouse stores them: UTF-8 length of message + fields.
async fn stored_bytes(ch: &nami_engine::clickhouse::ClickHouse, source_id: Uuid) -> i64 {
    let rows: Vec<Value> = ch
        .query_rows(
            "SELECT sum(length(message) + length(fields)) AS n FROM logs WHERE source_id = {s:UUID}",
            &[("s", source_id.to_string())],
        )
        .await
        .unwrap();
    rows[0]["n"].as_i64().unwrap()
}

#[sqlx::test]
async fn accepted_events_and_bytes_are_counted_per_source_and_day(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (a, token_a) = seed_source(&pool, project, "a").await;
    let (b, token_b) = seed_source(&pool, project, "b").await;

    post_json(&app, &token_a, json!([{ "message": "one" }, { "message": "two", "k": "v" }])).await;
    post_json(&app, &token_a, json!({ "message": "three" })).await;
    post_json(&app, &token_b, json!({ "message": "four" })).await;
    let pending = logs.usage.pending(a, Utc::now().date_naive());
    assert_eq!(pending.events, 3, "counted in memory first");
    assert_eq!(logs.usage.flush(&pool).await.unwrap(), 2, "one row per source and day");
    assert_eq!(logs.usage.pending(a, Utc::now().date_naive()), UsageCounters::default());

    let counted = usage_row(&pool, a).await;
    assert_eq!(counted.events, 3);
    assert_eq!(counted.bytes, stored_bytes(&ch, a).await, "bytes = message + fields as stored");
    assert_eq!(usage_row(&pool, b).await.events, 1);

    // Flushes add up (another instance's flush would too).
    post_json(&app, &token_a, json!({ "message": "five" })).await;
    logs.usage.flush(&pool).await.unwrap();
    assert_eq!(usage_row(&pool, a).await.events, 4);
    assert_eq!(usage_row(&pool, a).await.bytes, stored_bytes(&ch, a).await);
    assert_eq!(logs.usage.flush(&pool).await.unwrap(), 0, "nothing pending, nothing written");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_usage_endpoint_returns_zero_filled_days_for_viewers(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (a, token) = seed_source(&pool, project, "a").await;
    let (foreign, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    post_json(&app, &token, json!([{ "message": "one" }, { "message": "two" }])).await;
    logs.usage.flush(&pool).await.unwrap();

    let base = format!("/projects/{project}/sources/{a}/usage");
    let (status, days) = call(&app, "GET", &format!("{base}?days=3"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{days}");
    let days = days.as_array().unwrap();
    assert_eq!(days.len(), 3);
    let today = Utc::now().date_naive();
    assert_eq!(days[2]["day"], today.to_string());
    assert_eq!(days[2]["events"], 2);
    assert_eq!(days[2]["bytes"], stored_bytes(&ch, a).await);
    assert_eq!(days[0]["day"], (today - chrono::Duration::days(2)).to_string());
    assert_eq!(days[0]["events"], 0);
    for key in ["events_dropped", "events_sampled_out", "requests_rate_limited", "requests_over_quota"] {
        assert_eq!(days[2][key], 0, "{key}");
    }

    let (_, default) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!(default.as_array().unwrap().len(), 30);
    for query in ["?days=0", "?days=401"] {
        let (status, json) = call(&app, "GET", &format!("{base}{query}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {json}");
    }
    assert_eq!(call(&app, "GET", &base, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    let foreign_usage = format!("/projects/{project}/sources/{foreign}/usage");
    assert_eq!(call(&app, "GET", &foreign_usage, Some(&viewer), None).await.0, StatusCode::NOT_FOUND);
    ch.drop_database().await.unwrap();
}

/// Security: a source over its quota is refused with a distinct 429 while
/// the project's other sources keep ingesting.
#[sqlx::test]
async fn a_source_over_its_quota_gets_429_quota_exceeded_and_others_keep_ingesting(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let quota = SourceConfig { daily_byte_quota: Some(1024 * 1024), ..source_config("capped") };
    let (capped, token) = seed_source_with(&pool, project, quota).await;
    let (_, other_token) = seed_source(&pool, project, "free").await;

    // 20 events x 64 KiB messages = 1.25 MiB: the request that crosses the
    // quota is accepted whole.
    let big = Value::Array(vec![json!({ "message": "x".repeat(64 * 1024) }); 20]);
    assert_eq!(post_json(&app, &token, big).await, (StatusCode::ACCEPTED, json!({ "accepted": 20 })));

    let (status, headers, json) =
        post_logs(&app, Some(&token), &[("content-type", "application/json")], "{\"message\":\"m\"}").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["code"], "quota_exceeded");
    let retry_after: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=86_400).contains(&retry_after), "{retry_after}");
    assert_eq!(post_json(&app, &other_token, json!({ "message": "m" })).await.0, StatusCode::ACCEPTED);

    logs.usage.flush(&pool).await.unwrap();
    let counted = usage_row(&pool, capped).await;
    assert_eq!((counted.events, counted.requests_over_quota), (20, 1));
    assert!(counted.bytes >= 1024 * 1024);

    // Another instance (fresh in-memory state) reads the flushed bytes.
    let fresh = app_with_fresh_logs(&state);
    assert_eq!(post_json(&fresh, &token, json!({ "message": "m" })).await.0, StatusCode::TOO_MANY_REQUESTS);

    // Raising the quota takes effect on the next request.
    sqlx::query("update sources set daily_byte_quota = 104857600 where id = $1")
        .bind(capped)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post_json(&app, &token, json!({ "message": "m" })).await.0, StatusCode::ACCEPTED);
    assert_eq!(stored_rows(&ch, project).await.len(), 22);
    ch.drop_database().await.unwrap();
}

fn app_with_fresh_logs(state: &AppState) -> axum::Router {
    app(AppState { logs: Arc::new(LogsState::default()), ..state.clone() })
}

#[sqlx::test]
async fn counters_of_deleted_sources_are_dropped_on_flush(pool: sqlx::PgPool) {
    let tracker = usage::UsageTracker::default();
    tracker.record_accepted(Uuid::new_v4(), Utc::now().date_naive(), 5, 500);
    assert_eq!(tracker.flush(&pool).await.unwrap(), 0);
    let rows: i64 = sqlx::query_scalar("select count(*) from source_usage_daily").fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test]
async fn a_failed_flush_keeps_every_counter_for_the_next_one(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    let tracker = usage::UsageTracker::default();
    tracker.record_accepted(source_id, today, 3, 300);
    tracker.record_rate_limited(source_id, today);

    let unreachable = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy("postgres://nami:nami@127.0.0.1:1/nami")
        .unwrap();
    assert!(tracker.flush(&unreachable).await.is_err());
    let kept = tracker.pending(source_id, today);
    assert_eq!((kept.events, kept.bytes, kept.requests_rate_limited), (3, 300, 1));

    assert_eq!(tracker.flush(&pool).await.unwrap(), 1);
    let counted = usage_row(&pool, source_id).await;
    assert_eq!((counted.events, counted.bytes, counted.requests_rate_limited), (3, 300, 1));
}

#[sqlx::test]
async fn usage_older_than_400_days_is_pruned(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    for age in [401, 400, 399] {
        sqlx::query("insert into source_usage_daily (source_id, day, events) values ($1, $2, 1)")
            .bind(source_id)
            .bind(today - chrono::Duration::days(age))
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(usage::prune(&pool, today).await.unwrap(), 2);
    let oldest: chrono::NaiveDate =
        sqlx::query_scalar("select min(day) from source_usage_daily").fetch_one(&pool).await.unwrap();
    assert_eq!(oldest, today - chrono::Duration::days(399), "400 days are kept, today included");
}
```

```diff
diff --git a/nami-engine/tests/migrations.rs b/nami-engine/tests/migrations.rs
--- a/nami-engine/tests/migrations.rs
+++ b/nami-engine/tests/migrations.rs
@@ -21,6 +21,7 @@ async fn all_tables_exist(pool: sqlx::PgPool) {
         "maintenance_windows",
         "monitor_groups",
         "sources",
+        "source_usage_daily",
     ] {
         assert!(names.contains(&expected.to_string()), "missing table {expected}");
     }
```

- [ ] **Step 4: Run them to see them fail**

Run: `cd nami-engine && cargo test --test logs_ingest --test logs_quota`
Expected: FAIL to compile — `error[E0432]: unresolved imports nami_engine::logs::rate_limit, nami_engine::logs::LogsState` (and `logs::usage`).

- [ ] **Step 5: Add the usage table**

`nami-engine/migrations/0041_create_source_usage_daily.sql`:

```sql
-- Per source and UTC day ingestion counters. Engine instances count in
-- memory and add their deltas here every 10 s (additive upsert); rows are
-- kept 400 days.
create table source_usage_daily (
    source_id uuid not null references sources(id) on delete cascade,
    day date not null,
    -- Accepted events and their bytes (UTF-8 length of message + fields).
    events bigint not null default 0,
    bytes bigint not null default 0,
    -- Ingest pipeline outcomes (the pipeline arrives with Logs part 2).
    events_dropped bigint not null default 0,
    events_sampled_out bigint not null default 0,
    -- Rejected requests (HTTP) and frames (syslog).
    requests_rate_limited bigint not null default 0,
    requests_over_quota bigint not null default 0,
    primary key (source_id, day)
);

create index source_usage_daily_day_idx on source_usage_daily (day);
```

- [ ] **Step 6: Implement the MessagePack decoder**

`nami-engine/src/logs/msgpack.rs` (unit tests: standard encoder round trip, bin/extension mapping, huge declared lengths, nesting, every truncation, 20,000 random inputs):

```rust
//! A small MessagePack decoder for ingest bodies (`application/msgpack`):
//! one map, or an array of maps, into `serde_json` values. Written for
//! untrusted input:
//! - declared lengths are checked against the bytes actually left and
//!   never used to preallocate, so "2^32 elements" in a 6-byte body fails
//!   at once instead of allocating;
//! - at most [`MAX_NESTING`] arrays/maps nest (the same count serde_json
//!   applies to JSON bodies);
//! - map keys must be strings; strings must be UTF-8;
//! - `bin` values become base64 strings, timestamp extensions (type -1)
//!   RFC 3339 strings, other extensions `null`; NaN/infinite floats `null`.

use base64::Engine;
use chrono::{DateTime, SecondsFormat};
use serde_json::{Map, Number, Value};

/// Arrays/maps may nest this deep, counting the outermost one (serde_json's
/// recursion limit for JSON bodies is the same).
pub const MAX_NESTING: usize = 127;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MsgpackError {
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("reserved marker byte 0xc1")]
    ReservedMarker,
    #[error("map keys must be strings")]
    NonStringKey,
    #[error("nesting deeper than {MAX_NESTING} arrays/maps")]
    TooDeep,
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("trailing bytes after the value")]
    TrailingBytes,
    #[error("expected a map or an array of maps")]
    NotAnObject,
    #[error("too many events")]
    TooManyEvents,
}

type Result<T> = std::result::Result<T, MsgpackError>;

/// Decodes exactly one value (no trailing bytes).
pub fn decode_value(input: &[u8]) -> Result<Value> {
    let mut decoder = Decoder { input, pos: 0 };
    let value = decoder.value(0)?;
    decoder.finish()?;
    Ok(value)
}

/// The events of an ingest body: one map, or an array of at most
/// `max_events` maps.
pub fn decode_events(input: &[u8], max_events: usize) -> Result<Vec<Map<String, Value>>> {
    let mut decoder = Decoder { input, pos: 0 };
    let events = match decoder.peek()? {
        marker if is_map(marker) => vec![decoder.object(0)?],
        marker if is_array(marker) => {
            decoder.pos += 1;
            let len = decoder.array_len(marker)?;
            if len > max_events {
                return Err(MsgpackError::TooManyEvents);
            }
            decoder.check_items(len, 1)?;
            let mut events = Vec::new();
            for _ in 0..len {
                events.push(decoder.object(1)?);
            }
            events
        }
        _ => return Err(MsgpackError::NotAnObject),
    };
    decoder.finish()?;
    Ok(events)
}

fn is_map(marker: u8) -> bool {
    matches!(marker, 0x80..=0x8f | 0xde | 0xdf)
}

fn is_array(marker: u8) -> bool {
    matches!(marker, 0x90..=0x9f | 0xdc | 0xdd)
}

fn is_str(marker: u8) -> bool {
    matches!(marker, 0xa0..=0xbf | 0xd9..=0xdb)
}

struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn remaining(&self) -> usize {
        self.input.len() - self.pos
    }

    fn finish(&self) -> Result<()> {
        if self.remaining() == 0 { Ok(()) } else { Err(MsgpackError::TrailingBytes) }
    }

    fn peek(&self) -> Result<u8> {
        self.input.get(self.pos).copied().ok_or(MsgpackError::UnexpectedEnd)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.remaining() {
            return Err(MsgpackError::UnexpectedEnd);
        }
        let bytes = &self.input[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn be<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returns exactly N bytes"))
    }

    fn u16(&mut self) -> Result<usize> {
        Ok(u16::from_be_bytes(self.be()?) as usize)
    }

    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_be_bytes(self.be()?) as usize)
    }

    /// Each item needs at least `min_bytes`: a declared count the input
    /// cannot possibly hold fails before any work.
    fn check_items(&self, count: usize, min_bytes: usize) -> Result<()> {
        if count.saturating_mul(min_bytes) > self.remaining() { Err(MsgpackError::UnexpectedEnd) } else { Ok(()) }
    }

    fn array_len(&mut self, marker: u8) -> Result<usize> {
        match marker {
            0x90..=0x9f => Ok((marker & 0x0f) as usize),
            0xdc => self.u16(),
            _ => self.u32(),
        }
    }

    fn map_len(&mut self, marker: u8) -> Result<usize> {
        match marker {
            0x80..=0x8f => Ok((marker & 0x0f) as usize),
            0xde => self.u16(),
            _ => self.u32(),
        }
    }

    /// A map at nesting `depth` (the number of containers around it).
    fn object(&mut self, depth: usize) -> Result<Map<String, Value>> {
        let marker = self.byte()?;
        if !is_map(marker) {
            return Err(MsgpackError::NotAnObject);
        }
        let len = self.map_len(marker)?;
        self.map_entries(len, depth)
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        let marker = self.byte()?;
        Ok(match marker {
            0x00..=0x7f => Value::from(marker),
            0x80..=0x8f | 0xde | 0xdf => {
                let len = self.map_len(marker)?;
                Value::Object(self.map_entries(len, depth)?)
            }
            0x90..=0x9f | 0xdc | 0xdd => {
                let len = self.array_len(marker)?;
                self.array_items(len, depth)?
            }
            0xa0..=0xbf => self.str((marker & 0x1f) as usize)?,
            0xc0 => Value::Null,
            0xc1 => return Err(MsgpackError::ReservedMarker),
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xc4 => {
                let len = self.byte()? as usize;
                self.bin(len)?
            }
            0xc5 => {
                let len = self.u16()?;
                self.bin(len)?
            }
            0xc6 => {
                let len = self.u32()?;
                self.bin(len)?
            }
            0xc7 => {
                let len = self.byte()? as usize;
                self.ext(len)?
            }
            0xc8 => {
                let len = self.u16()?;
                self.ext(len)?
            }
            0xc9 => {
                let len = self.u32()?;
                self.ext(len)?
            }
            0xca => float(f32::from_be_bytes(self.be()?) as f64),
            0xcb => float(f64::from_be_bytes(self.be()?)),
            0xcc => Value::from(self.byte()?),
            0xcd => Value::from(u16::from_be_bytes(self.be()?)),
            0xce => Value::from(u32::from_be_bytes(self.be()?)),
            0xcf => Value::from(u64::from_be_bytes(self.be()?)),
            0xd0 => Value::from(i8::from_be_bytes(self.be()?)),
            0xd1 => Value::from(i16::from_be_bytes(self.be()?)),
            0xd2 => Value::from(i32::from_be_bytes(self.be()?)),
            0xd3 => Value::from(i64::from_be_bytes(self.be()?)),
            0xd4 => self.ext(1)?,
            0xd5 => self.ext(2)?,
            0xd6 => self.ext(4)?,
            0xd7 => self.ext(8)?,
            0xd8 => self.ext(16)?,
            0xd9 => {
                let len = self.byte()? as usize;
                self.str(len)?
            }
            0xda => {
                let len = self.u16()?;
                self.str(len)?
            }
            0xdb => {
                let len = self.u32()?;
                self.str(len)?
            }
            0xe0..=0xff => Value::from(marker as i8),
        })
    }

    fn array_items(&mut self, len: usize, depth: usize) -> Result<Value> {
        if depth + 1 > MAX_NESTING {
            return Err(MsgpackError::TooDeep);
        }
        self.check_items(len, 1)?;
        let mut items = Vec::new();
        for _ in 0..len {
            items.push(self.value(depth + 1)?);
        }
        Ok(Value::Array(items))
    }

    fn map_entries(&mut self, len: usize, depth: usize) -> Result<Map<String, Value>> {
        if depth + 1 > MAX_NESTING {
            return Err(MsgpackError::TooDeep);
        }
        self.check_items(len, 2)?;
        let mut map = Map::new();
        for _ in 0..len {
            // Reject a non-string key before decoding it (it may be a deep map).
            let marker = self.peek()?;
            if !is_str(marker) {
                return Err(MsgpackError::NonStringKey);
            }
            let Value::String(key) = self.value(depth + 1)? else {
                return Err(MsgpackError::NonStringKey);
            };
            let value = self.value(depth + 1)?;
            map.insert(key, value);
        }
        Ok(map)
    }

    fn str(&mut self, len: usize) -> Result<Value> {
        let bytes = self.take(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| MsgpackError::InvalidUtf8)?;
        Ok(Value::String(text.to_string()))
    }

    fn bin(&mut self, len: usize) -> Result<Value> {
        let bytes = self.take(len)?;
        Ok(Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)))
    }

    fn ext(&mut self, len: usize) -> Result<Value> {
        let ext_type = self.byte()? as i8;
        let data = self.take(len)?;
        if ext_type != -1 {
            return Ok(Value::Null);
        }
        Ok(timestamp(data).map(Value::String).unwrap_or(Value::Null))
    }
}

fn float(value: f64) -> Value {
    Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
}

/// The MessagePack timestamp extension (32-, 64- and 96-bit forms) as an
/// RFC 3339 string; `None` for an invalid length or value.
fn timestamp(data: &[u8]) -> Option<String> {
    let (seconds, nanos) = match data.len() {
        4 => (u32::from_be_bytes(data.try_into().ok()?) as i64, 0u32),
        8 => {
            let raw = u64::from_be_bytes(data.try_into().ok()?);
            ((raw & 0x3_ffff_ffff) as i64, (raw >> 34) as u32)
        }
        12 => (
            i64::from_be_bytes(data[4..].try_into().ok()?),
            u32::from_be_bytes(data[..4].try_into().ok()?),
        ),
        _ => return None,
    };
    if nanos > 999_999_999 {
        return None;
    }
    let dt = DateTime::from_timestamp(seconds, nanos)?;
    Some(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn encode(value: &Value) -> Vec<u8> {
        rmp_serde::to_vec(value).unwrap()
    }

    #[test]
    fn decodes_what_a_standard_encoder_produces() {
        let value = json!({
            "message": "hello",
            "n": [0, 1, -1, 127, 128, -33, 65535, 4_294_967_296_u64, -9_000_000_000_i64, 1.5],
            "nested": { "ok": true, "none": null, "empty": {} },
            "long": "x".repeat(70_000)
        });
        assert_eq!(decode_value(&encode(&value)).unwrap(), value);
    }

    #[test]
    fn bin_becomes_base64_and_extensions_become_timestamps_or_null() {
        // { "b": bin8 [1,2,3], "t32": ts32(1), "t64": ts64(1 s + 500 ms), "t96": ts96(-1 s), "x": ext(5) }
        let mut input = vec![0x85];
        input.extend([0xa1, b'b', 0xc4, 3, 1, 2, 3]);
        input.extend([0xa3, b't', b'3', b'2', 0xd6, 0xff, 0, 0, 0, 1]);
        let ts64: u64 = (500_000_000u64 << 34) | 1;
        input.extend([0xa3, b't', b'6', b'4', 0xd7, 0xff]);
        input.extend(ts64.to_be_bytes());
        input.extend([0xa3, b't', b'9', b'6', 0xc7, 12, 0xff, 0, 0, 0, 0]);
        input.extend((-1i64).to_be_bytes());
        input.extend([0xa1, b'x', 0xd4, 5, 0]);
        assert_eq!(
            decode_value(&input).unwrap(),
            json!({
                "b": "AQID",
                "t32": "1970-01-01T00:00:01Z",
                "t64": "1970-01-01T00:00:01.500Z",
                "t96": "1969-12-31T23:59:59Z",
                "x": null
            })
        );
    }

    #[test]
    fn invalid_timestamps_and_non_finite_floats_become_null() {
        // ts64 with nanoseconds > 999,999,999, then a 3-byte ts (no such form).
        let bad_nanos: u64 = (1_000_000_000u64 << 34) | 1;
        let mut input = vec![0x93, 0xd7, 0xff];
        input.extend(bad_nanos.to_be_bytes());
        input.extend([0xc7, 3, 0xff, 1, 2, 3]);
        input.push(0xcb);
        input.extend(f64::NAN.to_be_bytes());
        assert_eq!(decode_value(&input).unwrap(), json!([null, null, null]));
    }

    #[test]
    fn events_are_one_map_or_an_array_of_maps() {
        let one = encode(&json!({ "message": "a" }));
        assert_eq!(decode_events(&one, 10).unwrap().len(), 1);
        let many = encode(&json!([{ "message": "a" }, { "message": "b" }]));
        assert_eq!(decode_events(&many, 10).unwrap().len(), 2);
        assert_eq!(decode_events(&many, 1), Err(MsgpackError::TooManyEvents));
        assert_eq!(decode_events(&encode(&json!([1])), 10), Err(MsgpackError::NotAnObject));
        assert_eq!(decode_events(&encode(&json!("text")), 10), Err(MsgpackError::NotAnObject));
    }

    #[test]
    fn non_string_keys_are_rejected() {
        // { 1: "a" } and { {}: 1 }
        assert_eq!(decode_value(&[0x81, 0x01, 0xa1, b'a']), Err(MsgpackError::NonStringKey));
        assert_eq!(decode_value(&[0x81, 0x80, 0x01]), Err(MsgpackError::NonStringKey));
    }

    #[test]
    fn huge_declared_lengths_fail_without_allocating() {
        for input in [
            vec![0xdd, 0xff, 0xff, 0xff, 0xff],             // array32 of 2^32-1
            vec![0xdf, 0xff, 0xff, 0xff, 0xff, 0xa0],       // map32
            vec![0xdb, 0xff, 0xff, 0xff, 0xff, b'a'],       // str32
            vec![0xc6, 0xff, 0xff, 0xff, 0xff, 0],          // bin32
            vec![0xc9, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00], // ext32
        ] {
            assert_eq!(decode_value(&input), Err(MsgpackError::UnexpectedEnd), "{input:?}");
        }
        assert_eq!(decode_events(&[0xdd, 0x00, 0x00, 0x27, 0x11], 10_000), Err(MsgpackError::TooManyEvents));
        assert_eq!(decode_events(&[0xdd, 0x00, 0x00, 0x27, 0x10], 10_000), Err(MsgpackError::UnexpectedEnd));
    }

    #[test]
    fn nesting_is_limited_like_json() {
        let nested = |depth: usize| {
            let mut input = vec![0x91; depth - 1];
            input.push(0x90);
            input
        };
        assert!(decode_value(&nested(MAX_NESTING)).is_ok());
        assert_eq!(decode_value(&nested(MAX_NESTING + 1)), Err(MsgpackError::TooDeep));
        let as_json = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(serde_json::from_str::<Value>(&as_json(MAX_NESTING)).is_ok());
        assert!(serde_json::from_str::<Value>(&as_json(MAX_NESTING + 1)).is_err(), "serde_json agrees");
    }

    #[test]
    fn invalid_utf8_reserved_markers_and_trailing_bytes_are_errors() {
        assert_eq!(decode_value(&[0xa2, 0xff, 0xfe]), Err(MsgpackError::InvalidUtf8));
        assert_eq!(decode_value(&[0xc1]), Err(MsgpackError::ReservedMarker));
        assert_eq!(decode_value(&[0xc0, 0xc0]), Err(MsgpackError::TrailingBytes));
        assert_eq!(decode_value(&[]), Err(MsgpackError::UnexpectedEnd));
    }

    #[test]
    fn every_truncation_of_a_valid_body_is_an_error_not_a_panic() {
        let input = encode(&json!([{ "message": "hello", "n": [1, 2.5, -70000], "m": { "k": "v" } }]));
        for len in 0..input.len() {
            assert!(decode_events(&input[..len], 100).is_err(), "prefix of {len} bytes");
        }
        assert!(decode_events(&input, 100).is_ok());
    }

    #[test]
    fn random_bytes_never_panic() {
        // xorshift: deterministic, no extra dependency.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = decode_value(&bytes);
            let _ = decode_events(&bytes, 10_000);
        }
    }
}
```

- [ ] **Step 7: Implement body handling**

`nami-engine/src/logs/ingest.rs`:

```rust
//! HTTP ingest bodies: authentication header, content negotiation, size
//! limits, gzip, and parsing into events. The handler is
//! `routes::logs_ingest`; everything here except [`read_body`] is
//! synchronous and runs on a blocking thread.
//!
//! Formats: JSON — one object, an array of objects, or several of those
//! separated by whitespace, which covers NDJSON (`application/x-ndjson`)
//! and shippers that send NDJSON as `application/json` — and MessagePack
//! (`application/msgpack`, `application/x-msgpack`): one map or an array of
//! maps. Either may be gzip-compressed (`Content-Encoding: gzip`).

use std::{fmt, io::Read};

use axum::{
    body::{Body, Bytes},
    http::{
        header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};

use super::msgpack::{self, MsgpackError};
use crate::error::AppError;

/// Largest accepted request body, as sent (compressed or not).
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// Largest accepted body after gzip decompression (gzip-bomb guard).
pub const MAX_DECOMPRESSED_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_EVENTS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("daily byte quota exceeded")]
    QuotaExceeded { retry_after_seconds: u64 },
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("{0}")]
    BadRequest(String),
    /// A failure already logged where it happened; the client sees a
    /// generic 500.
    #[error("internal error")]
    Internal,
    #[error(transparent)]
    App(#[from] AppError),
}

impl From<sqlx::Error> for IngestError {
    fn from(error: sqlx::Error) -> Self {
        IngestError::App(AppError::Database(error))
    }
}

impl From<crate::clickhouse::ClickHouseError> for IngestError {
    fn from(error: crate::clickhouse::ClickHouseError) -> Self {
        IngestError::App(AppError::ClickHouse(error))
    }
}

impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let (status, body, retry_after) = match self {
            IngestError::Unauthorized => return AppError::Unauthorized.into_response(),
            IngestError::App(error) => return error.into_response(),
            IngestError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({ "error": "rate limit exceeded: at most 50 requests per second per source", "code": "rate_limited" }),
                Some(1),
            ),
            IngestError::QuotaExceeded { retry_after_seconds } => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({ "error": "this source has reached its daily byte quota", "code": "quota_exceeded" }),
                Some(retry_after_seconds),
            ),
            IngestError::PayloadTooLarge(message) => (StatusCode::PAYLOAD_TOO_LARGE, json!({ "error": message }), None),
            IngestError::UnsupportedMediaType(message) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, json!({ "error": message }), None)
            }
            IngestError::BadRequest(message) => (StatusCode::BAD_REQUEST, json!({ "error": message }), None),
            IngestError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "internal error" }), None),
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            response.headers_mut().insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    Json,
    MessagePack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyEncoding {
    pub format: BodyFormat,
    pub gzip: bool,
}

/// The token of an `Authorization: Bearer <token>` header (scheme
/// case-insensitive).
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn header(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_ascii_lowercase())
}

pub fn body_encoding(headers: &HeaderMap) -> Result<BodyEncoding, IngestError> {
    let gzip = match header(headers, CONTENT_ENCODING).as_deref() {
        None | Some("") | Some("identity") => false,
        Some("gzip") | Some("x-gzip") => true,
        Some(_) => {
            return Err(IngestError::UnsupportedMediaType(
                "unsupported Content-Encoding: send gzip or an uncompressed body".to_string(),
            ));
        }
    };
    let media_type = header(headers, CONTENT_TYPE).map(|value| value.split(';').next().unwrap_or("").trim().to_string());
    let format = match media_type.as_deref() {
        None
        | Some("")
        | Some("application/json")
        | Some("text/json")
        | Some("text/plain")
        | Some("application/x-www-form-urlencoded")
        | Some("application/x-ndjson")
        | Some("application/ndjson")
        | Some("application/jsonl")
        | Some("application/jsonlines")
        | Some("application/x-jsonlines") => BodyFormat::Json,
        Some("application/msgpack") | Some("application/x-msgpack") | Some("application/vnd.msgpack") => {
            BodyFormat::MessagePack
        }
        Some(_) => {
            return Err(IngestError::UnsupportedMediaType(
                "unsupported Content-Type: send application/json, application/x-ndjson or application/msgpack"
                    .to_string(),
            ));
        }
    };
    Ok(BodyEncoding { format, gzip })
}

fn body_too_large() -> IngestError {
    IngestError::PayloadTooLarge(format!("request body exceeds {} MiB", MAX_BODY_BYTES / (1024 * 1024)))
}

/// Reads at most [`MAX_BODY_BYTES`]; a larger declared `Content-Length` is
/// refused before reading anything.
pub async fn read_body(body: Body, headers: &HeaderMap) -> Result<Bytes, IngestError> {
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return Err(body_too_large());
    }
    axum::body::to_bytes(body, MAX_BODY_BYTES).await.map_err(|error| {
        if is_length_limit(&error) {
            body_too_large()
        } else {
            IngestError::BadRequest("could not read the request body".to_string())
        }
    })
}

fn is_length_limit(error: &axum::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = current {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        current = e.source();
    }
    false
}

/// Decompresses (when gzip) and parses a body into event objects.
pub fn decode_events(body: Bytes, encoding: BodyEncoding) -> Result<Vec<Map<String, Value>>, IngestError> {
    let raw = if encoding.gzip { Bytes::from(gunzip(&body)?) } else { body };
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Err(IngestError::BadRequest("request body is empty".to_string()));
    }
    match encoding.format {
        BodyFormat::Json => parse_json_events(&raw),
        BodyFormat::MessagePack => msgpack::decode_events(&raw, MAX_EVENTS).map_err(|error| match error {
            MsgpackError::TooManyEvents => too_many_events(),
            other => IngestError::BadRequest(format!("invalid MessagePack body: {other}")),
        }),
    }
}

fn too_many_events() -> IngestError {
    IngestError::PayloadTooLarge(format!("too many events: at most {MAX_EVENTS} per request"))
}

/// gzip (multi-member) with the output capped at [`MAX_DECOMPRESSED_BYTES`].
pub fn gunzip(compressed: &[u8]) -> Result<Vec<u8>, IngestError> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(compressed)
        .take(MAX_DECOMPRESSED_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| IngestError::BadRequest(format!("invalid gzip body: {e}")))?;
    if out.len() > MAX_DECOMPRESSED_BYTES {
        return Err(IngestError::PayloadTooLarge(format!(
            "decompressed request body exceeds {} MiB",
            MAX_DECOMPRESSED_BYTES / (1024 * 1024)
        )));
    }
    Ok(out)
}

/// A whitespace-separated sequence of JSON objects or arrays of objects,
/// failing fast on the event after the [`MAX_EVENTS`]th.
pub fn parse_json_events(raw: &[u8]) -> Result<Vec<Map<String, Value>>, IngestError> {
    let mut events = Vec::new();
    let mut overflow = false;
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    // `end` succeeds once only whitespace is left; otherwise it has skipped
    // the whitespace before the next value.
    while deserializer.end().is_err() {
        let seed = TopLevel { events: &mut events, overflow: &mut overflow };
        if let Err(error) = seed.deserialize(&mut deserializer) {
            return Err(if overflow {
                too_many_events()
            } else {
                IngestError::BadRequest(format!("invalid JSON body: {error}"))
            });
        }
    }
    Ok(events)
}

/// One top-level JSON value: an object (one event) or an array of objects.
struct TopLevel<'a> {
    events: &'a mut Vec<Map<String, Value>>,
    overflow: &'a mut bool,
}

impl TopLevel<'_> {
    fn push<E: de::Error>(&mut self, event: Map<String, Value>) -> Result<(), E> {
        if self.events.len() >= MAX_EVENTS {
            *self.overflow = true;
            return Err(E::custom("too many events"));
        }
        self.events.push(event);
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for TopLevel<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TopLevel<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object or an array of objects")
    }

    fn visit_map<A: MapAccess<'de>>(mut self, map: A) -> Result<(), A::Error> {
        let event = serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        self.push(event)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(event) = seq.next_element::<Map<String, Value>>()? {
            self.push(event)?;
        }
        Ok(())
    }
}

/// Seconds until the next UTC midnight (at least 1), for `Retry-After` on a
/// quota rejection.
pub fn seconds_until_utc_midnight(now: DateTime<Utc>) -> u64 {
    let next_midnight = now
        .date_naive()
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|midnight| midnight.and_utc())
        .unwrap_or(now);
    (next_midnight - now).num_seconds().max(1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn bearer_tokens_are_read_case_insensitively() {
        assert_eq!(bearer_token(&headers(&[("authorization", "Bearer abc")])), Some("abc"));
        assert_eq!(bearer_token(&headers(&[("authorization", "bearer   abc  ")])), Some("abc"));
        for value in ["Basic abc", "Bearer", "Bearer ", "abc", "Token abc"] {
            assert_eq!(bearer_token(&headers(&[("authorization", value)])), None, "{value}");
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn content_types_and_encodings_select_the_parser() {
        let json = BodyEncoding { format: BodyFormat::Json, gzip: false };
        for content_type in [None, Some("application/json; charset=utf-8"), Some("application/x-ndjson"), Some("text/plain")] {
            let h = content_type.map(|ct| headers(&[("content-type", ct)])).unwrap_or_default();
            assert_eq!(body_encoding(&h).unwrap(), json, "{content_type:?}");
        }
        for content_type in ["application/msgpack", "Application/X-MsgPack"] {
            let h = headers(&[("content-type", content_type), ("content-encoding", "gzip")]);
            assert_eq!(body_encoding(&h).unwrap(), BodyEncoding { format: BodyFormat::MessagePack, gzip: true });
        }
        for (name, value) in [("content-type", "application/x-protobuf"), ("content-encoding", "br")] {
            assert!(matches!(body_encoding(&headers(&[(name, value)])), Err(IngestError::UnsupportedMediaType(_))));
        }
    }

    #[test]
    fn json_bodies_may_be_objects_arrays_or_ndjson() {
        let events = parse_json_events(br#"{"a":1}"#).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(parse_json_events(br#"[{"a":1},{"b":2}]"#).unwrap().len(), 2);
        let ndjson = b"{\"a\":1}\n\n{\"b\":2}\r\n[{\"c\":3}]\n";
        assert_eq!(parse_json_events(ndjson).unwrap().len(), 3);
        assert_eq!(parse_json_events(b"[]").unwrap().len(), 0);
    }

    #[test]
    fn malformed_json_bodies_are_bad_requests() {
        for body in [&b"{"[..], b"\"text\"", b"42", b"[1]", b"[{\"a\":1}, 2]", b"{\"a\":1} x", b"{\"a\":1}}"] {
            match parse_json_events(body) {
                Err(IngestError::BadRequest(message)) => assert!(message.starts_with("invalid JSON body"), "{message}"),
                other => panic!("{:?}: {other:?}", String::from_utf8_lossy(body)),
            }
        }
    }

    #[test]
    fn the_event_after_the_limit_is_413_whatever_the_framing() {
        let array = format!("[{}]", vec!["{}"; MAX_EVENTS].join(","));
        assert_eq!(parse_json_events(array.as_bytes()).unwrap().len(), MAX_EVENTS);
        let over = format!("[{}]", vec!["{}"; MAX_EVENTS + 1].join(","));
        assert!(matches!(parse_json_events(over.as_bytes()), Err(IngestError::PayloadTooLarge(_))));
        let ndjson = "{}\n".repeat(MAX_EVENTS + 1);
        assert!(matches!(parse_json_events(ndjson.as_bytes()), Err(IngestError::PayloadTooLarge(_))));
    }

    #[test]
    fn json_nesting_follows_the_shared_limit() {
        let nested = |depth: usize| format!("{{\"a\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1));
        assert!(parse_json_events(nested(msgpack::MAX_NESTING).as_bytes()).is_ok());
        assert!(matches!(
            parse_json_events(nested(msgpack::MAX_NESTING + 1).as_bytes()),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn gzip_is_capped_after_decompression() {
        assert_eq!(gunzip(&gzip(b"hello")).unwrap(), b"hello");
        // 51 gzip members of 1 MiB of zeros: about 50 KiB that inflate to 51 MiB.
        let bomb = gzip(&vec![0u8; 1024 * 1024]).repeat(51);
        assert!(bomb.len() < 100 * 1024, "{}", bomb.len());
        assert!(matches!(gunzip(&bomb), Err(IngestError::PayloadTooLarge(_))));
        assert!(matches!(gunzip(b"not gzip"), Err(IngestError::BadRequest(_))));
        let mut multi = gzip(b"{\"a\":1}\n");
        multi.extend(gzip(b"{\"b\":2}\n"));
        assert_eq!(gunzip(&multi).unwrap(), b"{\"a\":1}\n{\"b\":2}\n", "multi-member gzip");
    }

    #[test]
    fn empty_bodies_are_rejected_for_every_format() {
        for format in [BodyFormat::Json, BodyFormat::MessagePack] {
            let encoding = BodyEncoding { format, gzip: false };
            assert!(matches!(decode_events(Bytes::from_static(b" \n"), encoding), Err(IngestError::BadRequest(_))));
        }
    }

    #[test]
    fn retry_after_counts_down_to_utc_midnight() {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(seconds_until_utc_midnight(at("2026-10-13T23:00:00Z")), 3600);
        assert_eq!(seconds_until_utc_midnight(at("2026-10-13T23:59:59.900Z")), 1);
        assert_eq!(seconds_until_utc_midnight(at("2026-10-13T00:00:00Z")), 86_400);
    }
}
```

- [ ] **Step 8: Implement the rate limiter and usage tracking**

`nami-engine/src/logs/rate_limit.rs`:

```rust
//! Per-source request rate limit for HTTP ingestion: an in-memory token
//! bucket per source id (50 requests/s, burst 100), local to each engine
//! instance. Only authenticated sources get a bucket, so the map is bounded
//! by the number of sources.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::Instant,
};

use uuid::Uuid;

pub const REQUESTS_PER_SECOND: f64 = 50.0;
pub const BURST: f64 = 100.0;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Debug)]
pub struct TokenBuckets {
    rate: f64,
    burst: f64,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

impl Default for TokenBuckets {
    fn default() -> Self {
        TokenBuckets::new(REQUESTS_PER_SECOND, BURST)
    }
}

impl TokenBuckets {
    /// `rate` tokens per second refill each bucket, up to `burst`; a new
    /// bucket starts full.
    pub fn new(rate: f64, burst: f64) -> TokenBuckets {
        TokenBuckets { rate, burst, buckets: Mutex::new(HashMap::new()) }
    }

    /// Takes one token from `key`'s bucket; `false` when it is empty.
    pub fn try_acquire(&self, key: Uuid) -> bool {
        self.try_acquire_at(key, Instant::now())
    }

    pub fn try_acquire_at(&self, key: Uuid, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        let bucket = buckets.entry(key).or_insert(Bucket { tokens: self.burst, updated: now });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.updated = bucket.updated.max(now);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_default_is_50_per_second_with_a_burst_of_100() {
        let limiter = TokenBuckets::default();
        let key = Uuid::new_v4();
        let t0 = Instant::now();
        for i in 0..100 {
            assert!(limiter.try_acquire_at(key, t0), "request {i} is within the burst");
        }
        assert!(!limiter.try_acquire_at(key, t0), "the 101st is limited");
        // 50 per second: one new token every 20 ms (fractions carry over).
        assert!(!limiter.try_acquire_at(key, t0 + Duration::from_millis(19)));
        assert!(limiter.try_acquire_at(key, t0 + Duration::from_millis(30)));
        assert!(!limiter.try_acquire_at(key, t0 + Duration::from_millis(30)));
    }

    #[test]
    fn buckets_refill_up_to_the_burst_only() {
        let limiter = TokenBuckets::new(50.0, 100.0);
        let key = Uuid::new_v4();
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(key, t0));
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..100 {
            assert!(limiter.try_acquire_at(key, later));
        }
        assert!(!limiter.try_acquire_at(key, later), "an hour idle still allows only the burst");
    }

    #[test]
    fn sources_have_independent_buckets() {
        let limiter = TokenBuckets::new(1.0, 1.0);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(a, t0));
        assert!(!limiter.try_acquire_at(a, t0));
        assert!(limiter.try_acquire_at(b, t0), "another source is unaffected");
    }

    #[test]
    fn a_clock_going_backwards_never_adds_tokens() {
        let limiter = TokenBuckets::new(50.0, 1.0);
        let key = Uuid::new_v4();
        let t0 = Instant::now() + Duration::from_secs(10);
        assert!(limiter.try_acquire_at(key, t0));
        assert!(!limiter.try_acquire_at(key, t0 - Duration::from_secs(5)));
    }
}
```

`nami-engine/src/logs/usage.rs`:

```rust
//! Per-source ingestion stats and the daily byte quota. Each engine
//! instance counts in memory ([`UsageTracker`]) and adds its deltas to
//! Postgres `source_usage_daily` every [`FLUSH_INTERVAL`] (additive upsert,
//! so instances never overwrite each other); rows are kept
//! [`RETENTION_DAYS`] days.
//!
//! The quota check reads the day's flushed bytes (cached for at most
//! [`FLUSHED_CACHE_TTL`]) plus this instance's unflushed delta. A request
//! that crosses the quota is accepted whole, so the overshoot is bounded by
//! one request plus about one flush interval of traffic per instance.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::{NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
pub const RETENTION_DAYS: i64 = 400;
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
pub const FLUSHED_CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct UsageCounters {
    pub events: i64,
    pub bytes: i64,
    pub events_dropped: i64,
    pub events_sampled_out: i64,
    pub requests_rate_limited: i64,
    pub requests_over_quota: i64,
}

impl UsageCounters {
    fn add(&mut self, other: &UsageCounters) {
        self.events += other.events;
        self.bytes += other.bytes;
        self.events_dropped += other.events_dropped;
        self.events_sampled_out += other.events_sampled_out;
        self.requests_rate_limited += other.requests_rate_limited;
        self.requests_over_quota += other.requests_over_quota;
    }
}

/// One day of `GET /projects/{id}/sources/{source_id}/usage`.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct DailyUsage {
    pub day: NaiveDate,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub counters: UsageCounters,
}

#[derive(Debug, Clone, Copy)]
struct FlushedBytes {
    day: NaiveDate,
    bytes: i64,
    fetched_at: Instant,
}

#[derive(Debug, Default)]
pub struct UsageTracker {
    pending: Mutex<HashMap<(Uuid, NaiveDate), UsageCounters>>,
    flushed: Mutex<HashMap<Uuid, FlushedBytes>>,
}

impl UsageTracker {
    pub fn record(&self, source_id: Uuid, day: NaiveDate, delta: UsageCounters) {
        let mut pending = self.pending.lock().expect("usage lock poisoned");
        pending.entry((source_id, day)).or_default().add(&delta);
    }

    pub fn record_accepted(&self, source_id: Uuid, day: NaiveDate, events: u64, bytes: u64) {
        let delta = UsageCounters { events: events as i64, bytes: bytes as i64, ..UsageCounters::default() };
        self.record(source_id, day, delta);
    }

    pub fn record_rate_limited(&self, source_id: Uuid, day: NaiveDate) {
        self.record(source_id, day, UsageCounters { requests_rate_limited: 1, ..UsageCounters::default() });
    }

    pub fn record_over_quota(&self, source_id: Uuid, day: NaiveDate, rejected: u64) {
        let delta = UsageCounters { requests_over_quota: rejected as i64, ..UsageCounters::default() };
        self.record(source_id, day, delta);
    }

    /// This instance's counters not yet flushed.
    pub fn pending(&self, source_id: Uuid, day: NaiveDate) -> UsageCounters {
        let pending = self.pending.lock().expect("usage lock poisoned");
        pending.get(&(source_id, day)).copied().unwrap_or_default()
    }

    /// Bytes accepted for the source on `day`: flushed (all instances) plus
    /// this instance's unflushed delta.
    pub async fn bytes_today(&self, pool: &PgPool, source_id: Uuid, day: NaiveDate) -> Result<i64, sqlx::Error> {
        let cached = {
            let flushed = self.flushed.lock().expect("usage lock poisoned");
            flushed
                .get(&source_id)
                .filter(|entry| entry.day == day && entry.fetched_at.elapsed() < FLUSHED_CACHE_TTL)
                .map(|entry| entry.bytes)
        };
        let flushed_bytes = match cached {
            Some(bytes) => bytes,
            None => {
                let bytes: i64 = sqlx::query_scalar(
                    "select coalesce((select bytes from source_usage_daily where source_id = $1 and day = $2), 0)",
                )
                .bind(source_id)
                .bind(day)
                .fetch_one(pool)
                .await?;
                self.remember_flushed(source_id, day, bytes);
                bytes
            }
        };
        Ok(flushed_bytes + self.pending(source_id, day).bytes)
    }

    fn remember_flushed(&self, source_id: Uuid, day: NaiveDate, bytes: i64) {
        let mut flushed = self.flushed.lock().expect("usage lock poisoned");
        let newer = flushed.get(&source_id).is_none_or(|entry| entry.day <= day);
        if newer {
            flushed.insert(source_id, FlushedBytes { day, bytes, fetched_at: Instant::now() });
        }
    }

    /// Adds every pending delta to `source_usage_daily` in one statement and
    /// returns how many (source, day) rows it touched. Counters of deleted
    /// sources are dropped; on a database error every delta is kept for the
    /// next flush.
    pub async fn flush(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let batch = std::mem::take(&mut *self.pending.lock().expect("usage lock poisoned"));
        if batch.is_empty() {
            return Ok(0);
        }
        let mut source_ids = Vec::with_capacity(batch.len());
        let mut days = Vec::with_capacity(batch.len());
        let mut columns: [Vec<i64>; 6] = Default::default();
        for ((source_id, day), c) in &batch {
            source_ids.push(*source_id);
            days.push(*day);
            for (column, value) in columns.iter_mut().zip([
                c.events,
                c.bytes,
                c.events_dropped,
                c.events_sampled_out,
                c.requests_rate_limited,
                c.requests_over_quota,
            ]) {
                column.push(value);
            }
        }
        let [events, bytes, dropped, sampled_out, rate_limited, over_quota] = columns;
        let result: Result<Vec<(Uuid, NaiveDate, i64)>, sqlx::Error> = sqlx::query_as(
            "insert into source_usage_daily as u
                 (source_id, day, events, bytes, events_dropped, events_sampled_out,
                  requests_rate_limited, requests_over_quota)
             select d.source_id, d.day, d.events, d.bytes, d.events_dropped, d.events_sampled_out,
                    d.requests_rate_limited, d.requests_over_quota
             from unnest($1::uuid[], $2::date[], $3::bigint[], $4::bigint[], $5::bigint[], $6::bigint[],
                         $7::bigint[], $8::bigint[])
                  as d(source_id, day, events, bytes, events_dropped, events_sampled_out,
                       requests_rate_limited, requests_over_quota)
             where exists (select 1 from sources s where s.id = d.source_id)
             on conflict (source_id, day) do update set
                 events = u.events + excluded.events,
                 bytes = u.bytes + excluded.bytes,
                 events_dropped = u.events_dropped + excluded.events_dropped,
                 events_sampled_out = u.events_sampled_out + excluded.events_sampled_out,
                 requests_rate_limited = u.requests_rate_limited + excluded.requests_rate_limited,
                 requests_over_quota = u.requests_over_quota + excluded.requests_over_quota
             returning source_id, day, bytes",
        )
        .bind(&source_ids)
        .bind(&days)
        .bind(&events)
        .bind(&bytes)
        .bind(&dropped)
        .bind(&sampled_out)
        .bind(&rate_limited)
        .bind(&over_quota)
        .fetch_all(pool)
        .await;
        match result {
            Ok(rows) => {
                for (source_id, day, total) in &rows {
                    self.remember_flushed(*source_id, *day, *total);
                }
                Ok(rows.len())
            }
            Err(e) => {
                let mut pending = self.pending.lock().expect("usage lock poisoned");
                for (key, counters) in batch {
                    pending.entry(key).or_default().add(&counters);
                }
                Err(e)
            }
        }
    }
}

/// Keeps the last [`RETENTION_DAYS`] days (today included); deletes the rest.
pub async fn prune(pool: &PgPool, today: NaiveDate) -> Result<u64, sqlx::Error> {
    let cutoff = today - chrono::Duration::days(RETENTION_DAYS - 1);
    let result = sqlx::query("delete from source_usage_daily where day < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// The last `days` days up to `today`, oldest first, zero-filled.
pub async fn daily_usage(
    pool: &PgPool,
    source_id: Uuid,
    days: i64,
    today: NaiveDate,
) -> Result<Vec<DailyUsage>, sqlx::Error> {
    sqlx::query_as(
        "select d.day::date as day,
                coalesce(u.events, 0) as events, coalesce(u.bytes, 0) as bytes,
                coalesce(u.events_dropped, 0) as events_dropped,
                coalesce(u.events_sampled_out, 0) as events_sampled_out,
                coalesce(u.requests_rate_limited, 0) as requests_rate_limited,
                coalesce(u.requests_over_quota, 0) as requests_over_quota
         from generate_series($2::date - ($3::int - 1), $2::date, interval '1 day') as d(day)
         left join source_usage_daily u on u.source_id = $1 and u.day = d.day::date
         order by d.day",
    )
    .bind(source_id)
    .bind(today)
    .bind(days as i32)
    .fetch_all(pool)
    .await
}

/// Flushes every [`FLUSH_INTERVAL`] and prunes hourly, forever. Runs on
/// every instance that ingests.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_prune: Option<Instant> = None;
    loop {
        ticker.tick().await;
        if let Err(e) = state.logs.usage.flush(&state.db).await {
            tracing::error!(error = %e, "failed to flush log ingestion usage");
        }
        if last_prune.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL) {
            match prune(&state.db, Utc::now().date_naive()).await {
                Ok(_) => last_prune = Some(Instant::now()),
                Err(e) => tracing::error!(error = %e, "failed to prune log ingestion usage"),
            }
        }
    }
}
```

`nami-engine/src/logs/mod.rs` now lists the modules and holds the per-instance state:

```diff
diff --git a/nami-engine/src/logs/mod.rs b/nami-engine/src/logs/mod.rs
--- a/nami-engine/src/logs/mod.rs
+++ b/nami-engine/src/logs/mod.rs
@@ -1,5 +1,19 @@
 //! Log management: turning ingested events into rows (`normalize`,
-//! `pattern`) and storing them in ClickHouse (`store`).
+//! `pattern`), storing them in ClickHouse (`store`), and HTTP ingestion
+//! (`ingest`, `msgpack`) with its per-source rate limit (`rate_limit`) and
+//! usage accounting (`usage`).
+pub mod ingest;
+pub mod msgpack;
 pub mod normalize;
 pub mod pattern;
+pub mod rate_limit;
 pub mod store;
+pub mod usage;
+
+/// In-memory state of the ingest paths, one per engine instance: rate
+/// limits and unflushed usage counters are local to the instance.
+#[derive(Debug, Default)]
+pub struct LogsState {
+    pub rate_limiter: rate_limit::TokenBuckets,
+    pub usage: usage::UsageTracker,
+}
```

- [ ] **Step 9: Wire the state, the flusher and the routes**

```diff
diff --git a/nami-engine/src/state.rs b/nami-engine/src/state.rs
--- a/nami-engine/src/state.rs
+++ b/nami-engine/src/state.rs
@@ -1,6 +1,8 @@
+use std::sync::Arc;
+
 use sqlx::PgPool;
 
-use crate::{clickhouse::ClickHouse, config::Config, crypto::Cipher, email};
+use crate::{clickhouse::ClickHouse, config::Config, crypto::Cipher, email, logs::LogsState};
 
 /// A ClickHouse database name that is never created. `AppState::for_test`
 /// binds its client to it, so a test that touches ClickHouse without opting
@@ -15,6 +17,8 @@ pub struct AppState {
     pub clickhouse: ClickHouse,
     /// Seals and opens monitor secrets; built from `config.encryption_key`.
     pub cipher: Cipher,
+    /// Log ingest rate limits and usage counters (in memory, per instance).
+    pub logs: Arc<LogsState>,
     pub config: Config,
 }
 
@@ -23,7 +27,8 @@ impl AppState {
     /// `router::with_fallback_connect_info`): every field gets the
     /// defaults tests expect — `Config::from_env_for_test()`, the Mailpit
     /// mailer, a ClickHouse client bound to a database that does not
-    /// exist, and a cipher over the fixed `crypto::TEST_KEY`. Tests override
+    /// exist, a cipher over the fixed `crypto::TEST_KEY`, and fresh log
+    /// ingest state (default rate limits, no usage). Tests override
     /// single fields with struct-update syntax, e.g.
     /// `AppState { mailer: Mailer::new("localhost", 1), ..AppState::for_test(pool) }`,
     /// so adding a field to `AppState` never touches test files again.
@@ -39,6 +44,7 @@ impl AppState {
                 UNCONFIGURED_TEST_CLICKHOUSE_DATABASE,
             ),
             cipher: Cipher::new(&config.encryption_key),
+            logs: Arc::new(LogsState::default()),
             config,
         }
     }
```

```diff
diff --git a/nami-engine/src/main.rs b/nami-engine/src/main.rs
--- a/nami-engine/src/main.rs
+++ b/nami-engine/src/main.rs
@@ -1,5 +1,5 @@
-use nami_engine::{clickhouse::ClickHouse, config::Config, crypto::Cipher, db, email, router, state::AppState};
-use std::net::SocketAddr;
+use nami_engine::{clickhouse::ClickHouse, config::Config, crypto::Cipher, db, email, logs::LogsState, router, state::AppState};
+use std::{net::SocketAddr, sync::Arc};
 use tower_sessions_sqlx_store::PostgresStore;
 
 #[tokio::main]
@@ -53,9 +53,13 @@ async fn main() {
         mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
         clickhouse,
         cipher: Cipher::new(&config.encryption_key),
+        logs: Arc::new(LogsState::default()),
         config,
     };
 
+    // Every instance that ingests logs flushes its usage counters.
+    tokio::spawn(nami_engine::logs::usage::run_forever(state.clone()));
+
     if state.config.uptime_worker_enabled {
         tokio::spawn(nami_engine::uptime::worker::run_forever(state.clone()));
         tracing::info!("uptime worker started");
```

`nami-engine/src/routes/logs_ingest.rs`:

```rust
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use chrono::Utc;
use serde_json::{json, Value};

use crate::{
    logs::{
        ingest::{self, IngestError},
        normalize,
        store::{self, LogRow},
    },
    models::source,
    state::AppState,
};

/// `POST /api/v1/logs`. No session: the source token in
/// `Authorization: Bearer` is the credential, and shippers call the engine
/// directly. The token is only ever hashed, never logged.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/v1/logs", post(ingest_logs))
}

/// Order of checks: token (401) -> rate limit (429 `rate_limited`) ->
/// quota (429 `quota_exceeded`) -> content type (415) -> body size (413)
/// -> parse (400, 413 for too many events) -> store -> 202.
async fn ingest_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Value>), IngestError> {
    let token = ingest::bearer_token(&headers).ok_or(IngestError::Unauthorized)?;
    let source = source::find_by_token(&state.db, token).await?.ok_or(IngestError::Unauthorized)?;
    let received_at = Utc::now();
    let today = received_at.date_naive();

    if !state.logs.rate_limiter.try_acquire(source.id) {
        state.logs.usage.record_rate_limited(source.id, today);
        return Err(IngestError::RateLimited);
    }
    if let Some(quota) = source.daily_byte_quota
        && state.logs.usage.bytes_today(&state.db, source.id, today).await? >= quota
    {
        state.logs.usage.record_over_quota(source.id, today, 1);
        return Err(IngestError::QuotaExceeded { retry_after_seconds: ingest::seconds_until_utc_midnight(received_at) });
    }

    let encoding = ingest::body_encoding(&headers)?;
    let body = ingest::read_body(body, &headers).await?;
    let row_source = source.clone();
    // Decompression, parsing and normalization are CPU-bound.
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<LogRow>, IngestError> {
        let events = ingest::decode_events(body, encoding)?;
        Ok(events
            .into_iter()
            .map(|object| store::build_row(normalize::from_json(object, received_at), &row_source))
            .collect())
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "log ingest worker failed");
        IngestError::Internal
    })??;

    if !rows.is_empty() {
        store::insert_logs(&state.clickhouse, &rows).await?;
        let bytes = rows.iter().map(LogRow::stored_bytes).sum();
        state.logs.usage.record_accepted(source.id, today, rows.len() as u64, bytes);
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": rows.len() }))))
}
```

```diff
diff --git a/nami-engine/src/routes/mod.rs b/nami-engine/src/routes/mod.rs
--- a/nami-engine/src/routes/mod.rs
+++ b/nami-engine/src/routes/mod.rs
@@ -7,3 +7,4 @@ pub mod heartbeats;
 pub mod maintenance;
 pub mod monitor_groups;
 pub mod sources;
+pub mod logs_ingest;
```

```diff
diff --git a/nami-engine/src/router.rs b/nami-engine/src/router.rs
--- a/nami-engine/src/router.rs
+++ b/nami-engine/src/router.rs
@@ -63,6 +63,7 @@ pub fn build_router(state: AppState) -> Router {
         .merge(routes::maintenance::router())
         .merge(routes::monitor_groups::router())
         .merge(routes::sources::router())
+        .merge(routes::logs_ingest::public_router())
         .with_state(state)
 }
 
```

The usage endpoint on sources:

```diff
diff --git a/nami-engine/src/routes/sources.rs b/nami-engine/src/routes/sources.rs
--- a/nami-engine/src/routes/sources.rs
+++ b/nami-engine/src/routes/sources.rs
@@ -1,14 +1,17 @@
 use axum::{
-    extract::{Path, State},
+    extract::{Path, Query, State},
     http::StatusCode,
     routing::{get, post},
     Json, Router,
 };
+use chrono::Utc;
+use serde::Deserialize;
 use serde_json::{json, Value};
 use uuid::Uuid;
 
 use crate::{
     error::AppError,
+    logs::usage::{self, DailyUsage},
     middleware::rbac::{require_project_role, CurrentUser},
     models::source::{self, CreateSourceRequest, PatchSourceRequest, Source, SourceWithToken},
     state::AppState,
@@ -25,6 +28,7 @@ pub fn router() -> Router<AppState> {
         )
         .route("/projects/{id}/sources/{source_id}/token", get(reveal_token))
         .route("/projects/{id}/sources/{source_id}/rotate", post(rotate_token))
+        .route("/projects/{id}/sources/{source_id}/usage", get(source_usage))
 }
 
 async fn list_sources(
@@ -114,3 +118,28 @@ async fn rotate_token(
         .ok_or(AppError::NotFound)?;
     Ok(Json(rotated))
 }
+
+#[derive(Debug, Deserialize)]
+struct UsageQuery {
+    days: Option<i64>,
+}
+
+/// Daily ingestion counters for the last `days` days (default 30, at most
+/// the 400 kept), oldest first and zero-filled. Counts flushed by every
+/// engine instance, so the current day lags by up to ~10 s.
+async fn source_usage(
+    State(state): State<AppState>,
+    user: CurrentUser,
+    Path((project_id, source_id)): Path<(Uuid, Uuid)>,
+    Query(query): Query<UsageQuery>,
+) -> Result<Json<Vec<DailyUsage>>, AppError> {
+    require_project_role(&state.db, &user, project_id, "viewer").await?;
+    source::find_in_project(&state.db, project_id, source_id)
+        .await?
+        .ok_or(AppError::NotFound)?;
+    let days = query.days.unwrap_or(30);
+    if !(1..=usage::RETENTION_DAYS).contains(&days) {
+        return Err(AppError::Validation(format!("days must be between 1 and {}", usage::RETENTION_DAYS)));
+    }
+    Ok(Json(usage::daily_usage(&state.db, source_id, days, Utc::now().date_naive()).await?))
+}
```

- [ ] **Step 10: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/logs_ingest.rs` 9 passed, `tests/logs_quota.rs` 6 passed, unit tests `logs::msgpack` 10, `logs::ingest` 9, `logs::rate_limit` 4; 293 tests in total on the verified base.

- [ ] **Step 11: Try it by hand**

With the engine running (`cargo run`, after `docker compose up -d`), create a source through the API (or wait for Task 7's page) and send `curl -X POST http://localhost:8080/api/v1/logs -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"message":"hello","level":"warning"}'` → `202 {"accepted":1}`; the same with a wrong token → `401 {"error":"unauthorized"}`.

- [ ] **Step 12: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/migrations/0041_create_source_usage_daily.sql \
  nami-engine/src/logs nami-engine/src/state.rs nami-engine/src/main.rs nami-engine/src/routes/logs_ingest.rs \
  nami-engine/src/routes/mod.rs nami-engine/src/router.rs nami-engine/src/routes/sources.rs \
  nami-engine/tests/common nami-engine/tests/logs_ingest.rs nami-engine/tests/logs_quota.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): HTTP log ingestion with limits, rate limit and quota (NAMI-LOG-3)

POST /api/v1/logs takes a JSON object, array or NDJSON, or MessagePack,
optionally gzip; 10 MiB / 50 MiB decompressed / 10,000 events (413),
50 req/s burst 100 per source (429 rate_limited), and an optional daily
byte quota (429 quota_exceeded until UTC midnight). Usage per source and
day is counted in memory and flushed to Postgres every 10 s.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4 (NAMI-LOG-4): RFC 5424 syslog ingestion over TCP and UDP

Branch `feat/NAMI-LOG-4-syslog-ingest`, worktree `.worktrees/NAMI-LOG-4/`.

**Files:**
- Create: `nami-engine/src/logs/syslog/mod.rs`, `nami-engine/src/logs/syslog/parse.rs`, `nami-engine/src/logs/syslog/framing.rs`
- Modify: `nami-engine/src/logs/mod.rs`, `nami-engine/src/models/source.rs`, `nami-engine/src/config.rs`, `nami-engine/src/main.rs`, `nami-engine/.env.example`
- Test: `nami-engine/tests/logs_syslog.rs` (+ unit tests in `parse.rs` and `framing.rs`)

**Interfaces:**
- Consumes: `logs::normalize::{LogEvent::new, in_accepted_range, level_from_number}`, `logs::store::{build_row, insert_logs}` (Task 2); `logs::usage::UsageTracker::{bytes_today, record_accepted, record_over_quota}` and `AppState.logs` (Task 3); `auth::token::hash_token`; `Config::from_env_defaults_only` (`env_flag`).
- Produces:
  - `Config.syslog_enabled: bool` (`SYSLOG_ENABLED`, default false), `Config.syslog_tcp_port: u16` / `Config.syslog_udp_port: u16` (`SYSLOG_TCP_PORT` / `SYSLOG_UDP_PORT`, default 5140).
  - `models::source::find_by_token_hashes(&PgPool, hashes: &[String]) -> Result<Vec<(String, IngestSource)>, sqlx::Error>`.
  - `logs::syslog`: consts `DEFAULT_PORT = 5140`, `IDLE_TIMEOUT` (300 s), `MAX_TCP_CONNECTIONS = 1024`, `QUEUE_CAPACITY = 10_000`, `BATCH_MAX_ROWS = 1000`, `BATCH_MAX_WAIT` (1 s); `SyslogConfig { tcp_addr: SocketAddr, udp_addr: SocketAddr, idle_timeout: Duration, batch_max_rows: usize, batch_max_wait: Duration }`, `SyslogConfig::from_config(&Config) -> SyslogConfig`; `SyslogStats` (atomic `received`, `accepted`, `malformed`, `unauthenticated`, `over_quota`, `overloaded`, `store_failed`) with `snapshot(&self) -> SyslogStatsSnapshot`; `SyslogHandle { tcp_addr, udp_addr, stats: Arc<SyslogStats> }`; `spawn(state: AppState, config: SyslogConfig) -> std::io::Result<SyslogHandle>`.
  - `logs::syslog::parse`: `NAMI_SD_ID = "nami"`, `SyslogMessage { facility: u8, severity: u8, timestamp: Option<DateTime<Utc>>, hostname, app_name, procid, msgid: Option<String>, structured_data: Vec<SdElement>, message: String }`, `SdElement { id: String, params: Vec<(String, String)> }`, `ParseError`, `parse(frame: &[u8]) -> Result<SyslogMessage, ParseError>`, `SyslogMessage::token(&self) -> Option<&str>`, `SyslogMessage::into_event(self, received_at: DateTime<Utc>) -> LogEvent`.
  - `logs::syslog::framing`: `MAX_FRAME_BYTES = 65536`, `FrameError` (`TooLarge`, `InvalidLength`, `Truncated`, `Io`), `FrameReader::<R: AsyncRead + Unpin>::new(R)`, `next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError>`.

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/logs_syslog.rs` (both TCP framings, UDP, dropped and counted frames, malformed input of every kind, the idle timeout, the quota):

```rust
// Syslog ingestion (RFC 5424 over TCP and UDP). Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use common::{
    logs::{seed_source, seed_source_with, source_config, stored_rows},
    seed_project, state_with_clickhouse, test_cipher,
};
use nami_engine::{
    clickhouse::ClickHouse,
    logs::syslog::{self, SyslogConfig, SyslogHandle, SyslogStatsSnapshot},
    models::source::{self, SourceConfig},
    state::AppState,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};
use uuid::Uuid;

async fn start(state: AppState) -> SyslogHandle {
    let config = SyslogConfig {
        tcp_addr: "127.0.0.1:0".parse().unwrap(),
        udp_addr: "127.0.0.1:0".parse().unwrap(),
        idle_timeout: Duration::from_millis(500),
        batch_max_rows: 1000,
        batch_max_wait: Duration::from_millis(50),
    };
    syslog::spawn(state, config).await.unwrap()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn frame(token: &str, message: &str) -> String {
    format!("<165>1 {} web-1 nginx 4123 ID47 [nami token=\"{token}\"][origin ip=\"10.0.0.1\" ip=\"10.0.0.2\"] {message}", now())
}

/// Waits (up to 10 s) until the project has `count` rows.
async fn rows_eventually(ch: &ClickHouse, project: Uuid, count: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let rows = stored_rows(ch, project).await;
        if rows.len() >= count || std::time::Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits (up to 10 s) until `check` holds for the counters.
async fn stats_eventually(handle: &SyslogHandle, check: impl Fn(&SyslogStatsSnapshot) -> bool) -> SyslogStatsSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stats = handle.stats.snapshot();
        if check(&stats) || std::time::Instant::now() > deadline {
            return stats;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn send_udp(handle: &SyslogHandle, datagram: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.send_to(datagram, handle.udp_addr).await.unwrap();
}

#[sqlx::test]
async fn tcp_frames_in_both_framings_are_stored_and_mapped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "edge").await;

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let counted = frame(&token, "octet counted");
    let wire = format!(
        "{}\n{}\r\n{} {}{} {}\n",
        frame(&token, "newline one"),
        frame(&token, "newline two"),
        counted.len(),
        counted,
        counted.len(),
        counted
    );
    tcp.write_all(wire.as_bytes()).await.unwrap();

    let rows = rows_eventually(&ch, project, 4).await;
    assert_eq!(rows.len(), 4, "{rows:?}");
    let row = rows.iter().find(|r| r["message"] == "newline one").unwrap();
    assert_eq!(row["level"], "notice", "severity 5");
    assert_eq!(row["source_id"], source_id.to_string());
    let fields: Value = serde_json::from_str(row["fields"].as_str().unwrap()).unwrap();
    assert_eq!(
        fields,
        json!({
            "origin": { "ip": ["10.0.0.1", "10.0.0.2"] },
            "facility": 20,
            "hostname": "web-1",
            "app_name": "nginx",
            "procid": "4123",
            "msgid": "ID47"
        })
    );
    assert_eq!(rows.iter().filter(|r| r["message"] == "octet counted").count(), 2);
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains(&token), "the token is never stored");
    assert_eq!(logs.usage.pending(source_id, Utc::now().date_naive()).events, 4, "syslog events are counted");
    assert_eq!(handle.stats.snapshot().accepted, 4);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn udp_datagrams_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    let datagram = format!("<11>1 {} host app - - [nami token=\"{token}\"] \u{feff}disk full\n", now());
    send_udp(&handle, datagram.as_bytes()).await;
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0]["message"].clone(), rows[0]["level"].clone()), (json!("disk full"), json!("error")));
    ch.drop_database().await.unwrap();
}

/// Security: frames without a live source token are dropped and counted.
#[sqlx::test]
async fn frames_without_a_valid_token_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (source_id, old_token) = seed_source(&pool, project, "edge").await;
    let new_token = source::rotate_token(&pool, &test_cipher(), project, source_id).await.unwrap().unwrap().token;

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let wire = format!(
        "<14>1 {ts} h a - - - no structured data\n<14>1 {ts} h a - - [other token=\"{new_token}\"] wrong element\n{}\n{}\n",
        frame("not-a-token", "unknown token"),
        frame(&old_token, "rotated token"),
        ts = now()
    );
    tcp.write_all(wire.as_bytes()).await.unwrap();
    let stats = stats_eventually(&handle, |s| s.unauthenticated == 4).await;
    assert_eq!((stats.received, stats.unauthenticated, stats.accepted), (4, 4, 0));

    tcp.write_all(format!("{}\n", frame(&new_token, "valid")).as_bytes()).await.unwrap();
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "valid");
    ch.drop_database().await.unwrap();
}

/// Security: malformed input of every kind never stops the listeners.
#[sqlx::test]
async fn malformed_input_never_stops_the_listeners(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    // Broken framing closes the connection (the server reads no further).
    for wire in [
        b"99999999999999999999 <14>1 - - - - - -".to_vec(),
        b"70000 <14>1".to_vec(),
        vec![b'<'; 70 * 1024],
    ] {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(&wire).await.unwrap();
        let mut buf = [0u8; 16];
        let closed = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf)).await.unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)), "the server closes the connection");
    }
    // A truncated octet-counted frame, garbage, and random bytes.
    let mut state_bits: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state_bits ^= state_bits << 13;
        state_bits ^= state_bits >> 7;
        state_bits ^= state_bits << 17;
        state_bits
    };
    for wire in [b"100 <14>1 - - - - - - short".to_vec(), b"\x00\xff\xfe garbage\n<<<>>>\n[[[\n".to_vec()] {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(&wire).await.unwrap();
        tcp.shutdown().await.unwrap();
    }
    for _ in 0..200 {
        let len = (next() % 512) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        send_udp(&handle, &bytes).await;
    }
    send_udp(&handle, b"").await;
    // A valid frame whose message is not UTF-8 is stored with replacement characters.
    let mut invalid_utf8 = format!("<14>1 {} h a - - [nami token=\"{token}\"] bad ", now()).into_bytes();
    invalid_utf8.extend(b"\xff\xfe bytes");
    send_udp(&handle, &invalid_utf8).await;

    let stats = stats_eventually(&handle, |s| s.malformed >= 5).await;
    assert!(stats.malformed >= 5, "{stats:?}");

    // Both listeners still serve.
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "after the storm tcp")).as_bytes()).await.unwrap();
    send_udp(&handle, frame(&token, "after the storm udp").as_bytes()).await;
    let rows = rows_eventually(&ch, project, 3).await;
    let mut messages: Vec<&str> = rows.iter().map(|r| r["message"].as_str().unwrap()).collect();
    messages.sort();
    assert_eq!(messages, ["after the storm tcp", "after the storm udp", "bad \u{fffd}\u{fffd} bytes"]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn idle_tcp_connections_are_closed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf)).await.expect("closed before 5 s");
    assert!(matches!(read, Ok(0)), "{read:?}");
    assert!(started.elapsed() >= Duration::from_millis(400), "closed after the idle timeout, not at once");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn frames_of_a_source_over_its_quota_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let quota = SourceConfig { daily_byte_quota: Some(1024 * 1024), ..source_config("capped") };
    let (capped, capped_token) = seed_source_with(&pool, project, quota).await;
    let (_, free_token) = seed_source(&pool, project, "free").await;
    sqlx::query("insert into source_usage_daily (source_id, day, bytes) values ($1, $2, 2097152)")
        .bind(capped)
        .bind(Utc::now().date_naive())
        .execute(&pool)
        .await
        .unwrap();

    send_udp(&handle, frame(&capped_token, "over quota").as_bytes()).await;
    send_udp(&handle, frame(&free_token, "still flowing").as_bytes()).await;
    let stats = stats_eventually(&handle, |s| s.over_quota == 1 && s.accepted == 1).await;
    assert_eq!((stats.over_quota, stats.accepted), (1, 1));
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "still flowing");

    logs.usage.flush(&pool).await.unwrap();
    let rejected: i64 =
        sqlx::query_scalar("select requests_over_quota from source_usage_daily where source_id = $1")
            .bind(capped)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rejected, 1);
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && cargo test --test logs_syslog`
Expected: FAIL to compile — `error[E0432]: unresolved import nami_engine::logs::syslog`.

- [ ] **Step 3: Implement the RFC 5424 parser**

`nami-engine/src/logs/syslog/parse.rs` (unit tests: the RFC's own examples, escapes, the token element, every malformed shape, the field mapping, and 20,000 random frames):

```rust
//! RFC 5424 syslog messages:
//! `<PRI>1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA [MSG]`.
//! The header is checked strictly (field lengths, printable ASCII); the
//! message may be any bytes (a leading UTF-8 BOM is dropped, invalid UTF-8
//! becomes U+FFFD).

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::logs::normalize::{in_accepted_range, level_from_number, LogEvent};

/// The structured-data element that carries the source token:
/// `[nami token="<source token>"]`. It is never stored.
pub const NAMI_SD_ID: &str = "nami";
const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq)]
pub struct SyslogMessage {
    pub facility: u8,
    pub severity: u8,
    pub timestamp: Option<DateTime<Utc>>,
    pub hostname: Option<String>,
    pub app_name: Option<String>,
    pub procid: Option<String>,
    pub msgid: Option<String>,
    pub structured_data: Vec<SdElement>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SdElement {
    pub id: String,
    pub params: Vec<(String, String)>,
}

/// Static messages only: an error never echoes frame content (which may
/// hold a token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("missing or invalid PRI")]
    Pri,
    #[error("not an RFC 5424 version 1 message")]
    Version,
    #[error("invalid TIMESTAMP")]
    Timestamp,
    #[error("invalid HOSTNAME, APP-NAME, PROCID or MSGID")]
    Header,
    #[error("invalid STRUCTURED-DATA")]
    StructuredData,
}

pub fn parse(frame: &[u8]) -> Result<SyslogMessage, ParseError> {
    let mut p = Parser { input: frame, pos: 0 };
    let pri = p.pri()?;
    p.version()?;
    let timestamp = p.timestamp()?;
    let hostname = p.header_field(255)?;
    let app_name = p.header_field(48)?;
    let procid = p.header_field(128)?;
    let msgid = p.header_field(32)?;
    let structured_data = p.structured_data()?;
    let message = p.message()?;
    Ok(SyslogMessage {
        facility: pri / 8,
        severity: pri % 8,
        timestamp,
        hostname,
        app_name,
        procid,
        msgid,
        structured_data,
        message,
    })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// The bytes up to (not including) the next space or the end.
    fn token(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b' ') {
            self.pos += 1;
        }
        &self.input[start..self.pos]
    }

    fn pri(&mut self) -> Result<u8, ParseError> {
        if !self.eat(b'<') {
            return Err(ParseError::Pri);
        }
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) && self.pos - start < 3 {
            self.pos += 1;
        }
        let digits = &self.input[start..self.pos];
        if digits.is_empty() || !self.eat(b'>') {
            return Err(ParseError::Pri);
        }
        let value: u16 = std::str::from_utf8(digits).ok().and_then(|d| d.parse().ok()).ok_or(ParseError::Pri)?;
        if value > 191 {
            return Err(ParseError::Pri);
        }
        Ok(value as u8)
    }

    fn version(&mut self) -> Result<(), ParseError> {
        if self.token() == b"1" && self.eat(b' ') { Ok(()) } else { Err(ParseError::Version) }
    }

    fn timestamp(&mut self) -> Result<Option<DateTime<Utc>>, ParseError> {
        let raw = self.token();
        if !self.eat(b' ') {
            return Err(ParseError::Timestamp);
        }
        if raw == b"-" {
            return Ok(None);
        }
        let text = std::str::from_utf8(raw).map_err(|_| ParseError::Timestamp)?;
        DateTime::parse_from_rfc3339(text)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| ParseError::Timestamp)
    }

    /// NILVALUE or 1..=`max` printable ASCII characters, then a space.
    fn header_field(&mut self, max: usize) -> Result<Option<String>, ParseError> {
        let raw = self.token();
        if !self.eat(b' ') || raw.is_empty() || raw.len() > max || !raw.iter().all(|b| (33..=126).contains(b)) {
            return Err(ParseError::Header);
        }
        if raw == b"-" {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(raw).into_owned()))
    }

    fn structured_data(&mut self) -> Result<Vec<SdElement>, ParseError> {
        if self.eat(b'-') {
            return Ok(Vec::new());
        }
        let mut elements = Vec::new();
        while self.peek() == Some(b'[') {
            elements.push(self.sd_element()?);
        }
        if elements.is_empty() { Err(ParseError::StructuredData) } else { Ok(elements) }
    }

    fn sd_element(&mut self) -> Result<SdElement, ParseError> {
        self.eat(b'[');
        let id = self.sd_name()?;
        let mut params = Vec::new();
        loop {
            if self.eat(b']') {
                return Ok(SdElement { id, params });
            }
            if !self.eat(b' ') {
                return Err(ParseError::StructuredData);
            }
            let name = self.sd_name()?;
            if !self.eat(b'=') || !self.eat(b'"') {
                return Err(ParseError::StructuredData);
            }
            params.push((name, self.param_value()?));
        }
    }

    /// SD-NAME: 1..=32 printable ASCII characters except `=`, space, `]`, `"`.
    fn sd_name(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while self.peek().is_some_and(|b| (33..=126).contains(&b) && !matches!(b, b'=' | b']' | b'"')) {
            self.pos += 1;
        }
        let name = &self.input[start..self.pos];
        if name.is_empty() || name.len() > 32 {
            return Err(ParseError::StructuredData);
        }
        Ok(String::from_utf8_lossy(name).into_owned())
    }

    /// PARAM-VALUE after its opening quote, through the closing one. `\"`,
    /// `\\` and `\]` are unescaped; any other backslash is kept as is.
    fn param_value(&mut self) -> Result<String, ParseError> {
        let mut value = Vec::new();
        loop {
            match self.peek() {
                None => return Err(ParseError::StructuredData),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(String::from_utf8_lossy(&value).into_owned());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(escaped @ (b'"' | b'\\' | b']')) => {
                            value.push(escaped);
                            self.pos += 1;
                        }
                        _ => value.push(b'\\'),
                    }
                }
                Some(byte) => {
                    value.push(byte);
                    self.pos += 1;
                }
            }
        }
    }

    /// Nothing, or a space and the message (BOM dropped, trailing CR/LF
    /// trimmed, invalid UTF-8 replaced).
    fn message(&mut self) -> Result<String, ParseError> {
        if self.peek().is_none() {
            return Ok(String::new());
        }
        if !self.eat(b' ') {
            return Err(ParseError::StructuredData);
        }
        let mut rest = &self.input[self.pos..];
        rest = rest.strip_prefix(BOM).unwrap_or(rest);
        while let Some((last, init)) = rest.split_last() {
            if matches!(last, b'\n' | b'\r') {
                rest = init;
            } else {
                break;
            }
        }
        Ok(String::from_utf8_lossy(rest).into_owned())
    }
}

impl SyslogMessage {
    /// The source token from `[nami token="..."]`, if present.
    pub fn token(&self) -> Option<&str> {
        self.structured_data
            .iter()
            .filter(|element| element.id == NAMI_SD_ID)
            .flat_map(|element| element.params.iter())
            .find(|(name, _)| name == "token")
            .map(|(_, value)| value.as_str())
    }

    /// The event this message becomes: severity -> level; every
    /// structured-data element except `nami` -> `fields.<SD-ID>` (a param
    /// repeated within an element becomes an array); `facility`,
    /// `hostname`, `app_name`, `procid` and `msgid` -> fields of those
    /// names. A timestamp outside the accepted range is replaced by the
    /// receive time and kept as `fields.timestamp`.
    pub fn into_event(self, received_at: DateTime<Utc>) -> LogEvent {
        let mut fields = Map::new();
        for element in self.structured_data {
            if element.id == NAMI_SD_ID {
                continue;
            }
            let mut params = Map::new();
            for (name, value) in element.params {
                match params.get_mut(&name) {
                    Some(Value::Array(values)) => values.push(Value::String(value)),
                    Some(existing) => *existing = Value::Array(vec![existing.take(), Value::String(value)]),
                    None => {
                        params.insert(name, Value::String(value));
                    }
                }
            }
            fields.insert(element.id, Value::Object(params));
        }
        fields.insert("facility".to_string(), Value::from(self.facility));
        for (key, value) in [
            ("hostname", self.hostname),
            ("app_name", self.app_name),
            ("procid", self.procid),
            ("msgid", self.msgid),
        ] {
            if let Some(value) = value {
                fields.insert(key.to_string(), Value::String(value));
            }
        }
        let dt = match self.timestamp {
            Some(dt) if in_accepted_range(dt, received_at) => dt,
            Some(dt) => {
                fields.insert("timestamp".to_string(), Value::String(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)));
                received_at
            }
            None => received_at,
        };
        LogEvent::new(dt, level_from_number(self.severity.into()), self.message, fields, String::new(), String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn parses_the_rfc_5424_examples() {
        let m = parse(b"<34>1 2003-10-11T22:14:15.003Z mymachine.example.com su - ID47 - \xEF\xBB\xBF'su root' failed for lonvick on /dev/pts/8").unwrap();
        assert_eq!((m.facility, m.severity), (4, 2));
        assert_eq!(m.timestamp, Some(at("2003-10-11T22:14:15.003Z")));
        assert_eq!(m.hostname.as_deref(), Some("mymachine.example.com"));
        assert_eq!((m.app_name.as_deref(), m.procid.as_deref(), m.msgid.as_deref()), (Some("su"), None, Some("ID47")));
        assert!(m.structured_data.is_empty());
        assert_eq!(m.message, "'su root' failed for lonvick on /dev/pts/8", "the BOM is dropped");

        let m = parse(b"<165>1 2003-08-24T05:14:15.000003-07:00 192.0.2.1 myproc 8710 - - %% It's time to make the do-nuts.").unwrap();
        assert_eq!((m.facility, m.severity), (20, 5));
        assert_eq!(m.timestamp, Some(at("2003-08-24T12:14:15.000003Z")));
        assert_eq!(m.procid.as_deref(), Some("8710"));
        assert_eq!(m.message, "%% It's time to make the do-nuts.");

        let m = parse(b"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"][examplePriority@32473 class=\"high\"]").unwrap();
        assert_eq!(m.structured_data.len(), 2);
        assert_eq!(m.structured_data[0].id, "exampleSDID@32473");
        assert_eq!(m.structured_data[0].params[1], ("eventSource".to_string(), "Application".to_string()));
        assert_eq!(m.structured_data[1].params, vec![("class".to_string(), "high".to_string())]);
        assert_eq!(m.message, "", "no MSG part");
    }

    #[test]
    fn param_values_unescape_only_quote_backslash_and_bracket() {
        let m = parse(br#"<14>1 - - - - - [x a="q\"uote \\back \]br \n"] m"#).unwrap();
        assert_eq!(m.structured_data[0].params[0].1, r#"q"uote \back ]br \n"#);
        assert_eq!(m.timestamp, None);
        assert_eq!(m.message, "m");
    }

    #[test]
    fn the_token_comes_from_the_nami_element_only() {
        let m = parse(b"<14>1 - h a - - [origin ip=\"1.2.3.4\"][nami token=\"abc\"] hi").unwrap();
        assert_eq!(m.token(), Some("abc"));
        assert_eq!(parse(b"<14>1 - h a - - [nami tokenx=\"abc\"] hi").unwrap().token(), None);
        assert_eq!(parse(b"<14>1 - h a - - [other token=\"abc\"] hi").unwrap().token(), None);
        assert_eq!(parse(b"<14>1 - h a - - - hi").unwrap().token(), None);
    }

    #[test]
    fn malformed_frames_are_errors() {
        for (frame, expected) in [
            (&b""[..], ParseError::Pri),
            (b"no pri", ParseError::Pri),
            (b"<>1 - - - - - -", ParseError::Pri),
            (b"<192>1 - - - - - -", ParseError::Pri),
            (b"<1234>1 - - - - - -", ParseError::Pri),
            (b"<34>Oct 11 22:14:15 mymachine su: 'su root' failed", ParseError::Version),
            (b"<34>2 - - - - - -", ParseError::Version),
            (b"<34>1 yesterday - - - - -", ParseError::Timestamp),
            (b"<34>1 -", ParseError::Timestamp),
            (b"<34>1 - - - - -", ParseError::Header),
            (b"<34>1 - h\xC3\xA9 - - - -", ParseError::Header),
            (b"<34>1 - - - - ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -", ParseError::Header),
            (b"<34>1 - - - - - [x a=\"unterminated]", ParseError::StructuredData),
            (b"<34>1 - - - - - [x a=b]", ParseError::StructuredData),
            (b"<34>1 - - - - - [x]extra", ParseError::StructuredData),
            (b"<34>1 - - - - - x", ParseError::StructuredData),
            (b"<34>1 - - - - - -msg", ParseError::StructuredData),
            (b"<34>1 - - - - - [=x]", ParseError::StructuredData),
        ] {
            assert_eq!(parse(frame), Err(expected), "{:?}", String::from_utf8_lossy(frame));
        }
        let long_host = format!("<34>1 - {} - - - -", "h".repeat(256));
        assert_eq!(parse(long_host.as_bytes()), Err(ParseError::Header));
    }

    #[test]
    fn events_map_severity_header_fields_and_structured_data() {
        let received = at("2026-10-13T12:00:00Z");
        let frame = b"<165>1 2026-10-13T11:59:59.5Z web-1 nginx 4123 ID47 [nami token=\"secret\"][origin ip=\"10.0.0.1\" ip=\"10.0.0.2\" software=\"x\"] GET / 200";
        let event = parse(frame).unwrap().into_event(received);
        assert_eq!(event.level, "notice");
        assert_eq!(event.dt, at("2026-10-13T11:59:59.500Z"));
        assert_eq!(event.message, "GET / 200");
        assert_eq!(
            Value::Object(event.fields),
            json!({
                "origin": { "ip": ["10.0.0.1", "10.0.0.2"], "software": "x" },
                "facility": 20,
                "hostname": "web-1",
                "app_name": "nginx",
                "procid": "4123",
                "msgid": "ID47"
            })
        );
        for (severity, level) in [(0, "emergency"), (1, "alert"), (2, "critical"), (3, "error"), (4, "warn"), (6, "info"), (7, "debug")] {
            let frame = format!("<{severity}>1 - - - - - -");
            assert_eq!(parse(frame.as_bytes()).unwrap().into_event(received).level, level);
        }
    }

    #[test]
    fn missing_or_out_of_range_timestamps_use_the_receive_time() {
        let received = at("2026-10-13T12:00:00Z");
        assert_eq!(parse(b"<14>1 - - - - - - m").unwrap().into_event(received).dt, received);
        let old = parse(b"<14>1 2003-10-11T22:14:15Z - - - - - m").unwrap().into_event(received);
        assert_eq!(old.dt, received);
        assert_eq!(old.fields["timestamp"], "2003-10-11T22:14:15Z");
    }

    #[test]
    fn invalid_utf8_in_the_message_is_replaced_and_trailing_newlines_trimmed() {
        let m = parse(b"<14>1 - - - - - - caf\xC3\xA9 \xFF\xFE end\r\n").unwrap();
        assert_eq!(m.message, "caf\u{e9} \u{fffd}\u{fffd} end");
    }

    #[test]
    fn arbitrary_bytes_and_every_truncation_never_panic() {
        let valid = b"<165>1 2026-10-13T11:59:59.5Z web-1 nginx 4123 ID47 [nami token=\"abc\"][o a=\"1\\\"\"] msg";
        for len in 0..=valid.len() {
            let _ = parse(&valid[..len]);
        }
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const ALPHABET: &[u8] = b"<>1 -[]=\"\\0123456789abcT:Z.\xff\n";
        for _ in 0..20_000 {
            let len = (next() % 80) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize]).collect();
            if let Ok(message) = parse(&bytes) {
                let _ = message.into_event(Utc::now());
            }
        }
    }
}
```

- [ ] **Step 4: Implement TCP framing**

`nami-engine/src/logs/syslog/framing.rs`:

```rust
//! Syslog over TCP framing (RFC 6587), detected per frame: octet counting
//! (`<length> <message>`, the frame starts with a digit) or non-transparent
//! framing (the message ends at LF, optional CR before it; it starts with
//! `<`). Separators between frames (LF, CR, spaces, NUL) are skipped. A
//! frame may be at most [`MAX_FRAME_BYTES`]; framing errors are fatal for
//! the connection, because a stream cannot be resynchronised after them.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// "65536" is the longest valid octet count.
const MAX_LENGTH_DIGITS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame longer than 64 KiB")]
    TooLarge,
    #[error("invalid octet count")]
    InvalidLength,
    #[error("connection closed in the middle of a frame")]
    Truncated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct FrameReader<R> {
    reader: BufReader<R>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        FrameReader { reader: BufReader::new(inner) }
    }

    /// The next frame without its framing; `None` once the stream ends
    /// between frames.
    pub async fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let first = loop {
            let buf = self.reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            let separators = buf.iter().take_while(|b| matches!(b, b'\n' | b'\r' | b' ' | b'\t' | 0)).count();
            let first = buf.get(separators).copied();
            self.reader.consume(separators);
            if let Some(first) = first {
                break first;
            }
        };
        if first.is_ascii_digit() {
            self.octet_counted().await.map(Some)
        } else {
            self.newline_delimited().await.map(Some)
        }
    }

    async fn read_byte(&mut self) -> Result<Option<u8>, FrameError> {
        let buf = self.reader.fill_buf().await?;
        let Some(&byte) = buf.first() else {
            return Ok(None);
        };
        self.reader.consume(1);
        Ok(Some(byte))
    }

    async fn octet_counted(&mut self) -> Result<Vec<u8>, FrameError> {
        let mut len = 0usize;
        let mut digits = 0;
        loop {
            match self.read_byte().await?.ok_or(FrameError::Truncated)? {
                digit @ b'0'..=b'9' if digits < MAX_LENGTH_DIGITS => {
                    len = len * 10 + usize::from(digit - b'0');
                    digits += 1;
                }
                b' ' if digits > 0 => break,
                _ => return Err(FrameError::InvalidLength),
            }
        }
        if len == 0 {
            return Err(FrameError::InvalidLength);
        }
        if len > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        let mut frame = vec![0u8; len];
        self.reader.read_exact(&mut frame).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => FrameError::Truncated,
            _ => FrameError::Io(e),
        })?;
        Ok(frame)
    }

    async fn newline_delimited(&mut self) -> Result<Vec<u8>, FrameError> {
        let mut frame = Vec::new();
        (&mut self.reader).take(MAX_FRAME_BYTES as u64 + 1).read_until(b'\n', &mut frame).await?;
        if frame.last() == Some(&b'\n') {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(frame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        // The stream ended without a final LF: that is the last frame.
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn frames(input: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        let mut reader = FrameReader::new(input);
        let mut frames = Vec::new();
        while let Some(frame) = reader.next_frame().await? {
            frames.push(frame);
        }
        Ok(frames)
    }

    #[tokio::test]
    async fn newline_and_octet_counted_frames_can_be_mixed() {
        let input = b"<1>1 a\r\n\n<2>1 b\n6 <3>1 c10 <4>1 d e f\n \x00<5>1 last";
        let got = frames(input).await.unwrap();
        let expected: Vec<&[u8]> = vec![b"<1>1 a", b"<2>1 b", b"<3>1 c", b"<4>1 d e f", b"<5>1 last"];
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn a_64_kib_frame_fits_in_both_framings() {
        let body = vec![b'x'; MAX_FRAME_BYTES];
        let mut newline = body.clone();
        newline.push(b'\n');
        assert_eq!(frames(&newline).await.unwrap(), vec![body.clone()]);
        let mut counted = format!("{MAX_FRAME_BYTES} ").into_bytes();
        counted.extend(&body);
        assert_eq!(frames(&counted).await.unwrap(), vec![body]);
    }

    #[tokio::test]
    async fn framing_errors_are_reported() {
        let too_long_line = vec![b'<'; MAX_FRAME_BYTES + 1];
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"123456 x".to_vec(), "InvalidLength"),
            (b"99999999999999999999 x".to_vec(), "InvalidLength"),
            (b"65537 x".to_vec(), "TooLarge"),
            (b"0 x".to_vec(), "InvalidLength"),
            (b"12x".to_vec(), "InvalidLength"),
            (b"12".to_vec(), "Truncated"),
            (b"10 <1>1 a".to_vec(), "Truncated"),
            (too_long_line, "TooLarge"),
        ];
        for (input, expected) in cases {
            let error = frames(&input).await.unwrap_err();
            assert!(format!("{error:?}").starts_with(expected), "{:?}: {error:?}", String::from_utf8_lossy(&input[..input.len().min(30)]));
        }
    }

    #[tokio::test]
    async fn an_empty_stream_has_no_frames() {
        assert!(frames(b"").await.unwrap().is_empty());
        assert!(frames(b"\n\r\n  \x00").await.unwrap().is_empty());
    }
}
```

- [ ] **Step 5: Implement the listeners and the batcher**

`nami-engine/src/logs/syslog/mod.rs`:

```rust
//! Syslog ingestion: RFC 5424 over TCP (`framing`) and UDP (one message
//! per datagram), enabled with `SYSLOG_ENABLED`. A frame authenticates with
//! `[nami token="<source token>"]`; frames that are malformed, carry no or
//! an unknown token, or belong to a source over its daily quota are dropped
//! and counted ([`SyslogStats`], plus a warning at most every 10 s), never
//! answered. Accepted frames are queued and stored in batches: one source
//! lookup and one ClickHouse insert per batch, so a rotated or deleted
//! token stops working from the next batch on.

pub mod framing;
pub mod parse;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use tokio::{
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Semaphore},
};
use uuid::Uuid;

use self::framing::{FrameError, FrameReader, MAX_FRAME_BYTES};
use super::{normalize::LogEvent, store};
use crate::{auth::token::hash_token, config::Config, models::source, state::AppState};

pub const DEFAULT_PORT: u16 = 5140;
/// A TCP connection that completes no frame for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_TCP_CONNECTIONS: usize = 1024;
/// Frames waiting for the batcher; beyond this they are dropped (counted
/// as `overloaded`) instead of growing memory.
pub const QUEUE_CAPACITY: usize = 10_000;
pub const BATCH_MAX_ROWS: usize = 1000;
pub const BATCH_MAX_WAIT: Duration = Duration::from_secs(1);
const WARN_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct SyslogConfig {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    pub idle_timeout: Duration,
    pub batch_max_rows: usize,
    pub batch_max_wait: Duration,
}

impl SyslogConfig {
    /// All interfaces on `SYSLOG_TCP_PORT` / `SYSLOG_UDP_PORT`.
    pub fn from_config(config: &Config) -> SyslogConfig {
        SyslogConfig {
            tcp_addr: SocketAddr::from(([0, 0, 0, 0], config.syslog_tcp_port)),
            udp_addr: SocketAddr::from(([0, 0, 0, 0], config.syslog_udp_port)),
            idle_timeout: IDLE_TIMEOUT,
            batch_max_rows: BATCH_MAX_ROWS,
            batch_max_wait: BATCH_MAX_WAIT,
        }
    }
}

/// Counters since startup (per engine instance).
#[derive(Debug, Default)]
pub struct SyslogStats {
    /// Frames read off the wire.
    pub received: AtomicU64,
    /// Frames stored as rows.
    pub accepted: AtomicU64,
    /// Not RFC 5424, or broken framing (the connection is closed).
    pub malformed: AtomicU64,
    /// No `[nami token=...]`, or a token of no source.
    pub unauthenticated: AtomicU64,
    /// The source is over its daily byte quota.
    pub over_quota: AtomicU64,
    /// The queue to the batcher was full.
    pub overloaded: AtomicU64,
    /// Lost to a database or ClickHouse error.
    pub store_failed: AtomicU64,
}

/// A plain copy of [`SyslogStats`], for comparisons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyslogStatsSnapshot {
    pub received: u64,
    pub accepted: u64,
    pub malformed: u64,
    pub unauthenticated: u64,
    pub over_quota: u64,
    pub overloaded: u64,
    pub store_failed: u64,
}

impl SyslogStats {
    pub fn snapshot(&self) -> SyslogStatsSnapshot {
        let get = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SyslogStatsSnapshot {
            received: get(&self.received),
            accepted: get(&self.accepted),
            malformed: get(&self.malformed),
            unauthenticated: get(&self.unauthenticated),
            over_quota: get(&self.over_quota),
            overloaded: get(&self.overloaded),
            store_failed: get(&self.store_failed),
        }
    }
}

fn bump(counter: &AtomicU64, by: u64) {
    counter.fetch_add(by, Ordering::Relaxed);
}

/// The bound addresses (useful when binding port 0) and the counters.
#[derive(Debug, Clone)]
pub struct SyslogHandle {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    pub stats: Arc<SyslogStats>,
}

/// Binds both listeners and starts them and the batcher in the background.
pub async fn spawn(state: AppState, config: SyslogConfig) -> std::io::Result<SyslogHandle> {
    let tcp = TcpListener::bind(config.tcp_addr).await?;
    let udp = UdpSocket::bind(config.udp_addr).await?;
    let stats = Arc::new(SyslogStats::default());
    let warnings = Arc::new(DropWarnings::default());
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let handle = SyslogHandle { tcp_addr: tcp.local_addr()?, udp_addr: udp.local_addr()?, stats: Arc::clone(&stats) };
    let intake = Arc::new(Intake { tx, stats: Arc::clone(&stats), warnings: Arc::clone(&warnings) });
    let batcher = Batcher { state, stats, warnings, max_rows: config.batch_max_rows, max_wait: config.batch_max_wait };
    tokio::spawn(batcher.run(rx));
    tokio::spawn(run_tcp(tcp, Arc::clone(&intake), config.idle_timeout));
    tokio::spawn(run_udp(udp, intake));
    Ok(handle)
}

/// One warning per [`WARN_EVERY`] at most, with how many drops it covers.
/// Never includes frame content: frames carry tokens.
#[derive(Debug, Default)]
struct DropWarnings {
    last: Mutex<Option<Instant>>,
    suppressed: AtomicU64,
}

impl DropWarnings {
    fn warn(&self, peer: Option<SocketAddr>, reason: &str) {
        let mut last = self.last.lock().expect("syslog warning lock poisoned");
        if last.is_some_and(|at| at.elapsed() < WARN_EVERY) {
            bump(&self.suppressed, 1);
            return;
        }
        *last = Some(Instant::now());
        let suppressed = self.suppressed.swap(0, Ordering::Relaxed);
        match peer {
            Some(peer) => tracing::warn!(%peer, reason, suppressed, "syslog frame dropped"),
            None => tracing::warn!(reason, suppressed, "syslog frame dropped"),
        }
    }
}

struct Pending {
    token_hash: String,
    event: LogEvent,
}

/// Parses frames from both listeners and queues them for the batcher.
struct Intake {
    tx: mpsc::Sender<Pending>,
    stats: Arc<SyslogStats>,
    warnings: Arc<DropWarnings>,
}

impl Intake {
    fn handle_frame(&self, frame: &[u8], peer: SocketAddr) {
        bump(&self.stats.received, 1);
        let message = match parse::parse(frame) {
            Ok(message) => message,
            Err(error) => {
                bump(&self.stats.malformed, 1);
                self.warnings.warn(Some(peer), &error.to_string());
                return;
            }
        };
        let Some(token) = message.token() else {
            bump(&self.stats.unauthenticated, 1);
            self.warnings.warn(Some(peer), "no [nami token=\"...\"] structured data");
            return;
        };
        let token_hash = hash_token(token);
        let event = message.into_event(Utc::now());
        if self.tx.try_send(Pending { token_hash, event }).is_err() {
            bump(&self.stats.overloaded, 1);
            self.warnings.warn(Some(peer), "ingest queue full");
        }
    }
}

async fn run_tcp(listener: TcpListener, intake: Arc<Intake>, idle_timeout: Duration) {
    let permits = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    loop {
        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, peer)) => {
                let intake = Arc::clone(&intake);
                tokio::spawn(async move {
                    read_connection(stream, peer, &intake, idle_timeout).await;
                    drop(permit);
                });
            }
            Err(error) => {
                // e.g. out of file descriptors: back off instead of spinning.
                tracing::warn!(error = %error, "syslog TCP accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn read_connection(stream: TcpStream, peer: SocketAddr, intake: &Intake, idle_timeout: Duration) {
    let mut frames = FrameReader::new(stream);
    loop {
        match tokio::time::timeout(idle_timeout, frames.next_frame()).await {
            // Idle for too long, closed by the peer, or a socket error.
            Err(_) | Ok(Ok(None)) | Ok(Err(FrameError::Io(_))) => return,
            Ok(Ok(Some(frame))) => intake.handle_frame(&frame, peer),
            Ok(Err(error)) => {
                bump(&intake.stats.malformed, 1);
                intake.warnings.warn(Some(peer), &error.to_string());
                return;
            }
        }
    }
}

async fn run_udp(socket: UdpSocket, intake: Arc<Intake>) {
    let mut buf = vec![0u8; MAX_FRAME_BYTES];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, peer)) => {
                let frame = buf[..len].trim_ascii();
                if !frame.is_empty() {
                    intake.handle_frame(frame, peer);
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "syslog UDP receive failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

struct Batcher {
    state: AppState,
    stats: Arc<SyslogStats>,
    warnings: Arc<DropWarnings>,
    max_rows: usize,
    max_wait: Duration,
}

impl Batcher {
    async fn run(self, mut rx: mpsc::Receiver<Pending>) {
        let mut batch = Vec::with_capacity(self.max_rows);
        loop {
            if rx.recv_many(&mut batch, self.max_rows).await == 0 {
                return;
            }
            let deadline = tokio::time::Instant::now() + self.max_wait;
            while batch.len() < self.max_rows {
                let room = self.max_rows - batch.len();
                match tokio::time::timeout_at(deadline, rx.recv_many(&mut batch, room)).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            self.store(std::mem::take(&mut batch)).await;
        }
    }

    async fn store(&self, batch: Vec<Pending>) {
        let state = &self.state;
        let mut hashes: Vec<String> = batch.iter().map(|p| p.token_hash.clone()).collect();
        hashes.sort_unstable();
        hashes.dedup();
        let sources: HashMap<String, source::IngestSource> = match source::find_by_token_hashes(&state.db, &hashes).await {
            Ok(found) => found.into_iter().collect(),
            Err(error) => {
                bump(&self.stats.store_failed, batch.len() as u64);
                tracing::error!(error = %error, frames = batch.len(), "syslog: could not look up source tokens");
                return;
            }
        };
        let today = Utc::now().date_naive();
        let mut blocked: HashMap<Uuid, bool> = HashMap::new();
        let mut accepted: HashMap<Uuid, (u64, u64)> = HashMap::new();
        let mut unauthenticated = 0u64;
        let mut rows = Vec::with_capacity(batch.len());
        for pending in batch {
            let Some(source) = sources.get(&pending.token_hash) else {
                unauthenticated += 1;
                continue;
            };
            let over_quota = match blocked.get(&source.id) {
                Some(over) => *over,
                None => {
                    let over = match source.daily_byte_quota {
                        None => false,
                        // A failed usage read lets the frame through: the
                        // quota bounds volume, it is not an access control.
                        Some(quota) => state
                            .logs
                            .usage
                            .bytes_today(&state.db, source.id, today)
                            .await
                            .is_ok_and(|used| used >= quota),
                    };
                    blocked.insert(source.id, over);
                    over
                }
            };
            if over_quota {
                bump(&self.stats.over_quota, 1);
                state.logs.usage.record_over_quota(source.id, today, 1);
                continue;
            }
            let row = store::build_row(pending.event, source);
            let counted = accepted.entry(source.id).or_default();
            counted.0 += 1;
            counted.1 += row.stored_bytes();
            rows.push(row);
        }
        if unauthenticated > 0 {
            bump(&self.stats.unauthenticated, unauthenticated);
            self.warnings.warn(None, "unknown source token");
        }
        if rows.is_empty() {
            return;
        }
        match store::insert_logs(&state.clickhouse, &rows).await {
            Ok(()) => {
                bump(&self.stats.accepted, rows.len() as u64);
                for (source_id, (events, bytes)) in accepted {
                    state.logs.usage.record_accepted(source_id, today, events, bytes);
                }
            }
            Err(error) => {
                bump(&self.stats.store_failed, rows.len() as u64);
                tracing::error!(error = %error, rows = rows.len(), "syslog: failed to store log rows");
            }
        }
    }
}
```

```diff
diff --git a/nami-engine/src/logs/mod.rs b/nami-engine/src/logs/mod.rs
--- a/nami-engine/src/logs/mod.rs
+++ b/nami-engine/src/logs/mod.rs
@@ -1,13 +1,14 @@
 //! Log management: turning ingested events into rows (`normalize`,
-//! `pattern`), storing them in ClickHouse (`store`), and HTTP ingestion
+//! `pattern`), storing them in ClickHouse (`store`), HTTP ingestion
 //! (`ingest`, `msgpack`) with its per-source rate limit (`rate_limit`) and
-//! usage accounting (`usage`).
+//! usage accounting (`usage`), and syslog ingestion (`syslog`).
 pub mod ingest;
 pub mod msgpack;
 pub mod normalize;
 pub mod pattern;
 pub mod rate_limit;
 pub mod store;
+pub mod syslog;
 pub mod usage;
 
 /// In-memory state of the ingest paths, one per engine instance: rate
```

The batch token lookup:

```diff
diff --git a/nami-engine/src/models/source.rs b/nami-engine/src/models/source.rs
--- a/nami-engine/src/models/source.rs
+++ b/nami-engine/src/models/source.rs
@@ -290,6 +290,27 @@ pub async fn find_by_token(pool: &PgPool, token: &str) -> Result<Option<IngestSo
         .await
 }
 
+/// The sources whose token hash is in `hashes`, as (token hash, source):
+/// one query for a whole batch of syslog frames.
+pub async fn find_by_token_hashes(
+    pool: &PgPool,
+    hashes: &[String],
+) -> Result<Vec<(String, IngestSource)>, sqlx::Error> {
+    #[derive(sqlx::FromRow)]
+    struct Row {
+        token_hash: String,
+        #[sqlx(flatten)]
+        source: IngestSource,
+    }
+    let rows: Vec<Row> = sqlx::query_as(
+        "select token_hash, id, project_id, retention_days, daily_byte_quota from sources where token_hash = any($1)",
+    )
+    .bind(hashes)
+    .fetch_all(pool)
+    .await?;
+    Ok(rows.into_iter().map(|row| (row.token_hash, row.source)).collect())
+}
+
 #[cfg(test)]
 mod tests {
     use super::*;
```

- [ ] **Step 6: Configuration and startup**

```diff
diff --git a/nami-engine/src/config.rs b/nami-engine/src/config.rs
--- a/nami-engine/src/config.rs
+++ b/nami-engine/src/config.rs
@@ -34,6 +34,12 @@ pub struct Config {
     /// (`RDAP_BASE_URL`, default `https://rdap.org`, which redirects to the
     /// registry's own RDAP server).
     pub rdap_base_url: String,
+    /// `SYSLOG_ENABLED=true` starts the RFC 5424 syslog listeners for log
+    /// ingestion (off by default; same spellings as `UPTIME_WORKER_ENABLED`).
+    pub syslog_enabled: bool,
+    /// `SYSLOG_TCP_PORT` and `SYSLOG_UDP_PORT`, default 5140 each.
+    pub syslog_tcp_port: u16,
+    pub syslog_udp_port: u16,
 }
 
 impl Config {
@@ -110,10 +116,25 @@ impl Config {
             encryption_key: crate::crypto::TEST_KEY,
             rdap_base_url: std::env::var("RDAP_BASE_URL")
                 .unwrap_or_else(|_| crate::uptime::checker::DEFAULT_RDAP_BASE_URL.to_string()),
+            syslog_enabled: env_flag("SYSLOG_ENABLED", false),
+            syslog_tcp_port: env_port("SYSLOG_TCP_PORT", crate::logs::syslog::DEFAULT_PORT),
+            syslog_udp_port: env_port("SYSLOG_UDP_PORT", crate::logs::syslog::DEFAULT_PORT),
         }
     }
 }
 
+/// A port number from the environment, `default` when unset; anything else
+/// stops startup.
+fn env_port(name: &str, default: u16) -> u16 {
+    match std::env::var(name) {
+        Ok(value) => value
+            .trim()
+            .parse()
+            .unwrap_or_else(|_| panic!("{name} must be a port number (0-65535), got {value:?}")),
+        Err(_) => default,
+    }
+}
+
 /// `NAMI_ENCRYPTION_KEY` is required outside tests: a missing or malformed
 /// key stops startup instead of running with secrets nobody can decrypt
 /// later (or encrypted under a key nobody kept).
```

```diff
diff --git a/nami-engine/src/main.rs b/nami-engine/src/main.rs
--- a/nami-engine/src/main.rs
+++ b/nami-engine/src/main.rs
@@ -60,6 +60,14 @@ async fn main() {
     // Every instance that ingests logs flushes its usage counters.
     tokio::spawn(nami_engine::logs::usage::run_forever(state.clone()));
 
+    if state.config.syslog_enabled {
+        let syslog_config = nami_engine::logs::syslog::SyslogConfig::from_config(&state.config);
+        let syslog = nami_engine::logs::syslog::spawn(state.clone(), syslog_config)
+            .await
+            .expect("failed to bind the syslog listeners (SYSLOG_TCP_PORT / SYSLOG_UDP_PORT)");
+        tracing::info!(tcp = %syslog.tcp_addr, udp = %syslog.udp_addr, "syslog listeners started");
+    }
+
     if state.config.uptime_worker_enabled {
         tokio::spawn(nami_engine::uptime::worker::run_forever(state.clone()));
         tracing::info!("uptime worker started");
```

```diff
diff --git a/nami-engine/.env.example b/nami-engine/.env.example
--- a/nami-engine/.env.example
+++ b/nami-engine/.env.example
@@ -39,3 +39,8 @@ NAMI_ENCRYPTION_KEY=
 # RDAP service for domain-expiry monitors; rdap.org redirects each lookup to
 # the registry's own RDAP server.
 RDAP_BASE_URL=https://rdap.org
+# RFC 5424 syslog ingestion over TCP and UDP (off by default). Frames
+# authenticate with the structured-data element [nami token="<source token>"].
+SYSLOG_ENABLED=false
+SYSLOG_TCP_PORT=5140
+SYSLOG_UDP_PORT=5140
```

- [ ] **Step 7: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/logs_syslog.rs` 6 passed (about 1 s), unit tests `logs::syslog::parse` 8 and `logs::syslog::framing` 4; 311 tests in total on the verified base.

- [ ] **Step 8: Try it by hand**

Start the engine with `SYSLOG_ENABLED=true` and send one frame: `printf '<165>1 %s web-1 nginx 4123 ID47 [nami token="%s"] hello syslog\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$TOKEN" | nc -w 1 127.0.0.1 5140`. It is stored within about a second (level `notice`, `fields.hostname` = `web-1`); the same frame with another token is dropped and a `syslog frame dropped` warning is logged without the frame's content.

- [ ] **Step 9: Commit**

```bash
git add nami-engine/src/logs nami-engine/src/models/source.rs nami-engine/src/config.rs nami-engine/src/main.rs \
  nami-engine/.env.example nami-engine/tests/logs_syslog.rs
git commit -m "feat(engine): RFC 5424 syslog ingestion over TCP and UDP (NAMI-LOG-4)

SYSLOG_ENABLED starts TCP (octet-counting and newline framing) and UDP
listeners on 5140. Frames authenticate with [nami token=\"...\"]; malformed,
unauthenticated and over-quota frames are dropped and counted, never
answered. Frames are stored in batches with one token lookup each; a
framing error or 5 minutes without a frame closes a TCP connection.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5 (NAMI-LOG-5): Log search and volume histogram

Branch `feat/NAMI-LOG-5-log-search`, worktree `.worktrees/NAMI-LOG-5/`.

**Files:**
- Create: `nami-engine/src/logs/query.rs`, `nami-engine/src/logs/search.rs`, `nami-engine/src/routes/logs.rs`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/logs/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`
- Test: `nami-engine/tests/logs_search.rs` (+ unit tests in `query.rs` and `search.rs`)

**Interfaces:**
- Consumes: `ClickHouse::query_rows_with` (Task 2); `logs::normalize::LEVELS` (Task 2); the ingest endpoint and `tests/common/logs.rs` helpers (Task 3) to load fixtures; `require_project_role`, `CurrentUser`.
- Produces:
  - `logs::query`: consts `DEFAULT_WINDOW` (1 h), `MAX_WINDOW` (31 d), `MAX_FILTERS = 10`, `MAX_PATH_DEPTH = 5`, `MAX_SEGMENT_LEN = 64`, `MAX_REGEX_LEN = 256`, `MAX_TEXT_LEN = 1024`, `MAX_LIST_VALUES = 100`, `DEFAULT_LIMIT = 100`, `MAX_LIMIT = 500`, `ERROR_LEVELS_SQL`; `FilterOp` (serde `snake_case`), `FilterValue::{None, Text(String), Number(f64)}`, `FieldFilter { path: Vec<String>, op: FilterOp, value: FilterValue }`, `LogFilters { source_ids: Vec<Uuid>, levels: Vec<String>, q: Option<String>, in_fields: bool, filters: Vec<FieldFilter>, trace_id: Option<String>, span_id: Option<String>, pattern_hashes: Vec<u64> }` (`Default`), `Window { from, to: DateTime<Utc> }`, `Cursor { dt_ms: i64, id: Uuid }` with `encode(&self) -> String` / `decode(&str) -> Option<Cursor>`, `SearchParams { window, filters, limit: u32, cursor: Option<Cursor> }`; `query_pairs(raw: Option<&str>) -> Vec<(String, String)>`; `parse_filters(&[(String, String)]) -> Result<LogFilters, String>`; `parse_filter(raw: &str) -> Result<FieldFilter, String>`; `parse_window(&[(String, String)], now: DateTime<Utc>) -> Result<Window, String>`; `parse_search(&[(String, String)], now) -> Result<SearchParams, String>`; `Sql::{new() -> Sql, push(&mut self, &'static str) -> &mut Sql, bind(&mut self, impl Into<String>) -> usize, placeholder(&mut self, usize, &'static str) -> &mut Sql, value(&mut self, &'static str, impl Into<String>) -> &mut Sql, text(&self) -> &str, params(&self) -> Vec<(&str, String)>}`; `push_where(&mut Sql, project_id: Uuid, &LogFilters)`; `push_window(&mut Sql, &Window)`.
  - `logs::search`: `SEARCH_SETTINGS` (`max_execution_time=30`), `ENTRY_COLUMNS` (the SELECT list + `FROM logs`), `LogEntry { id: Uuid, source_id: Uuid, dt: DateTime<Utc>, level: String, message: String, fields: Value, trace_id: String, span_id: String, pattern_hash: String }` (`Serialize`) with `cursor(&self) -> Cursor`, `RawEntry` (`Deserialize`, `impl From<RawEntry> for LogEntry`), `SearchPage { logs: Vec<LogEntry>, next_cursor: Option<String> }`, `user_error(ClickHouseError) -> AppError`, `search(&ClickHouse, project_id: Uuid, &SearchParams) -> Result<SearchPage, AppError>`, `HISTOGRAM_STEPS`, `TARGET_BUCKETS = 60`, `histogram_step(&Window) -> i64`, `Histogram { step_seconds: i64, total: u64, buckets: Vec<HistogramBucket> }`, `HistogramBucket { start: DateTime<Utc>, count: u64, errors: u64, warnings: u64 }`, `histogram(&ClickHouse, project_id, &Window, &LogFilters) -> Result<Histogram, AppError>`.
  - Routes (`routes::logs::router()`): `GET /projects/{id}/logs`, `GET /projects/{id}/logs/histogram`.

- [ ] **Step 1: Add the crate**

```bash
cd nami-engine
cargo add form_urlencoded@1.2.2
```

- [ ] **Step 2: Write the failing tests**

`nami-engine/tests/logs_search.rs` (newest first and project-confined, pagination over equal timestamps, every filter and operator, 400s, injection, the histogram, RBAC, a catastrophic regex):

```rust
// Log search and histogram (`GET /projects/{id}/logs`, `/logs/histogram`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::collections::HashSet;

use axum::{http::StatusCode, Router};
use chrono::{DateTime, Duration, DurationRound, SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json, seed_source},
    seed_project, state_with_clickhouse, user_with_role,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn parse_ts(value: &Value) -> DateTime<Utc> {
    value.as_str().unwrap().parse().unwrap()
}

fn filter(value: Value) -> String {
    format!("filter={}", encode(&value.to_string()))
}

struct Fixture {
    app: Router,
    ch: nami_engine::clickhouse::ClickHouse,
    project: Uuid,
    other_project: Uuid,
    api: Uuid,
    worker: Uuid,
    foreign_source: Uuid,
    viewer: axum::http::HeaderValue,
    base: DateTime<Utc>,
}

const EVIL: &str = "'; DROP TABLE logs; -- {p0:String} sleep(3) back\\slash\nnext line";

/// Five rows in `project` (two sources) and one in `other_project`, one
/// second apart from `base` (30 minutes ago).
async fn fixture(pool: &sqlx::PgPool) -> Fixture {
    let (state, ch) = state_with_clickhouse(pool).await;
    let app = app(state);
    let project = seed_project(pool).await;
    let other_project = seed_project(pool).await;
    let (api, api_token) = seed_source(pool, project, "api").await;
    let (worker, worker_token) = seed_source(pool, project, "worker").await;
    let (foreign_source, foreign_token) = seed_source(pool, other_project, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, pool, project, Some("viewer")).await;
    // A multiple of 10 s, so histogram buckets of 2 s start exactly at base.
    let base = (Utc::now() - Duration::minutes(30)).duration_trunc(Duration::seconds(10)).unwrap();
    let at = |s: i64| ts(base + Duration::seconds(s));
    let accepted = (StatusCode::ACCEPTED, json!({ "accepted": 1 }));
    for (token, event) in [
        (&api_token, json!({ "dt": at(1), "level": "info", "message": "user 42 logged in from 10.0.0.7",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7",
            "status": 200, "latency_ms": 12.5, "user": { "name": "O'Brien", "tier": "gold" } })),
        (&api_token, json!({ "dt": at(2), "level": "error", "message": "Disk FULL on /var",
            "status": "500", "path": "/api/orders" })),
        (&worker_token, json!({ "dt": at(3), "level": "warn", "message": "retrying job 7",
            "status": 404, "duration": "250" })),
        (&worker_token, json!({ "dt": at(4), "level": "debug", "message": "100% done_x" })),
        (&api_token, json!({ "dt": at(5), "level": "info", "message": EVIL, "note": "'; DROP TABLE logs; --" })),
        (&foreign_token, json!({ "dt": at(3), "level": "error", "message": "other project secret", "status": 500 })),
    ] {
        assert_eq!(post_json(&app, token, event).await, accepted);
    }
    Fixture { app, ch, project, other_project, api, worker, foreign_source, viewer, base }
}

impl Fixture {
    fn window(&self) -> String {
        format!("from={}&to={}", encode(&ts(self.base - Duration::minutes(1))), encode(&ts(Utc::now())))
    }

    async fn get(&self, path: &str, query: &str) -> (StatusCode, Value) {
        let uri = format!("/projects/{}/logs{path}?{}&{query}", self.project, self.window());
        call(&self.app, "GET", &uri, Some(&self.viewer), None).await
    }

    /// The messages a search returns, in order.
    async fn messages(&self, query: &str) -> Vec<String> {
        let (status, page) = self.get("", query).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        page["logs"].as_array().unwrap().iter().map(|row| row["message"].as_str().unwrap().to_string()).collect()
    }
}

const M1: &str = "user 42 logged in from 10.0.0.7";
const M2: &str = "Disk FULL on /var";
const M3: &str = "retrying job 7";
const M4: &str = "100% done_x";

#[sqlx::test]
async fn search_is_newest_first_and_confined_to_the_project(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (status, page) = f.get("", "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let logs = page["logs"].as_array().unwrap();
    let messages: Vec<&str> = logs.iter().map(|row| row["message"].as_str().unwrap()).collect();
    assert_eq!(messages, [EVIL, M4, M3, M2, M1]);
    assert_eq!(page["next_cursor"], Value::Null);
    let first = logs.last().unwrap();
    assert_eq!(first["fields"], json!({ "status": 200, "latency_ms": 12.5, "user": { "name": "O'Brien", "tier": "gold" } }));
    assert_eq!(first["source_id"], f.api.to_string());
    assert_eq!(first["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert!(first["pattern_hash"].as_str().unwrap().parse::<u64>().is_ok(), "pattern_hash is a decimal string");
    assert_eq!(parse_ts(&first["dt"]), f.base + Duration::seconds(1));

    // Security: another project's source id matches nothing here.
    assert!(f.messages(&format!("source_id={}", f.foreign_source)).await.is_empty());
    let other = format!("/projects/{}/logs?{}", f.other_project, f.window());
    assert_eq!(call(&f.app, "GET", &other, Some(&f.viewer), None).await.0, StatusCode::FORBIDDEN);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn pagination_returns_every_row_exactly_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let base = (Utc::now() - Duration::minutes(10)).duration_trunc(Duration::seconds(1)).unwrap();
    // Seven rows; three share one timestamp, so the id breaks the tie.
    let events: Vec<Value> = [0, 1, 1, 1, 2, 3, 4]
        .iter()
        .enumerate()
        .map(|(i, s)| json!({ "dt": ts(base + Duration::seconds(*s)), "message": format!("m{i}") }))
        .collect();
    assert_eq!(post_json(&app, &token, Value::Array(events)).await.0, StatusCode::ACCEPTED);

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut sizes = Vec::new();
    loop {
        let mut uri = format!("/projects/{project}/logs?limit=2&from={}", encode(&ts(base - Duration::minutes(1))));
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={c}"));
        }
        let (status, page) = call(&app, "GET", &uri, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let logs = page["logs"].as_array().unwrap();
        sizes.push(logs.len());
        seen.extend(logs.iter().map(|row| (row["dt"].as_str().unwrap().to_string(), row["id"].as_str().unwrap().to_string())));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(sizes, [2, 2, 2, 1]);
    let ids: HashSet<&String> = seen.iter().map(|(_, id)| id).collect();
    assert_eq!(ids.len(), 7, "no row twice, none skipped");
    assert!(seen.windows(2).all(|pair| pair[0].0 >= pair[1].0), "newest first: {seen:?}");
    for query in ["limit=0", "limit=501", "cursor=bogus"] {
        let (status, _) = call(&app, "GET", &format!("/projects/{project}/logs?{query}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn levels_sources_text_and_ids_narrow_the_results(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    assert_eq!(f.messages("level=error&level=WARN").await, [M3, M2]);
    assert_eq!(f.messages(&format!("source_id={}", f.worker)).await, [M4, M3]);
    assert_eq!(f.messages(&format!("source_id={}&source_id={}", f.worker, f.api)).await.len(), 5);
    assert_eq!(f.messages("q=disk%20full").await, [M2], "q is case-insensitive");
    assert_eq!(f.messages("q=100%25").await, [M4], "% is literal, not a wildcard");
    assert_eq!(f.messages("q=_").await, [M4], "_ is literal too");
    assert!(f.messages("q=orders").await.is_empty(), "fields are searched only with in_fields");
    assert_eq!(f.messages("q=orders&in_fields=true").await, [M2]);
    assert_eq!(f.messages("trace_id=4BF92F3577B34DA6A3CE929D0E0E4736").await, [M1]);
    assert_eq!(f.messages("span_id=00f067aa0ba902b7").await, [M1]);
    let (_, page) = f.get("", "").await;
    let hash = page["logs"].as_array().unwrap().last().unwrap()["pattern_hash"].as_str().unwrap().to_string();
    assert_eq!(f.messages(&format!("pattern_hash={hash}")).await, [M1]);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn field_filters_support_every_operator(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    for (filters, expected) in [
        (vec![json!({ "path": "status", "op": "eq", "value": 200 })], vec![M1]),
        (vec![json!({ "path": "status", "op": "eq", "value": "500" })], vec![M2]),
        (vec![json!({ "path": "status", "op": "neq", "value": "200" })], vec![EVIL, M4, M3, M2]),
        (vec![json!({ "path": "user.name", "op": "contains", "value": "BRIEN" })], vec![M1]),
        (vec![json!({ "path": "path", "op": "exists" })], vec![M2]),
        (vec![json!({ "path": "path", "op": "regex", "value": "^/api/" })], vec![M2]),
        (vec![json!({ "path": "status", "op": "not_regex", "value": "^5" })], vec![EVIL, M4, M3, M1]),
        (vec![json!({ "path": "status", "op": "gt", "value": 300 })], vec![M3, M2]),
        (vec![json!({ "path": "latency_ms", "op": "gte", "value": "12.5" })], vec![M1]),
        (vec![json!({ "path": "duration", "op": "lt", "value": 300 })], vec![M3]),
        (vec![json!({ "path": "status", "op": "lte", "value": 200 })], vec![M1]),
        (vec![json!({ "path": "user.tier", "op": "eq", "value": "gold" })], vec![M1]),
        (vec![json!({ "path": "user.name", "op": "gt", "value": 0 })], vec![]),
        (vec![json!({ "path": "user", "op": "eq", "value": "{\"name\":\"O'Brien\",\"tier\":\"gold\"}" })], vec![M1]),
        (
            vec![json!({ "path": "status", "op": "gt", "value": 100 }), json!({ "path": "path", "op": "exists" })],
            vec![M2],
        ),
    ] {
        let query = filters.iter().map(|value| filter(value.clone())).collect::<Vec<_>>().join("&");
        assert_eq!(f.messages(&query).await, expected, "{filters:?}");
    }
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn invalid_parameters_are_400(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let project = f.project;
    let now = Utc::now();
    for query in [
        format!("from={}&to={}", encode(&ts(now - Duration::days(32))), encode(&ts(now))),
        format!("from={}&to={}", encode(&ts(now)), encode(&ts(now - Duration::hours(1)))),
        "from=yesterday".to_string(),
        "source_id=not-a-uuid".to_string(),
        "level=verbose".to_string(),
        "in_fields=maybe".to_string(),
        "trace_id=xyz".to_string(),
        "pattern_hash=-1".to_string(),
        format!("q={}", "x".repeat(1025)),
        "filter=not-json".to_string(),
        filter(json!({ "path": "a", "op": "like", "value": "x" })),
        filter(json!({ "path": "a.b.c.d.e.f", "op": "exists" })),
        filter(json!({ "path": "a;b", "op": "exists" })),
        filter(json!({ "path": "status", "op": "gt", "value": "abc" })),
        filter(json!({ "path": "status", "op": "regex", "value": "(a" })),
        filter(json!({ "path": "status", "op": "regex", "value": "a".repeat(257) })),
        // Valid for Rust's regex crate (verbose mode), rejected by ClickHouse's RE2.
        filter(json!({ "path": "status", "op": "regex", "value": "(?x)5 0 0" })),
        (0..11).map(|_| filter(json!({ "path": "a", "op": "exists" }))).collect::<Vec<_>>().join("&"),
    ] {
        for path in ["", "/histogram"] {
            let (status, json) =
                call(&f.app, "GET", &format!("/projects/{project}/logs{path}?{query}"), Some(&f.viewer), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}?{query}: {json}");
            assert!(json["error"].is_string(), "{json}");
        }
    }
    f.ch.drop_database().await.unwrap();
}

/// Security: hostile text in q, filter values and paths is matched
/// literally or rejected, never executed.
#[sqlx::test]
async fn injection_attempts_are_matched_literally_or_rejected(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    for q in ["'; DROP TABLE logs; --", "{p0:String}", "sleep(3)", "back\\slash", "slash\nnext", "logs; --"] {
        assert_eq!(f.messages(&format!("q={}", encode(q))).await, [EVIL], "{q:?}");
    }
    let started = std::time::Instant::now();
    assert!(f.messages(&format!("q={}", encode("sleep(3) or 1=1"))).await.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "sleep(3) was not executed");
    for value in [
        json!({ "path": "note", "op": "eq", "value": "'; DROP TABLE logs; --" }),
        json!({ "path": "note", "op": "contains", "value": "drop table" }),
        json!({ "path": "note", "op": "regex", "value": "^'; DROP" }),
    ] {
        assert_eq!(f.messages(&filter(value.clone())).await, [EVIL], "{value}");
    }
    for path in ["note'", "note\"", "a) OR (1=1", "note\nx", "note\\", "fields", "a.b.c.d.e.f"] {
        let (status, _) = f.get("", &filter(json!({ "path": path, "op": "exists" }))).await;
        if path == "fields" {
            assert_eq!(status, StatusCode::OK, "a plain key named fields is just a key");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}");
        }
    }
    for query in ["level=error%27%20OR%20%271%27%3D%271", "source_id=x%27%20OR%201%3D1%20--"] {
        assert_eq!(f.get("", query).await.0, StatusCode::BAD_REQUEST, "{query}");
    }
    // Nothing was dropped or changed.
    assert_eq!(f.messages("").await.len(), 5);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_histogram_counts_rows_per_bucket_with_a_level_breakdown(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let window = format!(
        "from={}&to={}",
        encode(&ts(f.base - Duration::seconds(60))),
        encode(&ts(f.base + Duration::seconds(60)))
    );
    let uri = format!("/projects/{}/logs/histogram?{window}", f.project);
    let (status, histogram) = call(&f.app, "GET", &uri, Some(&f.viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{histogram}");
    assert_eq!(histogram["step_seconds"], 2);
    let buckets = histogram["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 61);
    assert_eq!(histogram["total"], 5, "the other project's row is not counted");
    assert_eq!(buckets.iter().map(|b| b["count"].as_u64().unwrap()).sum::<u64>(), 5);
    assert_eq!(buckets.iter().map(|b| b["errors"].as_u64().unwrap()).sum::<u64>(), 1);
    assert_eq!(buckets.iter().map(|b| b["warnings"].as_u64().unwrap()).sum::<u64>(), 1);
    let at = |seconds: i64| {
        let start = f.base + Duration::seconds(seconds);
        buckets.iter().find(|b| parse_ts(&b["start"]) == start).unwrap()["count"].as_u64().unwrap()
    };
    assert_eq!(at(0), 1, "base+1s falls in the bucket starting at base");
    assert_eq!(at(2), 2);
    assert_eq!(at(4), 2);
    assert!(buckets.windows(2).all(|pair| parse_ts(&pair[0]["start"]) + Duration::seconds(2) == parse_ts(&pair[1]["start"])));

    let (_, errors_only) = call(&f.app, "GET", &format!("{uri}&level=error"), Some(&f.viewer), None).await;
    assert_eq!(errors_only["total"], 1);
    let (_, hour) = call(&f.app, "GET", &format!("/projects/{}/logs/histogram", f.project), Some(&f.viewer), None).await;
    assert_eq!(hour["step_seconds"], 60);
    assert_eq!(hour["total"], 5);
    assert!((60..=61).contains(&hour["buckets"].as_array().unwrap().len()));
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn search_needs_a_session_and_project_access(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (_, _, outsider) = user_with_role(&f.app, &pool, f.project, None).await;
    for path in ["/logs", "/logs/histogram"] {
        let uri = format!("/projects/{}{path}", f.project);
        assert_eq!(call(&f.app, "GET", &uri, None, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&f.app, "GET", &uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&f.app, "GET", &uri, Some(&f.viewer), None).await.0, StatusCode::OK);
    }
    f.ch.drop_database().await.unwrap();
}

/// A pattern that backtracks catastrophically in a backtracking engine runs
/// in linear time under RE2.
#[sqlx::test]
async fn catastrophic_regexes_finish_quickly(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (_, token) = seed_source(&pool, f.project, "big").await;
    let event = json!({ "dt": ts(f.base + Duration::seconds(10)), "message": "big", "s": format!("{}!", "a".repeat(60_000)) });
    assert_eq!(post_json(&f.app, &token, event).await.0, StatusCode::ACCEPTED);
    let started = std::time::Instant::now();
    let found = f.messages(&filter(json!({ "path": "s", "op": "regex", "value": "(a+)+$" }))).await;
    assert!(found.is_empty(), "{found:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    f.ch.drop_database().await.unwrap();
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test logs_search`
Expected: FAIL — the requests get 404 (no `/projects/{id}/logs` route yet) and the first assertion fails with `left: 404, right: 200`.

- [ ] **Step 4: Implement parameters, filters and the SQL builder**

`nami-engine/src/logs/query.rs` (unit tests include "user input never reaches the SQL text"):

```rust
//! Log search parameters — shared by search, histogram and live tail — and
//! their translation into ClickHouse SQL.
//!
//! Injection safety: [`Sql`] only appends `&'static str` fragments and
//! numbered placeholders (`{p3:String}`); every value — the project id,
//! JSON path segments, `q`, filter values and regexes — travels as a query
//! parameter. No user input can reach the SQL text.

use std::fmt::Write as _;

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::normalize::LEVELS;

pub const DEFAULT_WINDOW: Duration = Duration::hours(1);
pub const MAX_WINDOW: Duration = Duration::days(31);
pub const MAX_FILTERS: usize = 10;
pub const MAX_PATH_DEPTH: usize = 5;
pub const MAX_SEGMENT_LEN: usize = 64;
pub const MAX_REGEX_LEN: usize = 256;
/// Longest `q` and longest text filter value, in characters.
pub const MAX_TEXT_LEN: usize = 1024;
/// Most values of a repeatable list parameter (`source_id`, `level`,
/// `pattern_hash`).
pub const MAX_LIST_VALUES: usize = 100;
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;
/// Levels counted as errors by the histogram.
pub const ERROR_LEVELS_SQL: &str = "('error', 'critical', 'alert', 'emergency', 'fatal')";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Neq,
    Contains,
    Exists,
    Regex,
    NotRegex,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    None,
    Text(String),
    Number(f64),
}

/// One `filter` parameter: `{"path":"a.b","op":"eq","value":"…"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldFilter {
    pub path: Vec<String>,
    pub op: FilterOp,
    pub value: FilterValue,
}

/// What rows to match, besides the project and the time window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogFilters {
    pub source_ids: Vec<Uuid>,
    pub levels: Vec<String>,
    pub q: Option<String>,
    pub in_fields: bool,
    pub filters: Vec<FieldFilter>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub pattern_hashes: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Opaque keyset position: the `(dt, id)` of the last row returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub dt_ms: i64,
    pub id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{}:{}", self.dt_ms, self.id))
    }

    pub fn decode(raw: &str) -> Option<Cursor> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw).ok()?;
        let text = String::from_utf8(bytes).ok()?;
        let (dt_ms, id) = text.split_once(':')?;
        Some(Cursor { dt_ms: dt_ms.parse().ok()?, id: id.parse().ok()? })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchParams {
    pub window: Window,
    pub filters: LogFilters,
    pub limit: u32,
    pub cursor: Option<Cursor>,
}

/// The decoded query string; repeated keys are kept in order.
pub fn query_pairs(raw: Option<&str>) -> Vec<(String, String)> {
    form_urlencoded::parse(raw.unwrap_or("").as_bytes()).into_owned().collect()
}

fn values<'a>(pairs: &'a [(String, String)], key: &str) -> impl Iterator<Item = &'a str> {
    pairs.iter().filter(move |(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn last<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values(pairs, key).last()
}

/// `source_id`, `level`, `q`, `in_fields`, `filter`, `trace_id`, `span_id`
/// and `pattern_hash`; other keys are ignored.
pub fn parse_filters(pairs: &[(String, String)]) -> Result<LogFilters, String> {
    let mut filters = LogFilters::default();
    for raw in values(pairs, "source_id") {
        let id = Uuid::parse_str(raw.trim()).map_err(|_| "source_id must be a UUID".to_string())?;
        filters.source_ids.push(id);
    }
    for raw in values(pairs, "level") {
        let level = raw.trim().to_ascii_lowercase();
        if !LEVELS.contains(&level.as_str()) {
            return Err(format!("level must be one of {}", LEVELS.join(", ")));
        }
        filters.levels.push(level);
    }
    for raw in values(pairs, "pattern_hash") {
        let hash = raw.trim().parse::<u64>().map_err(|_| "pattern_hash must be an unsigned 64-bit integer".to_string())?;
        filters.pattern_hashes.push(hash);
    }
    if [filters.source_ids.len(), filters.levels.len(), filters.pattern_hashes.len()]
        .iter()
        .any(|len| *len > MAX_LIST_VALUES)
    {
        return Err(format!("at most {MAX_LIST_VALUES} values per source_id, level or pattern_hash"));
    }
    if let Some(q) = last(pairs, "q").filter(|q| !q.is_empty()) {
        if q.chars().count() > MAX_TEXT_LEN {
            return Err(format!("q must be at most {MAX_TEXT_LEN} characters"));
        }
        filters.q = Some(q.to_string());
    }
    filters.in_fields = match last(pairs, "in_fields") {
        None | Some("") | Some("false") | Some("0") => false,
        Some("true") | Some("1") => true,
        Some(_) => return Err("in_fields must be true or false".to_string()),
    };
    if let Some(raw) = last(pairs, "trace_id").filter(|v| !v.is_empty()) {
        filters.trace_id = Some(hex_id(raw, 32, "trace_id")?);
    }
    if let Some(raw) = last(pairs, "span_id").filter(|v| !v.is_empty()) {
        filters.span_id = Some(hex_id(raw, 16, "span_id")?);
    }
    for (index, raw) in values(pairs, "filter").enumerate() {
        if index == MAX_FILTERS {
            return Err(format!("at most {MAX_FILTERS} filters"));
        }
        filters.filters.push(parse_filter(raw).map_err(|e| format!("filter {}: {e}", index + 1))?);
    }
    Ok(filters)
}

fn hex_id(raw: &str, len: usize, name: &str) -> Result<String, String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.len() == len && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(format!("{name} must be {len} hex characters"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFilter {
    path: String,
    op: FilterOp,
    #[serde(default)]
    value: Option<Value>,
}

pub fn parse_filter(raw: &str) -> Result<FieldFilter, String> {
    let raw: RawFilter = serde_json::from_str(raw).map_err(|_| {
        "must be JSON like {\"path\":\"a.b\",\"op\":\"eq\",\"value\":\"x\"} with op one of eq, neq, contains, \
         exists, regex, not_regex, gt, gte, lt, lte"
            .to_string()
    })?;
    let path: Vec<String> = raw.path.split('.').map(str::to_string).collect();
    if path.len() > MAX_PATH_DEPTH {
        return Err(format!("path may have at most {MAX_PATH_DEPTH} segments"));
    }
    let valid_segment = |segment: &String| {
        (1..=MAX_SEGMENT_LEN).contains(&segment.len())
            && segment.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if !path.iter().all(valid_segment) {
        return Err("path segments must be 1-64 characters of A-Z, a-z, 0-9, _ or -".to_string());
    }
    let value = match raw.op {
        FilterOp::Exists => FilterValue::None,
        FilterOp::Eq | FilterOp::Neq | FilterOp::Contains => {
            let text = match raw.value {
                Some(Value::String(text)) => text,
                Some(value @ (Value::Number(_) | Value::Bool(_))) => value.to_string(),
                _ => return Err("value must be a string, number or boolean".to_string()),
            };
            if text.chars().count() > MAX_TEXT_LEN {
                return Err(format!("value must be at most {MAX_TEXT_LEN} characters"));
            }
            FilterValue::Text(text)
        }
        FilterOp::Regex | FilterOp::NotRegex => {
            let Some(Value::String(pattern)) = raw.value else {
                return Err("value must be a regular expression string".to_string());
            };
            if pattern.chars().count() > MAX_REGEX_LEN {
                return Err(format!("regex must be at most {MAX_REGEX_LEN} characters"));
            }
            regex::RegexBuilder::new(&pattern)
                .size_limit(1 << 20)
                .build()
                .map_err(|_| "regex is not a valid regular expression".to_string())?;
            FilterValue::Text(pattern)
        }
        FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte => {
            let number = match raw.value {
                Some(Value::Number(number)) => number.as_f64(),
                Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
                _ => None,
            };
            match number {
                Some(number) if number.is_finite() => FilterValue::Number(number),
                _ => return Err("value must be a finite number for gt, gte, lt and lte".to_string()),
            }
        }
    };
    Ok(FieldFilter { path, op: raw.op, value })
}

/// `from` / `to` (RFC 3339): `to` defaults to now and `from` to one hour
/// before `to`; at most 31 days apart.
pub fn parse_window(pairs: &[(String, String)], now: DateTime<Utc>) -> Result<Window, String> {
    let instant = |key: &str| -> Result<Option<DateTime<Utc>>, String> {
        match last(pairs, key).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(raw) => DateTime::parse_from_rfc3339(raw.trim())
                .map(|dt| Some(dt.with_timezone(&Utc)))
                .map_err(|_| format!("{key} must be an RFC 3339 timestamp")),
        }
    };
    let to = instant("to")?.unwrap_or(now);
    let from = instant("from")?.unwrap_or(to - DEFAULT_WINDOW);
    if from >= to {
        return Err("from must be before to".to_string());
    }
    if to - from > MAX_WINDOW {
        return Err("the time window may span at most 31 days".to_string());
    }
    Ok(Window { from, to })
}

pub fn parse_search(pairs: &[(String, String)], now: DateTime<Utc>) -> Result<SearchParams, String> {
    let window = parse_window(pairs, now)?;
    let filters = parse_filters(pairs)?;
    let limit = match last(pairs, "limit").filter(|v| !v.is_empty()) {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
            _ => return Err(format!("limit must be between 1 and {MAX_LIMIT}")),
        },
    };
    let cursor = match last(pairs, "cursor").filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => Some(Cursor::decode(raw.trim()).ok_or_else(|| "invalid cursor".to_string())?),
    };
    Ok(SearchParams { window, filters, limit, cursor })
}

/// SQL text built from fixed fragments plus numbered parameters.
#[derive(Debug, Default)]
pub struct Sql {
    text: String,
    params: Vec<(String, String)>,
}

impl Sql {
    pub fn new() -> Sql {
        Sql::default()
    }

    pub fn push(&mut self, fragment: &'static str) -> &mut Sql {
        self.text.push_str(fragment);
        self
    }

    /// Adds a parameter and returns its index (it can be used several times).
    pub fn bind(&mut self, value: impl Into<String>) -> usize {
        let index = self.params.len();
        self.params.push((format!("p{index}"), value.into()));
        index
    }

    /// Appends the placeholder `{p<index>:<ty>}`.
    pub fn placeholder(&mut self, index: usize, ty: &'static str) -> &mut Sql {
        write!(self.text, "{{p{index}:{ty}}}").expect("writing to a String cannot fail");
        self
    }

    /// Binds `value` and appends its placeholder.
    pub fn value(&mut self, ty: &'static str, value: impl Into<String>) -> &mut Sql {
        let index = self.bind(value);
        self.placeholder(index, ty)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The parameters in the shape `ClickHouse::query_rows` takes.
    pub fn params(&self) -> Vec<(&str, String)> {
        self.params.iter().map(|(name, value)| (name.as_str(), value.clone())).collect()
    }

    fn list<T: ToString>(&mut self, ty: &'static str, items: &[T]) {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.value(ty, item.to_string());
        }
    }
}

/// ` WHERE project_id = … AND <filters>`: never rows of another project.
pub fn push_where(sql: &mut Sql, project_id: Uuid, filters: &LogFilters) {
    sql.push(" WHERE project_id = ").value("UUID", project_id.to_string());
    if !filters.source_ids.is_empty() {
        sql.push(" AND source_id IN (");
        sql.list("UUID", &filters.source_ids);
        sql.push(")");
    }
    if !filters.levels.is_empty() {
        sql.push(" AND level IN (");
        sql.list("String", &filters.levels);
        sql.push(")");
    }
    if let Some(q) = &filters.q {
        let q = sql.bind(q.clone());
        sql.push(" AND (positionCaseInsensitiveUTF8(message, ").placeholder(q, "String").push(") > 0");
        if filters.in_fields {
            sql.push(" OR positionCaseInsensitiveUTF8(fields, ").placeholder(q, "String").push(") > 0");
        }
        sql.push(")");
    }
    if let Some(trace_id) = &filters.trace_id {
        sql.push(" AND trace_id = ").value("String", trace_id.clone());
    }
    if let Some(span_id) = &filters.span_id {
        sql.push(" AND span_id = ").value("String", span_id.clone());
    }
    if !filters.pattern_hashes.is_empty() {
        sql.push(" AND pattern_hash IN (");
        sql.list("UInt64", &filters.pattern_hashes);
        sql.push(")");
    }
    for filter in &filters.filters {
        sql.push(" AND ");
        push_filter(sql, filter);
    }
}

/// ` AND dt >= from AND dt <= to`.
pub fn push_window(sql: &mut Sql, window: &Window) {
    sql.push(" AND dt >= fromUnixTimestamp64Milli(")
        .value("Int64", window.from.timestamp_millis().to_string())
        .push(", 'UTC') AND dt <= fromUnixTimestamp64Milli(")
        .value("Int64", window.to.timestamp_millis().to_string())
        .push(", 'UTC')");
}

fn push_path(sql: &mut Sql, segments: &[usize]) {
    for segment in segments {
        sql.push(", ").placeholder(*segment, "String");
    }
}

/// The value at the path as text: strings unquoted, anything else as its
/// JSON text (`200`, `true`, `{"a":1}`).
fn push_text(sql: &mut Sql, segments: &[usize]) {
    sql.push("if(JSONType(fields");
    push_path(sql, segments);
    sql.push(") = 'String', JSONExtractString(fields");
    push_path(sql, segments);
    sql.push("), JSONExtractRaw(fields");
    push_path(sql, segments);
    sql.push("))");
}

/// The value at the path as a number: JSON numbers, and strings that parse
/// as one; NULL (never matching) for anything else or a missing field.
fn push_number(sql: &mut Sql, segments: &[usize]) {
    sql.push("multiIf(JSONType(fields");
    push_path(sql, segments);
    sql.push(") IN ('Int64', 'UInt64', 'Double'), JSONExtractFloat(fields");
    push_path(sql, segments);
    sql.push("), JSONType(fields");
    push_path(sql, segments);
    sql.push(") = 'String', toFloat64OrNull(JSONExtractString(fields");
    push_path(sql, segments);
    sql.push(")), NULL)");
}

/// `neq` and `not_regex` also match rows without the field; the numeric
/// operators never match a missing or non-numeric field.
fn push_filter(sql: &mut Sql, filter: &FieldFilter) {
    let segments: Vec<usize> = filter.path.iter().map(|segment| sql.bind(segment.clone())).collect();
    match (&filter.op, &filter.value) {
        (FilterOp::Exists, _) => {
            sql.push("JSONHas(fields");
            push_path(sql, &segments);
            sql.push(")");
        }
        (FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte, FilterValue::Number(number)) => {
            let value = sql.bind(number.to_string());
            push_number(sql, &segments);
            sql.push(match filter.op {
                FilterOp::Gt => " > ",
                FilterOp::Gte => " >= ",
                FilterOp::Lt => " < ",
                _ => " <= ",
            });
            sql.placeholder(value, "Float64");
        }
        (op, FilterValue::Text(text)) => {
            let value = sql.bind(text.clone());
            if matches!(op, FilterOp::Neq | FilterOp::NotRegex) {
                sql.push("NOT ");
            }
            sql.push("(JSONHas(fields");
            push_path(sql, &segments);
            sql.push(") AND ");
            match op {
                FilterOp::Contains => {
                    sql.push("positionCaseInsensitiveUTF8(");
                    push_text(sql, &segments);
                    sql.push(", ").placeholder(value, "String").push(") > 0");
                }
                FilterOp::Regex | FilterOp::NotRegex => {
                    sql.push("match(");
                    push_text(sql, &segments);
                    sql.push(", ").placeholder(value, "String").push(")");
                }
                _ => {
                    push_text(sql, &segments);
                    sql.push(" = ").placeholder(value, "String");
                }
            }
            sql.push(")");
        }
        // `parse_filter` pairs every operator with its value kind.
        _ => {
            sql.push("0");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(query: &str) -> Vec<(String, String)> {
        query_pairs(Some(query))
    }

    fn now() -> DateTime<Utc> {
        "2026-10-13T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn the_window_defaults_to_the_last_hour_and_is_capped_at_31_days() {
        let window = parse_window(&pairs(""), now()).unwrap();
        assert_eq!((window.from, window.to), (now() - Duration::hours(1), now()));
        let custom = parse_window(&pairs("from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:00Z"), now()).unwrap();
        assert_eq!(custom.to - custom.from, Duration::days(31));
        for (query, message) in [
            ("from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:01Z", "at most 31 days"),
            ("from=2026-10-13T12:00:00Z&to=2026-10-13T12:00:00Z", "from must be before to"),
            ("from=yesterday", "from must be an RFC 3339 timestamp"),
            ("to=2026-13-01", "to must be an RFC 3339 timestamp"),
        ] {
            let error = parse_window(&pairs(query), now()).unwrap_err();
            assert!(error.contains(message), "{query}: {error}");
        }
    }

    #[test]
    fn list_parameters_repeat_and_are_validated() {
        let id = Uuid::new_v4();
        let f = parse_filters(&pairs(&format!("source_id={id}&level=ERROR&level=warn&pattern_hash=18446744073709551615"))).unwrap();
        assert_eq!(f.source_ids, vec![id]);
        assert_eq!(f.levels, vec!["error", "warn"]);
        assert_eq!(f.pattern_hashes, vec![u64::MAX]);
        for (query, message) in [
            ("source_id=x", "source_id must be a UUID"),
            ("level=verbose", "level must be one of"),
            ("level=error%27%20OR%201%3D1", "level must be one of"),
            ("pattern_hash=-1", "pattern_hash must be an unsigned 64-bit integer"),
            ("trace_id=abc", "trace_id must be 32 hex characters"),
            ("span_id=zzzzzzzzzzzzzzzz", "span_id must be 16 hex characters"),
            ("in_fields=maybe", "in_fields must be true or false"),
        ] {
            let error = parse_filters(&pairs(query)).unwrap_err();
            assert!(error.contains(message), "{query}: {error}");
        }
        let many = (0..101).map(|_| "level=info").collect::<Vec<_>>().join("&");
        assert!(parse_filters(&pairs(&many)).unwrap_err().contains("at most 100 values"));
    }

    #[test]
    fn q_trace_and_span_ids() {
        let f = parse_filters(&pairs("q=Disk%20Full&in_fields=true&trace_id=4BF92F3577B34DA6A3CE929D0E0E4736&span_id=00f067aa0ba902b7")).unwrap();
        assert_eq!(f.q.as_deref(), Some("Disk Full"));
        assert!(f.in_fields);
        assert_eq!(f.trace_id.as_deref(), Some("4bf92f3577b34da6a3ce929d0e0e4736"));
        assert_eq!(f.span_id.as_deref(), Some("00f067aa0ba902b7"));
        assert_eq!(parse_filters(&pairs("q=")).unwrap().q, None);
        assert!(parse_filters(&pairs(&format!("q={}", "x".repeat(1025)))).unwrap_err().contains("at most 1024"));
    }

    #[test]
    fn filters_are_validated() {
        let ok = parse_filter(r#"{"path":"user.tier-1_x","op":"eq","value":"gold"}"#).unwrap();
        assert_eq!(ok.path, vec!["user", "tier-1_x"]);
        assert_eq!(ok.value, FilterValue::Text("gold".to_string()));
        assert_eq!(parse_filter(r#"{"path":"status","op":"eq","value":200}"#).unwrap().value, FilterValue::Text("200".to_string()));
        assert_eq!(parse_filter(r#"{"path":"ms","op":"gte","value":"12.5"}"#).unwrap().value, FilterValue::Number(12.5));
        assert_eq!(parse_filter(r#"{"path":"a","op":"exists"}"#).unwrap().value, FilterValue::None);
        for (raw, message) in [
            (r#"{"path":"a","op":"like","value":"x"}"#, "must be JSON like"),
            (r#"{"path":"a","op":"eq","value":"x","extra":1}"#, "must be JSON like"),
            ("not json", "must be JSON like"),
            (r#"{"path":"a..b","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a b","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a'","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a.b.c.d.e.f","op":"exists"}"#, "at most 5 segments"),
            (r#"{"path":"é","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a","op":"eq"}"#, "value must be a string, number or boolean"),
            (r#"{"path":"a","op":"eq","value":[1]}"#, "value must be a string, number or boolean"),
            (r#"{"path":"a","op":"regex","value":"(a"}"#, "regex is not a valid regular expression"),
            (r#"{"path":"a","op":"regex","value":5}"#, "value must be a regular expression string"),
            (r#"{"path":"a","op":"gt","value":"abc"}"#, "finite number"),
            (r#"{"path":"a","op":"gt","value":"NaN"}"#, "finite number"),
            (r#"{"path":"a","op":"lt","value":"inf"}"#, "finite number"),
            (r#"{"path":"a","op":"lte","value":true}"#, "finite number"),
        ] {
            let error = parse_filter(raw).unwrap_err();
            assert!(error.contains(message), "{raw}: {error}");
        }
        let long = format!(r#"{{"path":"a","op":"regex","value":"{}"}}"#, "a".repeat(257));
        assert!(parse_filter(&long).unwrap_err().contains("at most 256"));
        let segment = format!(r#"{{"path":"{}","op":"exists"}}"#, "a".repeat(65));
        assert!(parse_filter(&segment).unwrap_err().contains("path segments must be"));
        let eleven = (0..11).map(|_| r#"filter={"path":"a","op":"exists"}"#).collect::<Vec<_>>().join("&");
        assert!(parse_filters(&pairs(&eleven)).unwrap_err().contains("at most 10 filters"));
        assert!(parse_filters(&pairs(r#"filter={"path":"a","op":"zz"}"#)).unwrap_err().starts_with("filter 1:"));
    }

    #[test]
    fn limit_and_cursor() {
        let cursor = Cursor { dt_ms: 1_791_892_800_123, id: Uuid::new_v4() };
        let params = parse_search(&pairs(&format!("limit=500&cursor={}", cursor.encode())), now()).unwrap();
        assert_eq!((params.limit, params.cursor), (500, Some(cursor)));
        assert_eq!(parse_search(&pairs(""), now()).unwrap().limit, 100);
        for query in ["limit=0", "limit=501", "limit=x", "cursor=not-a-cursor", "cursor=MTIzOm5vdC1hLXV1aWQ"] {
            assert!(parse_search(&pairs(query), now()).is_err(), "{query}");
        }
    }

    /// Security: whatever the input, the SQL text holds only fragments and
    /// placeholders; values arrive as parameters.
    #[test]
    fn user_input_never_reaches_the_sql_text() {
        let evil = "'; DROP TABLE logs; -- {p0:String} \\ sleep(3)\n";
        let query = format!(
            "q={q}&filter={f1}&filter={f2}&filter={f3}",
            q = urlencode(evil),
            f1 = urlencode(&serde_json::json!({ "path": "a.b", "op": "eq", "value": evil }).to_string()),
            f2 = urlencode(&serde_json::json!({ "path": "c", "op": "regex", "value": "'; DROP" }).to_string()),
            f3 = urlencode(&serde_json::json!({ "path": "n", "op": "gt", "value": 1e300 }).to_string()),
        );
        let filters = parse_filters(&pairs(&query)).unwrap();
        let mut sql = Sql::new();
        sql.push("SELECT count() FROM logs");
        push_where(&mut sql, Uuid::nil(), &filters);
        for fragment in ["DROP", "sleep", "'; ", "\n", "\\"] {
            assert!(!sql.text().contains(fragment), "{fragment:?} leaked into {}", sql.text());
        }
        let values: Vec<String> = sql.params().into_iter().map(|(_, value)| value).collect();
        assert!(values.contains(&evil.to_string()));
        assert!(values.contains(&"a".to_string()) && values.contains(&"b".to_string()));
    }

    fn urlencode(text: &str) -> String {
        form_urlencoded::byte_serialize(text.as_bytes()).collect()
    }

    #[test]
    fn placeholders_are_numbered_and_reusable() {
        let mut sql = Sql::new();
        let index = sql.bind("x");
        sql.push("a = ").placeholder(index, "String").push(" OR b = ").placeholder(index, "String");
        sql.push(" AND c = ").value("UInt32", "7");
        assert_eq!(sql.text(), "a = {p0:String} OR b = {p0:String} AND c = {p1:UInt32}");
        assert_eq!(sql.params(), vec![("p0", "x".to_string()), ("p1", "7".to_string())]);
    }
}
```

- [ ] **Step 5: Implement search and the histogram**

`nami-engine/src/logs/search.rs`:

```rust
//! Log search (newest first, keyset-paginated) and the volume histogram.
//! Both are always scoped to one project (bound from the path) and run with
//! a ClickHouse time limit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::query::{self, Cursor, LogFilters, SearchParams, Sql, Window, ERROR_LEVELS_SQL};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    error::AppError,
};

/// Search and histogram queries stop after this long (ClickHouse
/// `max_execution_time`); with RE2 matching, nothing a user sends can run
/// longer.
pub const SEARCH_SETTINGS: [(&str, &str); 1] = [("max_execution_time", "30")];

/// The columns every log query returns, in [`LogEntry`] order.
pub const ENTRY_COLUMNS: &str = "SELECT id, source_id, dt, level, message, fields, trace_id, span_id, \
     toString(pattern_hash) AS pattern_hash FROM logs";

/// One log row as the API returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogEntry {
    pub id: Uuid,
    pub source_id: Uuid,
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    /// The stored JSON object.
    pub fields: Value,
    pub trace_id: String,
    pub span_id: String,
    /// Decimal string: a UInt64 does not fit a JavaScript number.
    pub pattern_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct RawEntry {
    id: Uuid,
    source_id: Uuid,
    dt: DateTime<Utc>,
    level: String,
    message: String,
    fields: String,
    trace_id: String,
    span_id: String,
    pattern_hash: String,
}

impl From<RawEntry> for LogEntry {
    fn from(raw: RawEntry) -> Self {
        LogEntry {
            id: raw.id,
            source_id: raw.source_id,
            dt: raw.dt,
            level: raw.level,
            message: raw.message,
            fields: serde_json::from_str(&raw.fields).unwrap_or(Value::String(raw.fields)),
            trace_id: raw.trace_id,
            span_id: raw.span_id,
            pattern_hash: raw.pattern_hash,
        }
    }
}

impl LogEntry {
    pub fn cursor(&self) -> Cursor {
        Cursor { dt_ms: self.dt.timestamp_millis(), id: self.id }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPage {
    pub logs: Vec<LogEntry>,
    /// Pass back as `cursor` for the next (older) page; `null` on the last.
    pub next_cursor: Option<String>,
}

/// ClickHouse errors a user's input can cause become 400s with a useful
/// message; anything else stays a generic 500 (logged).
pub fn user_error(error: ClickHouseError) -> AppError {
    match &error {
        ClickHouseError::Server { body, .. } if body.contains("CANNOT_COMPILE_REGEXP") => {
            AppError::Validation("a filter regex is not valid RE2 syntax".to_string())
        }
        ClickHouseError::Server { body, .. } if body.contains("TIMEOUT_EXCEEDED") => AppError::Validation(
            "the query took longer than 30 s: narrow the time window or add filters".to_string(),
        ),
        _ => AppError::ClickHouse(error),
    }
}

/// One page, newest first: `(dt, id)` descending, strictly after the cursor.
pub async fn search(ch: &ClickHouse, project_id: Uuid, params: &SearchParams) -> Result<SearchPage, AppError> {
    let mut sql = Sql::new();
    sql.push(ENTRY_COLUMNS);
    query::push_where(&mut sql, project_id, &params.filters);
    query::push_window(&mut sql, &params.window);
    if let Some(cursor) = params.cursor {
        sql.push(" AND (dt, id) < (fromUnixTimestamp64Milli(")
            .value("Int64", cursor.dt_ms.to_string())
            .push(", 'UTC'), ")
            .value("UUID", cursor.id.to_string())
            .push(")");
    }
    // One extra row tells whether another page exists.
    sql.push(" ORDER BY dt DESC, id DESC LIMIT ").value("UInt32", (params.limit + 1).to_string());
    let rows: Vec<RawEntry> =
        ch.query_rows_with(sql.text(), &sql.params(), &SEARCH_SETTINGS).await.map_err(user_error)?;
    let mut logs: Vec<LogEntry> = rows.into_iter().map(LogEntry::from).collect();
    let next_cursor = if logs.len() > params.limit as usize {
        logs.truncate(params.limit as usize);
        logs.last().map(|entry| entry.cursor().encode())
    } else {
        None
    };
    Ok(SearchPage { logs, next_cursor })
}

/// Bucket sizes the histogram picks from (seconds).
pub const HISTOGRAM_STEPS: [i64; 18] =
    [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400];
pub const TARGET_BUCKETS: i64 = 60;

/// The smallest step giving at most [`TARGET_BUCKETS`] buckets.
pub fn histogram_step(window: &Window) -> i64 {
    let span = (window.to - window.from).num_milliseconds().max(1);
    HISTOGRAM_STEPS
        .iter()
        .copied()
        .find(|step| span <= step * 1000 * TARGET_BUCKETS)
        .unwrap_or(86_400)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Histogram {
    pub step_seconds: i64,
    pub total: u64,
    pub buckets: Vec<HistogramBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistogramBucket {
    pub start: DateTime<Utc>,
    pub count: u64,
    /// Rows at `error` level or above.
    pub errors: u64,
    /// Rows at `warn` level.
    pub warnings: u64,
}

#[derive(Debug, Deserialize)]
struct BucketRow {
    bucket_ms: i64,
    count: u64,
    errors: u64,
    warnings: u64,
}

/// Row counts per bucket over the window, every bucket present (zeros
/// included), oldest first.
pub async fn histogram(
    ch: &ClickHouse,
    project_id: Uuid,
    window: &Window,
    filters: &LogFilters,
) -> Result<Histogram, AppError> {
    let step_seconds = histogram_step(window);
    let step_ms = step_seconds * 1000;
    let mut sql = Sql::new();
    let step = sql.bind(step_ms.to_string());
    sql.push("SELECT intDiv(toUnixTimestamp64Milli(dt), ")
        .placeholder(step, "Int64")
        .push(") * ")
        .placeholder(step, "Int64")
        .push(" AS bucket_ms, count() AS count, countIf(level IN ")
        .push(ERROR_LEVELS_SQL)
        .push(") AS errors, countIf(level = 'warn') AS warnings FROM logs");
    query::push_where(&mut sql, project_id, filters);
    query::push_window(&mut sql, window);
    sql.push(" GROUP BY bucket_ms ORDER BY bucket_ms");
    let rows: Vec<BucketRow> =
        ch.query_rows_with(sql.text(), &sql.params(), &SEARCH_SETTINGS).await.map_err(user_error)?;

    let first = window.from.timestamp_millis().div_euclid(step_ms) * step_ms;
    let last = window.to.timestamp_millis();
    let mut buckets = Vec::new();
    let mut found = rows.into_iter().peekable();
    let mut start = first;
    while start <= last {
        let row = found.next_if(|row| row.bucket_ms == start);
        buckets.push(HistogramBucket {
            start: DateTime::from_timestamp_millis(start).unwrap_or(window.from),
            count: row.as_ref().map_or(0, |r| r.count),
            errors: row.as_ref().map_or(0, |r| r.errors),
            warnings: row.as_ref().map_or(0, |r| r.warnings),
        });
        start += step_ms;
    }
    let total = buckets.iter().map(|bucket| bucket.count).sum();
    Ok(Histogram { step_seconds, total, buckets })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn window(minutes: i64) -> Window {
        let to: DateTime<Utc> = "2026-10-13T12:00:00Z".parse().unwrap();
        Window { from: to - Duration::minutes(minutes), to }
    }

    #[test]
    fn steps_target_about_60_buckets() {
        assert_eq!(histogram_step(&window(15)), 15);
        assert_eq!(histogram_step(&window(60)), 60);
        assert_eq!(histogram_step(&window(6 * 60)), 600);
        assert_eq!(histogram_step(&window(24 * 60)), 1800);
        assert_eq!(histogram_step(&window(7 * 24 * 60)), 10_800);
        assert_eq!(histogram_step(&window(31 * 24 * 60)), 86_400);
        assert_eq!(histogram_step(&Window { from: window(1).to, to: window(1).to + Duration::milliseconds(1) }), 1);
    }

    #[test]
    fn regex_and_timeout_errors_become_400s_and_others_stay_500s() {
        let server = |body: &str| ClickHouseError::Server { status: 500, body: body.to_string() };
        assert!(matches!(user_error(server("Code: 427 ... (CANNOT_COMPILE_REGEXP)")), AppError::Validation(_)));
        assert!(matches!(user_error(server("Code: 159 ... (TIMEOUT_EXCEEDED)")), AppError::Validation(_)));
        assert!(matches!(user_error(server("Code: 60 ... (UNKNOWN_TABLE)")), AppError::ClickHouse(_)));
    }
}
```

```diff
diff --git a/nami-engine/src/logs/mod.rs b/nami-engine/src/logs/mod.rs
--- a/nami-engine/src/logs/mod.rs
+++ b/nami-engine/src/logs/mod.rs
@@ -1,12 +1,15 @@
 //! Log management: turning ingested events into rows (`normalize`,
 //! `pattern`), storing them in ClickHouse (`store`), HTTP ingestion
 //! (`ingest`, `msgpack`) with its per-source rate limit (`rate_limit`) and
-//! usage accounting (`usage`), and syslog ingestion (`syslog`).
+//! usage accounting (`usage`), syslog ingestion (`syslog`), and search
+//! (`query`, `search`).
 pub mod ingest;
 pub mod msgpack;
 pub mod normalize;
 pub mod pattern;
+pub mod query;
 pub mod rate_limit;
+pub mod search;
 pub mod store;
 pub mod syslog;
 pub mod usage;
```

- [ ] **Step 6: Add the routes**

`nami-engine/src/routes/logs.rs`:

```rust
use axum::{
    extract::{Path, RawQuery, State},
    routing::get,
    Json, Router,
};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    error::AppError,
    logs::{
        query,
        search::{self, Histogram, SearchPage},
    },
    middleware::rbac::{require_project_role, CurrentUser},
    state::AppState,
};

// Viewers and up. The project is always the one in the path: a source_id
// of another project simply matches nothing.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/logs", get(search_logs))
        .route("/projects/{id}/logs/histogram", get(log_histogram))
}

/// `GET /projects/{id}/logs?from&to&source_id*&level*&q&in_fields&filter*
/// &trace_id&span_id&pattern_hash*&limit&cursor` (`*` = repeatable).
async fn search_logs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SearchPage>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let params = query::parse_search(&query::query_pairs(raw.as_deref()), Utc::now()).map_err(AppError::Validation)?;
    Ok(Json(search::search(&state.clickhouse, project_id, &params).await?))
}

/// The same filters and window as search (no cursor/limit).
async fn log_histogram(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Histogram>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let pairs = query::query_pairs(raw.as_deref());
    let window = query::parse_window(&pairs, Utc::now()).map_err(AppError::Validation)?;
    let filters = query::parse_filters(&pairs).map_err(AppError::Validation)?;
    Ok(Json(search::histogram(&state.clickhouse, project_id, &window, &filters).await?))
}
```

```diff
diff --git a/nami-engine/src/routes/mod.rs b/nami-engine/src/routes/mod.rs
--- a/nami-engine/src/routes/mod.rs
+++ b/nami-engine/src/routes/mod.rs
@@ -8,3 +8,4 @@ pub mod maintenance;
 pub mod monitor_groups;
 pub mod sources;
 pub mod logs_ingest;
+pub mod logs;
```

```diff
diff --git a/nami-engine/src/router.rs b/nami-engine/src/router.rs
--- a/nami-engine/src/router.rs
+++ b/nami-engine/src/router.rs
@@ -64,6 +64,7 @@ pub fn build_router(state: AppState) -> Router {
         .merge(routes::monitor_groups::router())
         .merge(routes::sources::router())
         .merge(routes::logs_ingest::public_router())
+        .merge(routes::logs::router())
         .with_state(state)
 }
 
```

- [ ] **Step 7: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/logs_search.rs` 9 passed, unit tests `logs::query` 7 and `logs::search` 2; 329 tests in total on the verified base.

- [ ] **Step 8: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/src/logs nami-engine/src/routes/logs.rs \
  nami-engine/src/routes/mod.rs nami-engine/src/router.rs nami-engine/tests/logs_search.rs
git commit -m "feat(engine): log search and volume histogram (NAMI-LOG-5)

GET /projects/{id}/logs searches a window of at most 31 days by source,
level, text (optionally in fields), structured field filters (eq, neq,
contains, exists, regex, not_regex, gt, gte, lt, lte), trace/span id and
pattern hash, newest first with a (dt, id) keyset cursor. /histogram
counts rows in about 60 buckets with an error/warning breakdown. SQL is
built only from fixed fragments and numbered parameters.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6 (NAMI-LOG-6): Live tail over Server-Sent Events, proxied by nami-ui

Branch `feat/NAMI-LOG-6-live-tail`, worktree `.worktrees/NAMI-LOG-6/`.

**Files:**
- Create: `nami-engine/src/logs/tail.rs`, `nami-ui/src/lib/server/log-proxy.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/tail/+server.ts`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/logs/mod.rs`, `nami-engine/src/middleware/rbac.rs`, `nami-engine/src/routes/logs.rs`
- Test: `nami-engine/tests/logs_tail.rs`, `nami-ui/src/lib/server/log-proxy.test.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/tail/server.test.ts`

**Interfaces:**
- Consumes: `logs::query::{parse_filters, query_pairs, push_where, Sql, LogFilters}`, `logs::search::{ENTRY_COLUMNS, SEARCH_SETTINGS, RawEntry, LogEntry, user_error}` (Task 5); `LogsState` (Task 3); `tower_sessions::Session::load`; nami-ui `callEngine`, `isUuid` (`#lib/server/engine`).
- Produces:
  - `middleware::rbac::user_from_session(state: &AppState, session: &Session) -> Result<CurrentUser, AppError>` — the body of the `CurrentUser` extractor, moved unchanged so the tail can re-run it.
  - `logs::tail`: `TAIL_BATCH = 1000`, `TailTimings { poll, keep_alive, recheck, max_duration, lookback: Duration }` (`Default`: 1 s, 15 s, 60 s, 30 min, 60 s), `EventSender = mpsc::Sender<Result<Event, Infallible>>`, `run(state: AppState, session: Session, project_id: Uuid, filters: LogFilters, tx: EventSender)`; `LogsState.tail: TailTimings`.
  - Route `GET /projects/{id}/logs/tail` (same filters as search, no window/cursor/limit): `text/event-stream`, `X-Accel-Buffering: no`; `event: log` with a `LogEntry` JSON; `event: end` with `{"reason":"session"}` or `{"reason":"timeout"}`; an empty comment every `keep_alive`.
  - nami-ui `#lib/server/log-proxy`: `FILTER_PARAMS`, `TAIL_PARAMS`, `SEARCH_PARAMS: string[]`, `pickLogParams(params: URLSearchParams, keys: readonly string[]): string` (`''` or `?…`).
  - nami-ui `GET /projects/[id]/logs/tail` (`+server.ts`): streams the engine body through unread.

- [ ] **Step 1: Add the crate**

```bash
cd nami-engine
cargo add tokio-stream@0.1.19
```

- [ ] **Step 2: Write the failing engine tests**

`nami-engine/tests/logs_tail.rs` (new and late rows, keep-alive, the end on logout or lost access, the 30-minute end, 401/403/400):

```rust
// Live tail (`GET /projects/{id}/logs/tail`, Server-Sent Events). Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, StatusCode},
    Router,
};
use chrono::{SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json, seed_source},
    seed_project, state_with_clickhouse, user_with_role,
};
use http_body_util::BodyExt;
use nami_engine::{
    logs::{tail::TailTimings, LogsState},
    state::AppState,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

fn fast(overrides: impl FnOnce(&mut TailTimings)) -> Arc<LogsState> {
    let mut tail = TailTimings {
        poll: Duration::from_millis(100),
        keep_alive: Duration::from_secs(15),
        recheck: Duration::from_secs(60),
        max_duration: Duration::from_secs(60),
        lookback: Duration::from_secs(60),
    };
    overrides(&mut tail);
    Arc::new(LogsState { tail, ..LogsState::default() })
}

#[derive(Debug, PartialEq)]
enum Sse {
    Comment,
    Event { event: String, data: Value },
}

/// Reads Server-Sent Events off a streaming response body.
struct SseStream {
    body: Body,
    buffer: String,
}

impl SseStream {
    async fn open(app: &Router, uri: &str, cookie: Option<&HeaderValue>) -> Result<SseStream, (StatusCode, Value)> {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
        let status = response.status();
        if status != StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            return Err((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)));
        }
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/event-stream");
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        Ok(SseStream { body: response.into_body(), buffer: String::new() })
    }

    /// The next event or comment; `None` when the stream ended or nothing
    /// arrived within `wait`.
    async fn next(&mut self, wait: Duration) -> Option<Sse> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                return Some(parse_block(block.trim_end()));
            }
            let frame = tokio::time::timeout_at(deadline, self.body.frame()).await.ok()??.ok()?;
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }

    /// The next `event: log` row, skipping comments.
    async fn next_log(&mut self, wait: Duration) -> Option<Value> {
        loop {
            match self.next(wait).await? {
                Sse::Comment => continue,
                Sse::Event { event, data } if event == "log" => return Some(data),
                other => panic!("expected a log event, got {other:?}"),
            }
        }
    }

    /// True when the server closed the stream (rather than just being quiet).
    async fn ended(&mut self) -> bool {
        matches!(tokio::time::timeout(Duration::from_secs(5), self.body.frame()).await, Ok(None))
    }
}

fn parse_block(block: &str) -> Sse {
    let mut event = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
        }
    }
    match event {
        None if data.is_empty() => Sse::Comment,
        event => Sse::Event { event: event.unwrap_or_default(), data: serde_json::from_str(&data).unwrap() },
    }
}

fn ago(seconds: i64) -> String {
    (Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[sqlx::test]
async fn tail_streams_new_matching_rows_including_late_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState { logs: fast(|_| {}), ..state });
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, other_token) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&app, &token, json!({ "message": "before the tail", "level": "error" })).await;

    let mut stream = SseStream::open(&app, &format!("/projects/{project}/logs/tail?level=error"), Some(&viewer))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    post_json(&app, &token, json!([{ "message": "ignored info", "level": "info" }, { "message": "first error", "level": "error" }])).await;
    let row = stream.next_log(Duration::from_secs(5)).await.expect("a row");
    assert_eq!((row["message"].clone(), row["level"].clone()), (json!("first error"), json!("error")));
    assert!(row["fields"].is_object() && row["pattern_hash"].is_string(), "{row}");

    // A late row (dt 30 s ago, older than the row just sent) still arrives;
    // another project's row never does.
    post_json(&app, &other_token, json!({ "message": "not yours", "level": "error" })).await;
    post_json(&app, &token, json!({ "message": "late error", "level": "error", "dt": ago(30) })).await;
    let late = stream.next_log(Duration::from_secs(5)).await.expect("the late row");
    assert_eq!(late["message"], "late error");
    assert_eq!(stream.next(Duration::from_millis(700)).await, None, "nothing twice, nothing foreign, nothing from before");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn idle_streams_get_keep_alive_comments(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState { logs: fast(|t| t.keep_alive = Duration::from_millis(100)), ..state });
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(&app, &format!("/projects/{project}/logs/tail"), Some(&viewer)).await.unwrap();
    assert_eq!(stream.next(Duration::from_secs(2)).await, Some(Sse::Comment));
    ch.drop_database().await.unwrap();
}

/// Security: a stream ends once its session is no longer valid — logged
/// out, or the user lost access to the project.
#[sqlx::test]
async fn the_stream_ends_when_the_session_or_access_is_gone(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState { logs: fast(|t| t.recheck = Duration::from_millis(200)), ..state });
    let project = seed_project(&pool).await;
    let (_, _, leaving) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (removed_id, _, removed) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, staying) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");
    let mut logged_out = SseStream::open(&app, &uri, Some(&leaving)).await.unwrap();
    let mut kicked = SseStream::open(&app, &uri, Some(&removed)).await.unwrap();
    let mut kept = SseStream::open(&app, &uri, Some(&staying)).await.unwrap();

    assert_eq!(call(&app, "POST", "/auth/logout", Some(&leaving), None).await.0, StatusCode::OK);
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project)
        .bind(removed_id)
        .execute(&pool)
        .await
        .unwrap();

    for stream in [&mut logged_out, &mut kicked] {
        let end = stream.next(Duration::from_secs(3)).await;
        assert_eq!(end, Some(Sse::Event { event: "end".to_string(), data: json!({ "reason": "session" }) }));
        assert!(stream.ended().await, "the stream is closed after the end event");
    }
    assert_eq!(kept.next(Duration::from_millis(500)).await, None, "a valid session keeps streaming");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_stream_closes_after_its_maximum_duration(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState { logs: fast(|t| t.max_duration = Duration::from_millis(400)), ..state });
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(&app, &format!("/projects/{project}/logs/tail"), Some(&viewer)).await.unwrap();
    let end = stream.next(Duration::from_secs(3)).await;
    assert_eq!(end, Some(Sse::Event { event: "end".to_string(), data: json!({ "reason": "timeout" }) }));
    assert!(stream.ended().await);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_tail_needs_a_session_project_access_and_valid_filters(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState { logs: fast(|_| {}), ..state });
    let project = seed_project(&pool).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");
    assert_eq!(SseStream::open(&app, &uri, None).await.err().unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(SseStream::open(&app, &uri, Some(&outsider)).await.err().unwrap().0, StatusCode::FORBIDDEN);
    let (status, json) = SseStream::open(&app, &format!("{uri}?level=verbose"), Some(&viewer)).await.err().unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    let random = Uuid::new_v4();
    assert_eq!(
        SseStream::open(&app, &format!("/projects/{random}/logs/tail"), Some(&viewer)).await.err().unwrap().0,
        StatusCode::FORBIDDEN
    );
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test logs_tail`
Expected: FAIL to compile — `error[E0432]: unresolved import nami_engine::logs::tail`.

- [ ] **Step 4: Extract the session check**

The `CurrentUser` extractor's body moves into `user_from_session` unchanged (the extractor calls it):

```diff
diff --git a/nami-engine/src/middleware/rbac.rs b/nami-engine/src/middleware/rbac.rs
--- a/nami-engine/src/middleware/rbac.rs
+++ b/nami-engine/src/middleware/rbac.rs
@@ -15,41 +15,47 @@ impl FromRequestParts<AppState> for CurrentUser {
         let session = Session::from_request_parts(parts, state)
             .await
             .map_err(|_| AppError::Unauthorized)?;
+        user_from_session(state, &session).await
+    }
+}
 
-        let user_id: uuid::Uuid = session
-            .get("user_id")
-            .await
-            .map_err(|_| AppError::Unauthorized)?
-            .ok_or(AppError::Unauthorized)?;
-
-        // Same staleness check as `routes::auth::me`/`current_user_id`: a
-        // session authenticated before the user's last password change must
-        // not be usable to identify the current user here either — this
-        // extractor is the shared mechanism every RBAC-guarded route uses,
-        // so without this check a session invalidated by a password reset
-        // would still work for every route except `/auth/me`.
-        let auth_at: chrono::DateTime<chrono::Utc> = session
-            .get("auth_at")
-            .await
-            .map_err(|_| AppError::Unauthorized)?
-            .ok_or(AppError::Unauthorized)?;
-
-        let user: models::user::User = sqlx::query_as("select * from users where id = $1")
-            .bind(user_id)
-            .fetch_optional(&state.db)
-            .await?
-            .ok_or(AppError::Unauthorized)?;
-
-        if auth_at < user.password_changed_at {
-            // `.flush()` not `.delete()`: delete only removes the server-side
-            // row and leaves a stale cookie on the client until its Max-Age
-            // (same reasoning as `logout` in `routes::auth`). Flush does both.
-            session.flush().await.ok();
-            return Err(AppError::Unauthorized);
-        }
-
-        Ok(CurrentUser { id: user.id, email: user.email, global_role: user.global_role })
+/// The user `session` belongs to, or `Unauthorized`. Shared by the
+/// `CurrentUser` extractor and long-lived responses (the log live tail)
+/// that re-check their session while they stream.
+pub async fn user_from_session(state: &AppState, session: &Session) -> Result<CurrentUser, AppError> {
+    let user_id: uuid::Uuid = session
+        .get("user_id")
+        .await
+        .map_err(|_| AppError::Unauthorized)?
+        .ok_or(AppError::Unauthorized)?;
+
+    // Same staleness check as `routes::auth::me`/`current_user_id`: a
+    // session authenticated before the user's last password change must
+    // not be usable to identify the current user here either — this
+    // extractor is the shared mechanism every RBAC-guarded route uses,
+    // so without this check a session invalidated by a password reset
+    // would still work for every route except `/auth/me`.
+    let auth_at: chrono::DateTime<chrono::Utc> = session
+        .get("auth_at")
+        .await
+        .map_err(|_| AppError::Unauthorized)?
+        .ok_or(AppError::Unauthorized)?;
+
+    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
+        .bind(user_id)
+        .fetch_optional(&state.db)
+        .await?
+        .ok_or(AppError::Unauthorized)?;
+
+    if auth_at < user.password_changed_at {
+        // `.flush()` not `.delete()`: delete only removes the server-side
+        // row and leaves a stale cookie on the client until its Max-Age
+        // (same reasoning as `logout` in `routes::auth`). Flush does both.
+        session.flush().await.ok();
+        return Err(AppError::Unauthorized);
     }
+
+    Ok(CurrentUser { id: user.id, email: user.email, global_role: user.global_role })
 }
 
 /// Shared by `require_global_role` and `require_project_role` (previously
```

- [ ] **Step 5: Implement the tail**

`nami-engine/src/logs/tail.rs`:

```rust
//! Live tail (`GET /projects/{id}/logs/tail`): Server-Sent Events of rows
//! as they arrive.
//!
//! Every [`TailTimings::poll`] the engine reads the newest rows (at most
//! [`TAIL_BATCH`]) with `dt` in the last [`TailTimings::lookback`] and sends
//! the ones it has not sent yet as `event: log`, oldest first. Remembering
//! sent ids instead of keeping only the last `(dt, id)` means a row that
//! arrives late — a batching shipper's row whose `dt` is older than rows
//! already shown — is still delivered, as long as its `dt` is within the
//! look-back. Rows already stored when the tail starts are not sent.
//!
//! The session is re-checked every [`TailTimings::recheck`] (logged out,
//! password changed, or removed from the project ends the stream with
//! `event: end` / `{"reason":"session"}`), and the stream ends after
//! [`TailTimings::max_duration`] with `{"reason":"timeout"}`; the client
//! reconnects. A comment line keeps idle connections open.

use std::{collections::HashMap, convert::Infallible, time::Duration};

use axum::response::sse::Event;
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::{sync::mpsc, time::Instant};
use tower_sessions::Session;
use uuid::Uuid;

use super::{
    query::{self, LogFilters, Sql},
    search::{self, LogEntry, RawEntry, ENTRY_COLUMNS, SEARCH_SETTINGS},
};
use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, user_from_session},
    state::AppState,
};

/// Most rows one poll reads; beyond that the newest are shown.
pub const TAIL_BATCH: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailTimings {
    pub poll: Duration,
    pub keep_alive: Duration,
    pub recheck: Duration,
    pub max_duration: Duration,
    pub lookback: Duration,
}

impl Default for TailTimings {
    fn default() -> Self {
        TailTimings {
            poll: Duration::from_secs(1),
            keep_alive: Duration::from_secs(15),
            recheck: Duration::from_secs(60),
            max_duration: Duration::from_secs(30 * 60),
            lookback: Duration::from_secs(60),
        }
    }
}

pub type EventSender = mpsc::Sender<Result<Event, Infallible>>;

fn end_event(reason: &str) -> Event {
    Event::default().event("end").data(json!({ "reason": reason }).to_string())
}

/// The newest rows of the project matching `filters` with `dt >= since`.
async fn fetch(state: &AppState, project_id: Uuid, filters: &LogFilters, since: DateTime<Utc>) -> Result<Vec<LogEntry>, AppError> {
    let mut sql = Sql::new();
    sql.push(ENTRY_COLUMNS);
    query::push_where(&mut sql, project_id, filters);
    sql.push(" AND dt >= fromUnixTimestamp64Milli(")
        .value("Int64", since.timestamp_millis().to_string())
        .push(", 'UTC') ORDER BY dt DESC, id DESC LIMIT ")
        .value("UInt32", TAIL_BATCH.to_string());
    let rows: Vec<RawEntry> = state
        .clickhouse
        .query_rows_with(sql.text(), &sql.params(), &SEARCH_SETTINGS)
        .await
        .map_err(search::user_error)?;
    Ok(rows.into_iter().map(LogEntry::from).collect())
}

/// Reloads the session from the store and checks it still belongs to a
/// user who may read the project.
async fn session_still_valid(state: &AppState, session: &Session, project_id: Uuid) -> bool {
    if session.load().await.is_err() {
        return false;
    }
    match user_from_session(state, session).await {
        Ok(user) => require_project_role(&state.db, &user, project_id, "viewer").await.is_ok(),
        Err(_) => false,
    }
}

/// Streams into `tx` until the client goes away, the session becomes
/// invalid, or the maximum duration passes.
pub async fn run(state: AppState, session: Session, project_id: Uuid, filters: LogFilters, tx: EventSender) {
    let timings = state.logs.tail;
    let lookback = chrono::Duration::from_std(timings.lookback).unwrap_or(chrono::Duration::seconds(60));
    let deadline = Instant::now() + timings.max_duration;
    let mut next_recheck = Instant::now() + timings.recheck;
    let mut sent: HashMap<Uuid, DateTime<Utc>> = HashMap::new();
    match fetch(&state, project_id, &filters, Utc::now() - lookback).await {
        Ok(existing) => sent.extend(existing.into_iter().map(|row| (row.id, row.dt))),
        Err(error) => tracing::warn!(error = ?error, "live tail: first poll failed"),
    }
    let mut ticker = tokio::time::interval(timings.poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tx.closed() => return,
        }
        if Instant::now() >= deadline {
            let _ = tx.send(Ok(end_event("timeout"))).await;
            return;
        }
        if Instant::now() >= next_recheck {
            if !session_still_valid(&state, &session, project_id).await {
                let _ = tx.send(Ok(end_event("session"))).await;
                return;
            }
            next_recheck = Instant::now() + timings.recheck;
        }
        let since = Utc::now() - lookback;
        let rows = match fetch(&state, project_id, &filters, since).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(error = ?error, "live tail poll failed");
                continue;
            }
        };
        for row in rows.into_iter().rev() {
            if sent.contains_key(&row.id) {
                continue;
            }
            sent.insert(row.id, row.dt);
            let Ok(data) = serde_json::to_string(&row) else {
                continue;
            };
            if tx.send(Ok(Event::default().event("log").data(data))).await.is_err() {
                return;
            }
        }
        // Rows older than the look-back are never read again.
        sent.retain(|_, dt| *dt >= since);
    }
}
```

```diff
diff --git a/nami-engine/src/logs/mod.rs b/nami-engine/src/logs/mod.rs
--- a/nami-engine/src/logs/mod.rs
+++ b/nami-engine/src/logs/mod.rs
@@ -1,8 +1,8 @@
 //! Log management: turning ingested events into rows (`normalize`,
 //! `pattern`), storing them in ClickHouse (`store`), HTTP ingestion
 //! (`ingest`, `msgpack`) with its per-source rate limit (`rate_limit`) and
-//! usage accounting (`usage`), syslog ingestion (`syslog`), and search
-//! (`query`, `search`).
+//! usage accounting (`usage`), syslog ingestion (`syslog`), search
+//! (`query`, `search`) and the live tail (`tail`).
 pub mod ingest;
 pub mod msgpack;
 pub mod normalize;
@@ -12,12 +12,15 @@ pub mod rate_limit;
 pub mod search;
 pub mod store;
 pub mod syslog;
+pub mod tail;
 pub mod usage;
 
-/// In-memory state of the ingest paths, one per engine instance: rate
-/// limits and unflushed usage counters are local to the instance.
+/// In-memory state of the log paths, one per engine instance: rate limits
+/// and unflushed usage counters are local to the instance; `tail` holds the
+/// live-tail timings (tests shorten them).
 #[derive(Debug, Default)]
 pub struct LogsState {
     pub rate_limiter: rate_limit::TokenBuckets,
     pub usage: usage::UsageTracker,
+    pub tail: tail::TailTimings,
 }
```

```diff
diff --git a/nami-engine/src/routes/logs.rs b/nami-engine/src/routes/logs.rs
--- a/nami-engine/src/routes/logs.rs
+++ b/nami-engine/src/routes/logs.rs
@@ -1,9 +1,17 @@
 use axum::{
     extract::{Path, RawQuery, State},
+    http::{header::HeaderName, HeaderValue},
+    response::{
+        sse::{KeepAlive, Sse},
+        IntoResponse, Response,
+    },
     routing::get,
     Json, Router,
 };
 use chrono::Utc;
+use tokio::sync::mpsc;
+use tokio_stream::wrappers::ReceiverStream;
+use tower_sessions::Session;
 use uuid::Uuid;
 
 use crate::{
@@ -11,6 +19,7 @@ use crate::{
     logs::{
         query,
         search::{self, Histogram, SearchPage},
+        tail,
     },
     middleware::rbac::{require_project_role, CurrentUser},
     state::AppState,
@@ -22,6 +31,7 @@ pub fn router() -> Router<AppState> {
     Router::new()
         .route("/projects/{id}/logs", get(search_logs))
         .route("/projects/{id}/logs/histogram", get(log_histogram))
+        .route("/projects/{id}/logs/tail", get(tail_logs))
 }
 
 /// `GET /projects/{id}/logs?from&to&source_id*&level*&q&in_fields&filter*
@@ -50,3 +60,23 @@ async fn log_histogram(
     let filters = query::parse_filters(&pairs).map_err(AppError::Validation)?;
     Ok(Json(search::histogram(&state.clickhouse, project_id, &window, &filters).await?))
 }
+
+/// Server-Sent Events of new rows matching the search filters (no window,
+/// cursor or limit); see `logs::tail`.
+async fn tail_logs(
+    State(state): State<AppState>,
+    user: CurrentUser,
+    session: Session,
+    Path(project_id): Path<Uuid>,
+    RawQuery(raw): RawQuery,
+) -> Result<Response, AppError> {
+    require_project_role(&state.db, &user, project_id, "viewer").await?;
+    let filters = query::parse_filters(&query::query_pairs(raw.as_deref())).map_err(AppError::Validation)?;
+    let keep_alive = state.logs.tail.keep_alive;
+    let (tx, rx) = mpsc::channel(64);
+    tokio::spawn(tail::run(state, session, project_id, filters, tx));
+    let mut response = Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::new().interval(keep_alive)).into_response();
+    // Tell reverse proxies (nginx) not to buffer the stream.
+    response.headers_mut().insert(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"));
+    Ok(response)
+}
```

- [ ] **Step 6: Run the engine tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/logs_tail.rs` 5 passed (about 4 s), `tests/rbac.rs` and `tests/login.rs` still green after the extraction; 334 tests in total on the verified base. Run `cargo test --test logs_tail` twice more: the tests are timing-based and must not flake.

- [ ] **Step 7: Write the failing nami-ui tests**

`nami-ui/src/lib/server/log-proxy.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { pickLogParams, SEARCH_PARAMS, TAIL_PARAMS } from './log-proxy';

describe('pickLogParams', () => {
	it('keeps only the allowed keys, repeats included, in order', () => {
		const params = new URLSearchParams(
			'level=error&token=secret&level=warn&q=disk%20full&cursor=abc&filter=%7B%22path%22%3A%22a%22%7D'
		);
		expect(pickLogParams(params, TAIL_PARAMS)).toBe(
			'?level=error&level=warn&q=disk+full&filter=%7B%22path%22%3A%22a%22%7D'
		);
		expect(pickLogParams(params, SEARCH_PARAMS)).toContain('cursor=abc');
		expect(pickLogParams(params, SEARCH_PARAMS)).not.toContain('token');
	});

	it('returns an empty string when nothing is allowed', () => {
		expect(pickLogParams(new URLSearchParams('x=1'), TAIL_PARAMS)).toBe('');
		expect(pickLogParams(new URLSearchParams(), TAIL_PARAMS)).toBe('');
	});
});
```

`nami-ui/src/routes/(app)/projects/[id]/logs/tail/server.test.ts`:

```ts
import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT) {
	const controller = new AbortController();
	return {
		event: {
			fetch: fetchMock,
			params: { id },
			url: new URL(url),
			request: new Request(url, { headers: { cookie: 'id=session' }, signal: controller.signal }),
			cookies: { set: vi.fn() },
			getClientAddress: () => '203.0.113.5'
		} as any,
		controller
	};
}

describe('GET /projects/[id]/logs/tail', () => {
	it('streams the engine body through without waiting for it to end', async () => {
		let push!: (text: string) => void;
		const body = new ReadableStream<Uint8Array>({
			start(controller) {
				push = (text) => controller.enqueue(new TextEncoder().encode(text));
			}
		});
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
		);
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail?level=error&secret=x`);

		const response = await GET(event);
		expect(response.status).toBe(200);
		expect(response.headers.get('content-type')).toBe('text/event-stream');
		expect(response.headers.get('cache-control')).toContain('no-cache');
		expect(response.headers.get('x-accel-buffering')).toBe('no');

		// The engine stream is still open: the first event must come through anyway.
		push('event: log\ndata: {"message":"hi"}\n\n');
		const reader = response.body!.getReader();
		const { value } = await reader.read();
		expect(new TextDecoder().decode(value)).toContain('"message":"hi"');
		await reader.cancel();

		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe(`http://localhost:8080/projects/${PROJECT}/logs/tail?level=error`);
		expect((init.headers as Headers).get('cookie')).toBe('id=session');
		expect(init.signal).toBe(event.request.signal);
	});

	it('passes engine errors through', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'unauthorized' }), {
				status: 401,
				headers: { 'content-type': 'application/json' }
			})
		);
		const { event } = fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/tail`);
		const response = await GET(event);
		expect(response.status).toBe(401);
		expect(await response.json()).toEqual({ error: 'unauthorized' });
	});

	it('refuses a project id that is not a UUID before calling the engine', async () => {
		const fetchMock = vi.fn();
		const { event } = fakeEvent(fetchMock, 'http://ui.test/projects/x/logs/tail', '../monitors');
		await expect(GET(event)).rejects.toMatchObject({ status: 404 });
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
```

Run: `cd nami-ui && npx vitest run`
Expected: FAIL — `./log-proxy` and `./+server` do not resolve.

- [ ] **Step 8: Implement the proxy**

`nami-ui/src/lib/server/log-proxy.ts`:

```ts
// Query parameters the BFF forwards to nami-engine's log endpoints. Only
// these keys pass (repeats kept, in order), so a crafted URL cannot smuggle
// anything else into the engine request.

/** Filters shared by search, histogram and live tail. */
export const FILTER_PARAMS = ['source_id', 'level', 'q', 'in_fields', 'filter', 'trace_id', 'span_id', 'pattern_hash'];
/** Live tail: the filters only (no window, cursor or limit). */
export const TAIL_PARAMS = FILTER_PARAMS;
/** Search: the filters plus the window and pagination. */
export const SEARCH_PARAMS = [...FILTER_PARAMS, 'from', 'to', 'limit', 'cursor'];

/** `?key=value&…` with only `keys`, or `''` when none are present. */
export function pickLogParams(params: URLSearchParams, keys: readonly string[]): string {
	const picked = new URLSearchParams();
	for (const [key, value] of params) {
		if (keys.includes(key)) picked.append(key, value);
	}
	const text = picked.toString();
	return text ? `?${text}` : '';
}
```

`nami-ui/src/routes/(app)/projects/[id]/logs/tail/+server.ts`:

```ts
import { error } from '@sveltejs/kit';
import { callEngine, isUuid } from '#lib/server/engine';
import { pickLogParams, TAIL_PARAMS } from '#lib/server/log-proxy';
import type { RequestHandler } from './$types';

/**
 * Live tail: relays nami-engine's Server-Sent Events to the browser as they
 * arrive. The engine body is handed through as a stream (never read here),
 * and the browser disconnecting aborts the engine request.
 */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id)) throw error(404, 'Not found');
	const query = pickLogParams(event.url.searchParams, TAIL_PARAMS);
	const response = await callEngine(event, `/projects/${event.params.id}/logs/tail${query}`, {
		headers: { accept: 'text/event-stream' },
		signal: event.request.signal
	});
	if (!response.ok || !response.body) {
		return new Response(await response.text(), {
			status: response.status,
			headers: { 'content-type': response.headers.get('content-type') ?? 'application/json' }
		});
	}
	return new Response(response.body, {
		headers: {
			'content-type': 'text/event-stream',
			'cache-control': 'no-cache, no-transform',
			'x-accel-buffering': 'no'
		}
	});
};
```

- [ ] **Step 9: Check nami-ui**

Run: `cd nami-ui && npm run check && npx vitest run`
Expected: `svelte-check found 0 errors and 0 warnings`; vitest 68 passed on the verified base (63 before this task).

- [ ] **Step 10: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/src/logs nami-engine/src/middleware/rbac.rs \
  nami-engine/src/routes/logs.rs nami-engine/tests/logs_tail.rs \
  nami-ui/src/lib/server/log-proxy.ts nami-ui/src/lib/server/log-proxy.test.ts \
  "nami-ui/src/routes/(app)/projects/[id]/logs/tail"
git commit -m "feat: live tail over Server-Sent Events through the BFF (NAMI-LOG-6)

GET /projects/{id}/logs/tail streams new rows matching the search filters
(event: log), polling every second with a 60 s look-back so late rows
still arrive, a keep-alive comment every 15 s, a session and access
re-check every 60 s, and an end event after 30 minutes. nami-ui relays the
stream unbuffered and forwards only allow-listed parameters.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7 (NAMI-LOG-7): UI — log sources: list, create, token reveal and rotation, setup snippets, settings

Branch `feat/NAMI-LOG-7-ui-sources`, worktree `.worktrees/NAMI-LOG-7/`.

**Files:**
- Create: `nami-ui/src/lib/logs.ts`, `nami-ui/src/lib/server/source-form.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte`, `nami-ui/src/routes/(app)/projects/[id]/logs/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/sources/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/sources/+page.svelte`, `nami-ui/src/routes/(app)/projects/[id]/logs/sources/[sourceId]/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/sources/[sourceId]/+page.svelte`
- Modify: `nami-ui/src/lib/server/engine.ts`, `nami-ui/src/routes/(app)/projects/[id]/+layout.svelte`
- Test: `nami-ui/src/lib/logs.test.ts`, `nami-ui/src/lib/server/source-form.test.ts`, `nami-ui/src/lib/server/engine.test.ts`

**Interfaces:**
- Consumes: engine routes of Task 1 (`/projects/{id}/sources…`, `/token`, `/rotate`) and Task 3 (`/usage?days=1`, `POST /api/v1/logs` for the snippets); `callEngine`, `engineErrorMessage`, `isUuid`, `publicEngineUrl` (`#lib/server/engine`); `formText`, `formWholeNumber`, `FormResult` (`#lib/server/monitor-form`); `CopyButton.svelte`; `formatUtc` (`#lib/uptime`).
- Produces:
  - `#lib/logs`: `type Platform`, `PLATFORMS`, `PLATFORM_LABELS`, `type Source`, `type SourceWithToken`, `type DailyUsage`, `MIB`, `TOKEN_PLACEHOLDER`, `formatBytes(bytes: number): string`, `quotaInMib(quota: number | null): string`, `formatQuota(quota: number | null): string`, `ingestUrl(engineUrl: string): string`, `curlSnippet(url, token): string`, `vectorSnippet(url, token): string`, `rsyslogSnippet(host: string, port: number, token: string): string`, `dockerSnippet(url, token): string`.
  - `#lib/server/source-form`: `sourcePayloadFromForm(form: FormData): FormResult` (quota entered in MiB, blank → `null`).
  - `#lib/server/engine`: `publicSyslogAddress(): { host: string; port: number }` (`PUBLIC_SYSLOG_ADDRESS`, default the public engine host on 5140).
  - Pages: the "Logs" project tab (`/projects/[id]/logs`, redirecting to sources until Task 8), `/projects/[id]/logs/sources` (list + `?/create`, which returns `{ created: { id, name, token } }`), `/projects/[id]/logs/sources/[sourceId]` (`?/update`, `?/reveal` → `{ token }`, `?/rotate` → `{ token, rotated }`, `?/delete` → redirect to the list).

- [ ] **Step 1: Write the failing tests**

`nami-ui/src/lib/logs.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
	curlSnippet,
	dockerSnippet,
	formatBytes,
	formatQuota,
	ingestUrl,
	MIB,
	quotaInMib,
	rsyslogSnippet,
	vectorSnippet
} from './logs';

describe('formatBytes', () => {
	it('uses binary units with at most one decimal', () => {
		expect(formatBytes(0)).toBe('0 B');
		expect(formatBytes(1023)).toBe('1023 B');
		expect(formatBytes(1536)).toBe('1.5 KiB');
		expect(formatBytes(MIB)).toBe('1 MiB');
		expect(formatBytes(150 * MIB)).toBe('150 MiB');
		expect(formatBytes(3 * 1024 * MIB)).toBe('3 GiB');
	});

	it('formats quotas', () => {
		expect(formatQuota(null)).toBe('Unlimited');
		expect(formatQuota(10 * MIB)).toBe('10 MiB / day');
		expect(quotaInMib(null)).toBe('');
		expect(quotaInMib(MIB)).toBe('1');
		expect(quotaInMib(1.5 * MIB)).toBe('1.5');
	});
});

describe('setup snippets', () => {
	const url = ingestUrl('https://engine.example.com');
	const token = 'tok_123';

	it('point at the ingest endpoint with the token', () => {
		expect(url).toBe('https://engine.example.com/api/v1/logs');
		for (const snippet of [curlSnippet(url, token), vectorSnippet(url, token), dockerSnippet(url, token)]) {
			expect(snippet).toContain(url);
			expect(snippet).toContain(token);
		}
		expect(curlSnippet(url, token)).toContain("-H 'Authorization: Bearer tok_123'");
		expect(vectorSnippet(url, token)).toContain('strategy: bearer');
		expect(dockerSnippet(url, token)).toContain('type: docker_logs');
	});

	it('send RFC 5424 with the token in structured data for syslog', () => {
		const snippet = rsyslogSnippet('logs.example.com', 5140, token);
		expect(snippet).toContain('<%pri%>1 %timestamp:::date-rfc3339%');
		expect(snippet).toContain('[nami token=\\"tok_123\\"]');
		expect(snippet).toContain('target="logs.example.com" port="5140" protocol="tcp"');
	});
});
```

`nami-ui/src/lib/server/source-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { sourcePayloadFromForm } from './source-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('sourcePayloadFromForm', () => {
	it('builds the engine body, converting the quota from MiB', () => {
		expect(
			sourcePayloadFromForm(
				form({ name: ' api ', platform: 'vector', retention_days: '90', daily_quota_mib: '1.5' })
			)
		).toEqual({
			ok: true,
			payload: { name: 'api', platform: 'vector', retention_days: 90, daily_byte_quota: 1572864 }
		});
	});

	it('sends null for a blank quota and omits blank optional fields', () => {
		expect(sourcePayloadFromForm(form({ name: 'api', daily_quota_mib: '' }))).toEqual({
			ok: true,
			payload: { name: 'api', daily_byte_quota: null }
		});
	});

	it('rejects bad values with a readable message', () => {
		const cases: Record<string, string>[] = [
			{ name: 'x', platform: 'fluentd' },
			{ name: 'x', retention_days: '7.5' },
			{ name: 'x', daily_quota_mib: '0.5' },
			{ name: 'x', daily_quota_mib: 'lots' }
		];
		for (const fields of cases) {
			const result = sourcePayloadFromForm(form(fields));
			expect(result.ok, JSON.stringify(fields)).toBe(false);
		}
	});
});
```

```diff
diff --git a/nami-ui/src/lib/server/engine.test.ts b/nami-ui/src/lib/server/engine.test.ts
--- a/nami-ui/src/lib/server/engine.test.ts
+++ b/nami-ui/src/lib/server/engine.test.ts
@@ -1,5 +1,5 @@
 import { afterEach, describe, it, expect, vi } from 'vitest';
-import { callEngine, engineErrorMessage, isUuid, publicEngineUrl } from './engine';
+import { callEngine, engineErrorMessage, isUuid, publicEngineUrl, publicSyslogAddress } from './engine';
 
 describe('callEngine', () => {
 	it('forwards the cookie header and relays set-cookie back', async () => {
@@ -123,6 +123,32 @@ describe('publicEngineUrl', () => {
 	});
 });
 
+describe('publicSyslogAddress', () => {
+	const original = { syslog: process.env.PUBLIC_SYSLOG_ADDRESS, engine: process.env.PUBLIC_ENGINE_URL };
+	afterEach(() => {
+		for (const [key, value] of [
+			['PUBLIC_SYSLOG_ADDRESS', original.syslog],
+			['PUBLIC_ENGINE_URL', original.engine]
+		] as const) {
+			if (value === undefined) delete process.env[key];
+			else process.env[key] = value;
+		}
+	});
+
+	it('defaults to the public engine host on port 5140', () => {
+		delete process.env.PUBLIC_SYSLOG_ADDRESS;
+		process.env.PUBLIC_ENGINE_URL = 'https://engine.example.com';
+		expect(publicSyslogAddress()).toEqual({ host: 'engine.example.com', port: 5140 });
+	});
+
+	it('uses PUBLIC_SYSLOG_ADDRESS when set', () => {
+		process.env.PUBLIC_SYSLOG_ADDRESS = 'logs.example.com:6514';
+		expect(publicSyslogAddress()).toEqual({ host: 'logs.example.com', port: 6514 });
+		process.env.PUBLIC_SYSLOG_ADDRESS = 'logs.example.com';
+		expect(publicSyslogAddress()).toEqual({ host: 'logs.example.com', port: 5140 });
+	});
+});
+
 describe('isUuid', () => {
 	it('accepts UUIDs only, so form values cannot steer engine paths', () => {
 		expect(isUuid('0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69')).toBe(true);
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-ui && npx vitest run`
Expected: FAIL — `./logs` and `./source-form` do not resolve, and `publicSyslogAddress` does not exist yet.

- [ ] **Step 3: Implement the shared helpers**

`nami-ui/src/lib/logs.ts`:

```ts
// Shapes of nami-engine's log JSON, plus pure helpers for the log pages.
// All times render in UTC (as on the uptime pages).

export type Platform = 'http' | 'syslog' | 'vector' | 'docker' | 'other';

export const PLATFORMS: Platform[] = ['http', 'syslog', 'vector', 'docker', 'other'];

export const PLATFORM_LABELS: Record<Platform, string> = {
	http: 'HTTP',
	syslog: 'Syslog',
	vector: 'Vector',
	docker: 'Docker',
	other: 'Other'
};

export type Source = {
	id: string;
	project_id: string;
	name: string;
	platform: Platform;
	retention_days: number;
	daily_byte_quota: number | null;
	created_at: string;
	updated_at: string;
};

/** Create and rotate return the token once more (members and admins only). */
export type SourceWithToken = Source & { token: string };

export type DailyUsage = {
	day: string;
	events: number;
	bytes: number;
	events_dropped: number;
	events_sampled_out: number;
	requests_rate_limited: number;
	requests_over_quota: number;
};

export const MIB = 1024 * 1024;
/** Shown in snippets until a member reveals the real token. */
export const TOKEN_PLACEHOLDER = 'YOUR_SOURCE_TOKEN';

/** "512 B", "1.5 KiB", "120 MiB". */
export function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KiB', 'MiB', 'GiB', 'TiB'];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value >= 100 ? Math.round(value) : Number(value.toFixed(1))} ${units[unit]}`;
}

/** A stored quota as the MiB value the form shows ('' = unlimited). */
export function quotaInMib(quota: number | null): string {
	return quota === null ? '' : String(Number((quota / MIB).toFixed(2)));
}

/** "Unlimited", or the quota per day in readable units. */
export function formatQuota(quota: number | null): string {
	return quota === null ? 'Unlimited' : `${formatBytes(quota)} / day`;
}

export function ingestUrl(engineUrl: string): string {
	return `${engineUrl}/api/v1/logs`;
}

export function curlSnippet(url: string, token: string): string {
	return [
		`curl -X POST ${url} \\`,
		`  -H 'Authorization: Bearer ${token}' \\`,
		"  -H 'Content-Type: application/json' \\",
		`  -d '{"message":"Hello from Nami","level":"info"}'`
	].join('\n');
}

function vectorSink(url: string, token: string, input: string): string[] {
	return [
		'sinks:',
		'  nami:',
		'    type: http',
		`    inputs: [${input}]`,
		`    uri: ${url}`,
		'    compression: gzip',
		'    encoding:',
		'      codec: json',
		'    auth:',
		'      strategy: bearer',
		`      token: ${token}`
	];
}

export function vectorSnippet(url: string, token: string): string {
	return [
		'# vector.yaml',
		'sources:',
		'  app_logs:',
		'    type: file',
		'    include: ["/var/log/app/*.log"]',
		'',
		...vectorSink(url, token, 'app_logs')
	].join('\n');
}

/** rsyslog forwarding RFC 5424 over TCP, the token in structured data. */
export function rsyslogSnippet(host: string, port: number, token: string): string {
	return [
		'# /etc/rsyslog.d/90-nami.conf',
		'template(name="NamiFormat" type="string"',
		`  string="<%pri%>1 %timestamp:::date-rfc3339% %HOSTNAME% %app-name% %procid% %msgid% [nami token=\\"${token}\\"] %msg%\\n")`,
		`*.* action(type="omfwd" target="${host}" port="${port}" protocol="tcp" template="NamiFormat")`
	].join('\n');
}

/** Every container's logs, shipped by a Vector container. */
export function dockerSnippet(url: string, token: string): string {
	return [
		'# docker-compose.yml',
		'services:',
		'  vector:',
		'    image: timberio/vector:latest-alpine',
		'    restart: unless-stopped',
		'    volumes:',
		'      - /var/run/docker.sock:/var/run/docker.sock:ro',
		'      - ./vector.yaml:/etc/vector/vector.yaml:ro',
		'',
		'# vector.yaml',
		'sources:',
		'  docker:',
		'    type: docker_logs',
		'',
		...vectorSink(url, token, 'docker')
	].join('\n');
}
```

`nami-ui/src/lib/server/source-form.ts`:

```ts
import { MIB, PLATFORMS, type Platform } from '#lib/logs';
import { formText, formWholeNumber, type FormResult } from './monitor-form';

/**
 * Source form -> nami-engine JSON body. The daily quota is entered in MiB;
 * blank means unlimited (`null`, which also clears a quota on edit).
 */
export function sourcePayloadFromForm(form: FormData): FormResult {
	const payload: Record<string, unknown> = { name: formText(form, 'name') };
	const platform = formText(form, 'platform');
	if (platform !== '') {
		if (!PLATFORMS.includes(platform as Platform)) return { ok: false, message: 'Pick a platform from the list.' };
		payload.platform = platform;
	}
	const retention = formWholeNumber(form, 'retention_days');
	if (retention === 'invalid') return { ok: false, message: 'Retention must be a whole number of days.' };
	if (retention !== undefined) payload.retention_days = retention;
	const quota = formText(form, 'daily_quota_mib');
	if (quota === '') {
		payload.daily_byte_quota = null;
	} else {
		const mib = Number(quota);
		if (!Number.isFinite(mib) || mib < 1) {
			return { ok: false, message: 'The daily quota must be at least 1 MiB (leave it blank for unlimited).' };
		}
		payload.daily_byte_quota = Math.round(mib * MIB);
	}
	return { ok: true, payload };
}
```

```diff
diff --git a/nami-ui/src/lib/server/engine.ts b/nami-ui/src/lib/server/engine.ts
--- a/nami-ui/src/lib/server/engine.ts
+++ b/nami-ui/src/lib/server/engine.ts
@@ -97,6 +97,24 @@ export function publicEngineUrl(): string {
 	return (process.env.PUBLIC_ENGINE_URL ?? 'http://localhost:8080').replace(/\/+$/, '');
 }
 
+/**
+ * Where syslog senders reach nami-engine's syslog listener, for the setup
+ * snippets: PUBLIC_SYSLOG_ADDRESS ("host:port"), else the public engine
+ * host on the default port 5140.
+ */
+export function publicSyslogAddress(): { host: string; port: number } {
+	const configured = process.env.PUBLIC_SYSLOG_ADDRESS?.trim();
+	if (configured) {
+		const colon = configured.lastIndexOf(':');
+		const port = Number(configured.slice(colon + 1));
+		if (colon > 0 && Number.isInteger(port) && port > 0 && port < 65536) {
+			return { host: configured.slice(0, colon), port };
+		}
+		return { host: configured, port: 5140 };
+	}
+	return { host: new URL(publicEngineUrl()).hostname, port: 5140 };
+}
+
 const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
 
 /**
```

- [ ] **Step 4: Add the Logs section**

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte b/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
@@ -9,6 +9,7 @@
 		{ href: `/projects/${data.project.id}/heartbeats`, label: 'Heartbeats' },
 		{ href: `/projects/${data.project.id}/incidents`, label: 'Incidents' },
 		{ href: `/projects/${data.project.id}/maintenance`, label: 'Maintenance' },
+		{ href: `/projects/${data.project.id}/logs`, label: 'Logs' },
 		{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
 	]);
 </script>
```

`nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte`:

```svelte
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const base = $derived(`/projects/${data.project.id}/logs`);
	const sections = $derived([{ href: `${base}/sources`, label: 'Sources' }]);
</script>

<div class="space-y-6">
	<nav class="flex gap-2" aria-label="Logs sections">
		{#each sections as section (section.href)}
			{@const active = page.url.pathname.startsWith(section.href)}
			<a
				href={section.href}
				aria-current={active ? 'page' : undefined}
				class="rounded-md px-3 py-1.5 text-sm transition-colors {active
					? 'bg-[#1f1f1f] text-neutral-100'
					: 'text-neutral-500 hover:text-neutral-300'}"
			>
				{section.label}
			</a>
		{/each}
	</nav>

	{@render children()}
</div>
```

`nami-ui/src/routes/(app)/projects/[id]/logs/+page.server.ts` (replaced by the explorer in Task 8):

```ts
import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

// The explorer lands here in the next task; until then the Logs tab opens
// the sources.
export const load: PageServerLoad = async (event) => {
	throw redirect(307, `/projects/${event.params.id}/logs/sources`);
};
```

- [ ] **Step 5: The sources list**

`nami-ui/src/routes/(app)/projects/[id]/logs/sources/+page.server.ts`:

```ts
import { error, fail } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { sourcePayloadFromForm } from '#lib/server/source-form';
import type { Source, SourceWithToken } from '#lib/logs';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/sources`);
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load sources.'));
	}
	const sources: Source[] = await response.json();
	return { sources };
};

export const actions: Actions = {
	// Returns the new token instead of redirecting, so the page can show it
	// right away (it is never put in a URL).
	create: async (event) => {
		const parsed = sourcePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/sources`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not create the source.') });
		}
		const created: SourceWithToken = await response.json();
		return { created: { id: created.id, name: created.name, token: created.token } };
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/logs/sources/+page.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import { formatQuota, PLATFORM_LABELS, PLATFORMS } from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
</script>

<div class="space-y-6">
	{#if form?.created}
		<div class="space-y-3 rounded-xl border border-teal-500/30 bg-teal-500/10 p-6" role="status">
			<p class="text-sm text-teal-200">
				Source <span class="font-medium">{form.created.name}</span> created. Send logs with this token — members can
				reveal it again on the source page.
			</p>
			<div class="flex items-center gap-2">
				<code
					class="block flex-1 truncate rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs text-neutral-200 select-all"
					>{form.created.token}</code
				>
				<CopyButton value={form.created.token} />
			</div>
			<a href="/projects/{data.project.id}/logs/sources/{form.created.id}" class="text-sm text-teal-300 hover:text-teal-200"
				>Setup instructions →</a
			>
		</div>
	{/if}

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="border-b border-[#262626] px-6 py-4">
			<h2 class="text-base font-semibold text-neutral-100">Log sources</h2>
			<p class="text-xs text-neutral-500">
				Each source has its own ingest token and retention. Send logs over HTTP (JSON, NDJSON, MessagePack) or syslog.
			</p>
		</div>
		{#if data.sources.length === 0}
			<p class="px-6 py-10 text-center text-sm text-neutral-500">No sources yet — create one below.</p>
		{:else}
			<table class="w-full text-left text-sm">
				<thead class="text-xs text-neutral-500">
					<tr class="border-b border-[#262626]">
						<th class="px-6 py-2 font-medium">Name</th>
						<th class="px-6 py-2 font-medium">Platform</th>
						<th class="px-6 py-2 font-medium">Retention</th>
						<th class="px-6 py-2 font-medium">Daily quota</th>
						<th class="px-6 py-2 font-medium">Created</th>
					</tr>
				</thead>
				<tbody>
					{#each data.sources as source (source.id)}
						<tr class="border-b border-[#262626] last:border-0">
							<td class="px-6 py-3">
								<a
									href="/projects/{data.project.id}/logs/sources/{source.id}"
									class="font-medium text-neutral-100 hover:text-teal-300">{source.name}</a
								>
							</td>
							<td class="px-6 py-3 text-neutral-400">{PLATFORM_LABELS[source.platform]}</td>
							<td class="px-6 py-3 text-neutral-400">{source.retention_days} days</td>
							<td class="px-6 py-3 text-neutral-400">{formatQuota(source.daily_byte_quota)}</td>
							<td class="px-6 py-3 text-xs text-neutral-500">{formatUtc(source.created_at)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-base font-semibold text-neutral-100">New source</h2>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400" role="alert">
				{form.message}
			</div>
		{/if}
		<form method="POST" action="?/create" use:enhance class="mt-4 grid gap-4 sm:grid-cols-2 lg:grid-cols-[1fr_10rem_9rem_10rem_auto] lg:items-end">
			<div>
				<label for="source-name" class={label}>Name</label>
				<input id="source-name" name="name" type="text" required maxlength="200" placeholder="Production API" class={input} />
			</div>
			<div>
				<label for="source-platform" class={label}>Platform</label>
				<select id="source-platform" name="platform" class={input}>
					{#each PLATFORMS as platform (platform)}
						<option value={platform}>{PLATFORM_LABELS[platform]}</option>
					{/each}
				</select>
			</div>
			<div>
				<label for="source-retention" class={label}>Retention (days)</label>
				<input id="source-retention" name="retention_days" type="number" required min="1" max="365" defaultValue={30} class={input} />
			</div>
			<div>
				<label for="source-quota" class={label}>Daily quota (MiB)</label>
				<input id="source-quota" name="daily_quota_mib" type="number" min="1" step="any" placeholder="Unlimited" class={input} />
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Create
			</button>
		</form>
	</div>
</div>
```

- [ ] **Step 6: The source page**

`nami-ui/src/routes/(app)/projects/[id]/logs/sources/[sourceId]/+page.server.ts`:

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, isUuid, publicEngineUrl, publicSyslogAddress } from '#lib/server/engine';
import { sourcePayloadFromForm } from '#lib/server/source-form';
import type { DailyUsage, Source, SourceWithToken } from '#lib/logs';
import type { Actions, PageServerLoad } from './$types';

function sourcePath(params: { id: string; sourceId: string }): string {
	if (!isUuid(params.sourceId)) throw error(404, 'Source not found.');
	return `/projects/${params.id}/sources/${params.sourceId}`;
}

export const load: PageServerLoad = async (event) => {
	const path = sourcePath(event.params);
	const response = await callEngine(event, path);
	if (response.status === 404) throw error(404, 'Source not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load the source.'));
	}
	const source: Source = await response.json();
	const usageResponse = await callEngine(event, `${path}/usage?days=1`);
	const today: DailyUsage | null = usageResponse.ok ? ((await usageResponse.json()) as DailyUsage[])[0] ?? null : null;
	return { source, today, engineUrl: publicEngineUrl(), syslog: publicSyslogAddress() };
};

/** The token stays out of the page until asked for, and out of any URL. */
async function tokenResult(response: Response, fallback: string) {
	if (response.status === 403) {
		return fail(403, { message: 'Only project members and admins can see or rotate the token.' });
	}
	if (!response.ok) return fail(response.status, { message: await engineErrorMessage(response, fallback) });
	const body: { token: string } | SourceWithToken = await response.json();
	return { token: body.token };
}

export const actions: Actions = {
	update: async (event) => {
		const parsed = sourcePayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, sourcePath(event.params), {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not save the source.') });
		}
		return { saved: true };
	},
	reveal: async (event) => {
		const response = await callEngine(event, `${sourcePath(event.params)}/token`);
		return tokenResult(response, 'Could not reveal the token.');
	},
	rotate: async (event) => {
		const response = await callEngine(event, `${sourcePath(event.params)}/rotate`, { method: 'POST' });
		const result = await tokenResult(response, 'Could not rotate the token.');
		return 'token' in result ? { ...result, rotated: true } : result;
	},
	delete: async (event) => {
		const response = await callEngine(event, sourcePath(event.params), { method: 'DELETE' });
		if (!response.ok) {
			return fail(response.status, {
				message:
					response.status === 403
						? 'Only project admins can delete a source.'
						: await engineErrorMessage(response, 'Could not delete the source.')
			});
		}
		throw redirect(303, `/projects/${event.params.id}/logs/sources`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/logs/sources/[sourceId]/+page.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import {
		curlSnippet,
		dockerSnippet,
		formatBytes,
		formatQuota,
		ingestUrl,
		PLATFORM_LABELS,
		PLATFORMS,
		quotaInMib,
		rsyslogSnippet,
		TOKEN_PLACEHOLDER,
		vectorSnippet
	} from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	type SnippetKind = 'curl' | 'vector' | 'syslog' | 'docker';
	const SNIPPET_LABELS: Record<SnippetKind, string> = {
		curl: 'curl',
		vector: 'Vector',
		syslog: 'Syslog (rsyslog)',
		docker: 'Docker'
	};
	let snippetKind = $state<SnippetKind>('curl');

	const token = $derived(form?.token ?? TOKEN_PLACEHOLDER);
	const url = $derived(ingestUrl(data.engineUrl));
	const snippet = $derived.by(() => {
		switch (snippetKind) {
			case 'curl':
				return curlSnippet(url, token);
			case 'vector':
				return vectorSnippet(url, token);
			case 'syslog':
				return rsyslogSnippet(data.syslog.host, data.syslog.port, token);
			case 'docker':
				return dockerSnippet(url, token);
		}
	});

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-1">
				<h2 class="text-base font-semibold text-neutral-100">{data.source.name}</h2>
				<p class="text-sm text-neutral-500">
					{PLATFORM_LABELS[data.source.platform]} · {data.source.retention_days} days retention · {formatQuota(
						data.source.daily_byte_quota
					)}
				</p>
				<p class="text-xs text-neutral-600">Created {formatUtc(data.source.created_at)}</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<form
					method="POST"
					action="?/rotate"
					use:enhance={({ cancel }) => {
						if (!confirm('Rotate the token? The current token stops working immediately.')) cancel();
					}}
				>
					<button type="submit" class={button}>Rotate token</button>
				</form>
				<form
					method="POST"
					action="?/delete"
					use:enhance={({ cancel }) => {
						if (!confirm(`Delete source "${data.source.name}"? Ingestion stops now; stored logs age out with their retention.`))
							cancel();
					}}
				>
					<button
						type="submit"
						class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
					>
						Delete
					</button>
				</form>
			</div>
		</div>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400" role="alert">
				{form.message}
			</div>
		{/if}
		{#if form?.rotated}
			<div class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300" role="status">
				Token rotated. Update your senders: the old token no longer works.
			</div>
		{/if}
	</div>

	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Source token</h3>
		{#if form?.token}
			<div class="flex items-center gap-2">
				<code
					class="block flex-1 truncate rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs text-neutral-200 select-all"
					>{form.token}</code
				>
				<CopyButton value={form.token} />
			</div>
			<p class="text-xs text-neutral-500">Shown until you leave this page. Treat it like a password.</p>
		{:else}
			<p class="text-sm text-neutral-500">Members and admins can reveal the token; it also fills in the snippets below.</p>
			<form method="POST" action="?/reveal" use:enhance>
				<button type="submit" class={button}>Reveal token</button>
			</form>
		{/if}
	</div>

	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<h3 class="text-sm font-semibold text-neutral-200">Send logs</h3>
			<div class="flex gap-1" role="tablist" aria-label="Setup snippets">
				{#each Object.entries(SNIPPET_LABELS) as [kind, title] (kind)}
					<button
						type="button"
						role="tab"
						aria-selected={snippetKind === kind}
						onclick={() => (snippetKind = kind as SnippetKind)}
						class="rounded-md px-2.5 py-1 text-xs transition-colors {snippetKind === kind
							? 'bg-[#1f1f1f] text-neutral-100'
							: 'text-neutral-500 hover:text-neutral-300'}">{title}</button
					>
				{/each}
			</div>
		</div>
		<div class="flex items-start gap-2">
			<pre
				class="flex-1 overflow-x-auto rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs text-neutral-300">{snippet}</pre>
			<CopyButton value={snippet} />
		</div>
		<p class="text-xs text-neutral-500">
			HTTP accepts a JSON object, a JSON array, NDJSON or MessagePack (gzip optional) up to 10 MiB and 10,000 events
			per request, 50 requests per second per source. Syslog needs the engine's <code>SYSLOG_ENABLED=true</code>.
		</p>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Today (UTC)</h3>
		{#if data.today}
			<dl class="mt-3 grid grid-cols-2 gap-4 text-sm sm:grid-cols-4">
				<div>
					<dt class="text-xs text-neutral-500">Events</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.events.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Volume</dt>
					<dd class="text-neutral-100 tabular-nums">{formatBytes(data.today.bytes)}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Rate limited</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.requests_rate_limited.toLocaleString('en-US')}</dd>
				</div>
				<div>
					<dt class="text-xs text-neutral-500">Over quota</dt>
					<dd class="text-neutral-100 tabular-nums">{data.today.requests_over_quota.toLocaleString('en-US')}</dd>
				</div>
			</dl>
			<p class="mt-2 text-xs text-neutral-600">Counts reach this page within about 10 seconds.</p>
		{:else}
			<p class="mt-2 text-sm text-neutral-500">No usage data.</p>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Settings</h3>
		{#if form?.saved}
			<div class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300" role="status">Saved.</div>
		{/if}
		<!-- reset: false keeps the inputs showing what was just saved. -->
		<form
			method="POST"
			action="?/update"
			use:enhance={() =>
				async ({ update }) =>
					update({ reset: false })}
			class="mt-4 grid gap-4 sm:grid-cols-2"
		>
			<div>
				<label for="source-name" class={label}>Name</label>
				<input id="source-name" name="name" type="text" required maxlength="200" defaultValue={data.source.name} class={input} />
			</div>
			<div>
				<label for="source-platform" class={label}>Platform</label>
				<select id="source-platform" name="platform" value={data.source.platform} class={input}>
					{#each PLATFORMS as platform (platform)}
						<option value={platform}>{PLATFORM_LABELS[platform]}</option>
					{/each}
				</select>
			</div>
			<div>
				<label for="source-retention" class={label}>Retention (days)</label>
				<input
					id="source-retention"
					name="retention_days"
					type="number"
					required
					min="1"
					max="365"
					defaultValue={data.source.retention_days}
					class={input}
				/>
				<p class="mt-1 text-xs text-neutral-500">Changes affect new rows only; stored rows keep their retention.</p>
			</div>
			<div>
				<label for="source-quota" class={label}>Daily quota (MiB)</label>
				<input
					id="source-quota"
					name="daily_quota_mib"
					type="number"
					min="1"
					step="any"
					placeholder="Unlimited"
					defaultValue={quotaInMib(data.source.daily_byte_quota)}
					class={input}
				/>
				<p class="mt-1 text-xs text-neutral-500">
					Over the quota, requests get 429 <code>quota_exceeded</code> until midnight UTC; one request plus about 10 s of
					traffic per engine instance can go over.
				</p>
			</div>
			<div class="sm:col-span-2">
				<button
					type="submit"
					class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
				>
					Save
				</button>
			</div>
		</form>
	</div>
</div>
```

- [ ] **Step 7: Check and test**

Run: `cd nami-ui && npm run check && npx vitest run`
Expected: `svelte-check found 0 errors and 0 warnings`; vitest 77 passed on the verified base.

- [ ] **Step 8: Walk through it in the browser**

Run the engine (`SYSLOG_ENABLED=true`) and `npm run dev` with `ENGINE_URL`, `PUBLIC_ENGINE_URL` and (optionally) `PUBLIC_SYSLOG_ADDRESS` set. As a project admin: Logs tab → Sources → create a source with a 2 MiB quota (the token panel appears, the form resets); open it via "Setup instructions"; Reveal token (the snippets fill in); send the curl snippet and a syslog frame, then reload — "Today" counts them within ~10 s; Rotate (confirm) → the old token gets 401 from `curl`, the new one 202; set retention to 7 and clear the quota → Saved, header shows "7 days retention · Unlimited". As a project viewer on the same page: Reveal and Rotate show "Only project members and admins can see or rotate the token.", Delete shows "Only project admins can delete a source." As admin, delete a throwaway source → back on the list without it. `/logs/sources/not-a-uuid` is a 404.

- [ ] **Step 9: Commit**

```bash
git add nami-ui/src/lib/logs.ts nami-ui/src/lib/logs.test.ts nami-ui/src/lib/server/source-form.ts \
  nami-ui/src/lib/server/source-form.test.ts nami-ui/src/lib/server/engine.ts nami-ui/src/lib/server/engine.test.ts \
  "nami-ui/src/routes/(app)/projects/[id]/+layout.svelte" "nami-ui/src/routes/(app)/projects/[id]/logs"
git commit -m "feat(ui): log sources pages with token reveal, rotation and setup snippets (NAMI-LOG-7)

A Logs tab with a sources list and create form (the new token is shown
once, never in a URL), and a source page: reveal (members), rotate,
delete (admins), curl/Vector/rsyslog/Docker snippets, today's usage, and
settings with the retention and quota notes.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8 (NAMI-LOG-8): UI — log explorer: search, filters, histogram, results, columns, live tail

Branch `feat/NAMI-LOG-8-ui-explorer`, worktree `.worktrees/NAMI-LOG-8/`.

**Files:**
- Create: `nami-ui/src/lib/components/LogVolumeChart.svelte`, `nami-ui/src/routes/(app)/projects/[id]/logs/+page.svelte`, `nami-ui/src/routes/(app)/projects/[id]/logs/search/+server.ts`
- Modify: `nami-ui/src/lib/logs.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte`, `nami-ui/src/routes/(app)/projects/[id]/logs/+page.server.ts` (rewritten)
- Test: `nami-ui/src/lib/logs.test.ts`, `nami-ui/src/routes/(app)/projects/[id]/logs/search/server.test.ts`

**Interfaces:**
- Consumes: engine `GET /projects/{id}/logs`, `/logs/histogram` (Task 5) and `/logs/tail` (Task 6); nami-ui `GET /projects/[id]/logs/tail` and `pickLogParams`, `SEARCH_PARAMS` (Task 6); the `Source` type and the Logs layout (Task 7); `niceTicks`, `formatUtc` (`#lib/uptime`); `CopyButton.svelte`.
- Produces:
  - `#lib/logs` (added): `type LogLevel`, `LEVELS`, `type LogEntry`, `type HistogramBucket`, `type Histogram`, `type TimePreset`, `TIME_PRESETS`, `DEFAULT_PRESET`, `type FilterOp`, `FILTER_OPS`, `type FieldFilter`, `MAX_FILTERS`, `MAX_COLUMNS`, `PAGE_SIZE`, `LIVE_ROW_CAP`, `type ExplorerState`, `isFieldPath(path): boolean`, `filterNeedsValue(op): boolean`, `parseExplorerState(params: URLSearchParams): ExplorerState`, `explorerParams(state): URLSearchParams`, `explorerHref(path, state): string`, `resolveWindow(state, now: number): { from: string; to: string }`, `tailParams(state): URLSearchParams`, `engineParams(state, window): URLSearchParams`, `fieldAt(fields: unknown, path: string): unknown`, `formatFieldValue(value: unknown): string`, `formatLogTime(iso): string`, `levelClasses(level): string`, `toggled<T>(list: T[], item: T): T[]`, `toInputValue(iso | null): string`, `fromInputValue(value): string | null`.
  - `LogVolumeChart.svelte` props `{ histogram: Histogram; onselect?: (from: string, to: string) => void }`.
  - nami-ui `GET /projects/[id]/logs/search` ("load more" proxy) and the explorer page `/projects/[id]/logs`.

- [ ] **Step 1: Write the failing tests**

The explorer state and row helpers (appended to `nami-ui/src/lib/logs.test.ts`):

```diff
diff --git a/nami-ui/src/lib/logs.test.ts b/nami-ui/src/lib/logs.test.ts
--- a/nami-ui/src/lib/logs.test.ts
+++ b/nami-ui/src/lib/logs.test.ts
@@ -2,13 +2,28 @@ import { describe, expect, it } from 'vitest';
 import {
 	curlSnippet,
 	dockerSnippet,
+	engineParams,
+	explorerHref,
+	explorerParams,
+	fieldAt,
 	formatBytes,
+	formatFieldValue,
+	formatLogTime,
 	formatQuota,
+	fromInputValue,
 	ingestUrl,
+	isFieldPath,
+	levelClasses,
 	MIB,
+	parseExplorerState,
 	quotaInMib,
+	resolveWindow,
 	rsyslogSnippet,
-	vectorSnippet
+	tailParams,
+	toggled,
+	toInputValue,
+	vectorSnippet,
+	type ExplorerState
 } from './logs';
 
 describe('formatBytes', () => {
@@ -52,3 +67,125 @@ describe('setup snippets', () => {
 		expect(snippet).toContain('target="logs.example.com" port="5140" protocol="tcp"');
 	});
 });
+
+const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
+
+describe('explorer URL state', () => {
+	it('defaults to the last hour with nothing selected', () => {
+		const state = parseExplorerState(new URLSearchParams());
+		expect(state).toEqual({
+			range: '1h',
+			from: null,
+			to: null,
+			q: '',
+			inFields: false,
+			levels: [],
+			sources: [],
+			filters: [],
+			columns: [],
+			live: false
+		});
+		expect(explorerParams(state).toString()).toBe('');
+		expect(explorerHref('/p/logs', state)).toBe('/p/logs');
+	});
+
+	it('round-trips every setting through the URL', () => {
+		const state: ExplorerState = {
+			range: 'custom',
+			from: '2026-10-13T10:00:00.000Z',
+			to: '2026-10-13T11:00:00.000Z',
+			q: 'disk full',
+			inFields: true,
+			levels: ['error', 'warn'],
+			sources: [SOURCE],
+			filters: [
+				{ path: 'user.id', op: 'eq', value: '42' },
+				{ path: 'trace', op: 'exists' }
+			],
+			columns: ['user.id', 'status'],
+			live: true
+		};
+		const url = explorerParams(state);
+		expect(parseExplorerState(url)).toEqual(state);
+		expect(url.getAll('level')).toEqual(['error', 'warn']);
+		expect(url.get('filter')).toBe('{"path":"user.id","op":"eq","value":"42"}');
+	});
+
+	it('keeps relative presets relative', () => {
+		const state = parseExplorerState(new URLSearchParams('range=24h'));
+		expect(explorerParams(state).toString()).toBe('range=24h');
+		const now = Date.parse('2026-10-13T12:00:00Z');
+		expect(resolveWindow(state, now)).toEqual({ from: '2026-10-12T12:00:00.000Z', to: '2026-10-13T12:00:00.000Z' });
+	});
+
+	it('drops anything invalid instead of failing', () => {
+		const params = new URLSearchParams();
+		params.set('range', 'custom');
+		params.set('from', 'not a date');
+		params.append('level', 'verbose');
+		params.append('level', 'error');
+		params.append('level', 'error');
+		params.append('source', '../x');
+		params.append('filter', 'not json');
+		params.append('filter', JSON.stringify({ path: 'a b', op: 'eq', value: 'x' }));
+		params.append('filter', JSON.stringify({ path: 'a', op: 'like', value: 'x' }));
+		params.append('filter', JSON.stringify({ path: 'a', op: 'eq' }));
+		params.append('filter', JSON.stringify({ path: 'a', op: 'exists', value: 'ignored' }));
+		for (let i = 0; i < 12; i++) params.append('col', `c${i}`);
+		params.append('col', 'bad path!');
+		params.set('token', 'secret');
+		const state = parseExplorerState(params);
+		expect(state.range).toBe('1h');
+		expect(state.levels).toEqual(['error']);
+		expect(state.sources).toEqual([]);
+		expect(state.filters).toEqual([{ path: 'a', op: 'exists' }]);
+		expect(state.columns).toHaveLength(10);
+		expect(explorerParams(state).toString()).not.toContain('token');
+	});
+
+	it('maps to the engine parameters', () => {
+		const state = parseExplorerState(
+			new URLSearchParams(`source=${SOURCE}&level=error&q=Disk&in_fields=1&filter=${encodeURIComponent('{"path":"a","op":"gt","value":"5"}')}`)
+		);
+		const window = { from: '2026-10-13T10:00:00.000Z', to: '2026-10-13T11:00:00.000Z' };
+		const engine = engineParams(state, window);
+		expect(engine.getAll('source_id')).toEqual([SOURCE]);
+		expect(engine.get('in_fields')).toBe('true');
+		expect(engine.get('filter')).toBe('{"path":"a","op":"gt","value":"5"}');
+		expect([engine.get('from'), engine.get('to')]).toEqual([window.from, window.to]);
+		const tail = tailParams(state);
+		expect(tail.has('from') || tail.has('to')).toBe(false);
+		expect(tailParams(parseExplorerState(new URLSearchParams('in_fields=1'))).has('in_fields')).toBe(false);
+	});
+});
+
+describe('row helpers', () => {
+	it('reads field paths', () => {
+		const fields = { user: { id: 42, tags: ['a'] }, status: 'ok' };
+		expect(fieldAt(fields, 'user.id')).toBe(42);
+		expect(fieldAt(fields, 'status')).toBe('ok');
+		expect(fieldAt(fields, 'user.missing')).toBeUndefined();
+		expect(fieldAt(fields, 'status.deeper')).toBeUndefined();
+		expect(fieldAt(fields, 'user.tags.0')).toBeUndefined();
+		expect(formatFieldValue(fieldAt(fields, 'user'))).toBe('{"id":42,"tags":["a"]}');
+		expect(formatFieldValue(undefined)).toBe('');
+		expect(formatFieldValue('text')).toBe('text');
+	});
+
+	it('validates paths like the engine', () => {
+		expect(isFieldPath('a.b-c_d.1')).toBe(true);
+		for (const path of ['', 'a..b', 'a b', 'a.b.c.d.e.f', "a'", 'é']) expect(isFieldPath(path)).toBe(false);
+	});
+
+	it('formats times, levels and toggles', () => {
+		expect(formatLogTime('2026-10-13T14:30:05.123Z')).toBe('2026-10-13 14:30:05.123');
+		expect(levelClasses('error')).toContain('text-red-300');
+		expect(levelClasses('fatal')).toContain('font-semibold');
+		expect(levelClasses('unknown')).toContain('text-neutral-400');
+		expect(toggled(['a'], 'b')).toEqual(['a', 'b']);
+		expect(toggled(['a', 'b'], 'a')).toEqual(['b']);
+		expect(toInputValue('2026-10-13T14:30:00.000Z')).toBe('2026-10-13T14:30');
+		expect(fromInputValue('2026-10-13T14:30')).toBe('2026-10-13T14:30:00.000Z');
+		expect(fromInputValue('')).toBeNull();
+	});
+});
```

`nami-ui/src/routes/(app)/projects/[id]/logs/search/server.test.ts`:

```ts
import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT) {
	return {
		fetch: fetchMock,
		params: { id },
		url: new URL(url),
		request: new Request(url, { headers: { cookie: 'id=session' } }),
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

describe('GET /projects/[id]/logs/search', () => {
	it('forwards only search parameters and passes the page through', async () => {
		const page = { logs: [], next_cursor: null };
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify(page), { status: 200, headers: { 'content-type': 'application/json' } })
		);
		const event = fakeEvent(
			fetchMock,
			`http://ui.test/projects/${PROJECT}/logs/search?from=a&to=b&cursor=c&limit=100&level=error&col=x&live=1`
		);
		const response = await GET(event);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual(page);
		expect(fetchMock.mock.calls[0][0]).toBe(
			`http://localhost:8080/projects/${PROJECT}/logs?from=a&to=b&cursor=c&limit=100&level=error`
		);
	});

	it('passes engine errors through with their status', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'invalid cursor' }), { status: 400 })
		);
		const response = await GET(fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/search?cursor=x`));
		expect(response.status).toBe(400);
		expect(await response.json()).toEqual({ error: 'invalid cursor' });
	});

	it('refuses a project id that is not a UUID', async () => {
		const fetchMock = vi.fn();
		await expect(GET(fakeEvent(fetchMock, 'http://ui.test/x', 'nope'))).rejects.toMatchObject({ status: 404 });
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-ui && npx vitest run`
Expected: FAIL — the new `#lib/logs` exports (`parseExplorerState`, `engineParams`, …) do not exist yet and `./+server` does not resolve.

- [ ] **Step 3: Implement the explorer helpers**

Append to `nami-ui/src/lib/logs.ts`:

```diff
diff --git a/nami-ui/src/lib/logs.ts b/nami-ui/src/lib/logs.ts
--- a/nami-ui/src/lib/logs.ts
+++ b/nami-ui/src/lib/logs.ts
@@ -135,3 +135,265 @@ export function dockerSnippet(url: string, token: string): string {
 		...vectorSink(url, token, 'docker')
 	].join('\n');
 }
+
+// ---- Explorer -------------------------------------------------------------
+
+export type LogLevel =
+	| 'trace'
+	| 'debug'
+	| 'info'
+	| 'notice'
+	| 'warn'
+	| 'error'
+	| 'critical'
+	| 'alert'
+	| 'emergency'
+	| 'fatal';
+
+export const LEVELS: LogLevel[] = [
+	'trace',
+	'debug',
+	'info',
+	'notice',
+	'warn',
+	'error',
+	'critical',
+	'alert',
+	'emergency',
+	'fatal'
+];
+
+export type LogEntry = {
+	id: string;
+	source_id: string;
+	dt: string;
+	level: LogLevel;
+	message: string;
+	fields: unknown;
+	trace_id: string;
+	span_id: string;
+	pattern_hash: string;
+};
+
+export type HistogramBucket = { start: string; count: number; errors: number; warnings: number };
+export type Histogram = { step_seconds: number; total: number; buckets: HistogramBucket[] };
+
+export type TimePreset = '15m' | '1h' | '6h' | '24h' | '7d';
+export const TIME_PRESETS: { value: TimePreset; label: string; minutes: number }[] = [
+	{ value: '15m', label: '15 m', minutes: 15 },
+	{ value: '1h', label: '1 h', minutes: 60 },
+	{ value: '6h', label: '6 h', minutes: 360 },
+	{ value: '24h', label: '24 h', minutes: 1440 },
+	{ value: '7d', label: '7 d', minutes: 10080 }
+];
+export const DEFAULT_PRESET: TimePreset = '1h';
+
+export type FilterOp = 'eq' | 'neq' | 'contains' | 'exists' | 'regex' | 'not_regex' | 'gt' | 'gte' | 'lt' | 'lte';
+export const FILTER_OPS: { value: FilterOp; label: string }[] = [
+	{ value: 'eq', label: '=' },
+	{ value: 'neq', label: '≠' },
+	{ value: 'contains', label: 'contains' },
+	{ value: 'exists', label: 'exists' },
+	{ value: 'regex', label: 'matches regex' },
+	{ value: 'not_regex', label: 'does not match regex' },
+	{ value: 'gt', label: '>' },
+	{ value: 'gte', label: '≥' },
+	{ value: 'lt', label: '<' },
+	{ value: 'lte', label: '≤' }
+];
+export type FieldFilter = { path: string; op: FilterOp; value?: string };
+
+export const MAX_FILTERS = 10;
+export const MAX_COLUMNS = 10;
+export const PAGE_SIZE = 100;
+/** Live rows kept in the page; older ones fall off. */
+export const LIVE_ROW_CAP = 1000;
+
+/** Everything the explorer shows, as it lives in the URL (never a token). */
+export type ExplorerState = {
+	range: TimePreset | 'custom';
+	/** ISO 8601, only for `range: 'custom'`. */
+	from: string | null;
+	to: string | null;
+	q: string;
+	inFields: boolean;
+	levels: LogLevel[];
+	sources: string[];
+	filters: FieldFilter[];
+	columns: string[];
+	live: boolean;
+};
+
+const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
+const SEGMENT = /^[A-Za-z0-9_-]{1,64}$/;
+
+/** The engine's field path rule: 1-5 dot-separated segments of A-Z a-z 0-9 _ -. */
+export function isFieldPath(path: string): boolean {
+	const segments = path.split('.');
+	return segments.length <= 5 && segments.every((segment) => SEGMENT.test(segment));
+}
+
+export function filterNeedsValue(op: FilterOp): boolean {
+	return op !== 'exists';
+}
+
+function isoOrNull(raw: string | null): string | null {
+	if (!raw) return null;
+	const time = Date.parse(raw);
+	return Number.isNaN(time) ? null : new Date(time).toISOString();
+}
+
+function parseFilter(raw: string): FieldFilter | null {
+	try {
+		const value = JSON.parse(raw);
+		if (!value || typeof value.path !== 'string' || !isFieldPath(value.path)) return null;
+		if (!FILTER_OPS.some((op) => op.value === value.op)) return null;
+		if (!filterNeedsValue(value.op)) return { path: value.path, op: value.op };
+		if (typeof value.value !== 'string') return null;
+		return { path: value.path, op: value.op, value: value.value };
+	} catch {
+		return null;
+	}
+}
+
+function unique<T>(items: T[]): T[] {
+	return [...new Set(items)];
+}
+
+/** Reads the explorer URL; anything invalid is dropped rather than failing. */
+export function parseExplorerState(params: URLSearchParams): ExplorerState {
+	const from = isoOrNull(params.get('from'));
+	const to = isoOrNull(params.get('to'));
+	const rawRange = params.get('range');
+	let range: ExplorerState['range'] = DEFAULT_PRESET;
+	if (rawRange === 'custom' && from && to && from < to) range = 'custom';
+	else if (TIME_PRESETS.some((preset) => preset.value === rawRange)) range = rawRange as TimePreset;
+	return {
+		range,
+		from: range === 'custom' ? from : null,
+		to: range === 'custom' ? to : null,
+		q: params.get('q') ?? '',
+		inFields: params.get('in_fields') === '1',
+		levels: unique(params.getAll('level').filter((level): level is LogLevel => LEVELS.includes(level as LogLevel))),
+		sources: unique(params.getAll('source').filter((id) => UUID.test(id))),
+		filters: params
+			.getAll('filter')
+			.map(parseFilter)
+			.filter((filter): filter is FieldFilter => filter !== null)
+			.slice(0, MAX_FILTERS),
+		columns: unique(params.getAll('col').filter(isFieldPath)).slice(0, MAX_COLUMNS),
+		live: params.get('live') === '1'
+	};
+}
+
+/** The canonical query string of a state (defaults left out). */
+export function explorerParams(state: ExplorerState): URLSearchParams {
+	const params = new URLSearchParams();
+	if (state.range === 'custom' && state.from && state.to) {
+		params.set('range', 'custom');
+		params.set('from', state.from);
+		params.set('to', state.to);
+	} else if (state.range !== DEFAULT_PRESET) {
+		params.set('range', state.range);
+	}
+	if (state.q) params.set('q', state.q);
+	if (state.inFields) params.set('in_fields', '1');
+	for (const level of state.levels) params.append('level', level);
+	for (const source of state.sources) params.append('source', source);
+	for (const filter of state.filters) params.append('filter', JSON.stringify(filter));
+	for (const column of state.columns) params.append('col', column);
+	if (state.live) params.set('live', '1');
+	return params;
+}
+
+export function explorerHref(path: string, state: ExplorerState): string {
+	const query = explorerParams(state).toString();
+	return query ? `${path}?${query}` : path;
+}
+
+/** The absolute window: a preset counts back from `now`. */
+export function resolveWindow(state: ExplorerState, now: number): { from: string; to: string } {
+	if (state.range === 'custom' && state.from && state.to) return { from: state.from, to: state.to };
+	const minutes = TIME_PRESETS.find((preset) => preset.value === state.range)?.minutes ?? 60;
+	return { from: new Date(now - minutes * 60_000).toISOString(), to: new Date(now).toISOString() };
+}
+
+/** The filters as nami-engine's query parameters (no window). */
+export function tailParams(state: ExplorerState): URLSearchParams {
+	const params = new URLSearchParams();
+	for (const source of state.sources) params.append('source_id', source);
+	for (const level of state.levels) params.append('level', level);
+	if (state.q) params.set('q', state.q);
+	if (state.q && state.inFields) params.set('in_fields', 'true');
+	for (const filter of state.filters) params.append('filter', JSON.stringify(filter));
+	return params;
+}
+
+/** Search and histogram parameters: the filters plus the window. */
+export function engineParams(state: ExplorerState, window: { from: string; to: string }): URLSearchParams {
+	const params = tailParams(state);
+	params.set('from', window.from);
+	params.set('to', window.to);
+	return params;
+}
+
+/** The value at a dot path inside a row's fields (`undefined` when absent). */
+export function fieldAt(fields: unknown, path: string): unknown {
+	let current: unknown = fields;
+	for (const segment of path.split('.')) {
+		if (current === null || typeof current !== 'object' || Array.isArray(current)) return undefined;
+		current = (current as Record<string, unknown>)[segment];
+	}
+	return current;
+}
+
+/** A field value for a table cell: strings as they are, the rest as JSON. */
+export function formatFieldValue(value: unknown): string {
+	if (value === undefined) return '';
+	if (typeof value === 'string') return value;
+	return JSON.stringify(value);
+}
+
+/** "2026-10-13 14:30:05.123" (UTC, milliseconds). */
+export function formatLogTime(iso: string): string {
+	return new Date(iso).toISOString().slice(0, 23).replace('T', ' ');
+}
+
+/** Badge classes per level; the badge text always names the level. */
+export function levelClasses(level: string): string {
+	switch (level) {
+		case 'trace':
+			return 'bg-neutral-500/10 text-neutral-500';
+		case 'debug':
+			return 'bg-neutral-400/10 text-neutral-400';
+		case 'info':
+			return 'bg-sky-400/10 text-sky-300';
+		case 'notice':
+			return 'bg-teal-400/10 text-teal-300';
+		case 'warn':
+			return 'bg-amber-400/10 text-amber-300';
+		case 'error':
+			return 'bg-red-500/10 text-red-300';
+		case 'critical':
+		case 'alert':
+		case 'emergency':
+		case 'fatal':
+			return 'bg-rose-500/20 font-semibold text-rose-200';
+		default:
+			return 'bg-neutral-500/10 text-neutral-400';
+	}
+}
+
+/** A copy of `list` with `item` added, or removed when present. */
+export function toggled<T>(list: T[], item: T): T[] {
+	return list.includes(item) ? list.filter((other) => other !== item) : [...list, item];
+}
+
+/** `datetime-local` input value (UTC) <-> ISO. */
+export function toInputValue(iso: string | null): string {
+	return iso ? iso.slice(0, 16) : '';
+}
+
+export function fromInputValue(value: string): string | null {
+	return value ? isoOrNull(`${value}:00Z`) : null;
+}
```

- [ ] **Step 4: The "load more" proxy**

`nami-ui/src/routes/(app)/projects/[id]/logs/search/+server.ts`:

```ts
import { error } from '@sveltejs/kit';
import { callEngine, isUuid } from '#lib/server/engine';
import { pickLogParams, SEARCH_PARAMS } from '#lib/server/log-proxy';
import type { RequestHandler } from './$types';

/** The explorer's "Load more": one search page, passed through as JSON. */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id)) throw error(404, 'Not found');
	const query = pickLogParams(event.url.searchParams, SEARCH_PARAMS);
	const response = await callEngine(event, `/projects/${event.params.id}/logs${query}`);
	return new Response(await response.text(), {
		status: response.status,
		headers: { 'content-type': response.headers.get('content-type') ?? 'application/json' }
	});
};
```

- [ ] **Step 5: The volume chart**

`nami-ui/src/lib/components/LogVolumeChart.svelte`:

```svelte
<script lang="ts">
	// Log volume per bucket, stacked by severity: errors (status critical,
	// #d03b3b) at the baseline, then warnings (status warning, #fab219), then
	// everything else in neutral #737373 (3.9:1 on the #141414 card). Status
	// steps from the dataviz reference palette (deutan/tritan ΔE 24 apart);
	// identity is never color alone: legend, tooltip and table name each part.
	// A click on a bar zooms the explorer into that bucket; keyboard users get
	// the same through the range input (arrows to inspect, Enter to zoom),
	// hence the a11y ignore on the chart's click handler.
	import { formatUtc, niceTicks } from '#lib/uptime';
	import type { Histogram, HistogramBucket } from '#lib/logs';

	let {
		histogram,
		onselect
	}: { histogram: Histogram; onselect?: (from: string, to: string) => void } = $props();

	const HEIGHT = 160;
	const PAD = { top: 8, right: 8, bottom: 24, left: 48 };
	const PLOT_HEIGHT = HEIGHT - PAD.top - PAD.bottom;
	const GAP = 2;
	const SERIES = [
		{ key: 'errors', label: 'Errors', fill: 'fill-[#d03b3b]', swatch: 'bg-[#d03b3b]' },
		{ key: 'warnings', label: 'Warnings', fill: 'fill-[#fab219]', swatch: 'bg-[#fab219]' },
		{ key: 'other', label: 'Other', fill: 'fill-[#737373]', swatch: 'bg-[#737373]' }
	] as const;

	let width = $state(720);
	let hoverIndex = $state<number | null>(null);
	let focusIndex = $state<number | null>(null);

	const buckets = $derived(histogram.buckets);
	const plotWidth = $derived(Math.max(width - PAD.left - PAD.right, 1));
	const band = $derived(plotWidth / Math.max(buckets.length, 1));
	const barWidth = $derived(Math.max(1, Math.min(24, band - GAP)));
	const ticks = $derived(niceTicks(Math.max(0, ...buckets.map((b) => b.count)), 3));
	const yMax = $derived(Math.max(ticks[ticks.length - 1], 1));
	const active = $derived(hoverIndex ?? focusIndex);
	const stepMs = $derived(histogram.step_seconds * 1000);
	const labelEvery = $derived(Math.max(1, Math.ceil(buckets.length / (plotWidth < 420 ? 3 : 6))));

	function parts(bucket: HistogramBucket): Record<(typeof SERIES)[number]['key'], number> {
		return {
			errors: bucket.errors,
			warnings: bucket.warnings,
			other: Math.max(0, bucket.count - bucket.errors - bucket.warnings)
		};
	}

	function yAt(value: number): number {
		return PAD.top + PLOT_HEIGHT - (value / yMax) * PLOT_HEIGHT;
	}

	/** Stacked segments, bottom up, with a surface gap between them. */
	function segments(bucket: HistogramBucket, i: number) {
		const x = PAD.left + i * band + (band - barWidth) / 2;
		const values = parts(bucket);
		const present = SERIES.filter((series) => values[series.key] > 0);
		let base = PAD.top + PLOT_HEIGHT;
		return present.map((series, k) => {
			const full = (values[series.key] / yMax) * PLOT_HEIGHT;
			const gap = k < present.length - 1 ? Math.min(GAP, full / 2) : 0;
			const height = Math.max(full - gap, 1);
			const top = base - height;
			const segment = { key: series.key, fill: series.fill, x, y: top, height, last: k === present.length - 1 };
			base = top - gap;
			return segment;
		});
	}

	/** A rect with only its top corners rounded (data end), square at the base. */
	function roundedTop(x: number, y: number, w: number, h: number): string {
		const r = Math.min(4, w / 2, h);
		return `M${x},${y + h} V${y + r} Q${x},${y} ${x + r},${y} H${x + w - r} Q${x + w},${y} ${x + w},${y + r} V${y + h} Z`;
	}

	function label(iso: string): string {
		const text = new Date(iso).toISOString();
		const span = buckets.length * stepMs;
		if (histogram.step_seconds >= 86_400) return text.slice(5, 10);
		return span > 86_400_000 ? `${text.slice(5, 10)} ${text.slice(11, 16)}` : text.slice(11, 16);
	}

	function bucketEnd(bucket: HistogramBucket): string {
		return new Date(Date.parse(bucket.start) + stepMs).toISOString();
	}

	function describe(i: number): string {
		const bucket = buckets[i];
		if (!bucket) return '';
		const p = parts(bucket);
		return `${formatUtc(bucket.start)}: ${bucket.count} logs (${p.errors} errors, ${p.warnings} warnings, ${p.other} other)`;
	}

	function onPointerMove(event: PointerEvent) {
		const box = (event.currentTarget as SVGSVGElement).getBoundingClientRect();
		const index = Math.floor((event.clientX - box.left - PAD.left) / band);
		hoverIndex = index >= 0 && index < buckets.length ? index : null;
	}

	function select(i: number | null) {
		if (i === null || !onselect || !buckets[i]) return;
		onselect(new Date(buckets[i].start).toISOString(), bucketEnd(buckets[i]));
	}
</script>

<div class="space-y-2">
	<div class="flex flex-wrap items-center gap-4 text-xs text-neutral-400">
		{#each SERIES as series (series.key)}
			<span class="flex items-center gap-1.5"><span class="h-2.5 w-2.5 rounded-sm {series.swatch}"></span>{series.label}</span>
		{/each}
	</div>
	<div class="relative" bind:clientWidth={width}>
		<label class="sr-only" for="log-volume-inspector">Inspect log volume by time bucket</label>
		<input
			id="log-volume-inspector"
			class="peer sr-only"
			type="range"
			min="0"
			max={Math.max(buckets.length - 1, 0)}
			step="1"
			value={focusIndex ?? Math.max(buckets.length - 1, 0)}
			aria-valuetext={describe(focusIndex ?? buckets.length - 1)}
			oninput={(event) => (focusIndex = Number(event.currentTarget.value))}
			onfocus={(event) => (focusIndex = Number(event.currentTarget.value))}
			onblur={() => (focusIndex = null)}
			onkeydown={(event) => {
				if (event.key === 'Enter') select(focusIndex);
			}}
		/>
		<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
		<svg
			{width}
			height={HEIGHT}
			role="img"
			aria-label="Log volume over the selected time range, stacked by errors, warnings and other levels"
			class="block cursor-pointer rounded-md peer-focus-visible:ring-2 peer-focus-visible:ring-teal-500/50"
			onpointermove={onPointerMove}
			onpointerleave={() => (hoverIndex = null)}
			onclick={() => select(hoverIndex)}
		>
			{#each ticks as tick (tick)}
				<line x1={PAD.left} x2={PAD.left + plotWidth} y1={yAt(tick)} y2={yAt(tick)} class="stroke-[#262626]" stroke-width="1" />
				<text x={PAD.left - 8} y={yAt(tick)} dy="0.32em" text-anchor="end" class="fill-neutral-500 text-[11px] tabular-nums"
					>{tick}</text
				>
			{/each}
			{#each buckets as bucket, i (bucket.start)}
				{#if active === i}
					<rect x={PAD.left + i * band} y={PAD.top} width={band} height={PLOT_HEIGHT} class="fill-neutral-800/60" />
				{/if}
				{#each segments(bucket, i) as segment (segment.key)}
					{#if segment.last}
						<path d={roundedTop(segment.x, segment.y, barWidth, segment.height)} class={segment.fill} />
					{:else}
						<rect x={segment.x} y={segment.y} width={barWidth} height={segment.height} class={segment.fill} />
					{/if}
				{/each}
				{#if i % labelEvery === 0}
					<text x={PAD.left + i * band + band / 2} y={HEIGHT - 8} text-anchor="middle" class="fill-neutral-500 text-[11px] tabular-nums"
						>{label(bucket.start)}</text
					>
				{/if}
			{/each}
		</svg>

		{#if histogram.total === 0}
			<p class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-neutral-500">
				No logs in this time range.
			</p>
		{/if}

		{#if active !== null && buckets[active]}
			{@const bucket = buckets[active]}
			{@const p = parts(bucket)}
			<div
				class="pointer-events-none absolute top-2 w-48 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-xs shadow-lg"
				style:left="{Math.max(0, Math.min(PAD.left + active * band + band + 8, width - 200))}px"
			>
				<div class="text-sm font-semibold text-neutral-100 tabular-nums">{bucket.count.toLocaleString('en-US')} logs</div>
				<div class="text-neutral-400 tabular-nums">{p.errors} errors · {p.warnings} warnings · {p.other} other</div>
				<div class="text-neutral-500">{formatUtc(bucket.start)}</div>
				{#if onselect}<div class="text-neutral-600">Click to zoom in</div>{/if}
			</div>
		{/if}
	</div>
	<details class="text-sm">
		<summary class="cursor-pointer text-neutral-500 hover:text-neutral-300">Show data table</summary>
		<div class="mt-2 max-h-64 overflow-y-auto">
			<table class="w-full text-left text-xs">
				<thead class="text-neutral-500">
					<tr>
						<th class="py-1 font-medium">Bucket start (UTC)</th>
						<th class="py-1 font-medium">Logs</th>
						<th class="py-1 font-medium">Errors</th>
						<th class="py-1 font-medium">Warnings</th>
					</tr>
				</thead>
				<tbody class="text-neutral-300 tabular-nums">
					{#each buckets as bucket (bucket.start)}
						<tr class="border-t border-[#262626]">
							<td class="py-1">{formatUtc(bucket.start)}</td>
							<td class="py-1">{bucket.count}</td>
							<td class="py-1">{bucket.errors}</td>
							<td class="py-1">{bucket.warnings}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	</details>
</div>
```

- [ ] **Step 6: The explorer page**

`nami-ui/src/routes/(app)/projects/[id]/logs/+page.server.ts` (replaces Task 7's redirect):

```ts
import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
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
 */
export const load: PageServerLoad = async (event) => {
	const state = parseExplorerState(event.url.searchParams);
	const window = resolveWindow(state, Date.now());
	const query = engineParams(state, window).toString();
	const base = `/projects/${event.params.id}`;
	const [searchResponse, histogramResponse, sourcesResponse] = await Promise.all([
		callEngine(event, `${base}/logs?${query}&limit=${PAGE_SIZE}`),
		callEngine(event, `${base}/logs/histogram?${query}`),
		callEngine(event, `${base}/sources`)
	]);
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
```

`nami-ui/src/routes/(app)/projects/[id]/logs/+page.svelte`:

```svelte
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { SvelteSet } from 'svelte/reactivity';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import LogVolumeChart from '#lib/components/LogVolumeChart.svelte';
	import {
		engineParams,
		explorerHref,
		fieldAt,
		FILTER_OPS,
		filterNeedsValue,
		formatFieldValue,
		formatLogTime,
		fromInputValue,
		isFieldPath,
		LEVELS,
		levelClasses,
		LIVE_ROW_CAP,
		MAX_COLUMNS,
		MAX_FILTERS,
		PAGE_SIZE,
		tailParams,
		TIME_PRESETS,
		toggled,
		toInputValue,
		type ExplorerState,
		type FieldFilter,
		type FilterOp,
		type LogEntry
	} from '#lib/logs';
	import { formatUtc } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const path = $derived(page.url.pathname);
	const sourceNames = $derived(new Map(data.sources.map((source) => [source.id, source.name])));
	const opLabels = new Map(FILTER_OPS.map((op) => [op.value, op.label]));
	const MAX_WINDOW_MS = 31 * 24 * 3600 * 1000;

	/** Every change is a navigation: the URL is the explorer's state. */
	function go(change: Partial<ExplorerState>) {
		void goto(explorerHref(path, { ...data.state, ...change }), { reset: false });
	}

	// Results: the loaded page, extended by "Load more"; both reset on navigation.
	let rows = $derived(data.logs);
	let nextCursor = $derived(data.nextCursor);
	let loadingMore = $state(false);
	let loadError = $state<string | null>(null);
	const expanded = new SvelteSet<string>();

	async function loadMore() {
		if (!nextCursor || loadingMore) return;
		loadingMore = true;
		loadError = null;
		const params = engineParams(data.state, data.window);
		params.set('cursor', nextCursor);
		params.set('limit', String(PAGE_SIZE));
		try {
			const response = await fetch(`${path}/search?${params}`);
			const body = await response.json().catch(() => null);
			if (response.ok && body) {
				rows = [...rows, ...body.logs];
				nextCursor = body.next_cursor;
			} else {
				loadError = body?.error ?? 'Could not load more logs.';
			}
		} catch {
			loadError = 'Could not load more logs.';
		} finally {
			loadingMore = false;
		}
	}

	// Live tail: Server-Sent Events through the BFF while `live` is on.
	let liveRows = $state<LogEntry[]>([]);
	let liveStatus = $state<'off' | 'connecting' | 'live' | 'ended'>('off');
	$effect(() => {
		if (!data.state.live) {
			liveStatus = 'off';
			return;
		}
		const query = tailParams(data.state).toString();
		const stream = new EventSource(`${path}/tail${query ? `?${query}` : ''}`);
		liveRows = [];
		liveStatus = 'connecting';
		stream.onopen = () => (liveStatus = 'live');
		stream.addEventListener('log', (event) => {
			const row: LogEntry = JSON.parse((event as MessageEvent).data);
			liveRows = [row, ...liveRows].slice(0, LIVE_ROW_CAP);
		});
		// After 30 minutes the engine ends the stream and EventSource
		// reconnects by itself; an invalid session must not reconnect.
		stream.addEventListener('end', (event) => {
			if (JSON.parse((event as MessageEvent).data).reason === 'session') {
				stream.close();
				liveStatus = 'ended';
			}
		});
		stream.onerror = () => {
			if (stream.readyState === EventSource.CLOSED) liveStatus = 'ended';
		};
		return () => stream.close();
	});

	const shown = $derived.by(() => {
		const seen = new Set<string>();
		return [...liveRows, ...rows].filter((row) => {
			if (seen.has(row.id)) return false;
			seen.add(row.id);
			return true;
		});
	});

	// Toolbar forms
	let showCustom = $state(false);
	let rangeError = $state<string | null>(null);
	let filterOp = $state<FilterOp>('eq');
	let filterError = $state<string | null>(null);
	let columnError = $state<string | null>(null);

	function search(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		go({ q: String(form.get('q') ?? '').trim(), inFields: form.get('in_fields') === 'on' });
	}

	function applyCustom(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const from = fromInputValue(String(form.get('from') ?? ''));
		const to = fromInputValue(String(form.get('to') ?? ''));
		if (!from || !to || from >= to) {
			rangeError = 'Pick a start before the end.';
			return;
		}
		if (Date.parse(to) - Date.parse(from) > MAX_WINDOW_MS) {
			rangeError = 'The time range may span at most 31 days.';
			return;
		}
		rangeError = null;
		go({ range: 'custom', from, to });
	}

	function addFilter(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const filterPath = String(form.get('path') ?? '').trim();
		const value = String(form.get('value') ?? '');
		if (!isFieldPath(filterPath)) {
			filterError = 'A field path is 1-5 dot-separated names of letters, digits, _ or -.';
			return;
		}
		if (data.state.filters.length >= MAX_FILTERS) {
			filterError = `At most ${MAX_FILTERS} filters.`;
			return;
		}
		if (filterNeedsValue(filterOp) && value === '') {
			filterError = 'Enter a value to compare with.';
			return;
		}
		filterError = null;
		const filter: FieldFilter = filterNeedsValue(filterOp)
			? { path: filterPath, op: filterOp, value }
			: { path: filterPath, op: filterOp };
		go({ filters: [...data.state.filters, filter] });
	}

	function addColumn(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const column = String(form.get('column') ?? '').trim();
		if (!isFieldPath(column)) {
			columnError = 'A field path is 1-5 dot-separated names of letters, digits, _ or -.';
			return;
		}
		if (data.state.columns.includes(column)) {
			columnError = 'That column is already shown.';
			return;
		}
		if (data.state.columns.length >= MAX_COLUMNS) {
			columnError = `At most ${MAX_COLUMNS} columns.`;
			return;
		}
		columnError = null;
		go({ columns: [...data.state.columns, column] });
	}

	function toggleRow(id: string) {
		if (expanded.has(id)) expanded.delete(id);
		else expanded.add(id);
	}

	const input =
		'rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-1.5 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
	const chip = (on: boolean) =>
		`rounded-md px-2.5 py-1 text-xs transition-colors ${on ? 'bg-[#1f1f1f] text-neutral-100' : 'text-neutral-500 hover:text-neutral-300'}`;
</script>

<div class="space-y-4">
	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-4">
		{#key `${data.state.q}|${data.state.inFields}`}
			<form class="flex flex-wrap items-center gap-2" onsubmit={search}>
				<label class="sr-only" for="log-q">Search</label>
				<input
					id="log-q"
					name="q"
					type="search"
					maxlength="1024"
					placeholder="Search messages (case-insensitive)"
					defaultValue={data.state.q}
					class="{input} min-w-64 flex-1"
				/>
				<label class="flex items-center gap-2 text-sm text-neutral-400">
					<input type="checkbox" name="in_fields" defaultChecked={data.state.inFields} /> Also search fields
				</label>
				<button type="submit" class="rounded-md bg-teal-500 px-4 py-1.5 text-sm font-medium text-neutral-950 hover:bg-teal-400"
					>Search</button
				>
			</form>
		{/key}

		<div class="flex flex-wrap items-center justify-between gap-3">
			<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Time range">
				{#each TIME_PRESETS as preset (preset.value)}
					<button
						type="button"
						aria-pressed={data.state.range === preset.value}
						onclick={() => go({ range: preset.value, from: null, to: null })}
						class={chip(data.state.range === preset.value)}>{preset.label}</button
					>
				{/each}
				<button
					type="button"
					aria-pressed={data.state.range === 'custom'}
					onclick={() => (showCustom = !showCustom)}
					class={chip(data.state.range === 'custom')}>Custom</button
				>
			</div>
			<div class="flex flex-wrap items-center gap-2">
				<details class="relative">
					<summary class="{button} cursor-pointer list-none">
						{data.state.sources.length === 0
							? 'All sources'
							: data.state.sources.length === 1
								? (sourceNames.get(data.state.sources[0]) ?? '1 source')
								: `${data.state.sources.length} sources`}
					</summary>
					<div class="absolute right-0 z-10 mt-1 w-64 space-y-1 rounded-md border border-[#262626] bg-[#141414] p-2 shadow-lg">
						{#each data.sources as source (source.id)}
							<label class="flex items-center gap-2 rounded px-2 py-1 text-sm text-neutral-300 hover:bg-[#1f1f1f]">
								<input
									type="checkbox"
									checked={data.state.sources.includes(source.id)}
									onchange={() => go({ sources: toggled(data.state.sources, source.id) })}
								/>
								{source.name}
							</label>
						{:else}
							<p class="px-2 py-1 text-sm text-neutral-500">
								No sources yet — <a href="{path}/sources" class="text-teal-300">create one</a>.
							</p>
						{/each}
					</div>
				</details>
				<button
					type="button"
					aria-pressed={data.state.live}
					onclick={() => go({ live: !data.state.live })}
					class="{button} {data.state.live ? 'border-teal-500/40 text-teal-300' : ''}"
				>
					{data.state.live ? 'Stop live tail' : 'Live tail'}
				</button>
			</div>
		</div>

		{#if showCustom || data.state.range === 'custom'}
			<form class="flex flex-wrap items-center gap-2 text-sm text-neutral-400" onsubmit={applyCustom}>
				<label for="log-from">From</label>
				<input id="log-from" name="from" type="datetime-local" defaultValue={toInputValue(data.window.from)} class={input} />
				<label for="log-to">to</label>
				<input id="log-to" name="to" type="datetime-local" defaultValue={toInputValue(data.window.to)} class={input} />
				<span class="text-xs text-neutral-600">UTC</span>
				<button type="submit" class={button}>Apply</button>
				{#if rangeError}<span class="text-red-400" role="alert">{rangeError}</span>{/if}
			</form>
		{/if}

		<div class="flex flex-wrap items-center gap-1" role="group" aria-label="Levels">
			{#each LEVELS as level (level)}
				{@const on = data.state.levels.includes(level)}
				<button
					type="button"
					aria-pressed={on}
					onclick={() => go({ levels: toggled(data.state.levels, level) })}
					class="rounded-md px-2 py-0.5 text-xs transition-colors {on
						? `${levelClasses(level)} ring-1 ring-current`
						: 'text-neutral-500 hover:text-neutral-300'}">{level}</button
				>
			{/each}
		</div>

		<div class="flex flex-wrap items-center gap-2">
			{#each data.state.filters as filter, index (index)}
				<span class="flex items-center gap-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-2 py-0.5 text-xs text-neutral-300">
					<code>{filter.path}</code>
					<span class="text-neutral-500">{opLabels.get(filter.op)}</span>
					{#if filter.value !== undefined}<code class="text-teal-300">{filter.value}</code>{/if}
					<button
						type="button"
						aria-label="Remove filter on {filter.path}"
						onclick={() => go({ filters: data.state.filters.filter((_, other) => other !== index) })}
						class="text-neutral-500 hover:text-red-400">×</button
					>
				</span>
			{/each}
			<form class="flex flex-wrap items-center gap-1" onsubmit={addFilter}>
				<label class="sr-only" for="filter-path">Field path</label>
				<input id="filter-path" name="path" placeholder="field.path" maxlength="324" class="{input} w-36 py-1 text-xs" />
				<label class="sr-only" for="filter-op">Operator</label>
				<select id="filter-op" name="op" bind:value={filterOp} class="{input} py-1 text-xs">
					{#each FILTER_OPS as op (op.value)}
						<option value={op.value}>{op.label}</option>
					{/each}
				</select>
				{#if filterNeedsValue(filterOp)}
					<label class="sr-only" for="filter-value">Value</label>
					<input id="filter-value" name="value" placeholder="value" maxlength="1024" class="{input} w-36 py-1 text-xs" />
				{/if}
				<button type="submit" class="{button} py-1 text-xs">Add filter</button>
			</form>
			{#if filterError}<span class="text-xs text-red-400" role="alert">{filterError}</span>{/if}
		</div>
	</div>

	{#if data.searchError}
		<div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400" role="alert">
			{data.searchError}
		</div>
	{/if}

	{#if data.histogram}
		<div class="rounded-xl border border-[#262626] bg-[#141414] p-4">
			<div class="mb-2 flex flex-wrap items-baseline justify-between gap-2">
				<h2 class="text-sm font-semibold text-neutral-200">
					{data.histogram.total.toLocaleString('en-US')} logs
				</h2>
				<p class="text-xs text-neutral-500">{formatUtc(data.window.from)} – {formatUtc(data.window.to)}</p>
			</div>
			<LogVolumeChart histogram={data.histogram} onselect={(from, to) => go({ range: 'custom', from, to })} />
		</div>
	{/if}

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="flex flex-wrap items-center justify-between gap-2 border-b border-[#262626] px-4 py-3">
			<div class="flex items-center gap-2 text-sm text-neutral-400">
				{#if liveStatus === 'live'}
					<span class="h-2 w-2 animate-pulse rounded-full bg-teal-400"></span><span class="text-teal-300">Live</span>
				{:else if liveStatus === 'connecting'}
					<span class="text-neutral-500">Connecting live tail…</span>
				{:else if liveStatus === 'ended'}
					<span class="text-amber-300">Live tail stopped (session ended) — reload to resume.</span>
				{/if}
			</div>
			<form class="flex flex-wrap items-center gap-1" onsubmit={addColumn}>
				<label class="sr-only" for="column-path">Add a column</label>
				<input id="column-path" name="column" placeholder="Add column: field.path" maxlength="324" class="{input} w-52 py-1 text-xs" />
				<button type="submit" class="{button} py-1 text-xs">Add column</button>
				{#if columnError}<span class="text-xs text-red-400" role="alert">{columnError}</span>{/if}
			</form>
		</div>
		{#if shown.length === 0}
			<p class="px-4 py-10 text-center text-sm text-neutral-500">
				{#if data.sources.length === 0}
					No sources yet — <a href="{path}/sources" class="text-teal-300">create one</a> to start sending logs.
				{:else}
					No logs match in this time range.
				{/if}
			</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full table-fixed text-left text-sm">
					<thead class="text-xs text-neutral-500">
						<tr class="border-b border-[#262626]">
							<th class="w-48 px-4 py-2 font-medium">Time (UTC)</th>
							<th class="w-24 px-2 py-2 font-medium">Level</th>
							<th class="w-36 px-2 py-2 font-medium">Source</th>
							<th class="min-w-64 px-2 py-2 font-medium">Message</th>
							{#each data.state.columns as column (column)}
								<th class="w-40 px-2 py-2 font-medium">
									<span class="flex items-center gap-1">
										<code class="truncate">{column}</code>
										<button
											type="button"
											aria-label="Remove column {column}"
											onclick={() => go({ columns: data.state.columns.filter((other) => other !== column) })}
											class="text-neutral-500 hover:text-red-400">×</button
										>
									</span>
								</th>
							{/each}
						</tr>
					</thead>
					<tbody>
						{#each shown as row (row.id)}
							{@const open = expanded.has(row.id)}
							<tr class="border-b border-[#262626] align-top hover:bg-[#181818]">
								<td class="px-4 py-1.5 font-mono text-xs whitespace-nowrap text-neutral-400">{formatLogTime(row.dt)}</td>
								<td class="px-2 py-1.5">
									<span class="rounded px-1.5 py-0.5 text-xs {levelClasses(row.level)}">{row.level}</span>
								</td>
								<td class="truncate px-2 py-1.5 text-xs text-neutral-400">{sourceNames.get(row.source_id) ?? 'deleted source'}</td>
								<td class="px-2 py-1.5">
									<button
										type="button"
										aria-expanded={open}
										onclick={() => toggleRow(row.id)}
										class="block w-full truncate text-left font-mono text-xs text-neutral-200 hover:text-white"
										>{row.message || '(no message)'}</button
									>
								</td>
								{#each data.state.columns as column (column)}
									<td class="truncate px-2 py-1.5 font-mono text-xs text-neutral-400">{formatFieldValue(fieldAt(row.fields, column))}</td>
								{/each}
							</tr>
							{#if open}
								<tr class="border-b border-[#262626] bg-[#0f0f0f]">
									<td colspan={4 + data.state.columns.length} class="space-y-3 px-4 py-3">
										<pre class="font-mono text-xs break-all whitespace-pre-wrap text-neutral-200">{row.message}</pre>
										<pre
											class="max-h-96 overflow-auto rounded-md border border-[#262626] bg-[#0a0a0a] p-3 font-mono text-xs text-neutral-300">{JSON.stringify(row.fields, null, 2)}</pre>
										<dl class="grid gap-x-6 gap-y-1 text-xs text-neutral-500 sm:grid-cols-2">
											<div>id <code class="text-neutral-400">{row.id}</code></div>
											<div>pattern <code class="text-neutral-400">{row.pattern_hash}</code></div>
											{#if row.trace_id}<div>trace <code class="text-neutral-400">{row.trace_id}</code></div>{/if}
											{#if row.span_id}<div>span <code class="text-neutral-400">{row.span_id}</code></div>{/if}
										</dl>
										<CopyButton value={JSON.stringify(row, null, 2)} label="Copy JSON" />
									</td>
								</tr>
							{/if}
						{/each}
					</tbody>
				</table>
			</div>
			<div class="flex items-center justify-center gap-3 px-4 py-3">
				{#if nextCursor}
					<button type="button" onclick={loadMore} disabled={loadingMore} class={button}>
						{loadingMore ? 'Loading…' : 'Load more'}
					</button>
				{:else}
					<span class="text-xs text-neutral-600">End of results in this time range.</span>
				{/if}
				{#if loadError}<span class="text-xs text-red-400" role="alert">{loadError}</span>{/if}
			</div>
		{/if}
	</div>
</div>
```

The Explorer sub-tab:

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte b/nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/logs/+layout.svelte
@@ -5,13 +5,16 @@
 	let { data, children }: LayoutProps = $props();
 
 	const base = $derived(`/projects/${data.project.id}/logs`);
-	const sections = $derived([{ href: `${base}/sources`, label: 'Sources' }]);
+	const sections = $derived([
+		{ href: base, label: 'Explorer', exact: true },
+		{ href: `${base}/sources`, label: 'Sources', exact: false }
+	]);
 </script>
 
 <div class="space-y-6">
 	<nav class="flex gap-2" aria-label="Logs sections">
 		{#each sections as section (section.href)}
-			{@const active = page.url.pathname.startsWith(section.href)}
+			{@const active = section.exact ? page.url.pathname === section.href : page.url.pathname.startsWith(section.href)}
 			<a
 				href={section.href}
 				aria-current={active ? 'page' : undefined}
```

- [ ] **Step 7: Check and test**

Run: `cd nami-ui && npm run check && npx vitest run`
Expected: `svelte-check found 0 errors and 0 warnings`; vitest 88 passed on the verified base.

- [ ] **Step 8: Walk through it in the browser**

With a source receiving logs (e.g. 300 events with mixed levels over the last hour via the ingest endpoint): Logs → Explorer shows the total, the stacked histogram (errors, warnings, other; hover shows a bucket's counts) and the newest 100 rows. Click the `error` chip → the URL gains `level=error` and only error rows remain; search `orders` → `q=orders`; add the filter `status ≥ 502` → a chip appears and the count drops; add the column `user.tier` → a column with values; expand a row → pretty-printed fields and "Copy JSON"; "Load more" → 200 rows; click a histogram bar → `range=custom` zoomed to that bucket. Copy the URL into a fresh tab → the same view. Turn on Live tail → "Live"; ingest one event and one with `dt` 20 s in the past → both appear at the top within ~2 s. A filter regex RE2 rejects (`(?x)5 0`) shows "a filter regex is not valid RE2 syntax" on the page instead of an error page; a custom range over 31 days is refused before navigating.

- [ ] **Step 9: Commit**

```bash
git add nami-ui/src/lib/logs.ts nami-ui/src/lib/logs.test.ts nami-ui/src/lib/components/LogVolumeChart.svelte \
  "nami-ui/src/routes/(app)/projects/[id]/logs"
git commit -m "feat(ui): log explorer with histogram, filters, columns and live tail (NAMI-LOG-8)

Search text (optionally in fields), level chips, a source picker, time
presets 15m-7d and custom ranges, field filters, a stacked volume
histogram that zooms on click, results with level colors and expandable
fields, picked columns, load more, and live tail. All state lives in the
URL, so a copied link reopens the same view.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
