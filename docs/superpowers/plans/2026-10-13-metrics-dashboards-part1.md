# Metrics + Dashboards — Part 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Nami the core of BetterStack metrics and dashboards. That means metric ingestion with source tokens (JSON and Prometheus text exposition), ClickHouse storage with per-source retention, a cardinality guard, a query API (avg/min/max/sum/count/p50/p95/p99/rate over time buckets, tag filters, group-by), discovery endpoints for the editor, built-in metrics from uptime checks and log volume, dashboards with line/bar/stat/markdown panels, a flow layout, a time range and tag-value variables, and the nami-ui explorer, dashboard view and dashboard editor on the shared design system.

**Architecture:** Metrics reuse the Logs ingest stack. The same source token is accepted on two new public-listener endpoints. They share the listener's places, Postgres query slots and decode places with log ingest (`IngestLimits`), and keep their own per-source rate limiter and in-flight counters (`MetricsState`). A body is parsed and validated on a blocking thread into `MetricRow`s. Bad samples are rejected one by one and counted, never the whole request. A per-instance `SeriesGuard` admits new series against a per-source rate (10,000 new series per minute → 429) and a per-project cap of active series. It is seeded from a ClickHouse `metric_series` table, so a restart does not make every series look new. Samples go to a `metrics` MergeTree table, ordered by `(project_id, name, series_id, dt)`, with a per-row TTL. Queries are built only with `logs::query::Sql` (fixed fragments, numbered placeholders). Tag keys, tag values, regexes and variable values are always bound parameters, and the project id is always bound from the path. Every query runs under a fixed ClickHouse budget and holds a per-user slot and an instance-wide slot. Built-in metrics are query targets resolved over `uptime_checks` and `logs`, with monitor and source names resolved in Postgres. Dashboards, panels and variables live in Postgres (migration 0120) and are validated by the same query validator that the query API uses. Variables are substituted into filter values only, before validation. Panels are run by id through `POST .../panels/{panel_id}/query`, the shape Part 2's share links reuse. nami-ui renders charts with an in-house SVG `TimeSeriesChart` (line/bar) and the design system's `StatCard`/`Legend`, and loads panels through a 4-wide fetch queue.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, `regex` 1.13, `sha2` 0.11, `flate2`), ClickHouse 26.9 over HTTP (`clickhouse.rs`), Postgres 17, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest. **No new crate and no new npm package.**

**Spec:** `docs/superpowers/specs/2026-10-13-metrics-dashboards-design.md`, including "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §6 (the definition of done).

**When to start:** the engine tasks (T1-T9) build on `main` as of `cbc83c4` and need nothing unmerged. The UI tasks (T10-T13) need the design system branch `refactor/ui-design-system` merged to `main` first. On 2026-10-19 that branch exists only as a local worktree (`.worktrees/UI-design-system`, 7 commits ahead of `origin/main` and 3 behind; it is not on `origin`). T10 also takes two small UI modules from other plans, and creates them itself if they are not merged yet (see the table below).

### Interfaces assumed from unmerged work

If the merged code differs in a name, adapt minimally (same behaviour, same tests) and say so in the PR. If it differs in behaviour, stop and ask the controller.

| From | Interface | Used by |
|---|---|---|
| `refactor/ui-design-system` (local branch, `d88d952`) | `#lib/components/ui`: `Card {title, subtitle, tabs, actions, menu, children, padded}`, `PageHeader {title, subtitle, range, actions}`, `SegmentedTabs {items: {href,label,active}[], label, size}`, `StatCard {label, value, hint, spark, sparkTone, sparkLabel, valueTone}`, `Sparkline`, `Legend {items: {label, tone}[]}`, `Badge`, `Button`, `Input`, `Select`, `EmptyState`, `Icon`; `tones.ts` `Tone`, `TONE_VAR`, `TONE_TEXT`; tokens in `src/routes/layout.css`; the CLAUDE.md "UI conventions" section (no raw hex, no stock Tailwind palette, primitives `.input`, `.select`, `.btn`, `.table`, `.alert-*`) | T10-T13 |
| NAMI-LOG-16 (Logs Part 2) | `#lib/server/json-relay.ts`: `MAX_RELAY_BYTES = 1024 * 1024`, `relayJsonPost(event, path): Promise<Response>` (same-origin JSON POST → engine path, answered with the engine's status and JSON). **If not merged when T10 starts, T10 creates it with exactly that contract and the LOG-16 tests**, and whichever of the two lands second rebases onto the other. | T10, T12, T13 |
| NAMI-INC-12 (Incidents Part 1) | `#lib/markdown.ts`: `renderMarkdown(source: string): string`, which HTML-escapes first and then applies a fixed subset (headings, emphasis, inline code, fenced code, lists, paragraphs, `http(s)` links with `rel="noopener noreferrer nofollow"`). **If not merged when T10 starts, T10 creates it with that contract and the INC-12 tests.** T10 then extends it with `mailto:` links and pins that images never render (R14). | T10, T12 |
| NAMI-INC-7 (Incidents Part 1) | `GET /projects/{id}` returns `role: "admin" \| "member" \| "viewer"`, and the project layout passes it on as `data.role`. **If not merged**, T12 adds the field to the project layout load the same way (the engine already knows the role through `require_project_reader`; the layout calls a new `GET /projects/{id}/role`, viewer, `{"role": ...}`), and INC-7 later replaces it. | T12, T13 |

Merged on `main` (`cbc83c4`), checked by this plan: `clickhouse::{ClickHouse::{query_rows_with, insert_rows_with, insert_encoded_with}, encode_rows_within, MIGRATIONS, RESERVED_SETTINGS, MAX_RESPONSE_BYTES}`; `logs::query::Sql::{new, push, bind, placeholder, value, list, text, params}`; `logs::search::{READ_SETTINGS, user_error}`; `logs::slots::{UserSlots::acquire, UserPermit}`; `logs::rate_limit::{TokenBuckets::{new, try_acquire}, InFlight::{new, try_enter, limit}, InFlightGuard, DecodeShares::acquire, REQUESTS_PER_SECOND = 50.0, BURST = 100.0, MAX_IN_FLIGHT_PER_SOURCE = 4, MAX_IN_FLIGHT_PER_PROJECT = 8}`; `logs::ingest::{bearer_token, read_body, gunzip, BodyPace, IngestError, MAX_BODY_BYTES = 10 MiB}`; `logs::normalize::parse_timestamp`; `logs::store::INSERT_SETTINGS`; `routes::logs_ingest::IngestLimits {places, queries, decodes, body_pace}`; `models::source::{find_by_token, is_well_formed_token, IngestSource {id, project_id, retention_days, daily_byte_quota}}`; `models::validate_label`; `middleware::rbac::{CurrentUser, require_project_role, require_project_reader, ProjectReader}`; `AppState::for_test`; tests' `common::{app, public_app, call, seed_project, user_with_role, state_with_clickhouse, logs::{seed_source, post_json, eventually}}`; nami-ui `callEngine`, `enginePath`, `engineFailure`, `isUuid`, `[id=uuid]` matcher, `niceTicks`, `formatUtc`.

**Part 2 follows** (a separate plan, NAMI-MET-14 onward, Postgres migrations from 0121). It covers:
- **Prometheus remote-write** ingest (`POST /api/v1/metrics/prometheus/remote_write`, snappy + protobuf `WriteRequest`, decompression-bomb guard, fuzz cases). It needs two crates (`snap`, `prost`) and the collector bundle is its main client, so it moves out of Part 1 (R1).
- **Scrape targets** (`metric_scrape_targets`, migration 0121): the worker claims due targets with the monitors' atomic claim, GETs through a client built on the monitors' `GuardedResolver` (no proxy, link-local/metadata refusal at DNS time, no redirects, 10 MiB body cap through the capped reader, timeout below the interval). The body goes through T2's parser and T4's validation and guard, with at most 50,000 samples per scrape. Health (`last_scrape_at`, `last_duration_ms`, `last_samples`, `last_error`, `up/down/unknown`), the synthetic `up`, `scrape_duration_seconds`, `scrape_samples_scraped` series, and bearer/basic credentials sealed with `crypto::Cipher` under the per-row context `metric_scrape_targets.auth:{id}`, write-only. Part 2 also decides the operator outbound policy hook the spec defers to Platform.
- **Template library** (bundled JSON: Uptime overview, node_exporter, postgres_exporter, redis_exporter, nginx-prometheus-exporter, cAdvisor), created through T8's `create_with_panels`. A test instantiates each template and runs every panel against fixture series of that exporter.
- **Public share links** (`dashboard_shares`, one active per dashboard, 32-byte token stored as SHA-256, revoke = delete). Engine public routes return the definition and run panels only by id through T9's `run_panel_query`, range ≤ 90 days, per-IP rate limit. nami-ui serves `/share/dashboards/[token]` without a session (`noindex`), and the dialog warns that the panels' series become public. The snapshot/caching pattern follows Status Pages (SP-6).
- **Metric alerts and the shared alert evaluator** for metric and log alerts (`metric_alerts`, `alert_series_states`; `no_data` = `zero|keep|alert`, `confirmation_seconds` / `recovery_seconds` 0-3600, `incident_mode` = `per_alert|per_series` with at most 50 open per alert). Incidents open through Incident Management Part 1's `incidents::lifecycle` with a new `IncidentSource::MetricAlert(id)` (the `incidents.source` check already lists `metric_alert`, INC-1 migration 0060) and notify through escalation policies. Log alerts (NAMI-LOG-14) move onto the shared evaluator behind their R-P2-27 seam.
- **Collector bundle** (`deploy/collectors/`: Vector and OpenTelemetry Collector configs and compose snippets, optional node_exporter/cAdvisor, Docker-socket variants documented as root-equivalent), member-only setup pages that render the source token and public URL, and `vector validate` / `otelcol validate` container tests.
- UI for scrape targets, templates, share dialog, metric alerts and collector setup, and Part 2's security wave.

Part 1 leaves room for these:
- `run_panel_query(state, project_id, dashboard_id, panel_id, request)` takes no user. RBAC and slots stay in the route, so a share route can call it with its own limits.
- `models::dashboard::create_with_panels` creates a dashboard and its panels in one transaction (templates).
- `metrics::ingest::validate_body` takes parsed samples from any parser (remote-write, scrape).
- `SeriesGuard::admit` is the one admission point for every ingest path.

**Migrations.** Postgres migrations **0120-0139** are reserved for Metrics (ruling in `docs/superpowers/decisions/2026-10-13-uptime-phase1.md`: "Metrics 0120-0139"). On 2026-10-19 none was in use: `main` (`cbc83c4`) ends at `0042_add_sources_pipeline.sql`, no remote branch and no worktree has a `012x`/`013x` file, and no other plan names one (Logs Part 2 0042-0044, Uptime Phase 3 0022-0039, Incidents Part 1 0060-0066, Status Pages Part 1 0100-0104). **Part 1 uses 0120 only.** Part 2 continues at 0121. ClickHouse tables are added to `clickhouse::MIGRATIONS` (idempotent `CREATE TABLE IF NOT EXISTS`, appended in order): `metrics` and `metric_series` (T1).

**Verification status (2026-10-19):** written against `main` at `cbc83c4` (read: `clickhouse.rs`, `state.rs`, `router.rs`, `error.rs`, `config.rs` (field list), `logs/{mod, query, search, slots, ingest, rate_limit, store, normalize}.rs`, `routes/{logs, logs_ingest, sources}.rs`, `models/source.rs`, `middleware/rbac.rs`, `crypto.rs` (API), `tests/common/{mod, logs}.rs`, `tests/{clickhouse, migrations, json_bodies, logs_search}.rs`, nami-ui `engine.ts`, `params.ts`, `log-proxy.ts`, `logs/search/+server.ts`, `LogVolumeChart.svelte`, the project layout, and the design-system branch's `ui/` components and tokens). The code has **not** been compiled or applied to a scratch copy. Tests are given in full where they pin a security property: tenant isolation, injection, caps, cardinality, RBAC and XSS. Code that decides something is given in full: DDL, migrations, SQL, the parser grammar, the admission rule. Elsewhere the plan gives exact signatures, names and values, and the implementer writes the body. A test that fails for a reason the plan did not predict is a plan bug: fix it minimally and say so in the PR.

**Execution:** per CLAUDE.md. Use one worktree per task under `.worktrees/NAMI-MET-<n>/`, branched from the latest `origin/main`, with the branch named under each task heading. Commit with Conventional Commits whose message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Then run the task review, open the PR (stating "No CI is configured on this repo yet" and the commands run), and auto-merge on a clean review (pre-authorized for sub-projects 2-11). Order: T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8 → T9 → T10 → T11 → T12 → T13. T2 depends on T1 only (`Sample`, `MetricType`) and may run beside T3. T6 depends on T5 only and may run beside T7. T8 depends on T5 (`QueryDef` validation). T10 may start once T9 is merged and the design system is on `main`. Engine tests need `docker compose up -d postgres clickhouse` (never restart running containers).

The **engine command** is used throughout:

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_TARGET_DIR=/Users/mitengineer/development/nami/.worktrees/.cargo-target cargo test <args>
```

`cargo test` takes **one** test-name filter before `--`. Several `--test <binary>` flags in one command with no filter are fine. Unit tests by module go in their own command (`--lib -- metrics::sample metrics::prom_text`). Never combine `--test x` with a `--lib` filter.

The **UI command** is `cd nami-ui && npm run check && npx vitest run <files>` (expect `svelte-check found 0 errors and 0 warnings`).

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**). Never install natively. Never restart Docker.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`), SQL strings are `&'static str` literals, never `format!`. No ORM.
- ClickHouse SQL is assembled only with `logs::query::Sql` (`push` takes `&'static str`). Every value is a bound parameter: tag keys (`tags[{pN:String}]`), tag values, regex patterns, metric names, ids, times and steps. The project id is always bound from the request path (ingest: from the token's source), never from a body. Extra settings are fixed code constants and never a name in `RESERVED_SETTINGS`.
- Spec values, verbatim:
  - metric name `^[a-zA-Z_:][a-zA-Z0-9_:.]{0,199}$`;
  - at most **50** tags per sample;
  - tag key `^[a-zA-Z_][a-zA-Z0-9_.]{0,99}$`;
  - tag value at most **1 KiB** (1,024 bytes);
  - finite values only; NaN/Inf are rejected **per sample, not per request**;
  - body limits and per-source rate limit as in Logs (10 MiB as sent, 50 requests/s, burst 100 per source);
  - more than **10,000 new series per source per minute → 429**;
  - query aggregations `avg|min|max|sum|count|p50|p95|p99|rate`; filter ops `eq|neq|regex`;
  - auto step targets about **120** points; max range **90 days**; at most **50 series** per response, the top by magnitude, flagged `truncated`;
  - regex filters use ClickHouse `match` (RE2), pattern at most **256** characters;
  - dashboard variables at most **10**, name `^[a-z][a-z0-9_]{0,31}$`, fields `{name, label, metric, tag, multi, default}`; substituted into filter values only, never metric names, aggregations or group-by keys; "All" removes the filter;
  - panel `viz` `line|bar|stat|text`, width `1/3|1/2|full` (stored as `third|half|full`, R11);
  - text panels: markdown at most **10 KiB**, raw HTML disabled, links limited to `http`/`https`/`mailto`, images not rendered;
  - members create/edit, admins delete dashboards, viewers read.
- Plan limits (rulings below give the reasons): `METRICS_MAX_DECOMPRESSED_BYTES = 16 MiB`; `MAX_SAMPLES_PER_REQUEST = 50_000`; `MAX_LINE_BYTES = 64 KiB`; `MAX_TYPE_LINES = 10_000`; `MAX_SERIES_KEY_BYTES = 8_192`; `MAX_INSERT_BYTES = 64 MiB`; timestamps accepted in `[received − retention_days, received + 10 min]`; `METRICS_MAX_SERIES_PER_PROJECT` default **50,000** active series; `MIN_STEP_SECONDS = 10`; `MAX_STEP_SECONDS = 86_400`; `MAX_POINTS_PER_SERIES = 1_500`; at most **10** filters, **5** group-by keys, **100** values per substituted filter; `MAX_METRIC_QUERIES_PER_USER = 6`; `MAX_METRIC_QUERIES_PER_INSTANCE = 32`; **100** dashboards per project; **50** panels per dashboard; panel query JSON at most **4,096** bytes; title at most **200** characters; description at most **1,000** characters.
- RBAC: two levels, as in CLAUDE.md. Authorization failure is **403**, never 404. A dashboard or panel of another project (or another dashboard) inside an accessible project path is **404**. A metric of another project simply matches nothing.
- Every state-changing engine route takes a JSON body (`require_json_body`, already global) parsed by a `#[serde(deny_unknown_fields)]` struct (axum answers 422 for unknown fields and wrong enum values, before the handler; validation errors are 400). Each new route is added to `tests/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json` in the task that adds it. The new table is added to `tests/migrations.rs::all_tables_exist`; the ClickHouse tables to `tests/clickhouse.rs::migrate_is_idempotent`.
- Ingest is token only, on the public listener only (`router::build_public_router`). The token is hashed for lookup and never logged. Error bodies never echo the token.
- nami-ui: every engine call goes through `callEngine` and `enginePath`; route params use `[...=uuid]` matchers and `isUuid`; form actions return `engineFailure(...)`; deletes are two-step (`confirm()` plus `confirm=yes`); modules under `#lib/server` are never imported by components; times are shown in UTC; tokens never appear in a URL; the design system's tokens and components only (CLAUDE.md "UI conventions"). The only `{@html}` in the feature is `renderMarkdown(...)` output.
- Commits: Conventional Commits, message ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **An exporter that restarts resets its counters.** A `rate` over a counter that drops from 20 to 5 must count the 5 as an increase from zero, never a negative rate → Task 5 (`rate_handles_a_counter_reset`).
- **An engine restart must not make every series look new.** A project with 9,000 active series that restarts the engine must keep ingesting, not get 429 `cardinality_exceeded` for a minute → Task 3 (`known_series_survive_a_restart`).
- **A series with gaps must show gaps.** Buckets with no samples are `null` in the API and breaks in the line, never zeros and never interpolated → Task 5 (`empty_buckets_are_null`), Task 10 (`decimate keeps gaps`).
- **A variable value is data, never syntax.** A dropdown value such as `a'); DROP TABLE metrics; --`, `.*` or `{p0:String}` must match only a tag value spelled exactly like that, and `$var` must never reach a metric name or a group-by key → Task 9 (`a_variable_value_is_bound_literally`, `variables_never_reach_metric_names_or_group_by`).
- **A range that does not line up with the step must still cover the range.** `from = 12:00:07`, `to = 13:00:07`, step 60: the first bucket starts at 12:00:00, the last one holds 13:00:07, and no point lies outside `[aligned from, to]` → Task 5 (`buckets_cover_the_range_aligned_to_the_step`).
- **A dashboard whose metric no longer receives data, or whose panel query fails, must still open.** The failing panel shows its own error or "no data", and the other panels render → Task 12 (`a failing panel does not break the dashboard`).

## Decisions the spec leaves open (rulings)

- **R1 Split.** Part 1 is the core named in the header: JSON and Prometheus-text ingest, storage and retention, query API, discovery, built-in metrics, dashboards with line/bar/stat/text panels, layout, time range and variables, and the UI for those. **Remote-write moves to Part 2** (spec lists it under ingestion). It needs `snap` and `prost`, its main client is Part 2's collector bundle, and the parsed samples enter through `validate_body` like every other path, so nothing in Part 1 changes for it.
- **R2 Names.** Migration `0120_create_dashboards.sql`. ClickHouse tables `metrics`, `metric_series`. Engine modules: `metrics::{sample, store, prom_text, series_guard, ingest, query, sql, builtin, variables}` and `MetricsState` in `metrics/mod.rs`. Routes: `routes::metrics_ingest` (public listener), `routes::metrics` (query and discovery), `routes::dashboards`. Model: `models::dashboard`. UI: `#lib/metrics.ts`, `#lib/chart.ts`, `#lib/panel-queue.ts`, `#lib/server/metrics-proxy.ts`, `#lib/components/metrics/*`.
- **R3 Series id.** The spec's comment says `cityHash64(name, sorted tags)`. Part 1 computes the id in Rust at ingest instead, because the cardinality guard must know it before anything reaches ClickHouse, and no crate in the tree implements ClickHouse's CityHash v1.0.2. `series_id` = the first 8 bytes (big-endian) of SHA-256 over `name`, then for each tag in key order `0x1f key 0x1e value`. The separators cannot occur in valid names or keys, so `a{b="c"}` and `a{bc=""}` differ. The id is stable across instances and restarts.
- **R4 Per-sample rejection and the response.** A body that does not parse (malformed JSON, a Prometheus syntax error) is a 400 naming the first problem (`line 7: expected a value`). Inside a parsed body, each sample is validated alone: name, tags, value, timestamp and series limit. A bad sample is rejected and counted. The response is `202 {"accepted": n, "rejected": m, "errors": [...]}`, with at most 10 errors in the form `sample 12: tag value over 1 KiB (key "path")`. If **every** sample was rejected the answer is `400` with the same body, so a misconfigured shipper sees a failure (Vector and Prometheus do not retry a 400). An empty body (no samples) is `202 {"accepted": 0, "rejected": 0, "errors": []}`.
- **R5 Body formats.** `/api/v1/metrics` takes `application/json` (also `+json`): one object or an array of objects `{name, value, type?, dt?, tags?}`. `type` defaults to `gauge`. `dt` is parsed by `logs::normalize::parse_timestamp` (RFC 3339, or unix seconds/milliseconds) and defaults to the receive time. `tags` values must be strings (a number or boolean tag value rejects that sample: tags are strings in the data model, and silently stringifying would make `1` and `"1"` different series in other paths). An unknown key rejects that sample, not the body. `/api/v1/metrics/prometheus` takes `text/plain` (with or without `version=0.0.4`) and `application/openmetrics-text`. Both accept `Content-Encoding: gzip`. Anything else is 415.
- **R6 Decompressed size.** Logs allow 50 MiB inflated. A metrics JSON body is parsed into `serde_json::Value` first (for per-sample errors), which costs several times its size, so metrics inflate at most **16 MiB** (`ingest::gunzip_within`, a new sibling of `gunzip` with the cap as a parameter; `gunzip` becomes `gunzip_within(.., MAX_DECOMPRESSED_BYTES)`). 50,000 samples of a typical 200 bytes is 10 MiB. Worst case per decode is about 100 MiB; decodes share the 4 engine-wide decode places and the per-source/per-project decode shares with log ingest.
- **R7 Timestamp window.** A sample older than the source's `retention_days` or more than **10 minutes** in the future is rejected (`timestamp out of range`). Logs clamp out-of-range timestamps; a metric point moved in time would be a wrong value, so metrics reject. Rows carry `retention_days` from the source (spec), so the TTL is per row and a retention change applies to new rows only.
- **R8 Sharing the public listener.** Metrics endpoints take a place of the same `IngestLimits.places` (32) and run token lookups under the same `IngestLimits.queries` semaphore as log ingest, so the listener's memory and Postgres bounds stay the ones Logs Part 1 proved. Rate limits and in-flight counts are separate (`MetricsState`), so a source's metric traffic never eats its log budget: 50 requests/s, burst 100, 4 in flight per source, 8 per project. Metrics do not count against a source's `daily_byte_quota` (that quota is defined on stored log bytes; per-source metric usage stats are not in the spec).
- **R9 Cardinality guard.** Two rules, both in `SeriesGuard::admit`, the single admission point:
  1. **Rate (spec):** new series admitted for one source within the current UTC minute may not pass **10,000**. A request whose new series would pass it is refused whole: `429 {"error": "...", "code": "cardinality_exceeded"}` with `Retry-After` = seconds to the next minute (at least 1), and nothing is stored. It is transient, so a shipper's retry succeeds.
  2. **Project cap (plan addition):** a project may hold at most `METRICS_MAX_SERIES_PER_PROJECT` (default 50,000, env-configurable, positive) active series. "Active" = written within the last **2 days**. Once full, samples of *new* series are rejected per sample (`series limit reached`), while existing series keep ingesting. Refusing the whole request would make every shipper retry forever.
  - A series is "new" when the instance's per-project set does not hold it. The set is loaded from `metric_series` on the first request for a project and reloaded every hour (`ensure_loaded`), so restarts and other instances' series count as known. Each instance enforces the rules on its own view: with N instances, the minute rate is up to N × 10,000 and the cap may be passed by up to one reload period's worth of new series per instance (documented; per-instance limits match how Logs Part 1 rate-limits).
  - Each admitted new series, and each known series on its first sample of a UTC day on that instance, gets a `metric_series` row (insert-only, `ReplacingMergeTree(last_seen)`). Discovery reads that table, and the cap counts it.
  - Memory: at most `cap + one request's new series` ids per project (16 bytes each, about 1 MiB at 50,000), and one 16-byte window per source that sent in the last minute (pruned when the map passes 10,000 entries).
- **R10 Query budget.** Each query (and each discovery call) holds one of the user's `MAX_METRIC_QUERIES_PER_USER = 6` slots (`UserSlots`) **and** one of `MAX_METRIC_QUERIES_PER_INSTANCE = 32` (a `Semaphore`, `try_acquire`), else 429. A dashboard of 20 panels loads 4 at a time through the UI's queue, so 6 leaves room for the explorer and discovery calls. Every metric read runs with `METRIC_READ_SETTINGS`:
  `max_result_bytes = 33554432`, `result_overflow_mode = throw`, `max_rows_to_group_by = 1000000`, `group_by_overflow_mode = throw`, `max_rows_to_read = 2000000000`, `read_overflow_mode = throw`, `max_memory_usage = 2147483648`, plus the client's `max_execution_time = 25`, `readonly = 2` and cancel-on-disconnect. `metrics::sql::user_error` maps `TOO_MANY_ROWS`, `TOO_MANY_ROWS_OR_BYTES`, `MEMORY_LIMIT_EXCEEDED` and `ResponseTooLarge` to 400 `the query is too expensive: narrow the time range or add filters`, `CANNOT_COMPILE_REGEXP` to 400, `TIMEOUT_EXCEEDED` to 400, and everything else to `logs::search::user_error`.
- **R11 Query semantics.**
  - Buckets: `bucket = intDiv(t_ms, step_ms) * step_ms`. Points run from `floor(from / step) * step` to the bucket holding `to`, every bucket present, `null` where no sample falls. The engine fills the gaps; ClickHouse returns only non-empty buckets.
  - `avg/min/max/sum` and `p50/p95/p99` are taken over **all samples of all series in the group within the bucket** (`quantile(φ)(value)`, ClickHouse's reservoir quantile, exact below 8,192 samples per group and bucket). `count` is the number of samples.
  - `rate` is computed per series first: each sample's increase over the previous sample of the same series in the range (`value - prev` when `value >= prev`, else `value`, i.e. a counter reset counts from zero; the first sample of a series in the range contributes 0), summed per bucket and divided by the step in seconds; the series' rates are then **summed** per group. `rate` is allowed on gauges too (it is then the per-second rise of the gauge).
  - Magnitude for the top 50: `sum(abs(value))` over the whole range per group, ties broken by the group key ascending. 51 groups are fetched; a 51st sets `truncated: true` and is dropped.
  - A missing tag reads as `''` (ClickHouse `Map` default), so `neq "x"` also matches series without the tag, and grouping by an absent key gives a `""` label. Documented in the API help.
  - Auto step: the smallest of `[10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400]` seconds giving at most 120 buckets; 86,400 if none does. An explicit `step` must be an integer in `10..=86400` giving at most 1,500 buckets, else 400. `to` may not be later than now + 5 minutes; `from < to`; `to − from ≤ 90 days`.
  - Output: `{"step_seconds": 60, "from": "<aligned from>", "to": "<to>", "truncated": false, "series": [{"tags": {"host": "a"}, "points": [[1760000000000, 1.5], [1760000060000, null]]}]}`; series ordered by magnitude, descending.
- **R12 Discovery.** `GET /projects/{id}/metrics/names?q=` → `{"names": [{"name", "type", "builtin"}], "truncated"}`, at most **1,000**, built-ins first; `GET .../metrics/tags?metric=` → `{"tags": [...]}`, at most **200** keys; `GET .../metrics/tag-values?metric=&tag=&q=` → `{"values": [...], "truncated"}`, at most **500**. `q` at most 200 characters, matched case-insensitively as a substring (`positionCaseInsensitiveUTF8`, bound). They read `metric_series` (never the `metrics` table), within the project, under `METRIC_READ_SETTINGS`, holding a query slot. Viewers and up.
- **R13 Built-in metrics.** Query targets, not rows:

  | Metric | Table | Value | Tags | Aggregations |
  |---|---|---|---|---|
  | `nami.uptime.response_time_ms` | `uptime_checks` | `toFloat64(response_time_ms)` (rows with a null time are skipped) | `monitor` (monitor id), `monitor_name` | all |
  | `nami.uptime.up` | `uptime_checks` | `toFloat64(status = 'up')` | `monitor`, `monitor_name` | all (`avg` = availability) |
  | `nami.logs.count` | `logs` | one per row | `level`, `source` (source name), `source_id` | `count`, `sum` (same as count), `rate` (count per second); others → 400 |

  The series key is the monitor id (uptime) or the source id plus level (logs). `monitor_name` and `source` are resolved in Postgres within the project: a filter on them becomes `monitor_id IN (...)` / `source_id IN (...)` (`regex` is matched in Rust with `regex::RegexBuilder` and `size_limit(256 KiB)`; an empty set makes the query return no series), and grouping by them groups by the id and labels each series with both the id and the name (names are not unique, so two monitors with one name stay two series). Discovery lists the three names (marked `builtin: true`), their tag keys, and their values from Postgres (monitors, sources of the project) or, for `level`, the fixed level list. Built-in names start with `nami.`; ingest rejects nothing for that prefix, but a stored metric with a built-in's exact name is shadowed by the built-in (documented).
- **R14 Markdown.** The engine stores the text (at most 10,240 bytes, no NUL) and never renders it. nami-ui renders text panels with INC-12's `renderMarkdown`, extended in T10: `mailto:` links are allowed besides `http(s)`; an image (`![alt](src)`) renders as its escaped literal text and never as `<img>`; every other link scheme renders as text. XSS tests live with the renderer (T10) and with the panel component (T12).
- **R15 Dashboard model.**
  - `dashboards`: `name` (`validate_label`), `description` (≤ 1,000 chars), `default_range` (`15m|1h|6h|24h|7d|30d`, default `24h`), `refresh_seconds` (`0|30|60|300`, default 0 = off), `variables` (jsonb array, ≤ 10), `created_by`, timestamps.
  - `dashboard_panels`: `title` (1-200 chars), `viz`, `query` (jsonb, null for `text`), `markdown` (text, only for `text`), `options` (jsonb object), `position` (int, 0-based, unique per dashboard), `width` (`third|half|full`).
  - Panel `query` is a `QueryDef {metric, aggregation, filters, group_by}` validated by T5's validator with `$var` references checked against the dashboard's variables. Its serialized form is at most 4,096 bytes. A `stat` panel may not group (one series). `options` is `{unit?: ≤ 16 chars, decimals?: 0-6, stat_reduce?: last|avg|min|max|sum (stat only, default last), stacked?: bool (bar only)}`, deny unknown fields.
  - Layout is a flow: panels in `position` order on a 6-column grid (`third` = 2, `half` = 3, `full` = 6; one column below 768 px). `PUT .../layout {"panels": [{"id", "width"}]}` must list exactly the dashboard's panel ids, once each, and rewrites `position` to the array order and `width` (400 otherwise). No coordinates, no drag-and-drop library; the editor moves panels with up/down buttons.
  - Creating a dashboard may carry `panels` (at most 50), inserted in the same transaction (`create_with_panels`; Part 2 templates). Variables are replaced as a whole by PATCH; a PATCH that removes a variable still referenced by a panel is 400 naming the panel.
  - At most 100 dashboards per project and 50 panels per dashboard (409). Last write wins; there is no edit locking.
- **R16 Variables.** A filter value is a variable reference when it matches `^\$[a-z][a-z0-9_]{0,31}$` exactly. `$$` at the start escapes a literal `$` (`$$5` filters for `$5`). Values are passed as `{"variables": {"name": ["v1", "v2"]}}`. An empty array means "All" and removes the filter. A variable left out takes its `default` (`null` default = All). For a non-`multi` variable at most one value is accepted. A filter with several values becomes `IN` (`eq`) or `NOT IN` (`neq`); a `regex` filter may not reference a `multi` variable (400 at save) and takes exactly one value. At most 100 values per variable, each at most 1,024 bytes. A name the dashboard does not define is 400. On `/metrics/query`, a reference with no value in `variables` is 400 (`unbound variable $name`); a panel query uses the dashboard's definitions. Substitution produces a new `QueryDef` that then goes through the normal validator and binding, so a value is always a bound `String` parameter.
- **R17 Panel query route.** `POST /projects/{id}/dashboards/{dashboard_id}/panels/{panel_id}/query` (viewer) with `{"from", "to", "step"?, "variables"?}`. It loads the panel with `where id = $1 and dashboard_id = $2 and project_id = $3` (404 otherwise), refuses `text` panels (400), substitutes, validates, runs. Same slots and budget as `/metrics/query`.
- **R18 Charting.** In-house SVG components rather than a library. Reasons: the repo already draws its charts this way (`ResponseTimeChart`, `LogVolumeChart`, the design system's `Sparkline`) with an established accessibility pattern (range-input keyboard inspection, a visually hidden data table, legend + tooltip so colour never carries meaning alone); SVG strokes take the design tokens through CSS variables (`TONE_VAR`), so light/dark follow the theme with no chart-library theming; nothing is evaluated or injected at runtime, so any future CSP (`script-src 'self'`, no `unsafe-eval`) holds; and there is no new dependency. The worst case (50 series × 1,500 points) is decimated in `#lib/chart.ts` to at most 2 points per horizontal pixel per series (min and max, gaps kept) before drawing. Alternatives considered: uPlot (canvas, fast, about 50 KB, but its own CSS and theming and no SVG a11y), Chart.js (canvas, about 200 KB), LayerChart (Svelte 5, pulls d3 modules). If T10's render test shows the decimated worst case is too slow, uPlot is the fallback, decided by the controller.
- **R19 Theme.** The spec says "dark theme"; the owner's design system (Atom reference, light canvas, `refactor/ui-design-system`) supersedes it. Charts use the tokens only: series tones cycle `primary, cyan, periwinkle, success, warning, info, danger, neutral`; grid lines `--color-border`; text `--color-muted`.
- **R20 Panel loading.** The dashboard view loads panel data in the browser through `#lib/panel-queue.ts` (at most 4 requests at once, newest range wins: a range or variable change aborts queued and running requests). A 429 is retried once after 1 s; a second 429 shows "too many queries, retrying on the next refresh". Auto-refresh (30 s / 1 min / 5 min) pauses while the tab is hidden (`document.visibilityState`).
- **R21 Source setup snippet.** The source detail page (members only, as the token already is) gains a "Send metrics" block with a `curl` JSON example and a Prometheus text example, filled with the public URL and token like the existing log snippets. The full collector bundle is Part 2.
- **R22 Not in Part 1.** Remote-write, scrape targets, templates, share links, metric alerts and the alert improvements, collector bundle (Part 2, see header). Per-IP limits and graceful shutdown stay with Platform (Logs Part 1 ruling). Grafana import and PromQL are out of scope (roadmap).

## Roadmap §6 coverage

| Roadmap item | Part 1 task | Part 2 |
|---|---|---|
| Metric ingestion (JSON, Prometheus text) with source tokens | T1, T2, T3, T4 | remote-write |
| Prometheus scrape targets with health | — | yes |
| ClickHouse storage, retention | T1 | — |
| Query API: aggregations over buckets, group-by | T5, T6, T7 | — |
| Dashboards (line, bar, stat), layout, time range | T8, T10, T12, T13 | — |
| Dashboard variables | T8, T9, T12, T13 | — |
| Markdown text panels | T8, T10, T12, T13 | — |
| Template library | — | yes |
| Public share link | — (T9 leaves `run_panel_query`) | yes |
| Built-in metrics (uptime, log volume) | T7 | — |
| Metric alerts | — | yes |
| Alert improvements | — | yes |
| Collector bundle | — (T11 adds the source snippet) | yes |
| UI for all of the above | T10-T13 (Part 1 features) | Part 2 features |

After Task 13 merges, the controller ticks: metric ingestion (JSON, Prometheus text), ClickHouse storage and retention, query API, dashboards, variables, markdown panels, built-in metrics. "UI for all of the above" stays open until Part 2.

## File Structure

```
nami-engine/
  migrations/
    0120_create_dashboards.sql          T8 dashboards, dashboard_panels
  src/
    clickhouse.rs                       T1 MIGRATIONS += metrics, metric_series
    config.rs                           T4 metrics_max_series_per_project (METRICS_MAX_SERIES_PER_PROJECT)
    state.rs                            T4 AppState.metrics
    lib.rs                              T1 pub mod metrics
    router.rs, routes/mod.rs            T4 public metrics router; T5 metrics router; T8 dashboards router
    logs/ingest.rs                      T4 gunzip_within, IngestError::CardinalityExceeded
    metrics/
      mod.rs                            T1 module list; T4 MetricsState
      sample.rs                         T1 Sample, MetricType, Rejection, validation, series_id
      store.rs                          T1 MetricRow, SeriesRow, encode, insert
      prom_text.rs                      T2 Prometheus text parser
      series_guard.rs                   T3 SeriesGuard (rate + cap + metric_series seeding)
      ingest.rs                         T4 body kinds, JSON parse, validate_body, the blocking build
      query.rs                          T5 QueryDef/QueryRequest, validation, window/step; T7 Target variants; T9 variables field
      sql.rs                            T5 SQL builder, run, gap filling, user_error; T6 discovery SQL; T7 built-in SQL
      builtin.rs                        T7 built-in definitions, Postgres name resolution
      variables.rs                      T9 substitution
    models/dashboard.rs                 T8 rows, validation, CRUD, layout, create_with_panels
    routes/metrics_ingest.rs            T4 POST /api/v1/metrics, /api/v1/metrics/prometheus
    routes/metrics.rs                   T5 query; T6 discovery
    routes/dashboards.rs                T8 CRUD + layout; T9 panel query
  tests/
    common/metrics.rs                   T4 post_metrics, post_prometheus, seed_samples, query helpers
    fixtures/node_exporter.prom         T2 a real node_exporter scrape (trimmed)
    metrics_store.rs                    T1
    metrics_series_guard.rs             T3
    metrics_ingest.rs                   T4
    metrics_query.rs                    T5
    metrics_discovery.rs                T6
    metrics_builtin.rs                  T7
    dashboards_api.rs                   T8
    dashboard_variables.rs              T9
    clickhouse.rs, migrations.rs, json_bodies.rs   T1 / T8 / T5, T8, T9 additions

nami-ui/src/
  lib/metrics.ts (+ test)               T10 types, ranges, formatting, stat reduce, explorer URL state, snippets (T11)
  lib/chart.ts (+ test)                 T10 decimate, scales, time ticks, series tones
  lib/panel-queue.ts (+ test)           T10 4-wide abortable fetch queue
  lib/markdown.ts (+ test)              T10 extend (or create, see INC-12 row) renderMarkdown: mailto, no images
  lib/server/json-relay.ts (+ test)     T10 only if LOG-16 is not merged
  lib/server/metrics-proxy.ts (+ test)  T10 discovery params, panel-query paths
  lib/components/metrics/
    TimeSeriesChart.svelte              T10 line + bar, gaps, tooltip, keyboard, data table
    StatPanel.svelte                    T10 StatCard + sparkline
    MarkdownPanel.svelte                T10 renderMarkdown
    TimeRangePicker.svelte              T10 presets + custom
    QueryEditor.svelte                  T11 metric autocomplete, aggregation, filters, group-by
    VariableBar.svelte                  T12 dropdowns from tag-values
    PanelCard.svelte                    T12 Card + body by viz, loading/error/no-data
    PanelEditor.svelte                  T13 panel form with live preview
    VariableEditor.svelte               T13 variables form
  routes/(app)/projects/[id=uuid]/
    +layout.svelte                      T11 Metrics and Dashboards tabs
    metrics/+page.server.ts, +page.svelte                     T11 explorer
    metrics/query/+server.ts (+ test)                         T10 JSON relay to /metrics/query
    metrics/discovery/+server.ts (+ test)                     T10 GET relay to names/tags/tag-values
    logs/sources/[sourceId=uuid]/+page.svelte                 T11 "Send metrics" snippet
    dashboards/+page.server.ts, +page.svelte (+ test)         T12 list, create, delete
    dashboards/[dashboardId=uuid]/+page.server.ts, +page.svelte (+ test)   T12 view
    dashboards/[dashboardId=uuid]/panels/[panelId=uuid]/query/+server.ts (+ test)   T10 relay
    dashboards/[dashboardId=uuid]/edit/+page.server.ts, +page.svelte (+ test)   T13 editor
```

---
### Task 1 (NAMI-MET-1): Metric sample model, validation and ClickHouse storage

Branch `feat/NAMI-MET-1-metrics-storage`, worktree `.worktrees/NAMI-MET-1/`.

**Files:**
- Create: `nami-engine/src/metrics/mod.rs`, `nami-engine/src/metrics/sample.rs`, `nami-engine/src/metrics/store.rs`, `nami-engine/tests/metrics_store.rs`
- Modify: `nami-engine/src/lib.rs` (`pub mod metrics;`), `nami-engine/src/clickhouse.rs` (`MIGRATIONS` grows from 3 to 5 entries, appended), `nami-engine/tests/clickhouse.rs` (`migrate_is_idempotent` table list)

**Interfaces:**
- Consumes: `models::source::IngestSource`, `clickhouse::{ClickHouse, ClickHouseError, encode_rows_within}`, `logs::store::INSERT_SETTINGS`.
- Produces:

```rust
// metrics/sample.rs
pub const MAX_NAME_LEN: usize = 200;
pub const MAX_TAGS: usize = 50;
pub const MAX_TAG_KEY_LEN: usize = 100;
pub const MAX_TAG_VALUE_BYTES: usize = 1024;
/// name + every key + every value, in bytes.
pub const MAX_SERIES_KEY_BYTES: usize = 8192;
pub const MAX_FUTURE: chrono::Duration = chrono::Duration::minutes(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricType { Gauge, Counter }
impl MetricType { pub fn as_str(self) -> &'static str }   // "gauge" | "counter"

/// A sample as a parser produced it, not yet validated.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: String,
    pub metric_type: MetricType,
    pub tags: Vec<(String, String)>,   // order as sent; duplicates are a rejection
    pub value: f64,
    pub dt: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    InvalidName, TooManyTags, InvalidTagKey(String), ReservedTagKey(String), DuplicateTag(String),
    TagValueTooLong(String), TagValueNotString(String), SeriesKeyTooLong, NotFinite,
    TimestampOutOfRange, InvalidTimestamp, UnknownField(String), InvalidValue, SeriesLimit,
}
impl Rejection { pub fn message(&self) -> String }

pub fn is_valid_name(name: &str) -> bool;
pub fn is_valid_tag_key(key: &str) -> bool;      // also used by the query validator (T5)
pub fn series_id(name: &str, tags: &std::collections::BTreeMap<String, String>) -> u64;
pub fn validate(sample: Sample, source: &IngestSource, received_at: chrono::DateTime<chrono::Utc>)
    -> Result<crate::metrics::store::MetricRow, Rejection>;

// metrics/store.rs
pub const METRICS_TABLE: &str = "metrics";
pub const SERIES_TABLE: &str = "metric_series";
pub const MAX_INSERT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MetricRow {
    pub project_id: uuid::Uuid, pub source_id: uuid::Uuid, pub name: String,
    pub metric_type: &'static str, pub series_id: u64,
    pub tags: std::collections::BTreeMap<String, String>,
    pub dt: chrono::DateTime<chrono::Utc>, pub value: f64, pub retention_days: u16,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesRow {
    pub project_id: uuid::Uuid, pub source_id: uuid::Uuid, pub name: String,
    pub metric_type: &'static str, pub series_id: u64,
    pub tags: std::collections::BTreeMap<String, String>,
    pub last_seen: chrono::DateTime<chrono::Utc>, pub retention_days: u16,
}
impl SeriesRow { pub fn from_row(row: &MetricRow) -> SeriesRow }
pub fn encode_metric_rows(rows: &[MetricRow]) -> Result<String, ClickHouseError>;   // encode_rows_within(rows, MAX_INSERT_BYTES)
pub async fn insert_encoded(ch: &ClickHouse, body: String) -> Result<(), ClickHouseError>;  // METRICS_TABLE, INSERT_SETTINGS
pub async fn insert_series(ch: &ClickHouse, rows: &[SeriesRow]) -> Result<(), ClickHouseError>; // SERIES_TABLE, INSERT_SETTINGS
```

- [ ] **Step 1: Write the failing unit tests** in `metrics/sample.rs` (`#[cfg(test)] mod tests`):

```rust
use std::collections::BTreeMap;
use chrono::{Duration, TimeZone, Utc};
use uuid::Uuid;
use super::*;
use crate::models::source::IngestSource;

fn source() -> IngestSource {
    IngestSource { id: Uuid::new_v4(), project_id: Uuid::new_v4(), retention_days: 30, daily_byte_quota: None }
}
fn now() -> chrono::DateTime<Utc> { Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap() }
fn sample(name: &str, tags: &[(&str, &str)], value: f64) -> Sample {
    Sample { name: name.into(), metric_type: MetricType::Gauge,
        tags: tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), value, dt: None }
}

#[test]
fn names_follow_the_spec_charset() {
    for ok in ["a", "_x", ":y", "http_requests_total", "node.cpu:rate5m", &"a".repeat(200)] {
        assert!(is_valid_name(ok), "{ok}");
    }
    for bad in ["", "1abc", "a-b", "a b", "a{b}", "é", "a'b", "a\"", "a\n", ".a", &"a".repeat(201)] {
        assert!(!is_valid_name(bad), "{bad:?}");
    }
}

#[test]
fn tag_keys_follow_the_spec_charset_and_dunder_is_reserved() {
    assert!(is_valid_tag_key("host") && is_valid_tag_key("k8s.pod") && is_valid_tag_key(&"k".repeat(100)));
    for bad in ["", "1a", "a-b", "a:b", "a b", &"k".repeat(101)] {
        assert!(!is_valid_tag_key(bad), "{bad:?}");
    }
    let r = validate(sample("m", &[("__name__", "x")], 1.0), &source(), now());
    assert_eq!(r, Err(Rejection::ReservedTagKey("__name__".into())));
}

#[test]
fn each_tag_rule_rejects_the_sample() {
    let s = source();
    let many: Vec<(String, String)> = (0..51).map(|i| (format!("k{i}"), "v".into())).collect();
    let mut too_many = sample("m", &[], 1.0);
    too_many.tags = many;
    assert_eq!(validate(too_many, &s, now()), Err(Rejection::TooManyTags));
    let long = "v".repeat(1025);
    assert_eq!(validate(sample("m", &[("path", &long)], 1.0), &s, now()),
        Err(Rejection::TagValueTooLong("path".into())));
    assert!(validate(sample("m", &[("path", &"v".repeat(1024))], 1.0), &s, now()).is_ok());
    assert_eq!(validate(sample("m", &[("a", "1"), ("a", "2")], 1.0), &s, now()),
        Err(Rejection::DuplicateTag("a".into())));
    let wide: Vec<(String, String)> = (0..9).map(|i| (format!("k{i}"), "v".repeat(1000))).collect();
    let mut wide_sample = sample("m", &[], 1.0);
    wide_sample.tags = wide;
    assert_eq!(validate(wide_sample, &s, now()), Err(Rejection::SeriesKeyTooLong));
}

#[test]
fn non_finite_values_are_rejected() {
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(validate(sample("m", &[], v), &source(), now()), Err(Rejection::NotFinite));
    }
}

#[test]
fn timestamps_outside_retention_or_too_far_ahead_are_rejected() {
    let s = source(); // 30 days
    let mut old = sample("m", &[], 1.0);
    old.dt = Some(now() - Duration::days(30) - Duration::seconds(1));
    assert_eq!(validate(old, &s, now()), Err(Rejection::TimestampOutOfRange));
    let mut ahead = sample("m", &[], 1.0);
    ahead.dt = Some(now() + Duration::minutes(10) + Duration::seconds(1));
    assert_eq!(validate(ahead, &s, now()), Err(Rejection::TimestampOutOfRange));
    let mut edge = sample("m", &[], 1.0);
    edge.dt = Some(now() + Duration::minutes(10));
    assert!(validate(edge, &s, now()).is_ok());
    let row = validate(sample("m", &[], 1.0), &s, now()).unwrap();
    assert_eq!(row.dt, now());
    assert_eq!((row.project_id, row.source_id, row.retention_days), (s.project_id, s.id, 30));
}

#[test]
fn series_id_is_stable_order_independent_and_separator_safe() {
    let tags = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<BTreeMap<_, _>>();
    let a = series_id("m", &tags(&[("b", "1"), ("a", "2")]));
    assert_eq!(a, series_id("m", &tags(&[("a", "2"), ("b", "1")])));
    assert_ne!(series_id("a", &tags(&[("b", "c")])), series_id("a", &tags(&[("bc", "")])));
    assert_ne!(series_id("ab", &tags(&[])), series_id("a", &tags(&[("b", "")])));
    // Pinned: the id is part of stored data, so it must never change.
    assert_eq!(series_id("up", &tags(&[("job", "node")])), PINNED_UP_JOB_NODE);
}
```

`PINNED_UP_JOB_NODE` is a `const u64` in the test module: run the test once, copy the printed value in, and note in the PR that it pins R3's encoding.

- [ ] **Step 2: Run it to make sure it fails**

Run: engine command with `--lib -- metrics::sample`
Expected: FAIL to compile (`metrics` module missing).

- [ ] **Step 3: Implement `metrics::sample` and `metrics::store`**

- `is_valid_name` / `is_valid_tag_key`: hand-written byte checks against the spec regexes (ASCII only; no `regex` needed).
- `validate` order: name → tag count → each tag (key charset, `__` prefix → `ReservedTagKey`, duplicate → `DuplicateTag`, value bytes) → total series-key bytes → value finite → dt window `[received − retention_days days, received + MAX_FUTURE]` → build the `BTreeMap`, `series_id`, the row (`dt` defaults to `received_at`, truncated to whole milliseconds; `retention_days` as `u16` from the source).
- `series_id`: SHA-256 over `name`, then per tag in key order `0x1f`, key, `0x1e`, value; first 8 bytes big-endian (R3).
- `Rejection::message()` texts (fixed, never echoing a value): `invalid metric name`, `more than 50 tags`, `invalid tag key "<key>"`, `reserved tag key "<key>"`, `duplicate tag "<key>"`, `tag value over 1 KiB (key "<key>")`, `tag value must be a string (key "<key>")`, `series key over 8 KiB`, `value must be a finite number`, `timestamp out of range`, `invalid timestamp`, `unknown field "<field>"`, `value must be a number`, `series limit reached`. A key is at most 100 ASCII characters by then, so quoting it is safe; for `InvalidTagKey` and `UnknownField` quote only the first 100 bytes, with `"` and control characters replaced by `?`.

- [ ] **Step 4: Run the unit tests**

Run: engine command with `--lib -- metrics::sample`
Expected: PASS.

- [ ] **Step 5: Add the ClickHouse DDL** to `clickhouse::MIGRATIONS`, after `heartbeat_pings`, with a comment pointing at `metrics::store`:

```sql
CREATE TABLE IF NOT EXISTS metrics (
    project_id UUID,
    source_id UUID,
    name LowCardinality(String),
    metric_type LowCardinality(String),
    series_id UInt64,
    tags Map(LowCardinality(String), String),
    dt DateTime64(3, 'UTC'),
    value Float64,
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, name, series_id, dt)
TTL toDateTime(dt) + toIntervalDay(retention_days)
```

```sql
CREATE TABLE IF NOT EXISTS metric_series (
    project_id UUID,
    source_id UUID,
    name LowCardinality(String),
    metric_type LowCardinality(String),
    series_id UInt64,
    tags Map(LowCardinality(String), String),
    last_seen DateTime64(3, 'UTC'),
    retention_days UInt16
) ENGINE = ReplacingMergeTree(last_seen)
ORDER BY (project_id, name, series_id)
TTL toDateTime(last_seen) + toIntervalDay(retention_days)
```

`MIGRATIONS` becomes `[&str; 5]`. In `tests/clickhouse.rs::migrate_is_idempotent` the expected list becomes `heartbeat_pings, logs, metric_series, metrics, uptime_checks` (sorted by name). The 367 day partitions a row may land in (365-day retention + 10 min ahead) stay under `INSERT_SETTINGS`' `max_partitions_per_insert_block = 400`.

- [ ] **Step 6: Write the failing integration test** `tests/metrics_store.rs`:

```rust
// Metric rows round-trip through ClickHouse. Requires `docker compose up -d clickhouse`.
use std::collections::BTreeMap;
use chrono::{DateTime, Utc};
use nami_engine::{clickhouse::ClickHouse, metrics::{sample::{validate, MetricType, Sample}, store}};
use nami_engine::models::source::IngestSource;
use serde_json::Value;
use uuid::Uuid;

#[tokio::test]
async fn rows_and_series_round_trip_with_tags_stored_literally() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = IngestSource { id: Uuid::new_v4(), project_id: Uuid::new_v4(), retention_days: 7, daily_byte_quota: None };
    let evil = "'; DROP TABLE metrics; -- {p0:String} back\\slash \"q\" ünï";
    let now: DateTime<Utc> = Utc::now();
    let row = validate(Sample { name: "http_requests_total".into(), metric_type: MetricType::Counter,
        tags: vec![("path".into(), evil.into()), ("host".into(), "a".into())], value: 42.5, dt: Some(now) },
        &source, now).unwrap();
    store::insert_encoded(&ch, store::encode_metric_rows(&[row.clone()]).unwrap()).await.unwrap();
    store::insert_series(&ch, &[store::SeriesRow::from_row(&row)]).await.unwrap();

    let rows: Vec<Value> = ch.query_rows(
        "SELECT name, metric_type, toString(series_id) AS sid, tags, value, retention_days FROM metrics \
         WHERE project_id = {p:UUID}", &[("p", source.project_id.to_string())]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "http_requests_total");
    assert_eq!(rows[0]["metric_type"], "counter");
    assert_eq!(rows[0]["sid"], row.series_id.to_string());
    assert_eq!(rows[0]["tags"]["path"], evil);
    assert_eq!(rows[0]["value"], 42.5);
    assert_eq!(rows[0]["retention_days"], 7);
    let series: Vec<Value> = ch.query_rows(
        "SELECT toString(series_id) AS sid FROM metric_series WHERE project_id = {p:UUID}",
        &[("p", source.project_id.to_string())]).await.unwrap();
    assert_eq!(series, vec![serde_json::json!({ "sid": row.series_id.to_string() })]);
    ch.drop_database().await.unwrap();
}

#[test]
fn an_oversized_batch_is_refused_before_it_is_built() {
    let row = store::MetricRow { project_id: Uuid::nil(), source_id: Uuid::nil(), name: "m".into(),
        metric_type: "gauge", series_id: 1, tags: BTreeMap::from([("k".into(), "v".repeat(1024))]),
        dt: Utc::now(), value: 1.0, retention_days: 1 };
    let rows = vec![row; store::MAX_INSERT_BYTES / 1024 + 1];
    assert!(matches!(store::encode_metric_rows(&rows),
        Err(nami_engine::clickhouse::ClickHouseError::TooLarge { .. })));
}
```

- [ ] **Step 7: Run it, fix, run again**

Run: engine command with `--test metrics_store --test clickhouse`
Expected: PASS (after Step 5).

- [ ] **Step 8: Commit**

```bash
git add nami-engine/src/metrics nami-engine/src/lib.rs nami-engine/src/clickhouse.rs \
  nami-engine/tests/metrics_store.rs nami-engine/tests/clickhouse.rs
git commit -m "feat(engine): metric sample validation and ClickHouse storage (NAMI-MET-1)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2 (NAMI-MET-2): Prometheus text exposition parser

Branch `feat/NAMI-MET-2-prometheus-text`, worktree `.worktrees/NAMI-MET-2/`.

**Files:**
- Create: `nami-engine/src/metrics/prom_text.rs`, `nami-engine/tests/fixtures/node_exporter.prom`
- Modify: `nami-engine/src/metrics/mod.rs`

**Interfaces:**
- Consumes: `metrics::sample::{Sample, MetricType}` (T1).
- Produces:

```rust
pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const MAX_TYPE_LINES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("line {line}: {reason}")]
    Syntax { line: usize, reason: &'static str },
    #[error("line {line}: longer than 64 KiB")]
    LineTooLong { line: usize },
    #[error("more than {max} samples")]
    TooManySamples { max: usize },
    #[error("more than 10000 # TYPE lines")]
    TooManyTypes,
}

/// Parses a whole exposition body. `max_samples` is the request's cap.
pub fn parse(text: &str, max_samples: usize) -> Result<Vec<Sample>, ParseError>;
```

Grammar (decides behaviour, implement exactly):
- Lines split on `\n`; a trailing `\r` is stripped. Empty and whitespace-only lines are skipped. A line over `MAX_LINE_BYTES` (before stripping) → `LineTooLong` at once, before scanning it.
- `# TYPE <name> <type>`: `type` is `counter`, `gauge`, `histogram`, `summary`, `untyped`, or OpenMetrics `unknown`/`gaugehistogram`/`stateset`/`info` (all four map to gauge). A repeated `# TYPE` for a name keeps the first. More than `MAX_TYPE_LINES` → `TooManyTypes`. A malformed `# TYPE` → `Syntax { reason: "malformed # TYPE line" }`. Every other `#` line (HELP, UNIT, EOF, comments) is ignored.
- Sample: `name [ '{' label ( ',' label )* ','? '}' ] value [ timestamp ] [ ' # ' exemplar ]`, separated by spaces or tabs. `name` is `[a-zA-Z_:][a-zA-Z0-9_:]*` (the parser accepts the Prometheus charset; `.` and the 200-char cap are T1's validation). `label` is `key="value"`, key `[a-zA-Z_][a-zA-Z0-9_]*`, whitespace allowed around `=` and `,`. The error reasons for a malformed sample head are `"expected a metric name"` (the line starts with anything else, `{` included), `"expected a label name"`, `"expected = after the label name"`, `"expected a quoted label value"` and `"expected , or }"`. Value escapes `\\`, `\"`, `\n` only; any other escape → `Syntax { reason: "invalid escape in label value" }`. An unterminated quote → `"unterminated label value"`. `{}` is allowed.
- `value` goes through `f64::from_str`, after mapping `+Inf`/`Inf` → `inf`, `-Inf` → `-inf`, `NaN` → `NaN` (the result may be non-finite; T1 rejects that sample, R4). Anything unparsable → `"expected a value"`. Hex floats are not values.
- `timestamp` is an optional integer of milliseconds (`i64`); anything else → `"invalid timestamp"`. It becomes `dt` through `Utc.timestamp_millis_opt` (`None` → `"invalid timestamp"`). An exemplar (` # {...} v [ts]`) is ignored. Any other trailing text → `"unexpected text after the sample"`.
- Type of a sample: its name's `# TYPE` if present; else strip one suffix of `_bucket`, `_count`, `_sum`, `_total`, `_created`, `_gcount`, `_gsum` and look up the base name. Mapping: `counter` → Counter for the name itself and `_total`; `histogram`/`summary` → Counter for `_bucket`, `_count`, `_sum`, Gauge for the quantile series (the base name with a `quantile` label) and `_created`; `gauge`/`untyped`/unknown/none → Gauge.
- Labels are kept as a `Vec` in order, duplicates included (T1 rejects a duplicate per sample).
- The sample count is checked as each sample is pushed: the `max_samples + 1`th → `TooManySamples`. Invalid UTF-8 never reaches the parser (T4 refuses it with 400).

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)] mod tests` in `prom_text.rs`):

```rust
use super::*;
use crate::metrics::sample::MetricType;

fn one(text: &str) -> Sample { let mut v = parse(text, 10).unwrap(); assert_eq!(v.len(), 1); v.remove(0) }

#[test]
fn a_plain_sample_with_labels_timestamp_and_escapes() {
    let s = one("http_requests_total{method=\"post\",path=\"/a\\\"b\\\\c\\nd\"} 1027 1395066363000\n");
    assert_eq!(s.name, "http_requests_total");
    assert_eq!(s.tags, vec![("method".into(), "post".into()), ("path".into(), "/a\"b\\c\nd".into())]);
    assert_eq!(s.value, 1027.0);
    assert_eq!(s.dt.unwrap().timestamp_millis(), 1_395_066_363_000);
    assert_eq!(s.metric_type, MetricType::Gauge);
}

#[test]
fn types_apply_to_their_series() {
    let text = "# HELP x_total ignored\n# TYPE x counter\nx_total 3\n# TYPE h histogram\n\
        h_bucket{le=\"0.1\"} 2\nh_bucket{le=\"+Inf\"} 5\nh_sum 1.5\nh_count 5\n\
        # TYPE s summary\ns{quantile=\"0.99\"} 0.2\ns_count 9\n# TYPE g gauge\ng -1e-3\nuntyped_thing 7\n";
    let samples = parse(text, 100).unwrap();
    let types: Vec<(&str, MetricType)> = samples.iter().map(|s| (s.name.as_str(), s.metric_type)).collect();
    use MetricType::*;
    assert_eq!(types, vec![("x_total", Counter), ("h_bucket", Counter), ("h_bucket", Counter),
        ("h_sum", Counter), ("h_count", Counter), ("s", Gauge), ("s_count", Counter), ("g", Gauge),
        ("untyped_thing", Gauge)]);
}

#[test]
fn special_values_parse_and_crlf_and_blank_lines_are_fine() {
    let samples = parse("a NaN\r\n\r\nb +Inf\nc -Inf\nd 1.5e3\n", 10).unwrap();
    assert!(samples[0].value.is_nan());
    assert_eq!((samples[1].value, samples[2].value, samples[3].value), (f64::INFINITY, f64::NEG_INFINITY, 1500.0));
}

#[test]
fn syntax_errors_name_the_line() {
    for (text, reason) in [
        ("ok 1\nbad{a=\"x} 1\n", "unterminated label value"),
        ("ok 1\nbad{a=\"\\q\"} 1\n", "invalid escape in label value"),
        ("ok 1\nbad\n", "expected a value"),
        ("ok 1\nbad 1 2 3\n", "unexpected text after the sample"),
        ("ok 1\nbad 1 notatime\n", "invalid timestamp"),
        ("ok 1\n# TYPE\n", "malformed # TYPE line"),
        ("ok 1\n{a=\"b\"} 1\n", "expected a metric name"),
        ("ok 1\nbad{a=\"b\" 1\n", "expected , or }"),
        ("ok 1\nbad{1a=\"b\"} 1\n", "expected a label name"),
    ] {
        assert_eq!(parse(text, 10), Err(ParseError::Syntax { line: 2, reason }), "{text:?}");
    }
}

#[test]
fn a_line_over_64_kib_is_refused_before_it_is_scanned() {
    let text = format!("ok 1\nm{{a=\"{}\"}} 1\n", "x".repeat(MAX_LINE_BYTES));
    assert_eq!(parse(&text, 10), Err(ParseError::LineTooLong { line: 2 }));
}

#[test]
fn the_sample_cap_holds() {
    let text = "m 1\n".repeat(11);
    assert_eq!(parse(&text, 10), Err(ParseError::TooManySamples { max: 10 }));
    assert_eq!(parse(&"m 1\n".repeat(10), 10).unwrap().len(), 10);
}

#[test]
fn type_lines_are_capped() {
    let text: String = (0..=MAX_TYPE_LINES).map(|i| format!("# TYPE m{i} gauge\n")).collect();
    assert_eq!(parse(&text, 10), Err(ParseError::TooManyTypes));
}

#[test]
fn a_real_node_exporter_scrape_parses() {
    let text = include_str!("../../tests/fixtures/node_exporter.prom");
    let samples = parse(text, 50_000).unwrap();
    assert!(samples.len() > 100);
    assert!(samples.iter().any(|s| s.name == "node_cpu_seconds_total" && s.metric_type == MetricType::Counter));
    assert!(samples.iter().any(|s| s.name.ends_with("_bucket") && s.metric_type == MetricType::Counter));
    assert!(samples.iter().any(|s| s.name == "node_load1" && s.metric_type == MetricType::Gauge));
}

#[test]
fn arbitrary_mutations_never_panic_and_stay_linear() {
    let base = include_str!("../../tests/fixtures/node_exporter.prom").as_bytes().to_vec();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
    let started = std::time::Instant::now();
    for _ in 0..5_000 {
        let mut bytes = base.clone();
        for _ in 0..8 {
            let at = (next() as usize) % bytes.len();
            bytes[at] = b"{}\"\\=, \n#+-.e0aZ_:\t"[(next() as usize) % 20];
        }
        if let Ok(text) = std::str::from_utf8(&bytes) { let _ = parse(text, 50_000); }
    }
    let pathological = format!("m{{{}}} 1\n", "a=\"\\\\\",".repeat(9_000));
    let _ = parse(&pathological, 10);
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "parser is not linear");
}
```

The fixture is a trimmed real node_exporter 1.8 scrape (HELP/TYPE lines, counters, gauges, one histogram family, `{}`-less samples; about 300 lines, no hostnames).

- [ ] **Step 2: Run to fail**

Run: engine command with `--lib -- metrics::prom_text`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `parse`** as a single forward scan per line over bytes (no regex, no backtracking), with a `HashMap<String, PromType>` for `# TYPE` entries.

- [ ] **Step 4: Run to pass**

Run: engine command with `--lib -- metrics::prom_text`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src/metrics nami-engine/tests/fixtures/node_exporter.prom
git commit -m "feat(engine): Prometheus text exposition parser with line and sample caps (NAMI-MET-2)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3 (NAMI-MET-3): Cardinality guard

Branch `feat/NAMI-MET-3-series-guard`, worktree `.worktrees/NAMI-MET-3/`.

**Files:**
- Create: `nami-engine/src/metrics/series_guard.rs`, `nami-engine/tests/metrics_series_guard.rs`
- Modify: `nami-engine/src/metrics/mod.rs`

**Interfaces:**
- Consumes: `clickhouse::ClickHouse`, `metrics::store::SERIES_TABLE` (T1).
- Produces:

```rust
pub const NEW_SERIES_PER_SOURCE_PER_MINUTE: usize = 10_000;
pub const ACTIVE_SERIES_DAYS: i64 = 2;
pub const RELOAD_EVERY: chrono::Duration = chrono::Duration::hours(1);
pub const DEFAULT_MAX_SERIES_PER_PROJECT: usize = 50_000;

pub struct SeriesGuard { /* std Mutex<Inner>, tokio Mutex<()> load lock, max_series_per_project */ }

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Admitted(Reservation),
    /// Spec: more than 10,000 new series per source per minute.
    RateLimited { retry_after_seconds: u64 },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reservation {
    pub project_id: uuid::Uuid,
    /// New series admitted now (need a metric_series row).
    pub new_series: Vec<u64>,
    /// Known series on their first sample of this UTC day here (need a metric_series row).
    pub touched: Vec<u64>,
    /// New series refused because the project is full (their samples are rejected).
    pub refused: std::collections::HashSet<u64>,
}

impl SeriesGuard {
    pub fn new(max_series_per_project: usize) -> SeriesGuard;
    /// Loads the project's active series from metric_series when this instance has not,
    /// or when its copy is older than RELOAD_EVERY. Single flight per instance.
    pub async fn ensure_loaded(&self, ch: &ClickHouse, project_id: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>) -> Result<(), ClickHouseError>;
    /// `series`: the distinct series ids of one request, in first-seen order.
    pub fn admit(&self, project_id: uuid::Uuid, source_id: uuid::Uuid, series: &[u64],
        now: chrono::DateTime<chrono::Utc>) -> Admission;
    /// Undoes `new_series` of a reservation whose insert failed (room and rate come back).
    pub fn forget(&self, reservation: &Reservation, source_id: uuid::Uuid, now: chrono::DateTime<chrono::Utc>);
    pub fn known(&self, project_id: uuid::Uuid) -> usize;
}
```

Admission rule (decides behaviour; all under one `std::sync::Mutex`, never held across `.await`):
1. `today` = days since epoch of `now` (UTC); `minute` = `now.timestamp().div_euclid(60)`.
2. Project state `known: HashMap<u64, u32 /* day of last metric_series row */>`. `new` = ids of `series` not in `known`, in order.
3. Source window `(minute, count)`; reset to `(minute, 0)` when its minute differs. `room = cap.saturating_sub(known.len())`; `admitted = new[..min(room, new.len())]`; `refused = new[admitted.len()..]`.
4. If `count + admitted.len() > NEW_SERIES_PER_SOURCE_PER_MINUTE` → `RateLimited { retry_after_seconds: max(1, 60 - now.timestamp().rem_euclid(60)) }`, nothing recorded.
5. Else `count += admitted.len()`, insert `admitted` with `today`, collect `touched` = ids of `series` already known whose day is `< today` (set to `today`), return `Admitted`.
6. When the source-window map passes 10,000 entries, drop entries whose minute is not the current one.

`ensure_loaded` runs (under the load lock, re-checking after acquiring it):

```sql
SELECT toString(series_id) AS series_id, toUInt32(toRelativeDayNum(max(last_seen))) AS day
FROM metric_series
WHERE project_id = {p0:UUID} AND last_seen >= now64(3) - toIntervalDay({p1:UInt32})
GROUP BY series_id
LIMIT {p2:UInt64}
```

with `p1 = ACTIVE_SERIES_DAYS`, `p2 = cap + 1`, under `METRIC_READ_SETTINGS`' first two entries (`max_result_bytes`, `result_overflow_mode`; T5 defines the full constant, so this task declares `const LOAD_SETTINGS` with those two and T5 replaces it by the shared one). The loaded map is merged with the in-memory one (the larger day wins), so series admitted since the last load are kept. `series_id` travels as a string because the client reads JSON numbers into `u64` exactly, but a `UInt64` above 2^53 sent as a JSON number would lose precision in any JSON-number path; parse with `str::parse::<u64>`.

- [ ] **Step 1: Write the failing unit tests** in `series_guard.rs`:

```rust
use chrono::{Duration, TimeZone, Utc};
use uuid::Uuid;
use super::*;

fn at(secs: i64) -> chrono::DateTime<Utc> { Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap() + Duration::seconds(secs) }
fn ids(range: std::ops::Range<u64>) -> Vec<u64> { range.collect() }
fn admitted(a: Admission) -> Reservation { match a { Admission::Admitted(r) => r, other => panic!("{other:?}") } }

#[test]
fn ten_thousand_new_series_per_source_per_minute_then_429() {
    let g = SeriesGuard::new(1_000_000);
    let (p, s) = (Uuid::new_v4(), Uuid::new_v4());
    assert_eq!(admitted(g.admit(p, s, &ids(0..6_000), at(0))).new_series.len(), 6_000);
    assert_eq!(admitted(g.admit(p, s, &ids(6_000..10_000), at(30))).new_series.len(), 4_000);
    assert_eq!(g.admit(p, s, &ids(10_000..10_001), at(45)), Admission::RateLimited { retry_after_seconds: 15 });
    assert_eq!(g.known(p), 10_000, "a refused request records nothing");
    // The next minute admits again; another source of the project has its own window.
    assert_eq!(admitted(g.admit(p, s, &ids(10_000..10_001), at(60))).new_series, vec![10_000]);
    assert_eq!(admitted(g.admit(p, Uuid::new_v4(), &ids(20_000..29_999), at(61))).new_series.len(), 9_999);
}

#[test]
fn known_series_never_count_toward_the_rate() {
    let g = SeriesGuard::new(1_000_000);
    let (p, s) = (Uuid::new_v4(), Uuid::new_v4());
    admitted(g.admit(p, s, &ids(0..10_000), at(0)));
    let again = admitted(g.admit(p, s, &ids(0..10_000), at(1)));
    assert!(again.new_series.is_empty() && again.refused.is_empty());
}

#[test]
fn project_cap_refuses_only_new_series_and_other_projects_are_unaffected() {
    let g = SeriesGuard::new(5);
    let (p, q, s) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let r = admitted(g.admit(p, s, &ids(0..7), at(0)));
    assert_eq!(r.new_series, ids(0..5));
    assert_eq!(r.refused, [5u64, 6].into_iter().collect());
    let known_again = admitted(g.admit(p, s, &ids(0..5), at(1)));
    assert!(known_again.refused.is_empty());
    assert_eq!(admitted(g.admit(q, s, &ids(0..5), at(2))).new_series.len(), 5);
}

#[test]
fn forget_gives_room_and_rate_back() {
    let g = SeriesGuard::new(3);
    let (p, s) = (Uuid::new_v4(), Uuid::new_v4());
    let r = admitted(g.admit(p, s, &ids(0..3), at(0)));
    g.forget(&r, s, at(0));
    assert_eq!(g.known(p), 0);
    assert_eq!(admitted(g.admit(p, s, &ids(10..13), at(1))).new_series, ids(10..13));
}

#[test]
fn a_known_series_is_touched_once_per_day() {
    let g = SeriesGuard::new(10);
    let (p, s) = (Uuid::new_v4(), Uuid::new_v4());
    admitted(g.admit(p, s, &[1, 2], at(0)));
    assert!(admitted(g.admit(p, s, &[1, 2], at(60))).touched.is_empty());
    let next_day = at(86_400);
    assert_eq!(admitted(g.admit(p, s, &[1], next_day)).touched, vec![1]);
    assert!(admitted(g.admit(p, s, &[1], next_day + Duration::seconds(5))).touched.is_empty());
}
```

- [ ] **Step 2: Write the failing integration test** `tests/metrics_series_guard.rs`:

```rust
// The guard seeds itself from metric_series. Requires `docker compose up -d clickhouse`.
use std::collections::BTreeMap;
use chrono::{Duration, Utc};
use nami_engine::{clickhouse::ClickHouse, metrics::{series_guard::{Admission, SeriesGuard}, store::{self, SeriesRow}}};
use uuid::Uuid;

fn series_row(project: Uuid, id: u64, last_seen: chrono::DateTime<Utc>) -> SeriesRow {
    SeriesRow { project_id: project, source_id: Uuid::nil(), name: "m".into(), metric_type: "gauge",
        series_id: id, tags: BTreeMap::new(), last_seen, retention_days: 30 }
}

#[tokio::test]
async fn known_series_survive_a_restart() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, other) = (Uuid::new_v4(), Uuid::new_v4());
    let now = Utc::now();
    let mut rows: Vec<SeriesRow> = (0..9_000u64).map(|i| series_row(project, i, now - Duration::hours(3))).collect();
    rows.push(series_row(project, u64::MAX, now));                       // above 2^53: must round-trip
    rows.push(series_row(project, 77_777, now - Duration::days(3)));      // inactive: not loaded
    rows.push(series_row(other, 88_888, now));                            // another project
    store::insert_series(&ch, &rows).await.unwrap();

    let guard = SeriesGuard::new(50_000);                                  // a fresh instance
    guard.ensure_loaded(&ch, project, now).await.unwrap();
    assert_eq!(guard.known(project), 9_001);
    let mut sent: Vec<u64> = (0..9_000).collect();
    sent.push(u64::MAX);
    match guard.admit(project, Uuid::new_v4(), &sent, now) {
        Admission::Admitted(r) => assert!(r.new_series.is_empty(), "nothing known may look new"),
        other => panic!("{other:?}"),
    }
    match guard.admit(project, Uuid::new_v4(), &[77_777, 88_888], now) {
        Admission::Admitted(r) => assert_eq!(r.new_series, vec![77_777, 88_888]),
        other => panic!("{other:?}"),
    }
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn the_cap_counts_series_loaded_from_clickhouse() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let project = Uuid::new_v4();
    store::insert_series(&ch, &(0..5u64).map(|i| series_row(project, i, Utc::now())).collect::<Vec<_>>()).await.unwrap();
    let guard = SeriesGuard::new(5);
    guard.ensure_loaded(&ch, project, Utc::now()).await.unwrap();
    match guard.admit(project, Uuid::new_v4(), &[99], Utc::now()) {
        Admission::Admitted(r) => assert_eq!(r.refused, [99u64].into_iter().collect()),
        other => panic!("{other:?}"),
    }
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 3: Run to fail**

Run: engine command with `--lib -- metrics::series_guard`, then `--test metrics_series_guard`
Expected: FAIL to compile.

- [ ] **Step 4: Implement `SeriesGuard`** per the rule above.

- [ ] **Step 5: Run to pass** (both commands from Step 3).

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src/metrics nami-engine/tests/metrics_series_guard.rs
git commit -m "feat(engine): per-source new-series rate and per-project series cap (NAMI-MET-3)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4 (NAMI-MET-4): Ingest endpoints — JSON and Prometheus text

Branch `feat/NAMI-MET-4-metrics-ingest`, worktree `.worktrees/NAMI-MET-4/`.

**Files:**
- Create: `nami-engine/src/metrics/ingest.rs`, `nami-engine/src/routes/metrics_ingest.rs`, `nami-engine/tests/common/metrics.rs`, `nami-engine/tests/metrics_ingest.rs`
- Modify: `nami-engine/src/metrics/mod.rs` (`MetricsState`), `nami-engine/src/state.rs` (`AppState.metrics`, `for_test`), `nami-engine/src/main.rs` (build `MetricsState` from config), `nami-engine/src/config.rs` (`metrics_max_series_per_project`), `nami-engine/src/logs/ingest.rs` (`gunzip_within`, `IngestError::CardinalityExceeded`), `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs` (`build_public_router_with_limits` merges `routes::metrics_ingest::public_router(ingest_limits.clone())`), `nami-engine/tests/common/mod.rs` (`pub mod metrics;`)

**Interfaces:**
- Consumes: T1 `validate`, `store::{encode_metric_rows, insert_encoded, insert_series, SeriesRow}`; T2 `prom_text::parse`; T3 `SeriesGuard`; Logs' `IngestLimits`, `bearer_token`, `read_body`, `BodyPace`, `IngestError`, `DecodeShares`, `TokenBuckets`, `InFlight`, `source::find_by_token`.
- Produces:

```rust
// metrics/mod.rs
pub const MAX_METRIC_QUERIES_PER_USER: usize = 6;       // used from T5
pub const MAX_METRIC_QUERIES_PER_INSTANCE: usize = 32;  // used from T5
pub struct MetricsState {
    pub rate_limiter: crate::logs::rate_limit::TokenBuckets,  // 50/s, burst 100 per source
    pub in_flight: crate::logs::rate_limit::InFlight,         // 4 per source
    pub project_in_flight: crate::logs::rate_limit::InFlight, // 8 per project
    pub series: series_guard::SeriesGuard,
    pub query_slots: std::sync::Arc<crate::logs::slots::UserSlots>,
    pub query_total: std::sync::Arc<tokio::sync::Semaphore>,
}
impl MetricsState { pub fn new(max_series_per_project: usize) -> MetricsState }

// metrics/ingest.rs
pub const METRICS_MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SAMPLES_PER_REQUEST: usize = 50_000;
pub const MAX_REPORTED_ERRORS: usize = 10;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsFormat { Json, PrometheusText }
pub fn body_format(headers: &HeaderMap, expected: MetricsFormat) -> Result<bool /* gzip */, IngestError>;
/// Inflated body → samples (400 for unparsable, 413 for too many).
pub fn parse_json(raw: &[u8]) -> Result<Vec<Result<Sample, Rejection>>, IngestError>;
pub fn parse_prometheus(raw: &[u8]) -> Result<Vec<Result<Sample, Rejection>>, IngestError>;
/// Validation, admission and encoding of one request (runs on a blocking thread).
pub struct Built { pub body: String, pub accepted: usize, pub rejected: usize, pub errors: Vec<String>,
    pub series_rows: Vec<SeriesRow>, pub reservation: Option<Reservation> }
pub fn validate_body(samples: Vec<Result<Sample, Rejection>>, source: &IngestSource,
    received_at: DateTime<Utc>, guard: &SeriesGuard) -> Result<Built, IngestError>;

// config.rs
pub metrics_max_series_per_project: usize,   // METRICS_MAX_SERIES_PER_PROJECT, default 50_000, positive

// logs/ingest.rs
pub fn gunzip_within(compressed: &[u8], limit: usize) -> Result<Vec<u8>, IngestError>;   // gunzip = gunzip_within(.., MAX_DECOMPRESSED_BYTES)
IngestError::CardinalityExceeded { retry_after_seconds: u64 }
// → 429 {"error": "too many new series for this source: at most 10000 per minute", "code": "cardinality_exceeded"}, Retry-After

// routes/metrics_ingest.rs
pub fn public_router(limits: IngestLimits) -> Router<AppState>;   // POST /api/v1/metrics, POST /api/v1/metrics/prometheus

// tests/common/metrics.rs
pub async fn post_metrics(public: &Router, token: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value);
pub async fn post_prometheus(public: &Router, token: &str, text: &str) -> (StatusCode, serde_json::Value);
pub async fn post_raw(public: &Router, path: &str, token: Option<&str>, headers: &[(&str, &str)], body: Vec<u8>)
    -> (StatusCode, axum::http::HeaderMap, serde_json::Value);
pub async fn stored(ch: &ClickHouse, project_id: Uuid) -> Vec<serde_json::Value>;  // name, metric_type, tags, value, source_id ORDER BY name, dt
```

Handler order (each check cheaper than the next; the same two endpoints share one function with a `MetricsFormat` argument): token shape (401, no query) → a place of `limits.places` (503 `busy`) → token lookup under `limits.queries` (401) → `metrics.rate_limiter` (429 `rate_limited`) → `metrics.in_flight` per source and `metrics.project_in_flight` per project (429 `too_many_in_flight`) → `body_format` (415) → `read_body` (413/408) → `metrics.series.ensure_loaded(project)` (a ClickHouse failure → 500, logged with the source id) → a decode place (`limits.decodes` plus `state.logs.decode_shares` and `state.logs.project_limits.decode_shares`, acquired in the order `ingest::prepare` uses) → `spawn_blocking`: inflate (`gunzip_within(.., METRICS_MAX_DECOMPRESSED_BYTES)`, also checking an uncompressed body against that cap), UTF-8 check for Prometheus (400 `the body is not UTF-8`), parse, `validate_body` → if `reservation` is `None` and the guard said `RateLimited` → `CardinalityExceeded` → insert `series_rows` (if any), then the rows → on any insert error `series.forget(..)`, 500 → `202` or (`accepted == 0 && rejected > 0`) `400`, body `{"accepted", "rejected", "errors"}`. The work after `read_body` runs in a spawned task holding the place and both in-flight guards, as `routes::logs_ingest::Work` does, so a client that hangs up cannot free them early.

`validate_body`: parser rejections and `sample::validate` rejections are counted in order (`sample <index>: <message>`, first 10 kept). The distinct series ids of the valid rows go to `guard.admit` in first-seen order. `RateLimited` → `Err(IngestError::CardinalityExceeded { .. })`. Rows of `refused` series become `Rejection::SeriesLimit`. `series_rows` = one `SeriesRow::from_row` (with `last_seen = received_at`) per id in `new_series ∪ touched`, from the first row of that series. `body` = `encode_metric_rows` (413 `the stored rows would exceed 64 MiB` on `TooLarge`, after `guard.forget`).

`parse_json`: the body must be a JSON object or an array of values (else 400 `expected a JSON object or an array of objects`); more than `MAX_SAMPLES_PER_REQUEST` elements → 413 `more than 50000 samples`. Each element: not an object → `Rejection::InvalidValue`; keys other than `name`, `value`, `type`, `dt`, `tags` → `UnknownField(key)`; `name` not a string → `InvalidName`; `value` not a JSON number → `InvalidValue`; `type` not `"gauge"`/`"counter"` → `InvalidValue`; `dt` through `normalize::parse_timestamp` (`None` → `InvalidTimestamp`); `tags` not an object → `InvalidTagKey("tags")`, a non-string value → `TagValueNotString(key)`. `parse_prometheus` maps `ParseError::Syntax`/`LineTooLong` to 400 with the error's text (`LineTooLong` → 413), `TooManySamples` → 413, `TooManyTypes` → 400.

- [ ] **Step 1: Write the failing tests** `tests/metrics_ingest.rs`:

```rust
// POST /api/v1/metrics and /api/v1/metrics/prometheus on the public listener.
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::io::Write;
use axum::http::StatusCode;
use common::{logs::seed_source, metrics::{post_metrics, post_prometheus, post_raw, stored}, public_app, seed_project, state_with_clickhouse};
use nami_engine::state::AppState;
use serde_json::{json, Value};

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

#[sqlx::test]
async fn json_object_and_array_are_stored_under_the_tokens_source(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "app").await;
    let (status, body) = post_metrics(&public, &token,
        json!({ "name": "queue_depth", "value": 3, "tags": { "queue": "mail" } })).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 1, "rejected": 0, "errors": [] })));
    let (status, _) = post_metrics(&public, &token, json!([
        { "name": "jobs_total", "value": 10, "type": "counter", "dt": chrono::Utc::now().to_rfc3339() },
        { "name": "jobs_total", "value": 12, "type": "counter", "project_id": "ignored?" }
    ])).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rows = stored(&ch, project).await;
    assert_eq!(rows.len(), 2, "the sample with an unknown field is rejected, not stored");
    assert!(rows.iter().all(|r| r["source_id"] == source.to_string()));
    assert_eq!(rows[1]["tags"]["queue"], "mail");
    assert_eq!(rows[0]["metric_type"], "counter");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn bad_samples_are_rejected_one_by_one(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "app").await;
    let many_tags: serde_json::Map<String, Value> = (0..51).map(|i| (format!("k{i}"), json!("v"))).collect();
    let (status, body) = post_metrics(&public, &token, json!([
        { "name": "ok", "value": 1 },
        { "name": "bad-name", "value": 1 },
        { "name": "nan", "value": "NaN" },
        { "name": "wide", "value": 1, "tags": many_tags },
        { "name": "dunder", "value": 1, "tags": { "__name__": "x" } },
        { "name": "num_tag", "value": 1, "tags": { "code": 200 } },
        { "name": "x'); DROP TABLE metrics; --", "value": 1 },
        { "name": "old", "value": 1, "dt": "2000-01-01T00:00:00Z" }
    ])).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["accepted"], 1);
    assert_eq!(body["rejected"], 7);
    let errors: Vec<&str> = body["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    assert_eq!(errors, vec![
        "sample 1: invalid metric name", "sample 2: value must be a number", "sample 3: more than 50 tags",
        "sample 4: reserved tag key \"__name__\"", "sample 5: tag value must be a string (key \"code\")",
        "sample 6: invalid metric name", "sample 7: timestamp out of range",
    ]);
    // Every sample bad → 400 with the same body shape.
    let (status, body) = post_metrics(&public, &token, json!([{ "name": "1x", "value": 1 }])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["accepted"], 0);
    assert_eq!(stored(&ch, project).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn prometheus_text_is_stored_with_types_and_literal_label_values(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "node").await;
    let text = "# TYPE req counter\nreq_total{path=\"/a'; DROP TABLE metrics; --\",code=\"200\"} 5\nload1 0.25\nbad NaN\n";
    let (status, body) = post_prometheus(&public, &token, text).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!((body["accepted"].as_u64(), body["rejected"].as_u64()), (Some(2), Some(1)));
    let rows = stored(&ch, project).await;
    let req = rows.iter().find(|r| r["name"] == "req_total").unwrap();
    assert_eq!(req["metric_type"], "counter");
    assert_eq!(req["tags"]["path"], "/a'; DROP TABLE metrics; --");
    let (status, body) = post_prometheus(&public, &token, "ok 1\nbroken{a=\"x} 1\n").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "line 2: unterminated label value");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn auth_content_type_and_size_limits(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "app").await;
    let json_ct = [("content-type", "application/json")];
    for bad in [None, Some("not-a-token"), Some("A".repeat(43).as_str())] {
        let (status, headers, body) = post_raw(&public, "/api/v1/metrics", bad, &json_ct, b"{}".to_vec()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(headers["www-authenticate"], "Bearer");
        assert_eq!(body, json!({ "error": "unauthorized" }));
    }
    let (status, ..) = post_raw(&public, "/api/v1/metrics", Some(&token), &[("content-type", "text/plain")], b"m 1".to_vec()).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (status, ..) = post_raw(&public, "/api/v1/metrics/prometheus", Some(&token), &json_ct, b"{}".to_vec()).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // Over 50,000 samples → 413, nothing stored.
    let text = "m 1\n".repeat(50_001);
    let (status, ..) = post_raw(&public, "/api/v1/metrics/prometheus", Some(&token),
        &[("content-type", "text/plain; version=0.0.4")], text.into_bytes()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // A line over 64 KiB → 413.
    let line = format!("m{{a=\"{}\"}} 1\n", "x".repeat(70_000));
    let (status, ..) = post_raw(&public, "/api/v1/metrics/prometheus", Some(&token),
        &[("content-type", "text/plain")], line.into_bytes()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // A gzip bomb (17 MiB of zeros) → 413; a small gzip body works.
    let bomb = gzip(&vec![b' '; 17 * 1024 * 1024]);
    let (status, ..) = post_raw(&public, "/api/v1/metrics", Some(&token),
        &[("content-type", "application/json"), ("content-encoding", "gzip")], bomb).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, ..) = post_raw(&public, "/api/v1/metrics", Some(&token),
        &[("content-type", "application/json"), ("content-encoding", "gzip")],
        gzip(br#"{"name":"z","value":1}"#)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // Not UTF-8 → 400; malformed JSON → 400.
    let (status, ..) = post_raw(&public, "/api/v1/metrics/prometheus", Some(&token),
        &[("content-type", "text/plain")], vec![b'm', b' ', 0xff, b'\n']).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, ..) = post_raw(&public, "/api/v1/metrics", Some(&token), &json_ct, b"{\"name\":".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(stored(&ch, project).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn ingest_is_only_on_the_public_listener(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let (status, _) = common::call(&app, "POST", "/api/v1/metrics", None, Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn ten_thousand_and_one_new_series_in_a_minute_is_a_429_and_stores_nothing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "app").await;
    let text: String = (0..10_001).map(|i| format!("m{{i=\"{i}\"}} 1\n")).collect();
    let (status, headers, body) = post_raw(&public, "/api/v1/metrics/prometheus", Some(&token),
        &[("content-type", "text/plain")], text.into_bytes()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "cardinality_exceeded");
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry));
    assert!(stored(&ch, project).await.is_empty());
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_full_project_rejects_new_series_but_keeps_ingesting_known_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState { metrics: std::sync::Arc::new(nami_engine::metrics::MetricsState::new(5)), ..state };
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "app").await;
    let text: String = (0..7).map(|i| format!("m{{i=\"{i}\"}} 1\n")).collect();
    let (status, body) = post_prometheus(&public, &token, &text).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!((body["accepted"].as_u64(), body["rejected"].as_u64()), (Some(5), Some(2)));
    assert_eq!(body["errors"][0], "sample 5: series limit reached");
    let (status, body) = post_prometheus(&public, &token, "m{i=\"0\"} 2\n").await;
    assert_eq!((status, body["accepted"].as_u64()), (StatusCode::ACCEPTED, Some(1)));
    let series: Vec<Value> = ch.query_rows(
        "SELECT count() AS n FROM metric_series WHERE project_id = {p:UUID}", &[("p", project.to_string())]).await.unwrap();
    assert_eq!(series[0]["n"], 5);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_rate_limit_is_per_source_and_separate_from_logs(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "app").await;
    while state.metrics.rate_limiter.try_acquire(source) {}
    let (status, body) = post_metrics(&public, &token, json!({ "name": "m", "value": 1 })).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));
    let (status, _) = common::logs::post_json(&public, &token, json!({ "message": "still fine" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "log ingest has its own bucket");
    ch.drop_database().await.unwrap();
}
```

Note: the first test's second array element carries an unknown key `project_id`; it is rejected (R5), which also shows a body cannot choose the project.

- [ ] **Step 2: Run to fail**

Run: engine command with `--test metrics_ingest`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** `MetricsState`, `config.metrics_max_series_per_project` (`env_positive("METRICS_MAX_SERIES_PER_PROJECT", DEFAULT_MAX_SERIES_PER_PROJECT)` in `from_env`, the default in `from_env_defaults_only`), `AppState.metrics` (`for_test`: `MetricsState::new(config.metrics_max_series_per_project)`; `main.rs` the same), `gunzip_within`, `IngestError::CardinalityExceeded`, `metrics::ingest`, `routes::metrics_ingest`, the router merge, and `tests/common/metrics.rs`. Update the `build_public_router` doc comment to list the metrics endpoints.

- [ ] **Step 4: Run to pass**

Run: engine command with `--test metrics_ingest --test logs_ingest --test startup`
Expected: PASS (logs ingest unchanged).

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src nami-engine/tests/common nami-engine/tests/metrics_ingest.rs
git commit -m "feat(engine): JSON and Prometheus text metric ingest with per-sample rejection (NAMI-MET-4)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 5 (NAMI-MET-5): Query API — validation, SQL, budgets

Branch `feat/NAMI-MET-5-metrics-query`, worktree `.worktrees/NAMI-MET-5/`.

**Files:**
- Create: `nami-engine/src/metrics/query.rs`, `nami-engine/src/metrics/sql.rs`, `nami-engine/src/routes/metrics.rs`, `nami-engine/tests/metrics_query.rs`
- Modify: `nami-engine/src/metrics/mod.rs` (`QuerySlot`, `acquire_query_slot`), `nami-engine/src/metrics/series_guard.rs` (use `sql::METRIC_READ_SETTINGS`), `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs` (`.merge(routes::metrics::router())`), `nami-engine/tests/common/metrics.rs` (`seed_samples`, `query`), `nami-engine/tests/json_bodies.rs` (`POST /projects/{id}/metrics/query`)

**Interfaces:**
- Consumes: T1 `is_valid_name`, `is_valid_tag_key`, `METRICS_TABLE`; T4 `MetricsState`; `logs::query::Sql`; `logs::search::user_error`; `require_project_role`.
- Produces:

```rust
// metrics/query.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Aggregation { Avg, Min, Max, Sum, Count, P50, P95, P99, Rate }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum FilterOp { Eq, Neq, Regex }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct FilterDef { pub tag: String, pub op: FilterOp, pub value: String }
/// The stored shape of a query (panels keep this; no time range).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct QueryDef { pub metric: String, pub aggregation: Aggregation,
    #[serde(default)] pub filters: Vec<FilterDef>, #[serde(default)] pub group_by: Vec<String> }
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
pub struct QueryRequest { pub metric: String, pub aggregation: Aggregation,
    #[serde(default)] pub filters: Vec<FilterDef>, #[serde(default)] pub group_by: Vec<String>,
    pub from: DateTime<Utc>, pub to: DateTime<Utc>, #[serde(default)] pub step: Option<i64> }
impl QueryRequest { pub fn def(&self) -> QueryDef }
/// A filter ready to bind: one value, or several for eq/neq after substitution (T9).
#[derive(Debug, Clone, PartialEq)] pub struct Filter { pub tag: String, pub op: FilterOp, pub values: Vec<String> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Window { pub from_ms: i64, pub to_ms: i64, pub step_ms: i64 }
impl Window { pub fn aligned_from_ms(&self) -> i64; pub fn buckets(&self) -> i64 }
#[derive(Debug, Clone, PartialEq)] pub enum Target { Stored { name: String } }   // T7 adds built-ins
#[derive(Debug, Clone, PartialEq)] pub struct ValidQuery { pub target: Target, pub aggregation: Aggregation,
    pub filters: Vec<Filter>, pub group_by: Vec<String>, pub window: Window }

pub const MAX_RANGE: chrono::Duration = chrono::Duration::days(90);
pub const MAX_FUTURE: chrono::Duration = chrono::Duration::minutes(5);
pub const MIN_STEP_SECONDS: i64 = 10;
pub const MAX_STEP_SECONDS: i64 = 86_400;
pub const AUTO_STEPS: [i64; 15] = [10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400];
pub const AUTO_TARGET_POINTS: i64 = 120;
pub const MAX_POINTS_PER_SERIES: i64 = 1_500;
pub const MAX_FILTERS: usize = 10;
pub const MAX_GROUP_BY: usize = 5;
pub const MAX_FILTER_VALUES: usize = 100;
pub const MAX_VALUE_BYTES: usize = 1024;
pub const MAX_REGEX_CHARS: usize = 256;
pub const REGEX_SIZE_LIMIT: usize = 256 * 1024;

pub fn plan_window(from: DateTime<Utc>, to: DateTime<Utc>, step: Option<i64>, now: DateTime<Utc>) -> Result<Window, String>;
pub fn auto_step(span_ms: i64) -> i64;
/// Shape checks that need no time range (also used by dashboards, T8).
pub fn validate_def(def: &QueryDef) -> Result<Target, String>;
/// Each FilterDef as a one-value Filter (T9 replaces this with substitution on the query routes).
pub fn literal_filters(def: &QueryDef) -> Vec<Filter>;
pub fn validate(def: &QueryDef, filters: Vec<Filter>, window: Window) -> Result<ValidQuery, String>;

// metrics/sql.rs
pub const MAX_SERIES: usize = 50;
pub const METRIC_READ_SETTINGS: [(&str, &str); 7] = [
    ("max_result_bytes", "33554432"), ("result_overflow_mode", "throw"),
    ("max_rows_to_group_by", "1000000"), ("group_by_overflow_mode", "throw"),
    ("max_rows_to_read", "2000000000"), ("read_overflow_mode", "throw"),
    ("max_memory_usage", "2147483648"),
];
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series { pub tags: BTreeMap<String, String>, pub points: Vec<(i64, Option<f64>)> }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryResult { pub step_seconds: i64, pub from: DateTime<Utc>, pub to: DateTime<Utc>,
    pub truncated: bool, pub series: Vec<Series> }
pub fn build(project_id: Uuid, q: &ValidQuery) -> Sql;
pub async fn run(state: &AppState, project_id: Uuid, q: &ValidQuery) -> Result<QueryResult, AppError>;
pub fn user_error(error: ClickHouseError) -> AppError;

// metrics/mod.rs
pub struct QuerySlot { /* UserPermit + OwnedSemaphorePermit */ }
pub fn acquire_query_slot(metrics: &MetricsState, user_id: Uuid) -> Result<QuerySlot, AppError>;  // 429 on either cap

// routes/metrics.rs
pub fn router() -> Router<AppState>;   // POST /projects/{id}/metrics/query (viewer)
```

`run` takes `&AppState` (not just the ClickHouse client) because T7's built-ins resolve names in Postgres.

`validate_def` order: `metric` is a valid name (400 `invalid metric name`) → aggregation allowed for the target (always for stored) → `filters.len() <= 10` → `group_by.len() <= 5`, each a valid tag key, no duplicates → each filter: valid tag key; value at most 1,024 bytes; for `regex`, at most 256 characters and compiles with `regex::RegexBuilder::new(v).size_limit(REGEX_SIZE_LIMIT).build()` (400 `a filter regex is not valid`; never echo the pattern). `validate` repeats the per-filter checks on the substituted `Filter`s (values count 1..=100, `regex` exactly one value, each value's size), then builds `ValidQuery`. Messages are fixed texts naming the field (`filters[3]: ...`, `group_by[1]: invalid tag key`).

`plan_window`: `from < to` (400 `from must be before to`), `to <= now + MAX_FUTURE`, `to − from <= MAX_RANGE` (400 `the range may be at most 90 days`). Explicit step: `MIN_STEP_SECONDS..=MAX_STEP_SECONDS` (400 `step must be between 10 and 86400 seconds`), buckets `<= MAX_POINTS_PER_SERIES` (400 `the range and step give more than 1500 points`). Auto step per R11. `buckets()` = `(floor(to/step) − floor(from/step)) + 1`.

**The SQL** (`build`; decides behaviour). `<BASE>` appears once per statement; each value is bound once and its placeholder reused where an expression repeats (the step, the group keys). `<G>` is the group expression: `emptyArrayString()` with no group-by, else `[tags[{pK1:String}], tags[{pK2:String}]]`. For T5 the target fragments are `TABLE = metrics`, `TIME = dt`, `VALUE = value`, `SERIES = series_id`; T7 adds others.

```sql
-- <BASE>
 WHERE project_id = {pP:UUID} AND name = {pN:String}
   AND dt >= fromUnixTimestamp64Milli({pF:Int64}, 'UTC')      -- the aligned from
   AND dt <= fromUnixTimestamp64Milli({pT:Int64}, 'UTC')
   AND tags[{pK:String}] = {pV:String}                         -- eq, one value
   AND tags[{pK:String}] != {pV:String}                        -- neq, one value
   AND match(tags[{pK:String}], {pV:String})                   -- regex
   AND tags[{pK:String}] IN ({pV1:String}, {pV2:String})       -- eq, several values
   AND tags[{pK:String}] NOT IN ({pV1:String}, {pV2:String})   -- neq, several values

-- avg | min | max | sum | count | p50 | p95 | p99
SELECT r, g, bucket_ms, v FROM (
  SELECT g, bucket_ms, v, dense_rank() OVER (ORDER BY mag DESC, g ASC) AS r FROM (
    SELECT g, bucket_ms, v, sum(babs) OVER (PARTITION BY g) AS mag FROM (
      SELECT <G> AS g,
             intDiv(toUnixTimestamp64Milli(dt), {pS:Int64}) * {pS:Int64} AS bucket_ms,
             <AGG> AS v,
             sum(abs(value)) AS babs
      FROM metrics <BASE>
      GROUP BY g, bucket_ms)))
WHERE r <= 51
ORDER BY r, bucket_ms

-- rate: the innermost SELECT becomes
      SELECT <G> AS g, bucket_ms, sum(delta) / {pSS:Float64} AS v, sum(abs(delta)) AS babs FROM (
        SELECT tags, intDiv(toUnixTimestamp64Milli(dt), {pS:Int64}) * {pS:Int64} AS bucket_ms,
               if(row_number() OVER w = 1, 0,
                  if(value >= lagInFrame(value) OVER w, value - lagInFrame(value) OVER w, value)) AS delta
        FROM metrics <BASE>
        WINDOW w AS (PARTITION BY series_id ORDER BY dt ROWS BETWEEN 1 PRECEDING AND CURRENT ROW))
      GROUP BY g, bucket_ms
```

`<AGG>`: `avg(value)`, `min(value)`, `max(value)`, `sum(value)`, `toFloat64(count())`, `quantile(0.5)(value)`, `quantile(0.95)(value)`, `quantile(0.99)(value)` (fixed fragments; `pSS` = step in seconds). Rows decode as `{ r: u64, g: Vec<String>, bucket_ms: i64, v: Option<f64> }` under `METRIC_READ_SETTINGS`. Rust then: rank 51 present → `truncated = true`, dropped; each group's points cover every bucket from `aligned_from_ms` to `floor(to_ms / step) * step` with `None` where no row came back; `tags` = `group_by` zipped with `g`; order by `r`.

`user_error` (R10): match on the ClickHouse body for `TOO_MANY_ROWS` (also covers `TOO_MANY_ROWS_OR_BYTES`), `MEMORY_LIMIT_EXCEEDED`, `ResponseTooLarge` → 400 `the query is too expensive: narrow the time range or add filters`; everything else → `logs::search::user_error` (regex and timeout already map there).

Route order: `require_project_role(viewer)` → `plan_window` + `validate_def` + `validate` (400) → `acquire_query_slot` (429) → `sql::run` → `Json(QueryResult)`.

- [ ] **Step 1: Write the failing unit tests** in `metrics/query.rs`:

```rust
#[test]
fn auto_step_targets_about_120_points() {
    assert_eq!(auto_step(60 * 60 * 1000), 30_000);            // 1 h → 30 s, 121 buckets at most
    assert_eq!(auto_step(24 * 3600 * 1000), 900_000);         // 24 h → 15 min
    assert_eq!(auto_step(90 * 86_400 * 1000), 86_400_000);    // 90 d → 1 d
    assert_eq!(auto_step(1000), 10_000);                      // never below 10 s
}

#[test]
fn window_caps() {
    let now: DateTime<Utc> = "2026-10-19T12:00:00Z".parse().unwrap();
    let h = |n: i64| now - chrono::Duration::hours(n);
    assert!(plan_window(h(1), h(2), None, now).is_err());
    assert!(plan_window(h(24 * 91), now, None, now).is_err());
    assert!(plan_window(h(24 * 90), now, None, now).is_ok());
    assert!(plan_window(h(1), now + chrono::Duration::minutes(6), None, now).is_err());
    assert!(plan_window(h(1), now, Some(9), now).is_err());
    assert!(plan_window(h(1), now, Some(86_401), now).is_err());
    assert!(plan_window(h(24), now, Some(10), now).is_err(), "8641 points");
    assert!(plan_window(h(4), now, Some(10), now).is_ok(), "1441 points");
}

#[test]
fn definition_caps_and_charsets() {
    let def = |f: serde_json::Value| serde_json::from_value::<QueryDef>(f).unwrap();
    let ok = def(serde_json::json!({ "metric": "m", "aggregation": "avg" }));
    assert!(validate_def(&ok).is_ok());
    let bad = [
        serde_json::json!({ "metric": "1m", "aggregation": "avg" }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "group_by": ["a","b","c","d","e","f"] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "group_by": ["a","a"] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "group_by": ["x'); DROP"] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "a b", "op": "eq", "value": "x" }] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "a", "op": "eq", "value": "v".repeat(1025) }] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "a", "op": "regex", "value": "a".repeat(257) }] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "a", "op": "regex", "value": "(" }] }),
        serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "a", "op": "regex", "value": "a{1000}{1000}" }] }),
    ];
    for b in bad { assert!(validate_def(&def(b.clone())).is_err(), "{b}"); }
    let eleven: Vec<_> = (0..11).map(|i| serde_json::json!({ "tag": format!("t{i}"), "op": "eq", "value": "x" })).collect();
    assert!(validate_def(&def(serde_json::json!({ "metric": "m", "aggregation": "avg", "filters": eleven }))).is_err());
}
```

- [ ] **Step 2: Write the failing integration tests** `tests/metrics_query.rs`. Add to `tests/common/metrics.rs`:

```rust
/// Inserts samples directly: (name, tags, seconds after `base`, value) under `source` of `project`.
pub async fn seed_samples(ch: &ClickHouse, project: Uuid, source: Uuid, base: DateTime<Utc>,
    samples: &[(&str, &[(&str, &str)], i64, f64)]);
/// POST /projects/{project}/metrics/query as `cookie`.
pub async fn query(app: &Router, cookie: &HeaderValue, project: Uuid, body: Value) -> (StatusCode, Value);
```

```rust
// POST /projects/{id}/metrics/query. Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, DurationRound, SecondsFormat, Utc};
use common::{app, metrics::{query, seed_samples}, seed_project, state_with_clickhouse, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

fn ts(dt: DateTime<Utc>) -> String { dt.to_rfc3339_opts(SecondsFormat::Millis, true) }
fn base() -> DateTime<Utc> { (Utc::now() - Duration::hours(2)).duration_trunc(Duration::hours(1)).unwrap() }
fn points(series: &Value) -> Vec<Option<f64>> {
    series["points"].as_array().unwrap().iter().map(|p| p[1].as_f64()).collect()
}

struct Fx { app: axum::Router, ch: nami_engine::clickhouse::ClickHouse, project: Uuid, viewer: axum::http::HeaderValue, base: DateTime<Utc> }

async fn fixture(pool: &sqlx::PgPool) -> Fx {
    let (state, ch) = state_with_clickhouse(pool).await;
    let app = app(state);
    let project = seed_project(pool).await;
    let (_, _, viewer) = user_with_role(&app, pool, project, Some("viewer")).await;
    Fx { app, ch, project, viewer, base: base() }
}

#[sqlx::test]
async fn aggregations_over_buckets(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let s = Uuid::new_v4();
    // bucket 0 (step 60): 1, 2, 9 ; bucket 1: 4 ; bucket 2: empty ; bucket 3: 6
    seed_samples(&f.ch, f.project, s, f.base, &[("lat", &[("h", "a")], 0, 1.0), ("lat", &[("h", "b")], 10, 2.0),
        ("lat", &[("h", "a")], 20, 9.0), ("lat", &[("h", "a")], 70, 4.0), ("lat", &[("h", "a")], 190, 6.0)]).await;
    let run = |agg: &str| { let (app, viewer, project, base) = (f.app.clone(), f.viewer.clone(), f.project, f.base);
        let agg = agg.to_string();
        async move { query(&app, &viewer, project, json!({ "metric": "lat", "aggregation": agg,
            "from": ts(base), "to": ts(base + Duration::seconds(239)), "step": 60 })).await } };
    for (agg, want) in [("avg", [Some(4.0), Some(4.0), None, Some(6.0)]), ("min", [Some(1.0), Some(4.0), None, Some(6.0)]),
        ("max", [Some(9.0), Some(4.0), None, Some(6.0)]), ("sum", [Some(12.0), Some(4.0), None, Some(6.0)]),
        ("count", [Some(3.0), Some(1.0), None, Some(1.0)]), ("p50", [Some(2.0), Some(4.0), None, Some(6.0)])] {
        let (status, body) = run(agg).await;
        assert_eq!(status, StatusCode::OK, "{agg}: {body}");
        assert_eq!(body["step_seconds"], 60);
        assert_eq!(body["series"].as_array().unwrap().len(), 1);
        assert_eq!(points(&body["series"][0]), want.to_vec(), "{agg}");
    }
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn empty_buckets_are_null(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    seed_samples(&f.ch, f.project, Uuid::new_v4(), f.base, &[("g", &[], 0, 1.0), ("g", &[], 300, 2.0)]).await;
    let (_, body) = query(&f.app, &f.viewer, f.project, json!({ "metric": "g", "aggregation": "max",
        "from": ts(f.base), "to": ts(f.base + Duration::seconds(359)), "step": 60 })).await;
    assert_eq!(points(&body["series"][0]), vec![Some(1.0), None, None, None, None, Some(2.0)]);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn buckets_cover_the_range_aligned_to_the_step(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let from = f.base + Duration::seconds(7);
    let to = from + Duration::hours(1);
    seed_samples(&f.ch, f.project, Uuid::new_v4(), f.base, &[("g", &[], 1, 1.0), ("g", &[], 3607, 2.0)]).await;
    let (_, body) = query(&f.app, &f.viewer, f.project, json!({ "metric": "g", "aggregation": "max",
        "from": ts(from), "to": ts(to), "step": 60 })).await;
    let pts = body["series"][0]["points"].as_array().unwrap();
    assert_eq!(body["from"], ts(f.base));
    assert_eq!(pts.len(), 61);
    assert_eq!(pts[0][0].as_i64().unwrap(), f.base.timestamp_millis());
    assert_eq!(pts[60][0].as_i64().unwrap(), (f.base + Duration::hours(1)).timestamp_millis());
    assert_eq!((pts[0][1].as_f64(), pts[60][1].as_f64()), (Some(1.0), Some(2.0)));
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn rate_handles_a_counter_reset(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let s = Uuid::new_v4();
    seed_samples(&f.ch, f.project, s, f.base, &[("c", &[("i", "x")], 0, 0.0), ("c", &[("i", "x")], 10, 10.0),
        ("c", &[("i", "x")], 20, 20.0), ("c", &[("i", "x")], 30, 5.0), ("c", &[("i", "x")], 40, 15.0),
        ("c", &[("i", "y")], 0, 100.0), ("c", &[("i", "y")], 50, 160.0)]).await;
    let (_, body) = query(&f.app, &f.viewer, f.project, json!({ "metric": "c", "aggregation": "rate",
        "from": ts(f.base), "to": ts(f.base + Duration::seconds(59)), "step": 60 })).await;
    // x: 10 + 10 + 5 (reset) + 10 = 35 ; y: 60 ; summed: 95 per 60 s
    let v = points(&body["series"][0])[0].unwrap();
    assert!((v - 95.0 / 60.0).abs() < 1e-9, "{v}");
    let (_, grouped) = query(&f.app, &f.viewer, f.project, json!({ "metric": "c", "aggregation": "rate",
        "group_by": ["i"], "from": ts(f.base), "to": ts(f.base + Duration::seconds(59)), "step": 60 })).await;
    let by: std::collections::HashMap<String, f64> = grouped["series"].as_array().unwrap().iter()
        .map(|s| (s["tags"]["i"].as_str().unwrap().to_string(), points(s)[0].unwrap())).collect();
    assert!((by["x"] - 35.0 / 60.0).abs() < 1e-9 && (by["y"] - 1.0).abs() < 1e-9);
    assert!(by["x"] >= 0.0, "a reset never gives a negative rate");
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn more_than_50_series_are_truncated_to_the_largest(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let names: Vec<String> = (0..60).map(|i| format!("h{i:02}")).collect();
    let samples: Vec<(&str, Vec<(&str, &str)>, i64, f64)> = names.iter().enumerate()
        .map(|(i, n)| ("g", vec![("host", n.as_str())], 0, i as f64)).collect();
    let refs: Vec<(&str, &[(&str, &str)], i64, f64)> = samples.iter().map(|(m, t, s, v)| (*m, t.as_slice(), *s, *v)).collect();
    seed_samples(&f.ch, f.project, Uuid::new_v4(), f.base, &refs).await;
    let (_, body) = query(&f.app, &f.viewer, f.project, json!({ "metric": "g", "aggregation": "sum",
        "group_by": ["host"], "from": ts(f.base), "to": ts(f.base + Duration::seconds(59)), "step": 60 })).await;
    let series = body["series"].as_array().unwrap();
    assert_eq!(series.len(), 50);
    assert_eq!(body["truncated"], true);
    assert_eq!(series[0]["tags"]["host"], "h59");
    assert_eq!(series[49]["tags"]["host"], "h10");
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn filter_values_are_bound_literally_and_keys_are_checked(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let evil = "'; DROP TABLE metrics; -- {p0:String} back\\slash";
    seed_samples(&f.ch, f.project, Uuid::new_v4(), f.base, &[("g", &[("path", evil)], 0, 1.0),
        ("g", &[("path", "other")], 0, 2.0), ("g", &[("path", ".*")], 0, 3.0)]).await;
    let q = |filters: Value| json!({ "metric": "g", "aggregation": "sum", "filters": filters,
        "from": ts(f.base), "to": ts(f.base + Duration::seconds(59)), "step": 60 });
    let (_, eq) = query(&f.app, &f.viewer, f.project, q(json!([{ "tag": "path", "op": "eq", "value": evil }]))).await;
    assert_eq!(points(&eq["series"][0]), vec![Some(1.0)]);
    let (_, literal_dot_star) = query(&f.app, &f.viewer, f.project, q(json!([{ "tag": "path", "op": "eq", "value": ".*" }]))).await;
    assert_eq!(points(&literal_dot_star["series"][0]), vec![Some(3.0)]);
    let (_, re) = query(&f.app, &f.viewer, f.project, q(json!([{ "tag": "path", "op": "regex", "value": "^oth" }]))).await;
    assert_eq!(points(&re["series"][0]), vec![Some(2.0)]);
    for bad in [json!([{ "tag": evil, "op": "eq", "value": "x" }])] {
        let (status, _) = query(&f.app, &f.viewer, f.project, q(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let mut grouped = q(json!([]));
    grouped["group_by"] = json!([evil]);
    assert_eq!(query(&f.app, &f.viewer, f.project, grouped).await.0, StatusCode::BAD_REQUEST);
    let mut evil_metric = q(json!([]));
    evil_metric["metric"] = json!("g') OR 1=1 --");
    assert_eq!(query(&f.app, &f.viewer, f.project, evil_metric).await.0, StatusCode::BAD_REQUEST);
    // RE2 refuses lookbehind: a 400 from ClickHouse's error, not a 500.
    let (status, body) = query(&f.app, &f.viewer, f.project, q(json!([{ "tag": "path", "op": "regex", "value": "(?<=a)b" }]))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let tables: Vec<Value> = f.ch.query_rows("SELECT name FROM system.tables WHERE database = {d:String} AND name = 'metrics'",
        &[("d", f.ch.database().to_string())]).await.unwrap();
    assert_eq!(tables.len(), 1);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn another_projects_metrics_are_invisible_and_outsiders_are_refused(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let other = seed_project(&pool).await;
    seed_samples(&f.ch, f.project, Uuid::new_v4(), f.base, &[("shared_name", &[("who", "mine")], 0, 1.0)]).await;
    seed_samples(&f.ch, other, Uuid::new_v4(), f.base, &[("shared_name", &[("who", "theirs")], 0, 99.0)]).await;
    let body = json!({ "metric": "shared_name", "aggregation": "sum", "group_by": ["who"],
        "from": ts(f.base), "to": ts(f.base + Duration::seconds(59)), "step": 60 });
    let (_, mine) = query(&f.app, &f.viewer, f.project, body.clone()).await;
    let who: Vec<&str> = mine["series"].as_array().unwrap().iter().map(|s| s["tags"]["who"].as_str().unwrap()).collect();
    assert_eq!(who, vec!["mine"]);
    assert_eq!(query(&f.app, &f.viewer, other, body.clone()).await.0, StatusCode::FORBIDDEN);
    let (_, _, outsider) = user_with_role(&f.app, &pool, f.project, None).await;
    assert_eq!(query(&f.app, &outsider, f.project, body).await.0, StatusCode::FORBIDDEN);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn each_user_has_six_query_slots(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let held: Vec<_> = (0..6).map(|_| nami_engine::metrics::acquire_query_slot(&state.metrics, user).unwrap()).collect();
    let body = json!({ "metric": "g", "aggregation": "avg", "from": ts(base()), "to": ts(base() + Duration::minutes(1)) });
    assert_eq!(query(&app, &viewer, project, body.clone()).await.0, StatusCode::TOO_MANY_REQUESTS);
    drop(held);
    assert_eq!(query(&app, &viewer, project, body).await.0, StatusCode::OK);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn request_caps_are_400s(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let now = Utc::now();
    for body in [
        json!({ "metric": "g", "aggregation": "avg", "from": ts(now - Duration::days(91)), "to": ts(now) }),
        json!({ "metric": "g", "aggregation": "avg", "from": ts(now - Duration::days(1)), "to": ts(now), "step": 5 }),
        json!({ "metric": "g", "aggregation": "avg", "from": ts(now - Duration::days(1)), "to": ts(now), "step": 10 }),
        json!({ "metric": "g", "aggregation": "avg", "from": ts(now), "to": ts(now - Duration::hours(1)) }),
    ] {
        assert_eq!(query(&f.app, &f.viewer, f.project, body.clone()).await.0, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, _) = query(&f.app, &f.viewer, f.project, json!({ "metric": "g", "aggregation": "median",
        "from": ts(now - Duration::hours(1)), "to": ts(now) })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    f.ch.drop_database().await.unwrap();
}
```

Plus a unit test in `sql.rs`: `user_error_maps_budget_errors_to_400` (a `ClickHouseError::Server` body containing each of `TOO_MANY_ROWS_OR_BYTES`, `TOO_MANY_ROWS`, `MEMORY_LIMIT_EXCEEDED`, and a `ResponseTooLarge`, each → `AppError::Validation` with the R10 text; an unrelated body → `AppError::ClickHouse`), and `build_never_puts_values_in_the_sql_text` (build a query whose metric, tag keys, values and group-by keys are distinctive strings like `zz_metric`, `zz_key`, `zz_value`; assert none appears in `sql.text()` and all appear in `sql.params()`).

- [ ] **Step 3: Run to fail**

Run: engine command with `--lib -- metrics::query metrics::sql`, then `--test metrics_query`
Expected: FAIL to compile.

- [ ] **Step 4: Implement** `metrics::query`, `metrics::sql`, `acquire_query_slot`, the route, the router merge, the helpers, and the `json_bodies.rs` entry.

- [ ] **Step 5: Run to pass** (both commands from Step 3, plus `--test json_bodies --test metrics_series_guard`).

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src nami-engine/tests
git commit -m "feat(engine): metric query API with buckets, group-by, rate and query budgets (NAMI-MET-5)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6 (NAMI-MET-6): Discovery endpoints

Branch `feat/NAMI-MET-6-metrics-discovery`, worktree `.worktrees/NAMI-MET-6/`.

**Files:**
- Create: `nami-engine/tests/metrics_discovery.rs`
- Modify: `nami-engine/src/metrics/sql.rs` (`names`, `tag_keys`, `tag_values`), `nami-engine/src/routes/metrics.rs`

**Interfaces:**
- Consumes: T5 `METRIC_READ_SETTINGS`, `acquire_query_slot`, `user_error`; T1 `SERIES_TABLE`, `is_valid_name`, `is_valid_tag_key`.
- Produces:

```rust
pub const MAX_NAMES: usize = 1_000;
pub const MAX_TAG_KEYS: usize = 200;
pub const MAX_TAG_VALUES: usize = 500;
pub const MAX_DISCOVERY_Q_CHARS: usize = 200;
#[derive(Debug, Serialize, PartialEq)] pub struct MetricName { pub name: String, #[serde(rename = "type")] pub metric_type: String, pub builtin: bool }
pub async fn names(state: &AppState, project_id: Uuid, q: Option<&str>) -> Result<(Vec<MetricName>, bool), AppError>;
pub async fn tag_keys(state: &AppState, project_id: Uuid, metric: &str) -> Result<Vec<String>, AppError>;
pub async fn tag_values(state: &AppState, project_id: Uuid, metric: &str, tag: &str, q: Option<&str>) -> Result<(Vec<String>, bool), AppError>;
// routes: GET /projects/{id}/metrics/names?q=  → {"names": [...], "truncated": bool}
//         GET /projects/{id}/metrics/tags?metric=  → {"tags": [...]}
//         GET /projects/{id}/metrics/tag-values?metric=&tag=&q=  → {"values": [...], "truncated": bool}
```

SQL (all `FROM metric_series`, project bound from the path, the `LIMIT` one above the cap to detect truncation):

```sql
SELECT name, any(metric_type) AS type FROM metric_series
WHERE project_id = {p0:UUID} [AND positionCaseInsensitiveUTF8(name, {p1:String}) > 0]
GROUP BY name ORDER BY name LIMIT 1001

SELECT DISTINCT arrayJoin(mapKeys(tags)) AS tag FROM metric_series
WHERE project_id = {p0:UUID} AND name = {p1:String} ORDER BY tag LIMIT 200

SELECT DISTINCT tags[{p2:String}] AS value FROM metric_series
WHERE project_id = {p0:UUID} AND name = {p1:String} AND mapContains(tags, {p2:String})
  [AND positionCaseInsensitiveUTF8(tags[{p2:String}], {p3:String}) > 0]
ORDER BY value LIMIT 501
```

Validation (400): `metric` a valid name, `tag` a valid tag key, `q` at most 200 characters; a missing `metric`/`tag` is 400. Each call: viewer → validate → `acquire_query_slot` → query. Response `Cache-Control: no-store` is not needed (session API); leave defaults.

- [ ] **Step 1: Write the failing tests** `tests/metrics_discovery.rs`:

```rust
mod common;

use axum::http::StatusCode;
use common::{app, call, logs::seed_source, metrics::post_prometheus, public_app, seed_project, state_with_clickhouse, user_with_role};
use serde_json::json;

fn enc(s: &str) -> String { form_urlencoded::byte_serialize(s.as_bytes()).collect() }

#[sqlx::test]
async fn discovery_lists_only_the_projects_names_keys_and_values(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (app, public) = (app(state.clone()), public_app(state));
    let (project, other) = (seed_project(&pool).await, seed_project(&pool).await);
    let (_, token) = seed_source(&pool, project, "a").await;
    let (_, other_token) = seed_source(&pool, other, "b").await;
    post_prometheus(&public, &token, "# TYPE http_requests counter\nhttp_requests_total{path=\"/a'\",code=\"200\"} 1\nhttp_requests_total{path=\"/b\",code=\"500\"} 1\nload1{host=\"x\"} 1\n").await;
    post_prometheus(&public, &other_token, "secret_metric{tenant=\"theirs\"} 1\nload1{host=\"theirs\"} 1\n").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, body) = call(&app, "GET", &format!("/projects/{project}/metrics/names"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    let stored: Vec<&str> = body["names"].as_array().unwrap().iter()
        .filter(|n| n["builtin"] == false).map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(stored, vec!["http_requests_total", "load1"]);
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/metrics/names?q=LOAD"), Some(&viewer), None).await;
    assert_eq!(body["names"].as_array().unwrap().iter().filter(|n| n["builtin"] == false).count(), 1);
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/metrics/tags?metric=http_requests_total"), Some(&viewer), None).await;
    assert_eq!(body["tags"], json!(["code", "path"]));
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/metrics/tag-values?metric=load1&tag=host"), Some(&viewer), None).await;
    assert_eq!(body["values"], json!(["x"]), "another project's values never appear");
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/metrics/tag-values?metric=http_requests_total&tag=path&q={}", enc("'")), Some(&viewer), None).await;
    assert_eq!(body["values"], json!(["/a'"]));
    for bad in ["tags?metric=1bad", "tag-values?metric=load1&tag=a%20b", "tag-values?metric=load1", &format!("names?q={}", "q".repeat(201))] {
        let (status, _) = call(&app, "GET", &format!("/projects/{project}/metrics/{bad}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}/metrics/names"), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn tag_values_are_capped_and_flagged(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (app, public) = (app(state.clone()), public_app(state));
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "a").await;
    let text: String = (0..600).map(|i| format!("m{{pod=\"p{i:03}\"}} 1\n")).collect();
    post_prometheus(&public, &token, &text).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/metrics/tag-values?metric=m&tag=pod"), Some(&viewer), None).await;
    assert_eq!(body["values"].as_array().unwrap().len(), 500);
    assert_eq!(body["truncated"], true);
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 2: Run to fail** — engine command with `--test metrics_discovery`. Expected: FAIL (404 routes).

- [ ] **Step 3: Implement** the three functions and routes (built-ins come in T7; until then `names` returns stored names only, and the first test's `builtin == false` filter already allows for them).

- [ ] **Step 4: Run to pass** — same command.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src nami-engine/tests/metrics_discovery.rs
git commit -m "feat(engine): metric discovery endpoints for names, tag keys and tag values (NAMI-MET-6)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7 (NAMI-MET-7): Built-in metrics from uptime checks and log volume

Branch `feat/NAMI-MET-7-builtin-metrics`, worktree `.worktrees/NAMI-MET-7/`.

**Files:**
- Create: `nami-engine/src/metrics/builtin.rs`, `nami-engine/tests/metrics_builtin.rs`
- Modify: `nami-engine/src/metrics/query.rs` (`Target` variants, `validate_def` per target), `nami-engine/src/metrics/sql.rs` (target fragments, resolved filters, relabelling, discovery merge)

**Interfaces:**
- Consumes: T5 `Target`, `ValidQuery`, `build`, `run`; T6 `names`, `tag_keys`, `tag_values`; `uptime_checks`, `logs` tables; Postgres `monitors (id, project_id, name)`, `sources (id, project_id, name)`; `logs::normalize::LEVELS`.
- Produces:

```rust
// metrics/query.rs
pub enum Target { Stored { name: String }, UptimeResponseTime, UptimeUp, LogsCount }
// metrics/builtin.rs
pub const UPTIME_RESPONSE_TIME: &str = "nami.uptime.response_time_ms";
pub const UPTIME_UP: &str = "nami.uptime.up";
pub const LOGS_COUNT: &str = "nami.logs.count";
pub struct Builtin { pub name: &'static str, pub target: Target, pub tags: &'static [&'static str],
    pub aggregations: &'static [Aggregation], pub sql: TargetSql }
pub struct TargetSql { pub table: &'static str, pub time: &'static str, pub value: &'static str,
    pub series: &'static str, pub extra_where: &'static str }
pub const BUILTINS: [Builtin; 3];
pub fn lookup(name: &str) -> Option<&'static Builtin>;
/// Tag expression of a built-in key: a column expression, or a name resolved in Postgres.
pub enum TagExpr { Column(&'static str), MonitorName, SourceName }
pub fn tag_expr(target: &Target, key: &str) -> Option<TagExpr>;
/// Resolves monitor_name / source filters to ids within the project (regex via RegexBuilder, 256 KiB).
pub async fn resolve_ids(db: &PgPool, project_id: Uuid, expr: &TagExpr, filter: &Filter) -> Result<Vec<Uuid>, AppError>;
/// id → name, for relabelling grouped series.
pub async fn names_by_id(db: &PgPool, project_id: Uuid, expr: &TagExpr) -> Result<HashMap<Uuid, String>, AppError>;
```

Fragments (R13):

| Target | `table` | `time` | `value` | `series` | `extra_where` | tag keys → expressions |
|---|---|---|---|---|---|---|
| Stored | `metrics` | `dt` | `value` | `series_id` | ` AND name = {pN:String}` | any key → `tags[{pK:String}]` |
| UptimeResponseTime | `uptime_checks` | `checked_at` | `toFloat64(response_time_ms)` | `monitor_id` | ` AND response_time_ms IS NOT NULL` | `monitor` → `toString(monitor_id)`; `monitor_name` → resolved |
| UptimeUp | `uptime_checks` | `checked_at` | `toFloat64(status = 'up')` | `monitor_id` | — | same |
| LogsCount | `logs` | `dt` | `1` | `(source_id, level)` | — | `level` → `level`; `source_id` → `toString(source_id)`; `source` → resolved |

The T5 builder takes its fragments from the target (`<TABLE>`, `<TIME>` in the bucket and window, `<VALUE>` in `<AGG>`, `babs` and rate, `<SERIES>` in the rate window's `PARTITION BY`). For a resolved key: a filter becomes ` AND monitor_id IN ({..:UUID}, ...)` (`eq`/`regex`) or `NOT IN` (`neq`) over the ids from `resolve_ids`; an empty id list for `eq`/`regex` emits ` AND 0` (no series), an empty list for `neq` emits nothing; grouping uses `toString(monitor_id)` (or `toString(source_id)`), and `run` relabels each series with both keys (`monitor` + `monitor_name`, `source_id` + `source`), names from `names_by_id` (a deleted monitor keeps its id and gets `monitor_name: ""`). `LogsCount` with an aggregation other than `count`/`sum`/`rate` → 400 `nami.logs.count supports count, sum and rate`; `count` and `sum` both use `toFloat64(count())`, `rate` uses `count() / step` per group (no counter logic). An unknown tag key on a built-in → 400 `unknown tag for <name>`. Discovery: `names` puts the three built-ins first (filtered by `q`, `type` `"gauge"`, `builtin: true`); `tag_keys` returns the fixed list; `tag_values` reads Postgres (`monitors`/`sources` of the project, ordered by name, capped at 500) or `LEVELS` for `level`.

- [ ] **Step 1: Write the failing tests** `tests/metrics_builtin.rs`:

```rust
// Built-in metrics over uptime_checks and logs. Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{Duration, DurationRound, SecondsFormat, Utc};
use common::{app, call, logs::{post_json, seed_source}, metrics::query, public_app, seed_http_monitor, seed_project, state_with_clickhouse, user_with_role};
use nami_engine::uptime::{checker::CheckStatus, store::{insert_checks, UptimeCheckRow}};
use serde_json::{json, Value};

fn ts(d: chrono::DateTime<Utc>) -> String { d.to_rfc3339_opts(SecondsFormat::Millis, true) }

#[sqlx::test]
async fn uptime_builtins_compute_availability_and_latency_per_monitor(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let (project, other) = (seed_project(&pool).await, seed_project(&pool).await);
    let api = seed_http_monitor(&pool, project, "API", "https://a.test", 1).await;
    let web = seed_http_monitor(&pool, project, "Web", "https://w.test", 1).await;
    let foreign = seed_http_monitor(&pool, other, "API", "https://x.test", 1).await;
    let base = (Utc::now() - Duration::hours(1)).duration_trunc(Duration::minutes(10)).unwrap();
    let row = |m, p, s: i64, up: bool, ms: u32| UptimeCheckRow { monitor_id: m, project_id: p,
        checked_at: base + Duration::seconds(s), status: if up { CheckStatus::Up } else { CheckStatus::Down },
        status_code: Some(200), response_time_ms: Some(ms), error: None };
    let check = |m, s, up, ms| row(m, project, s, up, ms);
    insert_checks(&ch, &[check(api, 0, true, 100), check(api, 10, false, 300), check(api, 20, true, 200),
        check(api, 30, true, 400), check(web, 0, true, 50),
        row(foreign, other, 0, false, 9999)]).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let q = |metric: &str, agg: &str, extra: Value| { let mut b = json!({ "metric": metric, "aggregation": agg,
        "group_by": ["monitor_name"], "from": ts(base), "to": ts(base + Duration::seconds(59)), "step": 60 });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone()); b };
    let (status, up) = query(&app, &viewer, project, q("nami.uptime.up", "avg", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{up}");
    let by_name: std::collections::HashMap<String, f64> = up["series"].as_array().unwrap().iter()
        .map(|s| (s["tags"]["monitor_name"].as_str().unwrap().into(), s["points"][0][1].as_f64().unwrap())).collect();
    assert_eq!(by_name.len(), 2, "the other project's monitor never appears");
    assert_eq!((by_name["API"], by_name["Web"]), (0.75, 1.0));
    assert!(up["series"][0]["tags"]["monitor"].is_string(), "grouped by name still carries the id");
    let (_, p) = query(&app, &viewer, project, q("nami.uptime.response_time_ms", "max",
        json!({ "filters": [{ "tag": "monitor_name", "op": "regex", "value": "^AP" }] }))).await;
    assert_eq!(p["series"].as_array().unwrap().len(), 1);
    assert_eq!(p["series"][0]["points"][0][1], 400.0);
    // A foreign monitor id in a filter matches nothing.
    let (_, none) = query(&app, &viewer, project, q("nami.uptime.up", "avg",
        json!({ "filters": [{ "tag": "monitor", "op": "eq", "value": foreign.to_string() }] }))).await;
    assert_eq!(none["series"], json!([]));
    let (status, _) = query(&app, &viewer, project, q("nami.uptime.up", "avg", json!({ "group_by": ["host"] }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn log_volume_counts_by_level_and_source(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (app, public) = (app(state.clone()), public_app(state));
    let project = seed_project(&pool).await;
    let (_, api_token) = seed_source(&pool, project, "api").await;
    let (_, worker_token) = seed_source(&pool, project, "worker").await;
    let now = Utc::now();
    for (token, level) in [(&api_token, "error"), (&api_token, "error"), (&api_token, "info"), (&worker_token, "error")] {
        post_json(&public, token, json!({ "message": "x", "level": level, "dt": ts(now - Duration::minutes(5)) })).await;
    }
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let body = json!({ "metric": "nami.logs.count", "aggregation": "count", "group_by": ["source", "level"],
        "from": ts(now - Duration::minutes(10)), "to": ts(now), "step": 600 });
    let (status, res) = query(&app, &viewer, project, body).await;
    assert_eq!(status, StatusCode::OK, "{res}");
    let mut got: Vec<(String, String, f64)> = res["series"].as_array().unwrap().iter().map(|s| (
        s["tags"]["source"].as_str().unwrap().into(), s["tags"]["level"].as_str().unwrap().into(),
        s["points"].as_array().unwrap().iter().filter_map(|p| p[1].as_f64()).sum())).collect();
    got.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(got, vec![("api".into(), "error".into(), 2.0), ("api".into(), "info".into(), 1.0), ("worker".into(), "error".into(), 1.0)]);
    let (status, _) = query(&app, &viewer, project, json!({ "metric": "nami.logs.count", "aggregation": "p95",
        "from": ts(now - Duration::minutes(10)), "to": ts(now) })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, names) = call(&app, "GET", &format!("/projects/{project}/metrics/names?q=nami."), Some(&viewer), None).await;
    let builtins: Vec<&str> = names["names"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(builtins, vec!["nami.uptime.response_time_ms", "nami.uptime.up", "nami.logs.count"]);
    let (_, vals) = call(&app, "GET", &format!("/projects/{project}/metrics/tag-values?metric=nami.logs.count&tag=source"), Some(&viewer), None).await;
    assert_eq!(vals["values"], json!(["api", "worker"]));
    ch.drop_database().await.unwrap();
}
```

`UptimeCheckRow` (all fields `pub`) and `insert_checks` are `uptime::store`'s, and `CheckStatus` is `uptime::checker`'s, as on `main`.

- [ ] **Step 2: Run to fail** — engine command with `--test metrics_builtin`. Expected: FAIL (`invalid metric name` is not the failure: `nami.uptime.up` is a valid name; the queries return no series).

- [ ] **Step 3: Implement** `metrics::builtin`, the `Target` variants and the builder/run/discovery changes.

- [ ] **Step 4: Run to pass** — engine command with `--test metrics_builtin --test metrics_query --test metrics_discovery`.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src nami-engine/tests/metrics_builtin.rs
git commit -m "feat(engine): built-in metrics from uptime checks and log volume (NAMI-MET-7)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8 (NAMI-MET-8): Dashboards, panels and layout — schema, validation, API

Branch `feat/NAMI-MET-8-dashboards-api`, worktree `.worktrees/NAMI-MET-8/`.

**Files:**
- Create: `nami-engine/migrations/0120_create_dashboards.sql`, `nami-engine/src/models/dashboard.rs`, `nami-engine/src/routes/dashboards.rs`, `nami-engine/tests/dashboards_api.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/migrations.rs` (`dashboards`, `dashboard_panels`), `nami-engine/tests/json_bodies.rs` (dashboard and panel POST/PATCH, layout PUT)

**Interfaces:**
- Consumes: T5 `QueryDef`, `FilterOp`, `validate_def`; T7 `builtin::lookup` (through `validate_def`); `models::validate_label`; `require_project_role`.
- Produces:

```rust
// models/dashboard.rs
pub const MAX_DASHBOARDS_PER_PROJECT: i64 = 100;
pub const MAX_PANELS_PER_DASHBOARD: i64 = 50;
pub const MAX_VARIABLES: usize = 10;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 1_000;
pub const MAX_MARKDOWN_BYTES: usize = 10_240;
pub const MAX_QUERY_JSON_BYTES: usize = 4_096;
pub const MAX_VARIABLE_VALUES: usize = 100;
pub const RANGES: [&str; 6] = ["15m", "1h", "6h", "24h", "7d", "30d"];
pub const REFRESH_SECONDS: [i32; 4] = [0, 30, 60, 300];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct Variable { pub name: String, pub label: String, pub metric: String, pub tag: String,
    #[serde(default)] pub multi: bool, #[serde(default)] pub default: Option<Vec<String>> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Viz { Line, Bar, Stat, Text }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Width { Third, Half, Full }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct PanelOptions { #[serde(default, skip_serializing_if = "Option::is_none")] pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub decimals: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stat_reduce: Option<StatReduce>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stacked: Option<bool> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum StatReduce { Last, Avg, Min, Max, Sum }

#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct PanelInput { pub title: String, pub viz: Viz, #[serde(default)] pub query: Option<QueryDef>,
    #[serde(default)] pub markdown: Option<String>, #[serde(default)] pub options: PanelOptions,
    #[serde(default = "Width::default_half")] pub width: Width }
#[derive(Debug, Clone, Default, Deserialize)] #[serde(deny_unknown_fields)]
pub struct PanelPatch { pub title: Option<String>, pub viz: Option<Viz>, #[serde(default)] pub query: Option<Option<QueryDef>>,
    #[serde(default)] pub markdown: Option<Option<String>>, pub options: Option<PanelOptions>, pub width: Option<Width> }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct DashboardInput { pub name: String, #[serde(default)] pub description: String,
    #[serde(default = "default_range")] pub default_range: String, #[serde(default)] pub refresh_seconds: i32,
    #[serde(default)] pub variables: Vec<Variable>, #[serde(default)] pub panels: Vec<PanelInput> }
#[derive(Debug, Clone, Default, Deserialize)] #[serde(deny_unknown_fields)]
pub struct DashboardPatch { pub name: Option<String>, pub description: Option<String>,
    pub default_range: Option<String>, pub refresh_seconds: Option<i32>, pub variables: Option<Vec<Variable>> }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct LayoutItem { pub id: Uuid, pub width: Width }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct LayoutInput { pub panels: Vec<LayoutItem> }

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Panel { pub id: Uuid, pub title: String, pub viz: Viz, pub query: Option<QueryDef>,
    pub markdown: Option<String>, pub options: PanelOptions, pub position: i32, pub width: Width }
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Dashboard { pub id: Uuid, pub project_id: Uuid, pub name: String, pub description: String,
    pub default_range: String, pub refresh_seconds: i32, pub variables: Vec<Variable>, pub panels: Vec<Panel>,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DashboardSummary { pub id: Uuid, pub name: String, pub description: String, pub panel_count: i64, pub updated_at: DateTime<Utc> }

pub fn validate_variables(vars: &[Variable]) -> Result<(), String>;
pub fn validate_panel(panel: &PanelInput, vars: &[Variable]) -> Result<(), String>;
/// The variable names a query references (filter values matching ^\$[a-z][a-z0-9_]{0,31}$).
pub fn referenced_variables(def: &QueryDef) -> Vec<String>;
pub async fn list(db: &PgPool, project_id: Uuid) -> Result<Vec<DashboardSummary>, sqlx::Error>;
pub async fn get(db: &PgPool, project_id: Uuid, id: Uuid) -> Result<Option<Dashboard>, sqlx::Error>;
/// One transaction: the dashboard, then its panels in order (Part 2 templates call this).
pub async fn create_with_panels(db: &PgPool, project_id: Uuid, created_by: Uuid, input: DashboardInput) -> Result<Dashboard, AppError>;
pub async fn update(db: &PgPool, project_id: Uuid, id: Uuid, patch: DashboardPatch) -> Result<Option<Dashboard>, AppError>;
pub async fn delete(db: &PgPool, project_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>;
pub async fn add_panel(db: &PgPool, project_id: Uuid, dashboard_id: Uuid, input: PanelInput) -> Result<Option<Panel>, AppError>;
pub async fn update_panel(db: &PgPool, project_id: Uuid, dashboard_id: Uuid, panel_id: Uuid, patch: PanelPatch) -> Result<Option<Panel>, AppError>;
pub async fn delete_panel(db: &PgPool, project_id: Uuid, dashboard_id: Uuid, panel_id: Uuid) -> Result<bool, sqlx::Error>;
pub async fn set_layout(db: &PgPool, project_id: Uuid, dashboard_id: Uuid, layout: LayoutInput) -> Result<Option<Dashboard>, AppError>;
/// For T9: one panel with its dashboard's variables, scoped by project and dashboard.
pub async fn panel_for_query(db: &PgPool, project_id: Uuid, dashboard_id: Uuid, panel_id: Uuid)
    -> Result<Option<(Panel, Vec<Variable>)>, sqlx::Error>;
```

`Width` serializes as `"third"`, `"half"`, `"full"` (R15; the UI labels them 1/3, 1/2, full). `Width::default_half()` returns `Half`. `default_range()` returns `"24h"`.

Routes (`routes::dashboards::router()`):

| Method | Path | Role | Answer |
|---|---|---|---|
| GET | `/projects/{id}/dashboards` | viewer | `[DashboardSummary]` by name |
| POST | `/projects/{id}/dashboards` | member | 201 `Dashboard`; 409 at 100 |
| GET | `/projects/{id}/dashboards/{dashboard_id}` | viewer | `Dashboard` (panels by position) or 404 |
| PATCH | `/projects/{id}/dashboards/{dashboard_id}` | member | `Dashboard` |
| DELETE | `/projects/{id}/dashboards/{dashboard_id}` | **admin** | 204 |
| POST | `/projects/{id}/dashboards/{dashboard_id}/panels` | member | 201 `Panel` (appended at the end); 409 at 50 |
| PATCH | `/projects/{id}/dashboards/{dashboard_id}/panels/{panel_id}` | member | `Panel` |
| DELETE | `/projects/{id}/dashboards/{dashboard_id}/panels/{panel_id}` | member | 204 (positions closed up) |
| PUT | `/projects/{id}/dashboards/{dashboard_id}/layout` | member | `Dashboard` |

Validation (400, fixed messages naming the field; the body was already shape-checked by serde):
- `name` through `validate_label("name", ..)`; `description` ≤ 1,000 chars; `default_range` in `RANGES`; `refresh_seconds` in `REFRESH_SECONDS`.
- Variables: ≤ 10; name `^[a-z][a-z0-9_]{0,31}$`, unique; `label` 1-100 chars; `metric` valid name or a built-in; `tag` valid tag key (or a key of that built-in); `default` ≤ 100 values each ≤ 1,024 bytes, at most one for a non-`multi` variable.
- Panels: title 1-200 chars, no control characters. `text`: `markdown` required, ≤ 10,240 bytes, no `\0`; `query` must be absent; `options` must be empty. `line`/`bar`/`stat`: `query` required, `markdown` absent; `validate_def(query)`; serialized query ≤ 4,096 bytes; every referenced variable defined; a `regex` filter may not reference a `multi` variable; `stat` may not have `group_by`; `options.stat_reduce` only on `stat`, `stacked` only on `bar`, `unit` ≤ 16 chars without control characters, `decimals` ≤ 6. Creating with more than 50 `panels` is 400 (the request itself is over the cap); adding the 51st to an existing dashboard is 409.
- A PATCH that changes `viz` re-validates the whole resulting panel. A dashboard PATCH that replaces `variables` re-validates every panel against the new list (400 `panels[<position>] "<title>" references $<name>, which no longer exists`, title cut to 50 chars).
- Layout: the ids must be exactly the dashboard's panel ids, each once (400 `layout must list every panel of the dashboard exactly once`); positions are rewritten 0..n in one statement in the transaction (`update ... from unnest($1::uuid[], $2::int[], $3::text[])`), after `set constraints dashboard_panels_position_unique deferred`.
- Caps: counted under `select ... for update` of the dashboard row (panels) or `pg_advisory_xact_lock(hashtext('dashboards:' || $1::text))` (dashboards), so two concurrent creates cannot pass 100/50.

`nami-engine/migrations/0120_create_dashboards.sql`:

```sql
-- Dashboards (Metrics sub-project; migrations 0120-0139 are reserved for it).
create table dashboards (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null check (char_length(name) between 1 and 100),
    description text not null default '' check (char_length(description) <= 1000),
    default_range text not null default '24h'
        check (default_range in ('15m', '1h', '6h', '24h', '7d', '30d')),
    refresh_seconds int not null default 0 check (refresh_seconds in (0, 30, 60, 300)),
    -- Validated by the engine (models::dashboard::validate_variables); at most 10.
    variables jsonb not null default '[]'::jsonb
        check (jsonb_typeof(variables) = 'array' and jsonb_array_length(variables) <= 10),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (id, project_id)
);
create index dashboards_project_idx on dashboards (project_id, name);

create table dashboard_panels (
    id uuid primary key default gen_random_uuid(),
    dashboard_id uuid not null,
    project_id uuid not null,
    title text not null check (char_length(title) between 1 and 200),
    viz text not null check (viz in ('line', 'bar', 'stat', 'text')),
    -- A QueryDef (metrics::query), validated by the engine; null for text panels.
    query jsonb check (query is null or (jsonb_typeof(query) = 'object' and octet_length(query::text) <= 8192)),
    markdown text check (markdown is null or octet_length(markdown) <= 10240),
    options jsonb not null default '{}'::jsonb check (jsonb_typeof(options) = 'object'),
    position int not null check (position >= 0),
    width text not null default 'half' check (width in ('third', 'half', 'full')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    -- A panel always belongs to a dashboard of the same project.
    foreign key (dashboard_id, project_id) references dashboards (id, project_id) on delete cascade,
    constraint dashboard_panels_position_unique unique (dashboard_id, position) deferrable initially immediate,
    check ((viz = 'text') = (markdown is not null)),
    check ((viz = 'text') = (query is null))
);
```

(The unique constraint's index already serves `(dashboard_id, position)` lookups. The `octet_length(query::text) <= 8192` check is a backstop above the engine's 4,096-byte cap: jsonb's text form may differ from the engine's serialization.)

- [ ] **Step 1: Write the failing tests** `tests/dashboards_api.rs`:

```rust
// Dashboards, panels and layout. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, login, seed_project, seed_user, test_app, unique_email, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

fn line(title: &str) -> Value {
    json!({ "title": title, "viz": "line", "query": { "metric": "http_requests_total", "aggregation": "rate",
        "filters": [{ "tag": "host", "op": "eq", "value": "$host" }], "group_by": ["code"] } })
}
fn dash(name: &str) -> Value {
    json!({ "name": name, "variables": [{ "name": "host", "label": "Host", "metric": "http_requests_total", "tag": "host" }],
        "panels": [line("Requests"), { "title": "Notes", "viz": "text", "markdown": "# Runbook\n<script>x</script>" }] })
}

#[sqlx::test]
async fn crud_layout_and_roles(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let base = format!("/projects/{project}/dashboards");

    assert_eq!(call(&app, "POST", &base, Some(&viewer), Some(dash("v"))).await.0, StatusCode::FORBIDDEN);
    let (status, d) = call(&app, "POST", &base, Some(&member), Some(dash("Service"))).await;
    assert_eq!(status, StatusCode::CREATED, "{d}");
    let id = d["id"].as_str().unwrap().to_string();
    assert_eq!(d["panels"].as_array().unwrap().len(), 2);
    assert_eq!(d["panels"][1]["markdown"], "# Runbook\n<script>x</script>", "stored as text, rendered by the UI");
    assert_eq!((d["default_range"].as_str(), d["refresh_seconds"].as_i64()), (Some("24h"), Some(0)));

    let (status, list) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!((status, list[0]["panel_count"].as_i64()), (StatusCode::OK, Some(2)));
    assert_eq!(call(&app, "GET", &format!("{base}/{id}"), Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "PATCH", &format!("{base}/{id}"), Some(&viewer), Some(json!({ "name": "x" }))).await.0, StatusCode::FORBIDDEN);

    let (status, p) = call(&app, "POST", &format!("{base}/{id}/panels"), Some(&member),
        Some(json!({ "title": "Errors", "viz": "stat", "width": "third", "options": { "stat_reduce": "max", "unit": "req/s" },
            "query": { "metric": "http_requests_total", "aggregation": "sum" } }))).await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    assert_eq!(p["position"], 2);
    let ids: Vec<String> = call(&app, "GET", &format!("{base}/{id}"), Some(&viewer), None).await.1["panels"]
        .as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap().to_string()).collect();
    let reordered = json!({ "panels": [{ "id": ids[2], "width": "full" }, { "id": ids[0], "width": "half" }, { "id": ids[1], "width": "third" }] });
    let (status, d) = call(&app, "PUT", &format!("{base}/{id}/layout"), Some(&member), Some(reordered)).await;
    assert_eq!(status, StatusCode::OK, "{d}");
    let order: Vec<(&str, &str)> = d["panels"].as_array().unwrap().iter()
        .map(|p| (p["title"].as_str().unwrap(), p["width"].as_str().unwrap())).collect();
    assert_eq!(order, vec![("Errors", "full"), ("Requests", "half"), ("Notes", "third")]);
    for bad in [json!({ "panels": [{ "id": ids[0], "width": "half" }] }),
                json!({ "panels": [{ "id": ids[0], "width": "half" }, { "id": ids[0], "width": "half" }, { "id": ids[1], "width": "half" }] }),
                json!({ "panels": [{ "id": ids[0], "width": "half" }, { "id": ids[1], "width": "half" }, { "id": Uuid::new_v4(), "width": "half" }] })] {
        assert_eq!(call(&app, "PUT", &format!("{base}/{id}/layout"), Some(&member), Some(bad)).await.0, StatusCode::BAD_REQUEST);
    }
    let (status, p) = call(&app, "PATCH", &format!("{base}/{id}/panels/{}", ids[2]), Some(&member), Some(json!({ "title": "Errors/s" }))).await;
    assert_eq!((status, p["title"].as_str()), (StatusCode::OK, Some("Errors/s")));
    assert_eq!(call(&app, "DELETE", &format!("{base}/{id}/panels/{}", ids[1]), Some(&member), None).await.0, StatusCode::NO_CONTENT);
    let positions: Vec<i64> = call(&app, "GET", &format!("{base}/{id}"), Some(&viewer), None).await.1["panels"]
        .as_array().unwrap().iter().map(|p| p["position"].as_i64().unwrap()).collect();
    assert_eq!(positions, vec![0, 1], "positions are closed up");
    assert_eq!(call(&app, "DELETE", &format!("{base}/{id}"), Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &format!("{base}/{id}"), Some(&admin), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, "GET", &format!("{base}/{id}"), Some(&viewer), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn validation_and_caps(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let base = format!("/projects/{project}/dashboards");
    let id = call(&app, "POST", &base, Some(&member), Some(dash("D"))).await.1["id"].as_str().unwrap().to_string();
    let panels = format!("{base}/{id}/panels");
    let bad_panels = [
        json!({ "title": "md", "viz": "text", "markdown": "x".repeat(10_241) }),
        json!({ "title": "md", "viz": "text", "markdown": "a\u{0}b" }),
        json!({ "title": "md", "viz": "text", "markdown": "ok", "query": { "metric": "m", "aggregation": "avg" } }),
        json!({ "title": "q", "viz": "line" }),
        json!({ "title": "", "viz": "line", "query": { "metric": "m", "aggregation": "avg" } }),
        json!({ "title": "t".repeat(201), "viz": "line", "query": { "metric": "m", "aggregation": "avg" } }),
        json!({ "title": "s", "viz": "stat", "query": { "metric": "m", "aggregation": "avg", "group_by": ["host"] } }),
        json!({ "title": "s", "viz": "line", "options": { "stat_reduce": "max" }, "query": { "metric": "m", "aggregation": "avg" } }),
        json!({ "title": "u", "viz": "line", "query": { "metric": "m", "aggregation": "avg",
            "filters": [{ "tag": "host", "op": "eq", "value": "$nope" }] } }),
        json!({ "title": "big", "viz": "line", "query": { "metric": "m", "aggregation": "avg",
            "filters": (0..10).map(|i| json!({ "tag": format!("t{i}"), "op": "eq", "value": "v".repeat(500) })).collect::<Vec<_>>() } }),
        json!({ "title": "inj", "viz": "line", "query": { "metric": "m'); DROP TABLE x; --", "aggregation": "avg" } }),
    ];
    for b in bad_panels {
        let (status, body) = call(&app, "POST", &panels, Some(&member), Some(b.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{b}: {body}");
    }
    assert_eq!(call(&app, "POST", &panels, Some(&member),
        Some(json!({ "title": "md", "viz": "text", "markdown": "x".repeat(10_240) }))).await.0, StatusCode::CREATED);
    // Unknown fields are refused by serde (422), never stored.
    assert_eq!(call(&app, "POST", &panels, Some(&member),
        Some(json!({ "title": "x", "viz": "text", "markdown": "m", "html": "<b>" }))).await.0, StatusCode::UNPROCESSABLE_ENTITY);

    let vars = |v: Value| json!({ "variables": v });
    let eleven: Vec<Value> = (0..11).map(|i| json!({ "name": format!("v{i}"), "label": "L", "metric": "m", "tag": "t" })).collect();
    for b in [vars(json!(eleven)),
              vars(json!([{ "name": "Host", "label": "L", "metric": "m", "tag": "t" }])),
              vars(json!([{ "name": "a", "label": "L", "metric": "m", "tag": "t" }, { "name": "a", "label": "L", "metric": "m", "tag": "t" }])),
              vars(json!([{ "name": "a", "label": "L", "metric": "m", "tag": "t", "default": ["x", "y"] }])),
              vars(json!([]))] {  // removes $host, still used by "Requests"
        assert_eq!(call(&app, "PATCH", &format!("{base}/{id}"), Some(&member), Some(b.clone())).await.0, StatusCode::BAD_REQUEST, "{b}");
    }
    // A regex filter may not take a multi variable.
    let multi = json!({ "name": "M", "variables": [{ "name": "pods", "label": "Pods", "metric": "m", "tag": "pod", "multi": true }],
        "panels": [{ "title": "r", "viz": "line", "query": { "metric": "m", "aggregation": "avg",
            "filters": [{ "tag": "pod", "op": "regex", "value": "$pods" }] } }] });
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(multi)).await.0, StatusCode::BAD_REQUEST);
    // 50 panels per dashboard.
    let fifty: Vec<Value> = (0..50).map(|i| json!({ "title": format!("p{i}"), "viz": "text", "markdown": "x" })).collect();
    let full = call(&app, "POST", &base, Some(&member), Some(json!({ "name": "Full", "panels": fifty }))).await;
    assert_eq!(full.0, StatusCode::CREATED);
    let full_id = full.1["id"].as_str().unwrap();
    assert_eq!(call(&app, "POST", &format!("{base}/{full_id}/panels"), Some(&member),
        Some(json!({ "title": "51", "viz": "text", "markdown": "x" }))).await.0, StatusCode::CONFLICT);
    let fifty_one: Vec<Value> = (0..51).map(|i| json!({ "title": format!("p{i}"), "viz": "text", "markdown": "x" })).collect();
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(json!({ "name": "Over", "panels": fifty_one }))).await.0, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn a_hundred_dashboards_per_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for i in 0..100 {
        sqlx::query("insert into dashboards (project_id, name) values ($1, $2)").bind(project).bind(format!("d{i}"))
            .execute(&pool).await.unwrap();
    }
    let (status, _) = call(&app, "POST", &format!("/projects/{project}/dashboards"), Some(&member), Some(json!({ "name": "101" }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn another_projects_dashboards_and_panels_are_404_inside_an_accessible_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (mine, theirs) = (seed_project(&pool).await, seed_project(&pool).await);
    let admin_email = unique_email("root");
    seed_user(&pool, &admin_email, "admin").await;
    let root = login(&app, &admin_email).await;
    let foreign = call(&app, "POST", &format!("/projects/{theirs}/dashboards"), Some(&root), Some(dash("Theirs"))).await.1;
    let (fid, fpanel) = (foreign["id"].as_str().unwrap(), foreign["panels"][0]["id"].as_str().unwrap());
    let own = call(&app, "POST", &format!("/projects/{mine}/dashboards"), Some(&root), Some(dash("Mine"))).await.1;
    let oid = own["id"].as_str().unwrap();
    let (_, _, member) = user_with_role(&app, &pool, mine, Some("member")).await;
    let (_, _, project_admin) = user_with_role(&app, &pool, mine, Some("admin")).await;
    for (method, path, body) in [
        ("GET", format!("/projects/{mine}/dashboards/{fid}"), None),
        ("PATCH", format!("/projects/{mine}/dashboards/{fid}"), Some(json!({ "name": "x" }))),
        ("POST", format!("/projects/{mine}/dashboards/{fid}/panels"), Some(json!({ "title": "x", "viz": "text", "markdown": "x" }))),
        ("PATCH", format!("/projects/{mine}/dashboards/{oid}/panels/{fpanel}"), Some(json!({ "title": "x" }))),
        ("DELETE", format!("/projects/{mine}/dashboards/{oid}/panels/{fpanel}"), None),
        ("PUT", format!("/projects/{mine}/dashboards/{fid}/layout"), Some(json!({ "panels": [] }))),
    ] {
        assert_eq!(call(&app, method, &path, Some(&member), body).await.0, StatusCode::NOT_FOUND, "{method} {path}");
    }
    assert_eq!(call(&app, "DELETE", &format!("/projects/{mine}/dashboards/{fid}"), Some(&project_admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "GET", &format!("/projects/{theirs}/dashboards"), Some(&member), None).await.0, StatusCode::FORBIDDEN);
    let (_, list) = call(&app, "GET", &format!("/projects/{mine}/dashboards"), Some(&member), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}
```

The dashboard lookup runs before the layout's id-set check, so `{"panels": []}` on a foreign dashboard is a 404, not a 400.

- [ ] **Step 2: Run to fail** — engine command with `--test dashboards_api`. Expected: FAIL (routes missing).

- [ ] **Step 3: Write the migration**, the model, the routes and the router merge; add both tables to `migrations.rs` and the five state-changing routes to `json_bodies.rs`.

- [ ] **Step 4: Run to pass** — engine command with `--test dashboards_api --test migrations --test json_bodies`.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/0120_create_dashboards.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): dashboards, panels and layout with validation and caps (NAMI-MET-8)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9 (NAMI-MET-9): Variables and the panel query route

Branch `feat/NAMI-MET-9-dashboard-variables`, worktree `.worktrees/NAMI-MET-9/`.

**Files:**
- Create: `nami-engine/src/metrics/variables.rs`, `nami-engine/tests/dashboard_variables.rs`
- Modify: `nami-engine/src/metrics/query.rs` (`QueryRequest.variables`), `nami-engine/src/routes/metrics.rs` (substitute instead of `literal_filters`), `nami-engine/src/routes/dashboards.rs` (panel query route), `nami-engine/tests/json_bodies.rs` (panel query POST)

**Interfaces:**
- Consumes: T5 `QueryDef`, `Filter`, `FilterOp`, `plan_window`, `validate`, `sql::run`, `acquire_query_slot`; T8 `Variable`, `panel_for_query`, `Viz`.
- Produces:

```rust
// metrics/variables.rs
pub type Values = std::collections::HashMap<String, Vec<String>>;
/// Filter values that reference a variable are replaced by its values. `defs` = the dashboard's
/// variables (panel queries) or None (`/metrics/query`, where every reference must be in `values`).
/// A reference resolving to "All" (empty) drops the filter. `$$x` becomes the literal `$x`.
pub fn substitute(def: &QueryDef, values: &Values, defs: Option<&[Variable]>) -> Result<Vec<Filter>, String>;
pub fn is_reference(value: &str) -> Option<&str>;   // Some(name) for ^\$[a-z][a-z0-9_]{0,31}$

// metrics/query.rs: QueryRequest gains
#[serde(default)] pub variables: crate::metrics::variables::Values,

// routes/dashboards.rs
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct PanelQueryRequest { pub from: DateTime<Utc>, pub to: DateTime<Utc>,
    #[serde(default)] pub step: Option<i64>, #[serde(default)] pub variables: Values }
/// No user: RBAC and slots stay in the caller (Part 2's share route calls this too).
pub async fn run_panel_query(state: &AppState, project_id: Uuid, dashboard_id: Uuid, panel_id: Uuid,
    request: PanelQueryRequest) -> Result<QueryResult, AppError>;
// POST /projects/{id}/dashboards/{dashboard_id}/panels/{panel_id}/query (viewer)
```

`substitute` rules (R16), in this order: when `defs` is `Some`, every key of `values` must be a defined variable (400 `unknown variable <name>`, name cut to 32 chars and kept only if it matches the name charset, else `unknown variable`); each value list ≤ 100 entries, each entry ≤ 1,024 bytes; a non-`multi` variable takes at most one value. For each filter: `$$…` → literal with the first `$` removed; a reference → its values from `values`, else (panel) the variable's `default`, else (panel, `default: null`) All, else (`/metrics/query`) 400 `unbound variable $<name>`; All → the filter is dropped; a `regex` filter with more than one value → 400; otherwise `Filter { tag, op, values }`. A metric name or group-by key that looks like `$x` is not touched here: `validate_def` rejects it as an invalid name or key, which is the guarantee that variables cannot reach them.

`run_panel_query`: `panel_for_query` (404) → `viz == Text` → 400 `text panels have no query` → `plan_window` → `substitute(.., Some(&vars))` → `validate` → `sql::run`. The route: viewer → parse → `acquire_query_slot` → `run_panel_query`.

- [ ] **Step 1: Write the failing unit tests** in `variables.rs`:

```rust
use super::*;
use crate::{metrics::query::{Aggregation, FilterDef, FilterOp, QueryDef}, models::dashboard::Variable};

fn def(filters: &[(&str, FilterOp, &str)]) -> QueryDef {
    QueryDef { metric: "m".into(), aggregation: Aggregation::Avg, group_by: vec![],
        filters: filters.iter().map(|(t, op, v)| FilterDef { tag: t.to_string(), op: *op, value: v.to_string() }).collect() }
}
fn var(name: &str, multi: bool, default: Option<Vec<&str>>) -> Variable {
    Variable { name: name.into(), label: name.into(), metric: "m".into(), tag: "t".into(), multi,
        default: default.map(|d| d.into_iter().map(String::from).collect()) }
}
fn vals(pairs: &[(&str, &[&str])]) -> Values {
    pairs.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect()
}

#[test]
fn references_are_exact_and_dollar_dollar_escapes() {
    assert_eq!(is_reference("$host"), Some("host"));
    for not in ["host", "$Host", "$1a", "a$host", "$host ", "$", "$$host"] { assert_eq!(is_reference(not), None, "{not}"); }
    let f = substitute(&def(&[("p", FilterOp::Eq, "$$5")]), &Values::new(), None).unwrap();
    assert_eq!(f[0].values, vec!["$5".to_string()]);
}

#[test]
fn values_multi_all_and_defaults() {
    let defs = [var("host", false, Some(vec!["a"])), var("pods", true, None)];
    let d = def(&[("host", FilterOp::Eq, "$host"), ("pod", FilterOp::Neq, "$pods")]);
    let f = substitute(&d, &vals(&[("pods", &["x", "y"])]), Some(&defs)).unwrap();
    assert_eq!(f, vec![Filter { tag: "host".into(), op: FilterOp::Eq, values: vec!["a".into()] },
                       Filter { tag: "pod".into(), op: FilterOp::Neq, values: vec!["x".into(), "y".into()] }]);
    let all = substitute(&d, &vals(&[("host", &[]), ("pods", &[])]), Some(&defs)).unwrap();
    assert!(all.is_empty(), "All removes the filters");
    assert!(substitute(&d, &vals(&[("host", &["a", "b"])]), Some(&defs)).is_err(), "not multi");
    assert!(substitute(&d, &vals(&[("nope", &["a"])]), Some(&defs)).is_err());
    let hundred_one: Vec<String> = (0..101).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = hundred_one.iter().map(String::as_str).collect();
    assert!(substitute(&d, &vals(&[("pods", &refs)]), Some(&defs)).is_err());
    let long = "v".repeat(1025);
    assert!(substitute(&d, &vals(&[("host", &[long.as_str()])]), Some(&defs)).is_err());
    assert!(substitute(&def(&[("p", FilterOp::Eq, "$x")]), &Values::new(), None).is_err(), "unbound on /metrics/query");
    let re = def(&[("pod", FilterOp::Regex, "$pods")]);
    assert!(substitute(&re, &vals(&[("pods", &["a", "b"])]), Some(&defs)).is_err());
}
```

- [ ] **Step 2: Write the failing integration tests** `tests/dashboard_variables.rs`:

```rust
// Variable substitution and panel queries. Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{Duration, DurationRound, SecondsFormat, Utc};
use common::{app, call, metrics::{query, seed_samples}, seed_project, state_with_clickhouse, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

/// Quotes, a parameter placeholder, regex syntax and a backslash.
const EVIL: &str = "a'); DROP TABLE metrics; -- {p0:String} .* \\";

fn ts(d: chrono::DateTime<Utc>) -> String { d.to_rfc3339_opts(SecondsFormat::Millis, true) }

#[sqlx::test]
async fn a_variable_value_is_bound_literally(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let base = (Utc::now() - Duration::hours(1)).duration_trunc(Duration::minutes(1)).unwrap();
    seed_samples(&ch, project, Uuid::new_v4(), base, &[("m", &[("host", EVIL)], 0, 1.0),
        ("m", &[("host", "a")], 0, 10.0), ("m", &[("host", ".*")], 0, 100.0)]).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let d = call(&app, "POST", &format!("/projects/{project}/dashboards"), Some(&member), Some(json!({
        "name": "V", "variables": [{ "name": "host", "label": "Host", "metric": "m", "tag": "host", "multi": true },
                                   { "name": "re", "label": "Re", "metric": "m", "tag": "host" }],
        "panels": [{ "title": "eq", "viz": "line", "query": { "metric": "m", "aggregation": "sum",
                       "filters": [{ "tag": "host", "op": "eq", "value": "$host" }] } },
                   { "title": "re", "viz": "line", "query": { "metric": "m", "aggregation": "sum",
                       "filters": [{ "tag": "host", "op": "regex", "value": "$re" }] } },
                   { "title": "notes", "viz": "text", "markdown": "hi" }] }))).await.1;
    let did = d["id"].as_str().unwrap().to_string();
    let panel = |i: usize| d["panels"][i]["id"].as_str().unwrap().to_string();
    let run = |pid: String, vars: Value| { let (app, viewer) = (app.clone(), viewer.clone());
        let path = format!("/projects/{project}/dashboards/{did}/panels/{pid}/query");
        async move { call(&app, "POST", &path, Some(&viewer), Some(json!({ "from": ts(base), "to": ts(base + Duration::seconds(59)),
            "step": 60, "variables": vars }))).await } };
    let sum = |body: &Value| body["series"][0]["points"][0][1].as_f64();

    let (status, body) = run(panel(0), json!({ "host": [EVIL] })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(sum(&body), Some(1.0), "the payload matched only itself");
    assert_eq!(sum(&run(panel(0), json!({ "host": [".*"] })).await.1), Some(100.0));
    assert_eq!(sum(&run(panel(0), json!({ "host": ["a", ".*"] })).await.1), Some(110.0));
    assert_eq!(sum(&run(panel(0), json!({ "host": [] })).await.1), Some(111.0), "All removes the filter");
    assert_eq!(sum(&run(panel(0), json!({})).await.1), Some(111.0), "no default means All");
    // A regex variable runs as a regex, still as one bound value.
    assert_eq!(sum(&run(panel(1), json!({ "re": ["^a$"] })).await.1), Some(10.0));
    assert_eq!(run(panel(1), json!({ "re": ["("] })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(run(panel(0), json!({ "nope": ["a"] })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(run(panel(2), json!({})).await.0, StatusCode::BAD_REQUEST, "text panels have no query");
    let tables: Vec<Value> = ch.query_rows("SELECT count() AS n FROM system.tables WHERE database = {d:String} AND name = 'metrics'",
        &[("d", ch.database().to_string())]).await.unwrap();
    assert_eq!(tables[0]["n"], 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn variables_never_reach_metric_names_or_group_by(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    for q in [json!({ "metric": "$host", "aggregation": "avg" }),
              json!({ "metric": "m", "aggregation": "avg", "group_by": ["$host"] }),
              json!({ "metric": "m", "aggregation": "avg", "filters": [{ "tag": "$host", "op": "eq", "value": "x" }] })] {
        let body = json!({ "name": "X", "variables": [{ "name": "host", "label": "H", "metric": "m", "tag": "host" }],
            "panels": [{ "title": "p", "viz": "line", "query": q }] });
        assert_eq!(call(&app, "POST", &format!("/projects/{project}/dashboards"), Some(&member), Some(body)).await.0,
            StatusCode::BAD_REQUEST);
    }
    let now = Utc::now();
    let (status, _) = query(&app, &viewer, project, json!({ "metric": "m", "aggregation": "avg",
        "filters": [{ "tag": "host", "op": "eq", "value": "$host" }], "from": ts(now - Duration::hours(1)), "to": ts(now) })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unbound variable on /metrics/query");
    let (status, _) = query(&app, &viewer, project, json!({ "metric": "m", "aggregation": "avg",
        "filters": [{ "tag": "host", "op": "eq", "value": "$host" }], "variables": { "host": ["a"] },
        "from": ts(now - Duration::hours(1)), "to": ts(now) })).await;
    assert_eq!(status, StatusCode::OK);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn panel_queries_are_scoped_to_their_dashboard_and_project(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let (mine, theirs) = (seed_project(&pool).await, seed_project(&pool).await);
    let (_, _, member) = user_with_role(&app, &pool, mine, Some("member")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, theirs, Some("member")).await;
    let panel = json!({ "title": "p", "viz": "line", "query": { "metric": "m", "aggregation": "avg" } });
    let a = call(&app, "POST", &format!("/projects/{mine}/dashboards"), Some(&member), Some(json!({ "name": "A", "panels": [panel.clone()] }))).await.1;
    let b = call(&app, "POST", &format!("/projects/{mine}/dashboards"), Some(&member), Some(json!({ "name": "B", "panels": [panel.clone()] }))).await.1;
    let t = call(&app, "POST", &format!("/projects/{theirs}/dashboards"), Some(&other_member), Some(json!({ "name": "T", "panels": [panel] }))).await.1;
    let now = Utc::now();
    let body = json!({ "from": ts(now - Duration::hours(1)), "to": ts(now) });
    let path = |p: Uuid, d: &Value, panel_of: &Value| format!("/projects/{p}/dashboards/{}/panels/{}/query",
        d["id"].as_str().unwrap(), panel_of["panels"][0]["id"].as_str().unwrap());
    assert_eq!(call(&app, "POST", &path(mine, &a, &a), Some(&member), Some(body.clone())).await.0, StatusCode::OK);
    assert_eq!(call(&app, "POST", &path(mine, &a, &b), Some(&member), Some(body.clone())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "POST", &path(mine, &t, &t), Some(&member), Some(body.clone())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "POST", &path(theirs, &t, &t), Some(&member), Some(body)).await.0, StatusCode::FORBIDDEN);
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 3: Run to fail** — engine command with `--lib -- metrics::variables`, then `--test dashboard_variables`. Expected: FAIL.

- [ ] **Step 4: Implement** `metrics::variables`, the `variables` field, the substitution in `routes::metrics`, `run_panel_query` and its route, and the `json_bodies.rs` entry.

- [ ] **Step 5: Run to pass** — both commands, plus `--test metrics_query --test dashboards_api --test json_bodies`.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src nami-engine/tests
git commit -m "feat(engine): dashboard variables and the panel query route (NAMI-MET-9)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10 (NAMI-MET-10): UI foundation — types, BFF relays, chart components, safe markdown

Branch `feat/NAMI-MET-10-ui-foundation`, worktree `.worktrees/NAMI-MET-10/`.

**Prerequisite:** `refactor/ui-design-system` merged to `main` (header table). Check `nami-ui/src/lib/components/ui/index.ts` exists on `origin/main` before starting; if not, stop and tell the controller.

**Files:**
- Create: `nami-ui/src/lib/metrics.ts` (+ `metrics.test.ts`), `nami-ui/src/lib/chart.ts` (+ `chart.test.ts`), `nami-ui/src/lib/panel-queue.ts` (+ `panel-queue.test.ts`), `nami-ui/src/lib/server/metrics-proxy.ts` (+ `metrics-proxy.test.ts`), `nami-ui/src/lib/components/metrics/{TimeSeriesChart,StatPanel,MarkdownPanel,TimeRangePicker}.svelte` (+ `metrics-components-render.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/metrics/query/+server.ts` (+ `server.test.ts`), `.../metrics/discovery/+server.ts` (+ `server.test.ts`), `.../dashboards/[dashboardId=uuid]/panels/[panelId=uuid]/query/+server.ts` (+ `server.test.ts`)
- Create or modify: `nami-ui/src/lib/markdown.ts` (+ `markdown.test.ts`) (INC-12 row), `nami-ui/src/lib/server/json-relay.ts` (+ test) (LOG-16 row)

**Interfaces:**
- Consumes: engine T5/T6/T9 routes; `#lib/components/ui` (`Legend`, `StatCard`, `SegmentedTabs`, `Card`, `EmptyState`), `tones.ts`; `callEngine`, `enginePath`, `isUuid`, `relayJsonPost`, `renderMarkdown`; `niceTicks`, `formatUtc` from `#lib/uptime`.
- Produces:

```ts
// lib/metrics.ts
export type Aggregation = 'avg' | 'min' | 'max' | 'sum' | 'count' | 'p50' | 'p95' | 'p99' | 'rate';
export const AGGREGATIONS: { value: Aggregation; label: string }[];
export type FilterOp = 'eq' | 'neq' | 'regex';
export type FilterDef = { tag: string; op: FilterOp; value: string };
export type QueryDef = { metric: string; aggregation: Aggregation; filters: FilterDef[]; group_by: string[] };
export type Point = [number, number | null];
export type Series = { tags: Record<string, string>; points: Point[] };
export type QueryResult = { step_seconds: number; from: string; to: string; truncated: boolean; series: Series[] };
export type Viz = 'line' | 'bar' | 'stat' | 'text';
export type Width = 'third' | 'half' | 'full';
export type StatReduce = 'last' | 'avg' | 'min' | 'max' | 'sum';
export type PanelOptions = { unit?: string; decimals?: number; stat_reduce?: StatReduce; stacked?: boolean };
export type Panel = { id: string; title: string; viz: Viz; query: QueryDef | null; markdown: string | null;
  options: PanelOptions; position: number; width: Width };
export type Variable = { name: string; label: string; metric: string; tag: string; multi: boolean; default: string[] | null };
export type Dashboard = { id: string; project_id: string; name: string; description: string; default_range: RangePreset;
  refresh_seconds: 0 | 30 | 60 | 300; variables: Variable[]; panels: Panel[]; created_at: string; updated_at: string };
export type DashboardSummary = { id: string; name: string; description: string; panel_count: number; updated_at: string };
export type RangePreset = '15m' | '1h' | '6h' | '24h' | '7d' | '30d';
export const RANGE_PRESETS: { value: RangePreset; label: string; ms: number }[];
export const MAX_CUSTOM_RANGE_MS: number;            // 90 days
export type TimeRange = { preset: RangePreset } | { from: string; to: string };
export function parseRange(params: URLSearchParams, fallback: RangePreset): TimeRange;
export function rangeParams(range: TimeRange): URLSearchParams;
export function resolveRange(range: TimeRange, now: number): { from: string; to: string };
export const VARIABLE_PARAM_PREFIX = 'var-';
export function parseVariableValues(params: URLSearchParams, vars: Variable[]): Record<string, string[]>;
export function variableParams(values: Record<string, string[]>): URLSearchParams;
export function isQueryResult(value: unknown): value is QueryResult;
export function formatValue(value: number | null, unit?: string, decimals?: number): string;
export function reduceStat(points: Point[], reduce: StatReduce): number | null;
export function seriesLabel(series: Series, fallback: string): string;
export const WIDTH_CLASSES: Record<Width, string>;   // third 'md:col-span-2', half 'md:col-span-3', full 'md:col-span-6'
export const WIDTH_LABELS: Record<Width, string>;    // '1/3', '1/2', 'Full'

// lib/chart.ts
export const SERIES_TONES: Tone[];                    // primary, cyan, periwinkle, success, warning, info, danger, neutral
export function seriesTone(index: number): Tone;
export function decimate(points: Point[], columns: number): Point[];
export function linePath(points: Point[], x: (t: number) => number, y: (v: number) => number): string;
export function timeTicks(fromMs: number, toMs: number, maxTicks: number): number[];
export function formatTick(ms: number, spanMs: number): string;
export function stack(series: Series[]): { base: (number | null)[]; top: (number | null)[] }[];
export function valueDomain(series: Series[], stacked: boolean, viz: 'line' | 'bar'): [number, number];

// lib/panel-queue.ts
export const MAX_CONCURRENT = 4;
export const RETRY_DELAY_MS = 1000;
export type PanelQueue = {
  /** Runs `request` when a place is free. A newer call with the same key aborts the older one. */
  enqueue(key: string, request: (signal: AbortSignal) => Promise<Response>): Promise<Response>;
  /** Aborts everything running and queued (range or variable change, navigation). */
  abortAll(): void;
  running(): number;
};
export function createPanelQueue(options?: { concurrency?: number; retryDelayMs?: number;
  sleep?: (ms: number, signal: AbortSignal) => Promise<void> }): PanelQueue;

// lib/server/metrics-proxy.ts
export const DISCOVERY_KINDS: readonly ['names', 'tags', 'tag-values'];
export const MAX_DISCOVERY_PARAM_CHARS = 200;
/** The engine path for a discovery request, or null for an unknown kind or an oversize value. */
export function discoveryPath(projectId: string, params: URLSearchParams): string | null;
```

Behaviour that the signatures leave open:
- `parseRange`: `?range=<preset>` wins; else `?from=&to=` (ISO strings, `from < to`, span ≤ 90 days) → custom; anything else → `{ preset: fallback }`. `resolveRange` for a preset is `[now − ms, now]`.
- `parseVariableValues`: reads `var-<name>` (repeatable) only for defined variables; a non-`multi` variable keeps the first value; the literal value `__all` (or a defined variable with no parameter and `default: null`) → `[]` (All); a variable with no parameter and a default → its default. Values over 1,024 characters or more than 100 per variable are dropped.
- `isQueryResult` checks the shape deeply (numbers or `null` in points, string tag values); a panel renders "unexpected response" otherwise. Nothing from the engine reaches the DOM except as text.
- `formatValue`: `null` → `—`; `|v| ≥ 10^4` compact (`12.3k`, `4.5M`, `6.7G`); else `decimals` (default: 0 for integers, up to 2 otherwise); `unit` appended with a space except `%` (no space).
- `decimate(points, columns)`: when `points.length <= 2 * columns` return as is; else split the time range into `columns` equal slots and keep, per slot, the point with the minimum and the one with the maximum value in time order; a slot holding a `null` keeps one `null` point so the line still breaks there.
- `linePath`: `M` at the start of every run of non-null points, `L` inside it; a lone point in a run becomes a 0-length segment plus a dot drawn by the component.
- Panel queue: at most `concurrency` (default 4) running; FIFO otherwise; a `429` response is retried once after `retryDelayMs` (default 1,000) and the second answer is returned as is; `abortAll` aborts running signals and rejects queued jobs with a `DOMException('Aborted', 'AbortError')`.
- `discoveryPath`: `kind` must be in `DISCOVERY_KINDS`; only `q` (names), `metric` (tags), `metric`, `tag`, `q` (tag-values) pass, each at most 200 characters; built with `enginePath` (`/projects/${id}/metrics/${kind}?…` via `URLSearchParams`).
- Relays: `metrics/query/+server.ts` `POST` → `relayJsonPost(event, enginePath\`/projects/${id}/metrics/query\`)`; `panels/[panelId]/query/+server.ts` `POST` → `relayJsonPost(event, enginePath\`/projects/${id}/dashboards/${dashboardId}/panels/${panelId}/query\`)`; `metrics/discovery/+server.ts` `GET` → `callEngine(event, discoveryPath(...))` (400 for `null`), body streamed back with `cache-control: no-store`. Every handler checks `isUuid` on each param (404 otherwise) before building a path.
- `TimeSeriesChart.svelte` props `{ series: Series[]; viz: 'line' | 'bar'; from: string; to: string; stepSeconds: number; unit?: string; decimals?: number; stacked?: boolean; label: string }`. SVG, width from `bind:clientWidth`, height 200; tones from `seriesTone(i)` through `TONE_VAR`; grid `stroke: var(--color-border)`; axis text `fill: var(--color-muted)`; bars are grouped (or stacked when `stacked`); lines break at nulls; hover and keyboard (a visually hidden `<input type="range">` over the buckets, as `LogVolumeChart`) show a tooltip listing each series' label and value; below the chart a `Legend` (at most 8 shown plus "+N more"), and a visually hidden `<table>` of the data (first 200 rows) for screen readers. Decimate to `plotWidth` columns before drawing.
- `StatPanel.svelte` props `{ result: QueryResult; reduce: StatReduce; unit?: string; decimals?: number; label: string }`: one `StatCard` with `value = formatValue(reduceStat(points, reduce), unit, decimals)` and the series' sparkline (`Sparkline` through `StatCard.spark`).
- `MarkdownPanel.svelte` props `{ markdown: string }`: `<div class="prose-sm">{@html renderMarkdown(markdown)}</div>`; the only `{@html}` of the feature.
- `TimeRangePicker.svelte` props `{ range: TimeRange; hrefFor: (range: TimeRange) => string }`: `SegmentedTabs` over the presets (plain links, work without JS) plus a "Custom" `<details>` with two `datetime-local` inputs (UTC) in a GET form.

- [ ] **Step 1: Write the failing tests.** In full where they pin safety:

```ts
// lib/markdown.test.ts (added to INC-12's, or created with them)
import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown for text panels', () => {
	it('renders raw HTML inert', () => {
		for (const input of ['<script>alert(1)</script>', '<iframe src="https://x.test"></iframe>',
			'<img src=x onerror=alert(1)>', '<a href="javascript:alert(1)">x</a>', '<svg onload=alert(1)>']) {
			const html = renderMarkdown(input);
			expect(html).not.toMatch(/<(script|iframe|img|svg|a)\b/i);
			expect(html).toContain('&lt;');
		}
	});
	it('allows only http, https and mailto links', () => {
		expect(renderMarkdown('[a](https://x.test/?a=1&b=2)')).toContain(
			'<a href="https://x.test/?a=1&amp;b=2" rel="noopener noreferrer nofollow">a</a>');
		expect(renderMarkdown('[mail](mailto:ops@x.test)')).toContain('<a href="mailto:ops@x.test" rel="noopener noreferrer nofollow">mail</a>');
		for (const bad of ['[x](javascript:alert(1))', '[x](JaVaScRiPt:alert(1))', '[x](data:text/html,<b>)',
			'[x](vbscript:x)', '[x]( javascript:alert(1))', '[x](//evil.test)', '[x](java&#115;cript:x)']) {
			expect(renderMarkdown(bad)).not.toContain('<a ');
		}
	});
	it('never renders images', () => {
		const html = renderMarkdown('![logo](https://x.test/a.png)');
		expect(html).not.toContain('<img');
		expect(html).toContain('![logo](https://x.test/a.png)');
	});
	it('escapes attribute breakouts in link text and urls', () => {
		const html = renderMarkdown('[x"><script>1</script>](https://x.test/"onmouseover="a)');
		expect(html).not.toContain('<script');
		expect(html).not.toMatch(/"\s*onmouseover=/);
	});
});
```

```ts
// lib/server/metrics-proxy.test.ts
import { describe, expect, it } from 'vitest';
import { discoveryPath } from './metrics-proxy';
const P = '7f0c1a8e-1111-4222-8333-944445555666';
describe('discoveryPath', () => {
	it('passes only the keys of each kind', () => {
		expect(discoveryPath(P, new URLSearchParams('kind=names&q=cpu&metric=x&evil=1')))
			.toBe(`/projects/${P}/metrics/names?q=cpu`);
		expect(discoveryPath(P, new URLSearchParams('kind=tag-values&metric=m&tag=host&q=a%26b')))
			.toBe(`/projects/${P}/metrics/tag-values?metric=m&tag=host&q=a%26b`);
	});
	it('refuses unknown kinds, traversal and oversize values', () => {
		expect(discoveryPath(P, new URLSearchParams('kind=../../auth/me'))).toBeNull();
		expect(discoveryPath(P, new URLSearchParams('kind=query'))).toBeNull();
		expect(discoveryPath(P, new URLSearchParams(`kind=names&q=${'q'.repeat(201)}`))).toBeNull();
	});
});
```

Plus, by name and assertion (the implementer writes the bodies):
- `metrics.test.ts`: `parseRange` (preset wins; custom over 90 days → fallback; `from >= to` → fallback; garbage → fallback); `parseVariableValues` (undefined names ignored; non-multi keeps first; `__all` → `[]`; default applied; 101 values truncated to none of the extra; 1,025-char value dropped); `isQueryResult` (accepts the engine shape; rejects a string point value, a missing `series`, a tag value object); `formatValue` (`null` → `—`, `12345` → `12.3k`, `0.1234` with 2 decimals → `0.12`, `%` unit → `50%`); `reduceStat` (ignores nulls; `last` = last non-null; all null → null).
- `chart.test.ts`: `decimate keeps gaps` (a null in the middle survives decimation as a null); `decimate keeps extremes` (the global min and max survive); `decimating 50 series of 1,500 points to 800 columns takes under 50 ms` (wall clock, `performance.now()`), each result ≤ 1,600 points; `linePath` breaks at nulls (`M` count = run count); `timeTicks` returns ascending, in range, at most `maxTicks`; `stack` sums positive values per bucket and leaves `null` buckets `null`.
- `panel-queue.test.ts`: never more than 4 running (10 jobs with deferred promises); a newer job with the same key aborts the older (its signal is aborted); `abortAll` aborts running and rejects queued with `AbortError`; a 429 is retried once after the (fake) sleep and a second 429 is returned.
- Relay `server.test.ts` files (using `#lib/server/fake-event`): a non-UUID param → 404 without calling the engine; the engine path is exactly the documented one; the engine's status and JSON come back; a body over `MAX_RELAY_BYTES` → 413 (relayJsonPost's own test covers it; the query relay test checks it is used).
- `metrics-components-render.test.ts` (`svelte/server` `render`, like `monitor-form-render.test.ts`): `TimeSeriesChart` with 2 series renders 2 `<path>` lines, a legend naming both series, and a data table; a series whose tag value is `<img src=x onerror=alert(1)>` appears escaped in legend and table (no `<img` in the markup); `StatPanel` renders `formatValue` of the reduced value; `MarkdownPanel` with `<script>` renders no `<script`.

- [ ] **Step 2: Run to fail** — UI command with the new test files. Expected: FAIL (modules missing).

- [ ] **Step 3: Implement** the modules, relays and components per the behaviour above. If `json-relay.ts` or `markdown.ts` already exists on `main`, extend it; never fork a second copy.

- [ ] **Step 4: Run to pass** — UI command with the new test files plus the existing suite (`npx vitest run`). Expected: `svelte-check found 0 errors and 0 warnings`, all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): metrics types, relays, SVG time-series chart and safe markdown panels (NAMI-MET-10)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11 (NAMI-MET-11): UI — metrics explorer, navigation and the source snippet

Branch `feat/NAMI-MET-11-ui-explorer`, worktree `.worktrees/NAMI-MET-11/`.

**Files:**
- Create: `nami-ui/src/lib/components/metrics/QueryEditor.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/metrics/+page.server.ts`, `+page.svelte`, `explorer.test.ts`
- Modify: `nami-ui/src/lib/metrics.ts` (+ test: explorer state, snippets), `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` (tabs "Metrics" and "Dashboards", after "Logs"), `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/+page.svelte` (+ its test: "Send metrics" block)

**Interfaces:**
- Consumes: T10 everything; engine `GET .../metrics/names`, `POST .../metrics/query`, `GET .../dashboards`, `POST .../dashboards/{id}/panels`.
- Produces:

```ts
// lib/metrics.ts
export type ExplorerState = { query: QueryDef; range: TimeRange };
export const EMPTY_QUERY: QueryDef;   // { metric: '', aggregation: 'avg', filters: [], group_by: [] }
export function parseExplorerState(params: URLSearchParams): ExplorerState;   // metric, agg, filter (JSON {tag,op,value}, repeatable, ≤ 10), group (repeatable, ≤ 5), range/from/to
export function explorerHref(path: string, state: ExplorerState): string;
export function metricsIngestUrl(engineUrl: string): string;                  // `${base}/api/v1/metrics`
export function metricsCurlSnippet(url: string, token: string): string;
export function prometheusPushSnippet(url: string, token: string): string;    // curl --data-binary @metrics.prom to /api/v1/metrics/prometheus
```

Page behaviour:
- `+page.server.ts` `load`: viewer role from the layout; dashboards list (for "Add to dashboard", members only). Action `addToDashboard` (member): fields `dashboard_id` (`isUuid`), `title` (1-200), `viz` (`line|bar|stat`), `query` (JSON, parsed and shape-checked with the same checks as `isQueryDef` below) → engine `POST /projects/{id}/dashboards/{dashboard_id}/panels` → redirect to the dashboard; failures through `engineFailure`.
- `+page.svelte`: `PageHeader` "Metrics" with `TimeRangePicker`; `QueryEditor` bound to the URL state (`goto(explorerHref(...), { keepFocus: true, noScroll: true })`, so a query is a shareable URL); the query runs in the browser through `fetch('metrics/query', POST)` (one at a time; a newer state aborts the older request); result in a `Card` with `TimeSeriesChart` (line/bar toggle), `Legend`, "showing the 50 largest series" when `truncated`; errors show the engine's message in an `.alert-danger`; empty states via `EmptyState` ("Pick a metric", "No data in this range").
- `QueryEditor.svelte` props `{ query: QueryDef; projectId: string; onchange: (q: QueryDef) => void }`: metric `Input` with a `<datalist>` filled from `metrics/discovery?kind=names&q=` (debounced 250 ms, aborted on the next keystroke); built-ins listed first with a "built-in" `Badge`; aggregation `Select`; filter rows (tag datalist from `kind=tags`, op select, value datalist from `kind=tag-values`), at most 10; group-by chips from the tags list, at most 5; a hint under filters: "`$name` in a value refers to a dashboard variable; `$$` writes a literal `$`".
- `isQueryDef(value: unknown): value is QueryDef` in `lib/metrics.ts` (exported; used by the action and T13).
- Source page: members see "Send metrics" with `metricsCurlSnippet` and `prometheusPushSnippet` (each with `CopyButton`), using the same public URL and token the log snippets use; viewers see neither (they never see tokens).

- [ ] **Step 1: Write the failing tests.**
  - `metrics.test.ts`: `parseExplorerState` round-trips through `explorerHref`; more than 10 filters or 5 group keys are cut; a malformed `filter` JSON is skipped; `isQueryDef` rejects an unknown aggregation, a non-array `filters`, extra keys; `metricsCurlSnippet` shell-quotes the token (`'tok'"'"'en'` for `tok'en`) and posts `Content-Type: application/json`; `prometheusPushSnippet` posts `Content-Type: text/plain; version=0.0.4`.
  - `explorer.test.ts` (`fake-event`): `addToDashboard` refuses a non-UUID `dashboard_id` without calling the engine (400), refuses a `query` that is not a `QueryDef` (400), posts the exact engine path and body on success, and relays an engine 403 through `engineFailure`; a server render of the page for a viewer has no "Add to dashboard" form.
  - Source page test: the "Send metrics" block is rendered for a member (with the token) and absent for a viewer.

- [ ] **Step 2: Run to fail** — UI command with those files. Expected: FAIL.

- [ ] **Step 3: Implement** the page, editor, helpers, tabs and the source block.

- [ ] **Step 4: Run to pass** — UI command, whole suite. Expected: 0 errors, PASS.

- [ ] **Step 5: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): metrics explorer, project navigation and metric setup snippets (NAMI-MET-11)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12 (NAMI-MET-12): UI — dashboard list and dashboard view

Branch `feat/NAMI-MET-12-ui-dashboards`, worktree `.worktrees/NAMI-MET-12/`.

**Files:**
- Create: `nami-ui/src/lib/dashboard-view.ts` (+ test), `nami-ui/src/lib/components/metrics/{VariableBar,PanelCard}.svelte` (+ `dashboard-render.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/dashboards/+page.server.ts`, `+page.svelte`, `dashboards-list.test.ts`, `.../dashboards/[dashboardId=uuid]/+page.server.ts`, `+page.svelte`, `dashboard-view.test.ts`

**Interfaces:**
- Consumes: T10 (`parseRange`, `resolveRange`, `parseVariableValues`, `variableParams`, `createPanelQueue`, chart/stat/markdown components, panel query relay), engine dashboards routes (T8), `data.role` (INC-7 row).
- Produces:

```ts
// lib/dashboard-view.ts
export type PanelState =
  | { kind: 'loading' } | { kind: 'ready'; result: QueryResult } | { kind: 'empty' }
  | { kind: 'error'; message: string } | { kind: 'busy' };   // second 429
export function panelRequestBody(range: { from: string; to: string }, values: Record<string, string[]>): string;
/** Response → PanelState: 200 + isQueryResult → ready/empty; 429 → busy; other → error with the engine's message or a fixed fallback. */
export async function panelStateFrom(response: Response): Promise<PanelState>;
export const REFRESH_CHOICES: { value: 0 | 30 | 60 | 300; label: string }[];
export function nextRefreshDelay(seconds: number, hidden: boolean): number | null;   // null = paused
```

Page behaviour:
- List (`dashboards/+page.server.ts`): `load` → engine list. Actions: `create` (member; `name`, optional `description`) → engine POST → redirect to `.../dashboards/{id}/edit`; `delete` (admin; two-step `confirm=yes`) → engine DELETE. The page shows a `Card` table (name, description, panels, updated, UTC); the create form for members and admins only; delete for admins only. An engine 403/409 surfaces through `engineFailure`.
- View (`[dashboardId]/+page.server.ts`): `load` → engine GET (404 → SvelteKit 404). The page: `PageHeader` with the dashboard name, `TimeRangePicker` (`range` from the URL, default `dashboard.default_range`), a refresh `Select` (`REFRESH_CHOICES`, default `dashboard.refresh_seconds`), an "Edit" link for members; `VariableBar`; a 6-column grid (`grid grid-cols-1 md:grid-cols-6 gap-4`) of `PanelCard`s in `position` order with `WIDTH_CLASSES`.
- `VariableBar.svelte` props `{ variables: Variable[]; values: Record<string, string[]>; projectId: string; hrefFor: (values) => string }`: one `Select` per variable (multi-select for `multi`), options from `metrics/discovery?kind=tag-values&metric=&tag=` loaded once per variable (first 500), plus "All"; choosing navigates to `hrefFor` (URL state, shareable).
- `PanelCard.svelte` props `{ panel: Panel; projectId: string; dashboardId: string; queue: PanelQueue; range: { from: string; to: string }; values: Record<string, string[]>; refreshKey: number }`: `Card` titled with the panel title; `text` → `MarkdownPanel` (no request); other kinds enqueue `fetch(panelQueryPath, { method: 'POST', body: panelRequestBody(..), signal })` keyed by panel id whenever `range`, `values` or `refreshKey` change, and render by `PanelState`: skeleton, `TimeSeriesChart`, `StatPanel`, "No data in this range", the error message in `.alert-danger` (text only), or "Too many queries, retrying on the next refresh".
- The page owns one `createPanelQueue()`; `abortAll()` on range or variable change and in `onDestroy`; auto-refresh bumps `refreshKey` every `refresh_seconds`, paused while `document.visibilityState === 'hidden'` (`nextRefreshDelay`).

- [ ] **Step 1: Write the failing tests.**
  - `dashboard-view.test.ts` (unit, for `lib/dashboard-view.ts`): `panelStateFrom` maps 200 with a valid result → `ready`, 200 with no series → `empty`, 200 with a malformed body → `error` "unexpected response", 429 → `busy`, 400 `{error: "the range may be at most 90 days"}` → that message, 500 non-JSON → fixed fallback; `panelRequestBody` carries `from`, `to`, `variables` only; `nextRefreshDelay(30, true)` → `null`, `(0, false)` → `null`, `(60, false)` → `60000`.
  - `dashboard-render.test.ts` (`svelte/server`): **`a failing panel does not break the dashboard`** — render the view's grid with three panels whose initial states are `error`, `ready` and `text`; the markup contains the error message, a chart `<svg>` and the rendered markdown; a panel title `<script>x</script>` renders escaped; a text panel holding `<iframe src=x>` renders no `<iframe`; a viewer render has no "Edit" link.
  - `dashboards-list.test.ts`: `create` posts `{name, description}` and redirects to the editor; `delete` without `confirm=yes` does not call the engine; an engine 409 (101st dashboard) becomes the form message; a viewer render shows no create form and an admin render shows delete buttons.

- [ ] **Step 2: Run to fail** — UI command with those files. Expected: FAIL.

- [ ] **Step 3: Implement** the module, components and pages.

- [ ] **Step 4: Run to pass** — UI command, whole suite. Expected: 0 errors, PASS.

- [ ] **Step 5: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): dashboard list and view with variables, time range and auto-refresh (NAMI-MET-12)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13 (NAMI-MET-13): UI — dashboard editor (panels, live preview, layout, variables)

Branch `feat/NAMI-MET-13-ui-dashboard-editor`, worktree `.worktrees/NAMI-MET-13/`.

**Files:**
- Create: `nami-ui/src/lib/server/dashboard-form.ts` (+ test), `nami-ui/src/lib/components/metrics/{PanelEditor,VariableEditor}.svelte` (+ `editor-render.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/dashboards/[dashboardId=uuid]/edit/+page.server.ts`, `+page.svelte`, `dashboard-edit.test.ts`
- Modify: `docs/superpowers/roadmap.md` (§6 ticks, last step)

**Interfaces:**
- Consumes: T10, T11 (`QueryEditor`, `isQueryDef`), T12 (`PanelCard` for previews), engine T8 routes and `/metrics/query` with `variables` (T9).
- Produces:

```ts
// lib/server/dashboard-form.ts
export type PanelPayload = { title: string; viz: Viz; query?: QueryDef; markdown?: string; options: PanelOptions; width: Width };
export function panelFromForm(form: FormData): { ok: true; panel: PanelPayload } | { ok: false; message: string };
export function variablesFromForm(form: FormData): { ok: true; variables: Variable[] } | { ok: false; message: string };
export function settingsFromForm(form: FormData): { ok: true; patch: { name: string; description: string; default_range: RangePreset; refresh_seconds: number } } | { ok: false; message: string };
/** The layout body after moving `id` one place up or down, widths kept. */
export function layoutAfterMove(panels: Pick<Panel, 'id' | 'width'>[], id: string, direction: 'up' | 'down'): { panels: { id: string; width: Width }[] };
export function layoutAfterWidth(panels: Pick<Panel, 'id' | 'width'>[], id: string, width: Width): { panels: { id: string; width: Width }[] };
```

Page behaviour (`edit/+page.server.ts`, members and admins; a viewer gets the engine's 403 rendered as the page error):
- `load` → engine GET. Actions, each one engine call and `engineFailure` on error: `settings` (PATCH), `variables` (PATCH `{variables}`; the form posts a hidden JSON field from `VariableEditor`, parsed and checked by `variablesFromForm`: ≤ 10, name charset, label 1-100, metric/tag non-empty, default ≤ 100 values), `savePanel` (`panel_id` absent → POST, present (`isUuid`) → PATCH; body from `panelFromForm`: `viz` in the four kinds, title 1-200, `query` hidden JSON checked by `isQueryDef` for non-text, `markdown` ≤ 10,240 bytes (UTF-8, `TextEncoder`) for text, options per R15), `deletePanel` (two-step), `move` (PUT layout from `layoutAfterMove`), `width` (PUT layout from `layoutAfterWidth`).
- `+page.svelte`: dashboard settings form (name, description, default range, refresh), `VariableEditor` (rows: name, label, metric with the names datalist, tag with the tags datalist, multi, default), the panel list in order with up/down buttons (`aria-label="Move <title> up"`), width `Select`, Edit and Delete; "Add panel" opens `PanelEditor` in a `Card`.
- `PanelEditor.svelte` props `{ projectId: string; panel: Panel | null; variables: Variable[] }`: viz `SegmentedTabs`-styled radio group (line, bar, stat, text), title, `QueryEditor` (non-text) or a markdown `<textarea>` with a byte counter (`x / 10240 bytes`) and a live preview through `MarkdownPanel`, options (unit, decimals, stat reduce for stat, stacked for bar), width. **Live preview** for queries: debounced 500 ms, `fetch('../../metrics/query', POST)` (the T10 relay, resolved from the page URL) with the dashboard's default range and `variables` = each variable's default (or `[]`), rendered with `TimeSeriesChart`/`StatPanel`; preview errors show the engine's message.

- [ ] **Step 1: Write the failing tests.**
  - `dashboard-form.test.ts`: `panelFromForm` accepts a line panel with a valid hidden query, rejects a text panel whose markdown is 10,241 bytes (multi-byte characters counted as bytes: 3,414 × `€` = 10,242 bytes → rejected), rejects `viz=html`, rejects a query JSON with an unknown aggregation or extra keys, drops `stat_reduce` on a line panel (or rejects; pick reject, matching the engine), rejects a title of 201 characters; `variablesFromForm` rejects 11 variables and `Host` as a name; `layoutAfterMove` moves up/down, leaves the first-up and last-down unchanged, and keeps every id once; `layoutAfterWidth` changes only the target.
  - `dashboard-edit.test.ts` (`fake-event`): `savePanel` without `panel_id` POSTs to `.../panels`, with a non-UUID `panel_id` returns 400 without calling the engine, with a UUID PATCHes `.../panels/{panel_id}`; `deletePanel` without `confirm=yes` does not call the engine; `move` PUTs the exact layout body; an engine 400 message reaches the form.
  - `editor-render.test.ts` (`svelte/server`): a text panel's preview of `<script>alert(1)</script>` contains no `<script`; the move buttons carry their `aria-label`s; the byte counter shows `0 / 10240 bytes` for an empty panel.

- [ ] **Step 2: Run to fail** — UI command with those files. Expected: FAIL.

- [ ] **Step 3: Implement** the form module, components and page.

- [ ] **Step 4: Run to pass** — UI command, whole suite. Expected: 0 errors, PASS.

- [ ] **Step 5: Walk through it once** against a local engine (`docker compose up -d postgres clickhouse`, engine and UI in dev): push the node_exporter fixture with `prometheusPushSnippet`'s command, build a dashboard with a line panel (`node_cpu_seconds_total`, `rate`, group by `mode`), a stat panel and a text panel, add a `instance` variable, reorder, change widths, switch ranges, open the view as a viewer. Note what you saw in the PR (no screenshots of tokens).

- [ ] **Step 6: Tick the roadmap.** In `docs/superpowers/roadmap.md` §6 tick: "Metric ingestion (JSON, Prometheus text format) with source tokens", "ClickHouse storage, retention", "Query API …", "Dashboards with charts (line, bar, stat), layout, time range", "Dashboard variables (tag-value dropdowns)", "Markdown text panels", "Built-in metrics from uptime checks and log volume". Leave the rest open (Part 2).

- [ ] **Step 7: Commit**

```bash
git add nami-ui/src docs/superpowers/roadmap.md
git commit -m "feat(ui): dashboard editor with live preview, layout and variables (NAMI-MET-13)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## After Task 13: final review and security wave

Per CLAUDE.md, Part 1 ends with a whole-part review and a dedicated security review + fix wave, before Part 2 starts:

1. **Whole-branch review** of everything merged under NAMI-MET-1..13 against the spec and this plan's rulings: every R1-R22 decision implemented as written, or the deviation recorded; every Global Constraint value present in code (grep the constants); `tests/json_bodies.rs`, `tests/migrations.rs` and `tests/clickhouse.rs` list every new route and table.
2. **Security review** (a fresh reviewer, then a fix wave on `fix/NAMI-MET-security-wave`), checking at least:
   - **Tenant isolation:** every ClickHouse statement binds `project_id` from the path (grep `sql.rs`, `series_guard.rs`, discovery); built-in name resolution filters Postgres by project; dashboard/panel lookups always carry `project_id` (and `dashboard_id` for panels); the share-ready `run_panel_query` cannot be reached without RBAC from Part 1 routes.
   - **Injection:** no `format!` in any SQL path; tag keys only as `tags[{pN:String}]`; `METRIC_READ_SETTINGS` constants not in `RESERVED_SETTINGS`; variable values only as bound `String`s; `build_never_puts_values_in_the_sql_text` covers every target.
   - **DoS and cost:** body/inflate/sample/line caps, per-sample rejection limits, the cardinality guard's memory bound (R9) and its multi-instance slack, `max_rows_to_read`/`max_memory_usage` actually enforced on the ClickHouse version in compose (an expensive-query test against a large synthetic table, run once by hand and recorded), the panel queue's concurrency under 20 panels, per-user and per-instance slots.
   - **XSS:** the only `{@html}` is `renderMarkdown` (grep `nami-ui/src` for `{@html`); tag values, panel titles, variable values and engine error messages render as text everywhere (legend, tooltip, table, dropdowns).
   - **Secrets:** ingest never logs tokens; error bodies never echo tokens or ClickHouse text; viewers never see the source snippets.
   - **RBAC matrix:** viewer reads/queries, member edits dashboards and panels, admin deletes dashboards; every hidden UI action still answers 403 from the engine.
3. **Record the outcome** in `docs/superpowers/decisions/2026-10-13-metrics-part1.md` (rulings kept or changed, deferred items with a target: Part 2, Platform, or "won't do"), and update the roadmap ticks if the review moves anything.
4. **Hand-off to Part 2:** confirm that `validate_body`, `SeriesGuard::admit`, `create_with_panels` and `run_panel_query` are the seams Part 2's remote-write, scrape targets, templates and share links will use, and that Incident Management Part 1 has merged `IncidentSource` and the `metric_alert` source value before metric alerts are planned in detail.
