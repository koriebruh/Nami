# Metrics + Dashboards, Part 2 Implementation Plan

> **Status:** outline. Nothing of Part 2 is merged. Next: NAMI-MET-14, once Metrics Part 1 (NAMI-MET-1..13 and its security wave NAMI-MET-28), Logs Part 2 and Incidents Part 1 have merged; step-level code is written at phase start against the then-current `main`.
> **Branch:** `feat/metrics-part2` in `.worktrees/metrics-part2/`.
> **Depends on:** Metrics Part 1 (`docs/superpowers/plans/2026-10-13-metrics-dashboards-part1.md`), Logs Part 2 (log alerts, NAMI-LOG-14 and NAMI-LOG-18), Incidents Part 1 (lifecycle and escalation routing, NAMI-INC-1 and NAMI-INC-10). Status Pages Part 1 is used for one UI helper (`callPublicEngine`, NAMI-SP-13) and is created here to its contract when it has not merged.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Finish the Metrics + Dashboards sub-project. Part 2 adds Prometheus remote-write ingest, Prometheus scrape targets with target health, the dashboard template library, read-only public share links, metric alerts, one shared alert evaluator for metric and log alerts (no-data handling, confirmation and recovery periods, per-series incidents), the collector bundle (Vector and OpenTelemetry Collector configs with compose snippets) and the nami-ui pages for all of these.

**Architecture:** Every new ingest path ends in Part 1's single admission path: a parser produces samples, `metrics::ingest::validate_body` validates them per sample, `SeriesGuard::admit` admits new series, and the store inserts rows (all planned in NAMI-MET-1..4). Remote-write decodes a snappy block and a protobuf `WriteRequest` with a hand-written decoder built on `prost::encoding` primitives (no derive, no build script), charging Part 1's decode budget before every allocation. The scrape worker claims due targets with the monitors' atomic claim, fetches through the monitors' guarded HTTP client (no proxy, no redirects, link-local and metadata refusal at DNS time), reads at most 10 MiB, parses with Part 1's Prometheus text parser and inserts through the same path, then records target health and the synthetic `up`, `scrape_duration_seconds` and `scrape_samples_scraped` series. Templates are bundled JSON created through Part 1's `create_with_panels`. Share links follow the Status Pages pattern: a token stored as SHA-256, engine routes under `/public/...` on the private API without a session, a per-IP governor, a share-only query pool, and panels run only by id through Part 1's `run_panel_query`. Alerts get one evaluator in the incidents domain (`incidents::alerting`): a pure per-series state machine, an `alert_series_states` table, and the only code that opens and resolves alert incidents through `incidents::lifecycle`. Log alerts (NAMI-LOG-14) move onto it with unchanged default behaviour, and metric alerts are built on it. The collector bundle lives in `deploy/collectors/` as the one source of truth, validated in containers and rendered by member-only setup pages.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, `tower_governor` 0.8, `sha2`, `rand` 0.8), ClickHouse over HTTP (`core::clickhouse::ClickHouse`), Postgres 17, SvelteKit + Svelte 5 runes + Tailwind 4, vitest. **Two new crates:** `snap` and `prost` (R-M2-1). No new npm package. Containers for validation only: Vector and OpenTelemetry Collector contrib images, tags pinned at phase start (R-M2-24).

**Spec:** `docs/superpowers/specs/2026-10-13-metrics-dashboards-design.md`, the whole file, including "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` section 6. Part 1's "Part 2 follows" and "Part 1 leaves room for these" lists define this plan's scope.

**When to start:** after Metrics Part 1, Logs Part 2 and Incidents Part 1 have merged, from the latest `origin/main`. Because those phases are not merged as this is written, every reused piece below is named by its planned path and task. At phase start, read the merged code for each row of the table below; if a name differs, adapt with the same behaviour and say so in the PR; if a behaviour differs, stop and ask the controller.

### Interfaces assumed from unmerged work

| From | Interface | Used by |
|---|---|---|
| NAMI-MET-1 | `metrics::sample::{Sample, MetricType, Rejection}`, the name, tag and value rules, `series_id`; ClickHouse tables `metrics`, `metric_series`; `metrics::store` insert | NAMI-MET-14, NAMI-MET-16 |
| NAMI-MET-2 | the Prometheus text 0.0.4 parser (`metrics::prom_text`, `src/metrics/prom_text.rs`), per-sample results, budget charged before allocation, `tests/fixtures/node_exporter.prom` | NAMI-MET-16, NAMI-MET-17 |
| NAMI-MET-3 | `SeriesGuard::admit`, the one admission point (10,000 new series per source per minute, project active-series cap) | NAMI-MET-14, NAMI-MET-16 |
| NAMI-MET-4 | `metrics::ingest::validate_body` (takes parsed samples from any parser), `MetricsState` (per-source rate limiter, in-flight counters), the R4 response `{accepted, rejected, errors}` (all rejected = 400), `gunzip_within`, `METRICS_MAX_DECOMPRESSED_BYTES = 16 MiB`, `MAX_SAMPLES_PER_REQUEST = 50_000`, the public metrics router in `metrics/ingest/routes.rs`, `tests/common/metrics.rs` helpers | NAMI-MET-14, NAMI-MET-16, NAMI-MET-17 |
| NAMI-MET-5 | `QueryDef`, the query validator, the SQL fragments for targets and filters, `METRIC_READ_SETTINGS`, `metrics::sql::user_error`, `MAX_METRIC_QUERIES_PER_INSTANCE = 32` | NAMI-MET-18, NAMI-MET-20 |
| NAMI-MET-6, NAMI-MET-7 | discovery over `metric_series` (tag values), built-in metrics `nami.uptime.*`, `nami.logs.count` | NAMI-MET-17, NAMI-MET-18 |
| NAMI-MET-8 | `metrics::dashboard::model::create_with_panels` (one transaction, at most 50 panels), dashboards quota 100 per project (409), the panel and variable validators | NAMI-MET-17 |
| NAMI-MET-9 | `run_panel_query(state, project_id, dashboard_id, panel_id, request)` with no user (RBAC and slots stay in the caller), variable substitution into filter values only | NAMI-MET-17, NAMI-MET-18 |
| NAMI-MET-10..13 | nami-ui `#lib/metrics/*` types, `TimeSeriesChart`, `StatPanel`, `MarkdownPanel`, `PanelCard`, `VariableBar`, `#lib/panel-queue.ts` (4-wide abortable queue), the JSON relay with `signal`, the dashboard view and editor | NAMI-MET-23, NAMI-MET-24 |
| NAMI-LOG-13 | `logs::view::saved_query::LogQuery`, the log filter SQL and field-path validation (paths bound as parameters) | NAMI-MET-21 |
| NAMI-LOG-14 | `log_alerts` table, `logs::alert::{claim_due_alerts, apply_evaluation, record_failure, run_pass_with, run_forever}`, `MAX_ALERT_EVALUATIONS = 8`, `ALERT_TICK = 5 s`, `ALERT_CLAIM_BATCH = 50`, the edited-alert drop (`updated_at`), project-wide maintenance holding the state, the R-P2-27 incident seam | NAMI-MET-19, NAMI-MET-20, NAMI-MET-21 |
| NAMI-LOG-18 | nami-ui log alert pages | NAMI-MET-25 |
| NAMI-INC-1 | `incidents::lifecycle::{IncidentSource, OpenIncident, open, resolve_source}` (`#[non_exhaustive]` source enum, one conflict target per variant), the `incidents.source` check that already lists `metric_alert`, the unique index of one unresolved incident per log alert | NAMI-MET-19, NAMI-MET-20 |
| NAMI-INC-7 | `GET /projects/{id}` returns the caller's `role`; the project layout passes `data.role` | NAMI-MET-23..26 |
| NAMI-INC-10 | `escalation_policy_id` and `severity_id` routing through `lifecycle::open`, `lifecycle::resolve_source_with_reason`, `incidents::worker::kick(&state)` | NAMI-MET-19, NAMI-MET-20, NAMI-MET-21 |
| NAMI-SP-13 | `callPublicEngine` in `#lib/server/core/engine` (no cookie forwarded, no `set-cookie` relayed, exactly one client address in `X-Forwarded-For`, headers built from scratch). **If not merged when NAMI-MET-24 starts, that task creates it with exactly that contract and its tests.** | NAMI-MET-24 |

Merged on `main` and reused as is: `uptime::checker::{HttpClients, names_forbidden_address, is_forbidden_destination, read_capped, describe_request_error, is_unsafe_in_text, IpVersion}` in `src/uptime/checker/mod.rs` (the guarded resolver is applied inside `HttpClients`; `read_capped` stops silently at the cap, see R-M2-9); the monitors' claim `uptime::worker::claim_due_monitors` (the pattern, not the function); `core::crypto::Cipher` (seal with a per-row context); `core::validate::{validate_label, nullable}`; `logs::source::model::find_by_token` and the source token routes in `src/logs/source/routes.rs` (`GET /projects/{id}/sources/{source_id}/token`, member); `logs::ingest::{bearer_token, read_body, MAX_BODY_BYTES}`; `logs::ingest::budget::Budget`; `core::router::build_router` and its `rate_limited_with_governor` pattern (`SmartIpKeyExtractor`); nami-ui `#lib/server/core/engine` (`callEngine`, `enginePath`, `engineFailure`, `publicEngineUrl`), `#lib/core/uuid` (`isUuid`), `#lib/logs/logs` (`TOKEN_PLACEHOLDER`), the design system in `#lib/components/ui`.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R6.01 | Metric ingestion: the Prometheus remote-write half (JSON and text are Part 1) | NAMI-MET-14, NAMI-MET-27 |
| R6.02 | Prometheus scrape targets (pull) with target health | NAMI-MET-15, NAMI-MET-16, NAMI-MET-23, NAMI-MET-27 |
| R6.08 | Dashboard template library | NAMI-MET-17, NAMI-MET-24 |
| R6.09 | Read-only public share link per dashboard (revocable) | NAMI-MET-18, NAMI-MET-24, NAMI-MET-27 |
| R6.11 | Metric alerts (threshold to incident) | NAMI-MET-20, NAMI-MET-25, NAMI-MET-27 |
| R6.12 | Alert improvements (metric and log): no-data, confirmation/recovery, per-series incidents | NAMI-MET-19, NAMI-MET-20, NAMI-MET-21, NAMI-MET-25, NAMI-MET-27 |
| R6.13 | Collector bundle with configs, compose snippets and UI setup pages | NAMI-MET-22, NAMI-MET-26, NAMI-MET-27 |
| R6.14 | UI for all of the above: the Part 2 features (Part 1's UI is NAMI-MET-10..13) | NAMI-MET-23, NAMI-MET-24, NAMI-MET-25, NAMI-MET-26 |

NAMI-MET-27 is the phase security review and fix wave. When the phase PR merges, the controller ticks every section 6 item that is still open: the items above, and the Part 1 items if Part 1's merge has not ticked them already.

## Migrations

Postgres, one file per domain, numbered at phase start as the next free numbers in `nami-engine/migrations/`, **in this order** (foreign keys run from the first to the later ones):

1. `NNNN_metrics_part2.sql` (created by NAMI-MET-15, appended by NAMI-MET-18 and NAMI-MET-20): `metric_scrape_targets`, `dashboard_shares`, `metric_alerts`.
2. `NNNN_incidents_alert_series.sql` (created by NAMI-MET-19, appended by NAMI-MET-20): `alert_series_states`, `incidents.alert_series_key`, the per-series unique indexes, `incidents.metric_alert_id`. It references `metric_alerts` (file 1) and `log_alerts` (Logs Part 2), so it runs after file 1.
3. `NNNN_logs_alert_series.sql` (created by NAMI-MET-21): the new `log_alerts` columns. It runs after file 2 because its backfill writes `alert_series_states`.

A merged migration is never edited. The appends above are allowed because the files have not merged. ClickHouse: no new table (remote-write and scrape write Part 1's `metrics` and `metric_series`).

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port 5433). The collector validation and end-to-end runs use `docker run` of pinned images. Never install anything natively.
- Engine test command: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <domain> <file>::`. UI: `cd nami-ui && npm run check && npx vitest run <files>` (0 errors, 0 warnings).
- SQLx runtime queries only, SQL as string literals, values bound. ClickHouse SQL only from `&'static str` fragments through `logs::query::Sql`, every value bound, the project id from the path (or from the token's source, or from the stored row for workers and share links), never from a body.
- `docs/STANDARDS.md` applies in full: `deny_unknown_fields` bodies, closed sets as enums with `check` constraints, bounds as named constants with a test at the bound and one past it, 403 for authorization, 404 for a foreign id in the path, 400 for a foreign id stored in a row, 409 for name clashes and quotas, 429 for rate and slot limits, no lock across `.await`, decoding and parsing on `spawn_blocking`, secrets sealed or hashed and never logged.
- Spec values, verbatim, and where they land:
  - remote-write: protobuf `WriteRequest`, snappy-compressed; malformed protobuf or snappy is 400 without a panic (fuzz cases); a decompression-bomb guard (NAMI-MET-14);
  - scrape targets: `interval_seconds` 15-3600, default 60; `timeout_seconds` 1-30 and below the interval; `labels` at most 20, added to every sample, `job` and `instance` default to the target name and the URL's host:port; bearer or basic auth, encrypted, write-only; `Accept: text/plain;version=0.0.4`; 10 MiB response; 50,000 samples per scrape (more fails the scrape); redirects not followed; health `up|down|unknown` with `last_scrape_at`, `last_duration_ms`, `last_samples`, `last_error`; synthetic `up`, `scrape_duration_seconds`, `scrape_samples_scraped`; members manage (NAMI-MET-15, NAMI-MET-16);
  - templates: Uptime overview, Host (node_exporter), PostgreSQL (postgres_exporter), Redis (redis_exporter), Nginx (nginx-prometheus-exporter), Docker (cAdvisor), with variables and the exporters' standard metric names, each tested against fixture series (NAMI-MET-17);
  - share links: one active per dashboard, 32 random bytes, stored SHA-256, revoke = delete, `/share/dashboards/[token]` without a session and `noindex`, only that dashboard's own panels by panel id, range at most 90 days, variable values, never an arbitrary query, per-IP rate limit (429), unknown or revoked token 404, the dialog warning; members share (NAMI-MET-18, NAMI-MET-24);
  - metric alerts: aggregation over `window_seconds`, `gt|gte|lt|lte`, threshold, interval, enabled, state, optional `escalation_policy_id` and `severity_id`, incidents carry `metric_alert_id`, notifications through escalation (NAMI-MET-20);
  - evaluator: `no_data` `zero|keep|alert` (default `keep`, metric alerts only), `confirmation_seconds` and `recovery_seconds` 0-3600 (default 0), `incident_mode` `per_alert` (default) or `per_series` (group-by tags, or up to 3 field paths for log alerts), at most 50 open incidents per alert with further breaching series noted on the alert, `alert_series_states (alert_kind, alert_id, series_key, state, pending_since, recovering_since, incident_id)` (NAMI-MET-19, NAMI-MET-20, NAMI-MET-21);
  - collector bundle: `deploy/collectors/`, Vector (`file` on container log files, `journald`, `host_metrics`) to `/api/v1/logs` (NDJSON) and remote-write, OpenTelemetry Collector contrib (`hostmetrics`, `filelog`, `otlp`, `prometheusremotewrite`), optional node_exporter and cAdvisor with matching scrape configs, `/var/lib/docker/containers` read-only, Docker-socket variants opt-in and documented as root-equivalent, member-only setup pages with copy and download, `vector validate` and `otelcol validate` on every rendered config, an end-to-end run (NAMI-MET-22, NAMI-MET-26).
- nami-ui: every engine call through `callEngine` or `callPublicEngine` with `enginePath`; `[...=uuid]` matchers and `isUuid`; deletes two-step; `#lib/server` never imported by components; UTC times; design-system tokens and components only; the only `{@html}` stays `renderMarkdown(...)` output.
- Commits: Conventional Commits with the task ID, e.g. `feat(engine): prometheus remote-write ingest (NAMI-MET-14)`, ending with the attribution line the session gives.

## Review Focus

- **A 10 KiB remote-write body that claims to inflate to 4 GiB must cost nothing.** The snappy length header is read and refused before any buffer is allocated -> NAMI-MET-14 (`a_snappy_body_claiming_more_than_the_cap_is_refused_before_inflating`).
- **Prometheus marks a vanished series with a special NaN on every remote-write.** Those staleness markers must be dropped quietly, or every Prometheus shows a stream of rejections -> NAMI-MET-14 (`staleness_markers_are_dropped_without_counting_as_rejected`).
- **An exporter answering 11 MiB must fail the scrape, not store a truncated tail.** `read_capped` truncates silently, so the scrape reads one byte past the cap -> NAMI-MET-16 (`a_response_over_10_mib_fails_the_scrape_and_stores_nothing`).
- **A scrape URL is a request to a user-chosen host.** An IP literal of the metadata service in any spelling (`http://2852039166/` parses to `169.254.169.254`), a redirect and the environment's proxy must all fail to reach anything -> NAMI-MET-16. Names that resolve to such an address are refused by the monitors' guarded resolver inside `HttpClients`, already pinned on `main` (`allowed_addresses` unit tests in `src/uptime/checker/mod.rs`, `ping_refuses_a_name_that_resolves_to_a_refused_address` in `tests/uptime/checker_ping_dns.rs`).
- **A share token is a password for a dashboard.** It must never reach a log line (not in an engine path), never run a query outside its dashboard's panels and never survive a revoke -> NAMI-MET-18.
- **Moving log alerts onto the shared evaluator must change nothing by default.** Every NAMI-LOG-14 test stays green with `confirmation_seconds = 0`, `recovery_seconds = 0`, `per_alert` -> NAMI-MET-21.
- **Per-series incidents must stay bounded.** A group-by over 5,000 hosts that all breach opens 50 incidents, notes the rest, and keeps `alert_series_states` bounded -> NAMI-MET-19, NAMI-MET-20.
- **A rendered collector config holds a live token.** Only members see or download it, and the download is `no-store` -> NAMI-MET-26.

## Decisions the spec leaves open (rulings)

### Remote-write

- **R-M2-1 Crates.** Two crates, added with exact versions:
  - `cargo add snap@1.1.2`. Snappy **block** format (what remote-write sends; not the framed format). `snap::raw::decompress_len` reads the length a body claims without allocating, which is what the bomb guard needs. Pure Rust, no dependencies. Hand-writing a snappy decoder is the kind of parsing code that should not be new.
  - `cargo add prost@0.14.4 --no-default-features --features std`. Only the wire primitives (`prost::encoding::{decode_key, decode_varint, skip_field, WireType, DecodeContext}` and the `encode_*` helpers the tests use to build fixtures). It pulls only `bytes`, already in `Cargo.lock` (1.12.1). Without the default `derive` feature there is no proc-macro crate and no build script, so STANDARDS section 8 needs no decision record. A derived `Message::decode` is not used because it allocates every repeated message and string before anything can be charged: an empty `TimeSeries` costs 2 bytes on the wire and far more in memory. The hand decoder charges Part 1's budget first.
  - At phase start, check the helper signatures in the 0.14.4 docs. If a name differs, adapt with the same behaviour.
- **R-M2-2 Wire contract.** `POST /api/v1/metrics/prometheus/remote_write` on the public listener, next to Part 1's metric endpoints, with the same source token (`Authorization: Bearer`, Prometheus `remote_write.authorization.credentials`), the same `IngestLimits` places and the same `MetricsState` rate limit and in-flight counts as Part 1's endpoints.
  - `Content-Encoding: snappy` is required (else 415). `Content-Type` must be `application/x-protobuf` (parameters other than `proto` ignored). `proto=io.prometheus.write.v2.Request` is 415 `remote-write 2.0 is not supported: send prometheus.WriteRequest`. `X-Prometheus-Remote-Write-Version` is read but not required.
  - The body as sent is at most `logs::ingest::MAX_BODY_BYTES` (10 MiB, 413). The decompression-bomb guard: `decompress_len` above `METRICS_MAX_DECOMPRESSED_BYTES` (16 MiB, Part 1) is 413 **before** a buffer is allocated. A snappy error is 400.
  - The answer is Part 1's R4 response (202, or 400 when every sample was rejected), 413 for budget or sample count, 429 `cardinality_exceeded` with `Retry-After` from `SeriesGuard::admit`. Prometheus retries 5xx and 429 and drops a 400, which is the wanted behaviour for a misconfigured sender.
- **R-M2-3 Decoding.** `WriteRequest` field 1 `timeseries` and field 3 `metadata`. `TimeSeries` field 1 `labels`, 2 `samples`, 3 `exemplars` (skipped), 4 `histograms` (each one counted as rejected, `native histograms are not supported`). `Label` 1 `name`, 2 `value`. `Sample` 1 `value` (double), 2 `timestamp` (int64 ms). Unknown fields are skipped with `skip_field`.
  - `__name__` is the metric name; a series without it is rejected (`missing __name__`). Every other label is a tag, under Part 1's rules (at most 50, key and value limits). A label name repeated within one series rejects the series. Invalid UTF-8 in a label rejects that series.
  - A structural error (a varint past 10 bytes, a length past the end, a wrong wire type on a known field) is 400 `malformed protobuf at byte <offset>`, and nothing is stored.
  - Metadata types map by `metric_family_name` exactly as Part 1's text parser maps `# TYPE` lines (NAMI-MET-2), including the `_bucket`, `_count` and `_sum` series of histogram and summary families. A series without metadata is a gauge.
  - Every label string, series and sample charges `Budget::for_request()` before it is allocated (Part 1 R6), so a body never decodes to more than the budget (413).
  - The 50,001st sample answers what Part 1 answers for it (`MAX_SAMPLES_PER_REQUEST`).
  - **Staleness markers** (the exact NaN bit pattern `0x7ff0000000000002` Prometheus writes when a series disappears) are dropped without counting as rejected. Every other NaN or infinity is rejected per sample, as the spec says. Without this, every Prometheus would show a stream of rejections after each target restart.
  - Timestamps follow Part 1 R7 (older than the source's retention or more than 10 minutes ahead: rejected per sample).
- **R-M2-4 Fuzz cases without new tooling.** No cargo-fuzz (nightly and a new tool). The decoders get table-driven malformed inputs and a seeded mutation loop: `rand::rngs::StdRng::seed_from_u64` with fixed seeds, 20,000 mutations (bit flips, truncation, length and varint lies, inserted bytes) of valid fixtures, for the snappy plus protobuf path and for Part 1's text parser. Each iteration asserts an `Ok` or an `Err`, never a panic. If the loop takes more than 10 s in a debug build, lower the count and say so in the PR.

### Scrape targets

- **R-M2-5 Client.** Scrapes reuse the monitors' shared clients: `uptime::checker::HttpClients::select(false, true, has_auth, IpVersion::Any)` gives redirect mode `None`, `.no_proxy()` first, the guarded resolver on every connection (link-local and metadata addresses refused at DNS time) and no connection pool. The worker holds one `HttpClients`. The `User-Agent` is overridden per request with `Nami-Scraper/<version>`.
  - IP literals never reach a resolver, so `names_forbidden_address(url)` is checked at create and update (400 `destination not allowed`) and again before every scrape.
  - TLS is always verified (the spec has no option for it). A 3xx answer fails the scrape with `redirects are not followed (HTTP <code>)`.
- **R-M2-6 Outbound policy.** Part 1 left the operator policy hook to this plan. Ruling: no new setting in Part 2. Scrapes use the monitors' guard (private and loopback allowed, link-local and metadata refused), which is the spec's "default allow". Platform's one outbound policy (`OUTBOUND_ALLOW_PRIVATE_NETWORKS`, `OUTBOUND_DENY_CIDRS`) is applied in that shared guard, so scrapes follow it without a change here. Recorded in the decisions doc for Platform.
- **R-M2-7 Target fields.**
  - `name`: `validate_label`, unique per project (409).
  - `url`: `http` or `https`, host required, no userinfo (400 `put credentials in the auth fields`), no fragment, at most 2,048 bytes (`MAX_TARGET_URL_BYTES`).
  - `labels`: at most `MAX_TARGET_LABELS = 20`; keys follow Part 1's tag-key rule and may not start with `__`; values at most 1 KiB. Setting `job` or `instance` overrides the default.
  - `interval_seconds` 15..=3600, default 60. `timeout_seconds` 1..=30, below the interval, default 10.
  - `source_id`: a source of the same project (400 otherwise). Samples take its retention.
  - At most `MAX_SCRAPE_TARGETS_PER_PROJECT = 100` (409).
  - `auth`: `null`, `{"kind": "bearer", "token"}` (at most 4,096 bytes) or `{"kind": "basic", "username", "password"}` (username at most 256 bytes without `:`, password at most 1,024 bytes); no control characters. Sealed as one JSON value with `core::crypto::Cipher` under `metric_scrape_targets.auth:{id}` (the id is generated in Rust before the insert). Responses carry only `auth_kind` (`none|bearer|basic`). PATCH uses `Option<Option<_>>`: absent keeps, `null` clears, a value replaces.
  - A PATCH that changes the URL's origin (scheme, host or port) while keeping stored credentials is 400 `enter the credentials again when changing the target's host`, as monitors do (`tests/uptime/monitor_secret_retarget.rs`), so a member cannot send another member's token to a host of their choice.
  - Roles: viewers read (never credentials); **members create, edit and delete** (the spec says "Members manage"; an exception to STANDARDS section 4's admin delete, recorded).
- **R-M2-8 Worker.** Tick 1 s. At most `MAX_CONCURRENT_SCRAPES = 16` per instance; a pass claims only free capacity, reserved before the claim (the uptime worker's `pending` pattern). The claim is one statement: `update metric_scrape_targets set next_scrape_at = now() + make_interval(secs => interval_seconds) where id in (select id from metric_scrape_targets where enabled and next_scrape_at <= now() order by next_scrape_at limit $1 for update skip locked) returning ...`. `timeout_seconds` bounds the whole scrape (request and body) with `tokio::time::timeout`. Parsing runs on `spawn_blocking` with a fresh budget and waits for one of the engine-wide decode places (shared with ingest) within the remaining timeout. Scrapes first take one of `MAX_SCRAPE_DECODES = 2` permits, so at most two of the four decode places ever serve scrapes and push ingest always keeps two. A result is applied only when the row's `updated_at` still equals the claimed one (an edit during the scrape drops it). Spawned once in `main.rs`; a failed pass is logged and the loop continues.
- **R-M2-9 Body cap.** `read_capped` stops silently at its cap, so the worker reads with `MAX_SCRAPE_BYTES + 1` and fails when it got more than `MAX_SCRAPE_BYTES = 10 MiB` (`response over 10 MiB`); a `Content-Length` above the cap fails before reading. Requests send `Accept: text/plain;version=0.0.4` and `Accept-Encoding: identity`; a response with another `Content-Encoding` fails (`compressed responses are not supported`). `application/openmetrics-text` fails (`OpenMetrics is not supported: serve text format 0.0.4`, Part 1 R5); any other content type is parsed as 0.0.4. Only 2xx is a success (`HTTP <code>` otherwise).
- **R-M2-10 Scrape ingest.**
  - More than `MAX_SCRAPE_SAMPLES = 50_000` samples (the spec's limit, equal to `MAX_SAMPLES_PER_REQUEST`), a parse error (`line <n>: ...`) or a cardinality refusal (`cardinality limit reached`) fails the scrape and stores none of its samples.
  - Per-sample rejections store the valid samples, keep the health `up` and set `last_error` to `<k> samples rejected; first: <reason>`.
  - Labels: the target's labels, then the defaults `job` = target name and `instance` = the URL's `host:port` (port 80 or 443 when absent). A scraped label whose key the target sets is renamed `exported_<key>` (Prometheus `honor_labels: false`); a sample that then breaks Part 1's tag rules is rejected.
  - An exposition timestamp is honoured (Part 1 R7); without one the sample takes the scrape's start time.
  - Samples go through `validate_body` and `SeriesGuard::admit` like pushed ones. Scrapes skip the per-source request rate limit (they are bounded by the interval and the target cap).
  - After every scrape, success or failure, three gauges with the target's labels and the scrape's start time go through their own admission, so a refused main batch still records `up = 0`: `up` (1 or 0), `scrape_duration_seconds`, `scrape_samples_scraped` (0 on failure).
  - Health: `health` `up|down`, `last_scrape_at`, `last_duration_ms`, `last_samples` (stored count), `last_error` (at most 256 chars, `is_unsafe_in_text` characters replaced; fixed texts and `describe_request_error` only, never the response body or a credential). A new target is `unknown` until its first scrape. Disabling keeps the last values; the UI shows "disabled".

### Templates

- **R-M2-11 Templates.** Bundled JSON files under `nami-engine/src/metrics/template/` (`include_str!`, parsed once), each a dashboard with variables and panels in the shape `create_with_panels` takes. Ids: `uptime-overview`, `node-exporter`, `postgres-exporter`, `redis-exporter`, `nginx-exporter`, `cadvisor`. A unit test runs every template through Part 1's validators, so an invalid template fails the build's tests, never a request. Each template has one text panel naming the exporter and the scrape setup it expects. Panel metric names are the exporters' standard ones (listed in NAMI-MET-17). Part 1's query API has no ratios, so templates show raw gauges and rates (no "memory used %"). Fixtures are trimmed real scrapes in `tests/fixtures/metrics/`, the exporter version named in each file's first comment line.

### Share links

- **R-M2-12 Token and lifecycle.** 32 bytes from the OS RNG, base64url without padding (43 chars), stored only as `token_hash` (SHA-256, unique). The token is in the create response once and never again. One active link per dashboard (`unique (dashboard_id)`): a POST while one exists is 409 `revoke the current link first`, so replacing a link is a deliberate revoke and create. Revoke deletes the row (204; 404 when there is none). Deleting the dashboard cascades. Viewers read the share status (active, created at, created by); members create and revoke (spec: "Members create/edit/share").
- **R-M2-13 Engine public routes.** On the private API (nami-ui is the only caller, as Status Pages R3), without a session, under `/public/dashboard-share`. The token travels in the header `x-nami-share-token`, never in an engine path, so it stays out of request logs.
  - `GET /public/dashboard-share` returns the definition DTO: name, description, default range, refresh, variables (`name`, `label`, `multi`, `default`) and panels (`id`, `title`, `viz`, `width`, `position`, `options`, `markdown`). No panel query JSON, no variable metric or tag, no project or dashboard id.
  - `POST /public/dashboard-share/panels/{panel_id}/query` with `{from, to, step?, variables?}` runs `run_panel_query` for that panel only (a panel of another dashboard is 404, a text panel 400, Part 1's 90-day range and variable rules apply).
  - `GET /public/dashboard-share/variables/{name}/values?q=` returns the tag values for that variable's own metric and tag only (Part 1's discovery, at most 500). A name the dashboard does not define is 404. Without it a shared dashboard's dropdowns would be empty; the share dialog's warning covers these values.
  - A missing, malformed (`^[A-Za-z0-9_-]{43}$`) or unknown token is 404 with one body. The share row is read fresh on every request, so a revoke applies at once on every instance.
  - Per-IP governor (`SmartIpKeyExtractor`, the address nami-ui forwards): `per_millisecond(100)` (10/s), burst 40, 429 JSON. A 20-panel dashboard with 10 variables loads within one burst.
  - Slots: one of `MAX_SHARE_QUERIES_PER_INSTANCE = 8` and one of Part 1's `MAX_METRIC_QUERIES_PER_INSTANCE`, both `try_acquire` (429). There is no user, so no user slot.
- **R-M2-14 No result cache.** Ranges are absolute, so a cache key would be unique per viewer. Cost is bounded by the governor, the 8 share slots and `METRIC_READ_SETTINGS`. A cache is a Platform follow-up if measured. (Part 1's note on following Status Pages is kept for access: decided from the fresh row.)
- **R-M2-15 Share page.** `src/routes/share/dashboards/[token]/` sits outside `(app)`, so no session is needed. The token format is checked before any engine call (404 page otherwise). Responses send `x-robots-tag: noindex, nofollow`, `referrer-policy: no-referrer` and `cache-control: no-store`, and the page has `<meta name="robots" content="noindex">`. It reuses Part 1's panel components read-only (no edit links, no project navigation). Panel data and variable values go through BFF relays under the same route that call `callPublicEngine` with the token header. The BFF never logs the token.

### Alerts

- **R-M2-16 Where the evaluator lives.** `incidents::alerting` (`src/incidents/alerting/`): it turns alert observations into incidents. Logs and metrics both depend on incidents already; logs must not import metrics, and `core` holds no domain logic.
- **R-M2-17 State machine** (pure, `incidents::alerting::machine::step`). States `Ok`, `Pending { since }`, `Alerting`, `Recovering { since }`. Observations `Breach`, `Clear`, `NoData`. `NoDataMode` `Zero` (evaluate the condition with value 0), `Keep` (state and timers unchanged), `Alert` (a breach). Actions `None`, `Open`, `Resolve`.
  1. `Ok` + `Breach`: confirmation 0 → `Alerting`, `Open`; else `Pending { now }`.
  2. `Pending { s }` + `Breach`: `now - s >= confirmation` → `Alerting`, `Open`; else unchanged. `Pending` + `Clear` → `Ok`.
  3. `Alerting` + `Clear`: recovery 0 → `Ok`, `Resolve`; else `Recovering { now }`. `Alerting` + `Breach` → unchanged.
  4. `Recovering { s }` + `Clear`: `now - s >= recovery` → `Ok`, `Resolve`; else unchanged. `Recovering` + `Breach` → `Alerting` (no new incident).
  5. `Ok` + `Clear` → `Ok`.
  - `now` is the evaluation time, so a period resolves to the alert's interval (documented in the API help). A log count is never missing: log alerts never produce `NoData` (an absent group counts 0).
  - `per_alert` collapses observations first: `Breach` when any series breaches, `Clear` when some series exist and none breaches, `NoData` when there are none. Its series key is `''`.
  - `per_series`: the key is the canonical JSON array of `[key, value]` pairs sorted by key (metric group-by tags, or log field paths), at most Part 1's `MAX_SERIES_KEY_BYTES` (8,192).
  - A series whose linked incident was resolved by a person becomes `Ok` before its observation is applied, so a series that still breaches alerts again after its confirmation period.
- **R-M2-18 Bounds.**
  - `MAX_OPEN_INCIDENTS_PER_ALERT = 50`. Alerting series without an incident are opened in key order while fewer than 50 are open. The rest stay `Alerting` with no incident and are noted on the alert: `overflow_series` (count) and `overflow_sample` (at most 10 keys), cleared when nothing overflows. A later evaluation opens them as slots free.
  - `MAX_EVALUATED_SERIES = 1_000`. The evaluation query asks for 1,001 groups; a 1,001st fails the evaluation (`the query matches more than 1,000 series: add filters or group by fewer tags`) and changes no state.
  - `MAX_SERIES_STATES_PER_ALERT = 1_000` rows. `Ok` is never stored (no row means `Ok`). An absent `Pending` series is dropped. Absent `Alerting` and `Recovering` series follow `no_data` (`keep` keeps them). A new series beyond 1,000 rows is not tracked and is counted in `overflow_series`.
- **R-M2-19 Incident shape.** `incidents.alert_series_key text not null default ''` (`''` = the whole alert). INC-1's unique index of one unresolved incident per log alert becomes `(log_alert_id, alert_series_key) where status <> 'resolved'`, and `(metric_alert_id, alert_series_key) where status <> 'resolved'` is added. `IncidentSource::LogAlert(Uuid)` becomes `LogAlert { alert_id, series_key }` and `MetricAlert { alert_id, series_key }` is added, each with its own conflict-target literal (INC R4). The wrappers `incidents::model::{open_for_log_alert, resolve_for_log_alert}` keep their signatures and pass `''`. Titles: `Metric alert: <name>` or `Log alert: <name>`, plus ` {k="v", ...}` for a series, at most 200 chars, unsafe characters replaced. Cause: `<aggregation> of <metric> over <window> was <value> (threshold <op> <threshold>)`, or `no data` when missing data counts as a breach.
- **R-M2-20 One seam for alert incidents.** `incidents::alerting::{open_series_incident, resolve_series_incident}` are the only code that opens or resolves an alert incident. They supersede R-P2-27's `logs::alert::{open_alert_incident, resolve_alert_incident}`, which are removed with their behaviour moved. Opening passes the alert's `escalation_policy_id` and `severity_id` (INC-10 routing). Recovery uses `lifecycle::resolve_source`; disabling and editing use `resolve_source_with_reason` with `alert disabled` or `alert changed`. After its transaction commits, a pass that opened or resolved anything calls `incidents::worker::kick(&state)` once.
- **R-M2-21 Edits, disable, delete.** A PATCH that changes the query, the series fields or group-by, or `incident_mode` resolves the alert's open incidents (`alert changed`) and deletes its series states in the same transaction. Threshold, comparison, window, interval, periods and `no_data` changes keep the states and apply from the next evaluation. Disabling resolves with `alert disabled` (INC R17) and deletes the states. Deleting an alert cascades its states; its incidents follow LOG-14's `on delete cascade` precedent for both kinds.
- **R-M2-22 Evaluation budget.** One process-wide semaphore `incidents::alerting::ALERT_EVALUATIONS` of `MAX_ALERT_EVALUATIONS = 8`, shared by the log and the metric loop (LOG-14's semaphore moves there). Each loop ticks every 5 s, claims at most 50 due alerts and never beyond free capacity, drops an evaluation whose alert was edited meanwhile (`updated_at`), records `last_error` (fixed text) on failure without changing state, and holds the state machine while a project-wide maintenance window is in effect (LOG-14's rule, now for metric alerts too). Metric evaluations take no user query slot and run under `METRIC_READ_SETTINGS`.
- **R-M2-23 Metric alert query.** Part 1's `QueryDef` without variables (400 `alerts cannot use dashboard variables`). `window_seconds` 60..=86400, `interval_seconds` 60..=86400, at most `MAX_METRIC_ALERTS_PER_PROJECT = 100` (409), threshold finite with an absolute value at most `MAX_ABS_THRESHOLD = 1e15`, comparison `gt|gte|lt|lte`. One value per group over `[now - window, now)`: `avg|min|max|sum|count|p50|p95|p99` over the window's samples, and `rate` as Part 1's per-series increase (resets from zero) divided by the window, summed per group. `per_series` needs a non-empty `group_by` (400). Built-in targets are allowed (`nami.uptime.up` `avg` `lt` 0.99). A group with no sample in the window is absent: `NoData` for its key. Log alerts keep `gt|lt` (spec) and get no `no_data` setting.
- **R-M2-24 Alert state field.** `metric_alerts.state` and `log_alerts.state` stay `ok|alerting`, where `alerting` means at least one series is `Alerting` or `Recovering`. The per-series detail comes from `GET .../series` (viewer, at most 1,000 rows).

### Collector bundle

- **R-M2-25 One source of truth.** The files in `deploy/collectors/` use environment interpolation, so they work as shipped with an `.env`: Vector `${NAMI_URL}` and `${NAMI_TOKEN}`, the OpenTelemetry Collector `${env:NAMI_URL}` and `${env:NAMI_TOKEN}`. Every placeholder sits inside a scalar that is already double-quoted in the shipped file (`uri: "${NAMI_URL}/api/v1/logs"`, `Authorization: "Bearer ${env:NAMI_TOKEN}"`). The setup page renders a copy by replacing each placeholder string with its value, `\` and `"` escaped, and adds no quotes of its own (the URL comes from `publicEngineUrl`, the token from the reveal endpoint). A vitest fails when any placeholder in any file is not inside a double-quoted scalar. nami-ui reads the files with Vite `?raw` imports in a server-only module. If Vite refuses a path outside `nami-ui/`, NAMI-MET-26 copies them into `nami-ui/src/lib/server/metrics/collectors/` and adds a vitest that fails when a copy differs from `deploy/collectors/`.
- **R-M2-26 Images.** Vector (`-debian` variant, needed for `journald`), `otel/opentelemetry-collector-contrib`, `prom/node-exporter` and `gcr.io/cadvisor/cadvisor` are pinned to exact release tags chosen at phase start (newest stable then), never `latest`. The tags are written in the compose files and the decisions doc.
- **R-M2-27 OpenTelemetry logs and traces.** Part 2 ships the metrics pipeline (`hostmetrics` and `otlp` to `prometheusremotewrite`). `filelog` and the `otlphttp` logs and traces exporters are defined for the pipelines the Traces sub-project switches on, once it adds the OTLP endpoints. If `otelcol validate` rejects defined but unused components, that block ships commented out with that note.
- **R-M2-28 Docker socket.** Opt-in variants `vector/vector.docker.yaml` (`docker_logs`) and `otelcol/config.docker.yaml` (`docker_stats`), each starting with a comment that mounting the Docker socket, even `:ro`, is root-equivalent on the host. The default variants read `/var/lib/docker/containers` read-only.
- **R-M2-29 Collector tests.** `deploy/collectors/validate.sh` renders every config by plain text replacement of the four placeholder strings with URL-safe dummy values (`https://nami.example.test`, a 43-char base64url dummy token) into a temporary directory. For such values the escaping of R-M2-25 changes nothing, so the output is byte-identical to the setup page's for the same values. It then runs `vector validate --no-environment` and `otelcol-contrib validate --config` in `docker run --rm` containers. The end-to-end check is an ignored engine test (`cargo test --test metrics collector_e2e:: -- --ignored`, Linux, `--network host`): it serves a test public listener on a TCP port, runs the Vector config, rendered by the same plain replacement with the test's URL and token, against it for up to 60 s, and asserts that log rows and `host_*` metric rows arrive in the test ClickHouse database.
- **R-M2-30 Setup pages.** `src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/collectors/`. Viewers get 403 (`data.role`, INC-7). The configs (Vector, Vector with the Docker socket, OpenTelemetry Collector, its Docker variant, the exporters compose file) show `TOKEN_PLACEHOLDER` until a member uses "Reveal token" (the source's `GET .../token`, member). Copy uses `CopyButton`. Download is a POST to a `+server.ts` with a JSON `{kind}`; it fetches the token from the engine and answers the rendered file with `content-disposition: attachment` and `cache-control: no-store`. The token never appears in a URL or a GET response.

### Not in Part 2

- **R-M2-31** TLS skip-verify for scrape targets, Prometheus service discovery (spec non-goal), OpenMetrics, remote-write 2.0 and remote-read, native histograms, a share result cache (R-M2-14), per-IP limits on the public listener and the outbound policy settings (Platform).

## Spec deviations (recorded in the decisions doc)

- Remote-write lands in Part 2 (Part 1 R1, accepted 2026-10-19).
- Staleness markers are dropped, not rejected (R-M2-3).
- Members delete scrape targets (spec "Members manage", R-M2-7); scrape TLS is always verified (R-M2-5).
- The share token reaches the engine in a header (R-M2-13); share results are not cached (R-M2-14).
- `alert_series_states` replaces the spec's `(alert_kind, alert_id)` pair with two nullable foreign keys, `log_alert_id` and `metric_alert_id`, exactly one set, so deleting an alert cascades its rows (NAMI-MET-19, NAMI-MET-20). The kind is which key is set.
- R-P2-27's log-alert seam is replaced by the shared one (R-M2-20).

## File Structure

Part 1's module and file names are taken as its plan gives them in the domain layout (`nami-engine/src/metrics/...`, `tests/metrics/...`, `nami-ui/src/lib/metrics/...`); where the merged names differ, follow the merged code. `Tn` below is task NAMI-MET-n.

```
deploy/collectors/
  vector/vector.yaml, vector/vector.docker.yaml, vector/compose.yaml          T22
  otelcol/config.yaml, otelcol/config.docker.yaml, otelcol/compose.yaml      T22
  exporters/compose.yaml (node_exporter, cAdvisor)                            T22
  validate.sh                                                                 T22
nami-engine/
  Cargo.toml, Cargo.lock                        T14 snap, prost (R-M2-1)
  migrations/NNNN_metrics_part2.sql             T15 targets; T18 shares; T20 metric_alerts
  migrations/NNNN_incidents_alert_series.sql    T19 alert_series_states, series key; T20 metric_alert_id
  migrations/NNNN_logs_alert_series.sql         T21 log_alerts columns
  src/main.rs                                   T16 scrape worker; T20 metric alert worker
  src/core/router.rs                            T15, T17, T18, T20 mount routes; T18 share governor
  src/core/state.rs                             T18 share slots
  src/metrics/ingest/remote_write.rs            T14 snappy + WriteRequest decoder
  src/metrics/ingest/mod.rs, routes.rs          T14 body kind, route
  src/metrics/scrape/{mod,model,routes}.rs      T15 targets; T16 fetch and ingest
  src/metrics/scrape/worker.rs                  T16 claim, pass, loop
  src/metrics/template/{mod,routes}.rs, *.json  T17
  src/metrics/share/{mod,model,routes}.rs       T18
  src/metrics/sql.rs                            T20 evaluate_window
  src/metrics/alert/{mod,model,routes}.rs       T20
  src/incidents/alerting/{mod,machine,model}.rs T19 evaluator, series states, seam
  src/incidents/lifecycle.rs                    T19 LogAlert { .. }; T20 MetricAlert { .. }
  src/incidents/model.rs                        T19 wrappers pass ''
  src/logs/alert/{mod,model,routes}.rs          T21 onto the evaluator, per-series counts
  tests/fixtures/metrics/*.prom                 T17 exporter fixtures
  tests/metrics/main.rs                         every task adds its mod lines
  tests/metrics/remote_write.rs                 T14
  tests/metrics/scrape_targets.rs               T15
  tests/metrics/scrape_worker.rs                T16
  tests/metrics/templates.rs                    T17
  tests/metrics/shares.rs                       T18
  tests/metrics/metric_alerts.rs                T20
  tests/metrics/collector_e2e.rs                T22 (ignored by default)
  tests/incidents/alert_series.rs               T19
  tests/logs/log_alerts.rs                      T21 additions
  tests/core/migrations.rs, json_bodies.rs      T15, T18, T19, T20, T21 additions
nami-ui/src/
  lib/metrics/scrape.ts (+ test)                T23 types, health labels, form parsing
  lib/metrics/share.ts (+ test)                 T24 token format, DTO types
  lib/metrics/alerts.ts (+ test)                T25 shared alert form fields
  lib/server/metrics/collectors.ts (+ test)     T26 raw imports, rendering
  lib/components/metrics/ShareDialog.svelte     T24
  lib/components/metrics/TemplateGallery.svelte T24
  lib/components/metrics/AlertEvaluationFields.svelte   T25 (metric and log alert forms)
  routes/(app)/projects/[id=uuid]/metrics/scrape-targets/...          T23
  routes/(app)/projects/[id=uuid]/dashboards/templates/...            T24
  routes/(app)/projects/[id=uuid]/metrics/alerts/...                  T25
  routes/(app)/projects/[id=uuid]/logs/alerts/... (LOG-18 pages)      T25 new fields
  routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/collectors/...   T26
  routes/share/dashboards/[token]/...                                 T24
```

---

## Tasks

Order: T14, T15, T16, T17, T18, T19, T20, T21, T22, T23, T24, T25, T26, T27, on the one branch. T14 and T15 are independent; T16 needs T15; T17 and T18 need only Part 1; T20 and T21 need T19; T22 needs T14 (remote-write is Vector's metrics sink). UI tasks need their engine tasks: T23 after T16, T24 after T17 and T18, T25 after T20 and T21, T26 after T22. Engine and UI tasks may run in parallel worktrees once their API contract (below) is fixed, merged back before review (CLAUDE.md rule 5).

### Task 14 (NAMI-MET-14): Add Prometheus remote-write ingest

**Covers:** R6.01 (remote-write half)  **Depends on:** NAMI-MET-4

**Files:**
- Create: `nami-engine/src/metrics/ingest/remote_write.rs`, `nami-engine/tests/metrics/remote_write.rs`.
- Modify: `nami-engine/Cargo.toml` and `Cargo.lock` (R-M2-1), `nami-engine/src/metrics/ingest/mod.rs` (body kind `RemoteWrite`), `nami-engine/src/metrics/ingest/routes.rs` (route), `nami-engine/tests/metrics/main.rs`, `nami-engine/tests/common/metrics.rs` (`post_remote_write`, `encode_write_request`), `nami-engine/src/metrics/prom_text.rs` (its fuzz unit test).

**Data shape:**
- `pub const STALE_NAN_BITS: u64 = 0x7ff0_0000_0000_0002;`
- `pub fn inflate_snappy(body: &[u8], cap: usize) -> Result<Vec<u8>, IngestError>`: `decompress_len` first (over `cap`: `IngestError` for 413), then `snap::raw::Decoder::decompress_vec` (error: 400).
- `pub fn decode_write_request(raw: &[u8], budget: &mut Budget, received: DateTime<Utc>) -> Result<Decoded, RemoteWriteError>` with `pub struct Decoded { pub samples: Vec<Result<Sample, Rejection>>, pub stale_dropped: usize }` and `pub enum RemoteWriteError { Malformed { offset: usize }, TooLarge, TooManySamples }`. Per-series and per-sample problems are `Rejection`s (Part 1's type), structural ones are `Malformed`.
- Private message structs mirror the wire (`TimeSeries`, `Label`, `RawSample`); only decoded and charged values are kept.

**API:** `POST /api/v1/metrics/prometheus/remote_write` (public listener, source token). 202 / 400 (malformed, every sample rejected) / 401 (token) / 413 (as sent over 10 MiB, inflated claim over 16 MiB, budget, sample count) / 415 (encoding, content type, remote-write 2.0) / 429 (rate, in-flight, cardinality with `Retry-After`).

**Behaviour:** R-M2-2 and R-M2-3. Decoding runs on `spawn_blocking` holding a decode place, as Part 1's decoders do. The decoded samples go through `validate_body` and `SeriesGuard::admit` unchanged.

**Tests** (`tests/metrics/remote_write.rs` unless noted):
- `a_snappy_write_request_is_stored`, `the_name_label_is_the_metric_and_other_labels_are_tags`, `counter_metadata_marks_the_series_as_a_counter`, `histogram_family_series_take_the_text_parsers_types`.
- `a_series_without_a_name_is_rejected_alone`, `nan_and_infinite_values_are_rejected_per_sample`, `staleness_markers_are_dropped_without_counting_as_rejected`, `native_histograms_are_counted_as_rejected`, `out_of_window_timestamps_are_rejected_per_sample`.
- `a_snappy_body_claiming_more_than_the_cap_is_refused_before_inflating` (a 64-byte body whose header claims 4 GiB: 413), `a_body_at_the_inflated_cap_is_accepted`, `a_body_one_byte_over_the_inflated_cap_is_413`, `a_body_that_is_not_snappy_is_400`, `malformed_protobuf_is_400_and_stores_nothing`.
- `a_body_without_snappy_encoding_is_415`, `a_body_that_is_not_protobuf_is_415`, `remote_write_2_is_415`.
- `a_missing_or_unknown_token_is_401`, `the_cardinality_guard_applies_to_remote_write`, `the_decode_budget_stops_a_body_of_empty_series`, `the_50001st_sample_is_refused_like_the_other_endpoints`, `remote_write_shares_the_metric_rate_limit_and_in_flight_counts`.
- Unit (`remote_write.rs`): `unknown_fields_are_skipped`, `a_varint_longer_than_ten_bytes_is_malformed`, `a_length_past_the_end_is_malformed`, `a_wrong_wire_type_on_a_known_field_is_malformed`, `a_repeated_label_name_rejects_the_series`, `invalid_utf8_in_a_label_rejects_the_series`, `fuzzed_write_requests_never_panic`, `fuzzed_snappy_never_panics` (R-M2-4).
- Unit (`prom_text.rs`): `fuzzed_exposition_text_never_panics` (R-M2-4).

**Acceptance:** a Prometheus configured with `remote_write: [{url: <public>/api/v1/metrics/prometheus/remote_write, authorization: {credentials: <token>}}]` stores its samples (checked in T22's end-to-end run with Vector, which sends the same format); `cargo test --test metrics remote_write::` and the unit tests pass; no panic in any fuzz iteration; `cargo clippy --all-targets` is clean.

### Task 15 (NAMI-MET-15): Store and manage scrape targets

**Covers:** R6.02  **Depends on:** NAMI-MET-4

**Files:**
- Create: `nami-engine/migrations/NNNN_metrics_part2.sql`, `nami-engine/src/metrics/scrape/{mod,model,routes}.rs`, `nami-engine/tests/metrics/scrape_targets.rs`.
- Modify: `nami-engine/src/metrics/mod.rs`, `nami-engine/src/core/router.rs`, `nami-engine/tests/metrics/main.rs`, `nami-engine/tests/core/migrations.rs` (`all_tables_exist`), `nami-engine/tests/core/json_bodies.rs`.

**Data shape:** table `metric_scrape_targets`, every constraint named `metric_scrape_targets_<rule>`:
- `id uuid primary key` (generated in Rust, R-M2-7), `project_id uuid not null references projects(id) on delete cascade`, `source_id uuid not null references sources(id) on delete cascade`, `name text not null`, `url text not null` (`length(url) <= 2048`), `interval_seconds int not null default 60` (15-3600), `timeout_seconds int not null default 10` (1-30, `timeout_seconds < interval_seconds`), `labels jsonb not null default '{}'` (an object), `auth_kind text not null default 'none'` (`none|bearer|basic`), `auth_enc bytea` (`(auth_kind = 'none') = (auth_enc is null)`), `enabled boolean not null default true`, `next_scrape_at timestamptz not null default now()`, `health text not null default 'unknown'` (`up|down|unknown`), `last_scrape_at timestamptz`, `last_duration_ms int`, `last_samples int`, `last_error text`, `created_by uuid references users(id) on delete set null`, `created_at`, `updated_at timestamptz not null default now()`.
- `unique (project_id, name)`; index on `project_id`; claim index on `(next_scrape_at) where enabled`.
- Rust: `pub enum TargetHealth { Up, Down, Unknown }`, `pub enum AuthKind { None, Bearer, Basic }`, `#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)] pub enum TargetAuth { Bearer { token: String }, Basic { username: String, password: String } }` (zeroized on drop, never `Debug`-printed), `pub struct ScrapeTarget { id, project_id, source_id, name, url, interval_seconds, timeout_seconds, labels: BTreeMap<String, String>, auth_kind, enabled, health, last_scrape_at, last_duration_ms, last_samples, last_error, created_at, updated_at }`, `CreateScrapeTargetRequest { name, source_id, url, interval_seconds?, timeout_seconds?, labels?, auth?, enabled? }`, `PatchScrapeTargetRequest` with every field optional and `auth: Option<Option<TargetAuth>>` (`core::validate::nullable`). Constants `MAX_SCRAPE_TARGETS_PER_PROJECT`, `MAX_TARGET_LABELS`, `MAX_TARGET_URL_BYTES`, `INTERVAL_RANGE`, `TIMEOUT_RANGE`, `DEFAULT_INTERVAL_SECONDS`, `DEFAULT_TIMEOUT_SECONDS`.

**API:**

| Method and path | Role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/metrics/scrape-targets` | viewer | 200 `[ScrapeTarget]` | 403 |
| `POST /projects/{id}/metrics/scrape-targets` | member | 201 `ScrapeTarget` | 400, 403, 409 (name, quota) |
| `GET /projects/{id}/metrics/scrape-targets/{target_id}` | viewer | 200 | 403, 404 |
| `PATCH /projects/{id}/metrics/scrape-targets/{target_id}` | member | 200 | 400, 403, 404, 409 |
| `DELETE /projects/{id}/metrics/scrape-targets/{target_id}` | member | 204 | 403, 404 |

**Behaviour:** R-M2-7. The quota check and the insert share one transaction. A change of URL, interval or enabled sets `next_scrape_at = now()`. Every write bumps `updated_at`.

**Tests** (`tests/metrics/scrape_targets.rs`):
- `a_member_creates_a_target_and_a_viewer_reads_it_without_credentials`, `a_viewer_cannot_create_edit_or_delete_a_target`, `a_member_can_delete_a_target`.
- `interval_limits_are_15_and_3600` (15 and 3600 pass, 14 and 3601 fail), `timeout_limits_are_1_and_30_and_below_the_interval`, `labels_are_capped_at_twenty`, `label_keys_starting_with_two_underscores_are_refused`, `a_label_value_over_1_kib_is_refused`.
- `a_url_with_userinfo_or_another_scheme_is_refused`, `a_url_at_2048_bytes_passes_and_one_past_fails`, `an_ip_literal_of_a_forbidden_address_is_refused` (`169.254.169.254`, `[fe80::1]`, `0.0.0.0`, `[::ffff:169.254.169.254]`), `a_private_address_is_allowed`.
- `a_source_of_another_project_is_400`, `a_target_of_another_project_is_404`, `the_101st_target_is_409`, `a_duplicate_name_is_409`.
- `credentials_are_sealed_and_never_returned` (the raw `auth_enc` does not contain the token bytes and opens only under `metric_scrape_targets.auth:{id}`), `patch_null_clears_credentials_and_absent_keeps_them`, `changing_the_host_while_keeping_credentials_is_400`, `deleting_the_source_deletes_its_targets`.
- `tests/core/migrations.rs`: `metric_scrape_targets` in `all_tables_exist`; `tests/core/json_bodies.rs`: the POST and PATCH routes.

**Acceptance:** the five routes answer as the table says for each role; no response or log line carries a credential.

### Task 16 (NAMI-MET-16): Scrape targets and record their health

**Covers:** R6.02  **Depends on:** NAMI-MET-15

**Files:**
- Create: `nami-engine/src/metrics/scrape/worker.rs`, `nami-engine/tests/metrics/scrape_worker.rs`.
- Modify: `nami-engine/src/metrics/scrape/mod.rs` (fetch, parse, label merge, outcome), `nami-engine/src/metrics/scrape/model.rs` (claim, apply outcome), `nami-engine/src/main.rs` (spawn `run_forever`), `nami-engine/tests/metrics/main.rs`.

**Data shape:**
- `pub const SCRAPE_TICK: Duration = Duration::from_secs(1); pub const MAX_CONCURRENT_SCRAPES: usize = 16; pub const MAX_SCRAPE_BYTES: usize = 10 * 1024 * 1024; pub const MAX_SCRAPE_SAMPLES: usize = 50_000;`
- `pub struct DueTarget { id, project_id, source_id, name, url, timeout_seconds, labels, auth_kind, auth_enc, updated_at }`.
- `pub enum ScrapeOutcome { Up { duration: Duration, stored: usize, rejected: usize, first_rejection: Option<String> }, Down { duration: Duration, error: String } }`.
- `pub async fn claim_due_targets(pool: &PgPool, limit: i64) -> Result<Vec<DueTarget>, sqlx::Error>`; `pub async fn scrape(state: &AppState, clients: &HttpClients, target: &DueTarget) -> ScrapeOutcome`; `pub async fn apply_outcome(pool: &PgPool, target: &DueTarget, outcome: &ScrapeOutcome, at: DateTime<Utc>) -> Result<bool, sqlx::Error>` (false when the row changed meanwhile); `pub async fn run_pass(state: &AppState) -> Result<usize, AppError>` (waits for its scrapes; tests); `pub async fn run_forever(state: AppState)`.

**API:** none (health is read through T15's routes).

**Behaviour:** R-M2-5, R-M2-8, R-M2-9, R-M2-10. Order inside one scrape: forbidden-literal check, credentials opened (`core::crypto::Cipher`; a failure is `down` with `credentials could not be decrypted`), request, status and headers, capped body, parse (blocking, budget, sample cap), label merge, admission and insert, synthetic series, health update.

**Tests** (`tests/metrics/scrape_worker.rs`, wiremock on loopback):
- `a_due_target_is_scraped_and_its_samples_are_stored_with_its_labels`, `job_and_instance_default_to_the_target_name_and_host_port`, `a_scraped_label_the_target_sets_is_renamed_exported`, `samples_take_the_sources_retention`, `exposition_timestamps_are_honoured`.
- `every_scrape_writes_up_duration_and_samples_series` (after a success and after a failure).
- `overlapping_passes_scrape_each_target_once`, `a_disabled_or_not_yet_due_target_is_not_scraped`, `a_pass_never_claims_beyond_free_capacity`.
- `a_response_at_10_mib_is_parsed`, `a_response_over_10_mib_fails_the_scrape_and_stores_nothing`, `a_content_length_over_the_cap_fails_before_reading`.
- `a_scrape_of_50000_samples_passes_and_one_of_50001_fails`, `malformed_exposition_text_marks_the_target_down_and_the_worker_keeps_running`, `per_sample_rejections_keep_the_target_up_and_note_last_error`, `a_cardinality_refusal_fails_the_scrape_but_still_records_up_zero`.
- `a_redirect_is_not_followed` (the redirect target mock records no request), `a_forbidden_ip_literal_stored_in_the_row_is_not_contacted`, `a_numeric_spelling_of_the_metadata_address_is_refused`, `a_non_2xx_answer_marks_the_target_down`, `openmetrics_responses_fail_the_scrape`, `a_compressed_response_fails_the_scrape`, `a_slow_target_times_out`.
- `the_accept_header_asks_for_text_format_0_0_4`, `bearer_and_basic_credentials_are_sent`, `credentials_never_appear_in_last_error`.
- `an_edit_during_a_scrape_drops_its_result`.
- The environment-proxy rule is already pinned for `HttpClients` by the standalone `tests/uptime_checker_proxy.rs`; this task adds no process-wide test.

**Acceptance:** a node_exporter fixture served by wiremock becomes stored series and `up = 1`; every failure mode in the list leaves the worker running with health `down` and a fixed-text `last_error`.

### Task 17 (NAMI-MET-17): Ship the dashboard template library

**Covers:** R6.08  **Depends on:** NAMI-MET-8, NAMI-MET-9

**Files:**
- Create: `nami-engine/src/metrics/template/{mod,routes}.rs` and `uptime-overview.json`, `node-exporter.json`, `postgres-exporter.json`, `redis-exporter.json`, `nginx-exporter.json`, `cadvisor.json` in that directory; `nami-engine/tests/fixtures/metrics/{postgres_exporter,redis_exporter,nginx_exporter,cadvisor}.prom` (Part 1's `node_exporter.prom` is reused); `nami-engine/tests/metrics/templates.rs`.
- Modify: `nami-engine/src/metrics/mod.rs`, `nami-engine/src/core/router.rs`, `nami-engine/tests/metrics/main.rs`, `nami-engine/tests/core/json_bodies.rs`.

**Data shape:** `pub struct Template { pub id: &'static str, pub name: String, pub description: String, pub dashboard: <Part 1's create request with panels> }`; `pub fn all() -> &'static [Template]` (parsed once from `include_str!`); response `{templates: [{id, name, description, panels: n, variables: [name]}]}`; request `CreateFromTemplate { template: String, name: Option<String> }` (`deny_unknown_fields`).

Panels (titles abbreviated; each template also has one text panel):
- `uptime-overview`, variable `monitor` (metric `nami.uptime.up`, tag `monitor_name`, multi, default All): availability stat (`avg nami.uptime.up`), availability by monitor (line, `avg`, group `monitor_name`), response time (line, `p95 nami.uptime.response_time_ms`, group `monitor_name`), log volume (bar, `rate nami.logs.count`, group `level`). Monitor panels filter `monitor_name eq $monitor`.
- `node-exporter`, variable `instance` (`node_load1`, `instance`): CPU busy (`rate node_cpu_seconds_total`, `mode neq idle`, group `instance`), load (`avg node_load1`), memory available (`avg node_memory_MemAvailable_bytes`), memory total (stat, `node_memory_MemTotal_bytes`), filesystem available (`avg node_filesystem_avail_bytes`, group `mountpoint`), network in and out (`rate node_network_receive_bytes_total`, `rate node_network_transmit_bytes_total`, group `device`), disk read and written (`rate node_disk_read_bytes_total`, `rate node_disk_written_bytes_total`).
- `postgres-exporter`, variables `instance` (`pg_up`) and `datname` (`pg_stat_database_numbackends`, multi): up (stat, `pg_up`), connections (`pg_stat_database_numbackends`, group `datname`), commits and rollbacks (`rate pg_stat_database_xact_commit`, `rate pg_stat_database_xact_rollback`), block hits and reads (`rate pg_stat_database_blks_hit`, `rate pg_stat_database_blks_read`), database size (`pg_database_size_bytes`, group `datname`).
- `redis-exporter`, variable `instance` (`redis_up`): up, clients (`redis_connected_clients`), memory (`redis_memory_used_bytes`), commands (`rate redis_commands_processed_total`), hits and misses (`rate redis_keyspace_hits_total`, `rate redis_keyspace_misses_total`), keys (`redis_db_keys`, group `db`).
- `nginx-exporter`, variable `instance` (`nginx_up`): up, active connections (`nginx_connections_active`), accepted (`rate nginx_connections_accepted`), requests (`rate nginx_http_requests_total`), reading, writing, waiting (`nginx_connections_reading`, `nginx_connections_writing`, `nginx_connections_waiting`).
- `cadvisor`, variable `container` (`container_memory_working_set_bytes`, tag `name`, multi): CPU (`rate container_cpu_usage_seconds_total`, group `name`), memory (`container_memory_working_set_bytes`, group `name`), network in and out (`rate container_network_receive_bytes_total`, `rate container_network_transmit_bytes_total`, group `name`), filesystem (`container_fs_usage_bytes`, group `name`).

**API:**

| Method and path | Role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/dashboard-templates` | viewer | 200 | 403 |
| `POST /projects/{id}/dashboards/from-template` | member | 201 (Part 1's dashboard response) | 400 (unknown template, name), 403, 409 (Part 1's quota and name rules) |

**Behaviour:** R-M2-11. The name defaults to the template's name. Creation goes through `create_with_panels`, so Part 1's validators and quota apply exactly as for a dashboard made by hand.

**Tests** (`tests/metrics/templates.rs`, unit tests in `template/mod.rs`):
- Unit: `every_template_passes_the_dashboard_validators`, `every_template_has_a_text_panel_and_at_most_50_panels`.
- `a_viewer_lists_templates_but_cannot_create_from_one`, `a_member_creates_a_dashboard_from_each_template`, `an_unknown_template_is_400`, `the_dashboard_quota_applies_to_templates`.
- `every_panel_of_each_exporter_template_returns_data_from_its_fixture`: ingest the exporter's fixture twice, 60 s apart in sample time, through Part 1's Prometheus text endpoint; create from the template; run every non-text panel through the panel query route with default variables over the last hour; each returns at least one series with a non-null point.
- `the_uptime_overview_template_returns_data_from_checks_and_logs` (seeded `uptime_checks` and `logs` rows).

**Acceptance:** six templates, each creatable by a member and each panel answering with data against its exporter's fixture.

### Task 18 (NAMI-MET-18): Serve read-only public share links (engine)

**Covers:** R6.09  **Depends on:** NAMI-MET-9

**Files:**
- Create: `nami-engine/src/metrics/share/{mod,model,routes}.rs`, `nami-engine/tests/metrics/shares.rs`.
- Modify: `nami-engine/migrations/NNNN_metrics_part2.sql` (append), `nami-engine/src/metrics/mod.rs` (`MetricsState.share_slots`), `nami-engine/src/core/router.rs` (the share-management routes; the public share routes behind their own governor, merged into `build_router` like `rate_limited_with_governor`), `nami-engine/tests/metrics/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`.

**Data shape:**
- Table `dashboard_shares`: `id uuid primary key default gen_random_uuid()`, `dashboard_id uuid not null references dashboards(id) on delete cascade`, `project_id uuid not null references projects(id) on delete cascade`, `token_hash bytea not null` (`octet_length(token_hash) = 32`), `created_by uuid references users(id) on delete set null`, `created_at timestamptz not null default now()`. Constraints `dashboard_shares_one_per_dashboard unique (dashboard_id)`, `dashboard_shares_token_hash_unique unique (token_hash)`.
- Rust: `pub const SHARE_TOKEN_BYTES: usize = 32; pub const SHARE_TOKEN_HEADER: &str = "x-nami-share-token"; pub const MAX_SHARE_QUERIES_PER_INSTANCE: usize = 8;` `pub struct ShareStatus { active: bool, created_at: Option<DateTime<Utc>>, created_by: Option<Uuid> }`, `pub struct CreatedShare { token: String, path: String, created_at }` (`path` = `/share/dashboards/<token>`), `pub enum CreateShare { Created(CreatedShare), Exists, NoDashboard }`, `pub struct ShareRow { id, project_id, dashboard_id }`, `pub fn parse_token(header: &str) -> Option<[u8; 32]>`, `pub async fn find_by_token(pool, token: &[u8; 32]) -> Result<Option<ShareRow>, sqlx::Error>` (hashes, then one indexed lookup).
- Public DTO: `SharedDashboard { name, description, default_range, refresh_seconds, variables: [SharedVariable { name, label, multi, default }], panels: [SharedPanel { id, title, viz, width, position, options, markdown }] }`.

**API:**

| Method and path | Role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/dashboards/{dashboard_id}/share` | viewer | 200 `ShareStatus` | 403, 404 |
| `POST /projects/{id}/dashboards/{dashboard_id}/share` (body `{}`) | member | 201 `CreatedShare`, `cache-control: no-store` | 403, 404, 409 |
| `DELETE /projects/{id}/dashboards/{dashboard_id}/share` | member | 204 | 403, 404 (none active) |
| `GET /public/dashboard-share` (token header) | none | 200 `SharedDashboard` | 404, 429 |
| `POST /public/dashboard-share/panels/{panel_id}/query` | none | 200 (Part 1's query response) | 400, 404, 429 |
| `GET /public/dashboard-share/variables/{name}/values?q=` | none | 200 `{values, truncated}` | 400 (`q` over 200 chars), 404, 429 |

**Behaviour:** R-M2-12, R-M2-13, R-M2-14. Public responses carry `cache-control: no-store` and `x-robots-tag: noindex`. The public routes read the share row, then the dashboard and panel by `project_id` and `dashboard_id` from that row only. No public route writes anything.

**Tests** (`tests/metrics/shares.rs`):
- `a_member_creates_a_link_and_the_token_is_shown_once`, `a_viewer_sees_the_status_but_cannot_create_or_revoke`, `a_second_link_while_one_is_active_is_409`, `the_token_is_stored_only_as_its_sha256`.
- `revoking_makes_the_token_404_at_once`, `deleting_the_dashboard_revokes_its_link`, `unknown_malformed_and_missing_tokens_are_404_with_one_body`, `a_token_in_the_path_is_not_accepted`.
- `the_definition_carries_no_queries_and_no_ids`, `a_share_runs_only_its_own_dashboards_panels` (a panel of another dashboard of the same project, and of another project: 404), `a_text_panel_query_is_400`, `a_range_of_90_days_passes_and_one_past_is_400`.
- `a_variable_value_is_bound_literally_through_a_share` (Part 1's injection strings), `variable_values_are_limited_to_the_variables_own_metric_and_tag`, `an_undefined_variable_name_is_404`, `a_share_cannot_write` (other methods on the public paths: 405).
- `reads_are_rate_limited_per_client_address` (the 41st request of one forwarded address is 429, another address still 200), `share_queries_are_bounded_by_their_own_slots` (with the 8 permits held, a query is 429).
- `tests/core/json_bodies.rs`: the share POST and the public query POST.

**Acceptance:** a share token opens exactly one dashboard read-only, its panels by id and its variables' values, and nothing else; a revoke ends it at once.

### Task 19 (NAMI-MET-19): Build the shared alert evaluator and its incident seam

**Covers:** R6.12  **Depends on:** NAMI-INC-1, NAMI-INC-10, NAMI-LOG-14

**Files:**
- Create: `nami-engine/src/incidents/alerting/{mod,machine,model}.rs`, `nami-engine/migrations/NNNN_incidents_alert_series.sql`, `nami-engine/tests/incidents/alert_series.rs`.
- Modify: `nami-engine/src/incidents/mod.rs`, `nami-engine/src/incidents/lifecycle.rs` (`LogAlert { alert_id, series_key }` and its conflict-target literal), `nami-engine/src/incidents/model.rs` (the log-alert wrappers pass `''`), the `IncidentSource::LogAlert` call sites in `nami-engine/src/logs/alert/mod.rs` (mechanical), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`.

**Data shape:**
- Migration (incidents domain): `incidents.alert_series_key text not null default ''`; INC-1's one-unresolved-per-log-alert index replaced by `incidents_one_unresolved_per_log_alert_series` on `(log_alert_id, alert_series_key) where log_alert_id is not null and status <> 'resolved'`. Table `alert_series_states`: `id uuid primary key default gen_random_uuid()`, `log_alert_id uuid not null references log_alerts(id) on delete cascade` (T20 relaxes this), `series_key text not null` (`octet_length(series_key) <= 8192`), `labels jsonb not null default '[]'` (an array of `[key, value]` pairs), `state text not null` (`pending|alerting|recovering`; `ok` is never stored), `pending_since timestamptz` (set exactly when `pending`), `recovering_since timestamptz` (set exactly when `recovering`), `incident_id uuid references incidents(id) on delete set null`, `updated_at timestamptz not null default now()`; unique `(log_alert_id, series_key)`. Backfill: every `log_alerts` row in state `alerting` gets a row `('', 'alerting', <its unresolved incident>)`.
- `machine.rs` (pure): `pub enum SeriesState { Ok, Pending { since: DateTime<Utc> }, Alerting, Recovering { since: DateTime<Utc> } }`, `pub enum Observation { Breach, Clear, NoData }`, `pub enum NoDataMode { Zero, Keep, Alert }`, `pub enum IncidentMode { PerAlert, PerSeries }`, `pub enum Comparison { Gt, Gte, Lt, Lte }` with `fn holds(self, value: f64, threshold: f64) -> bool`, `pub struct Periods { confirmation: Duration, recovery: Duration }` (built only by `Periods::new(confirmation_seconds, recovery_seconds) -> Result<Periods, String>`, 0..=3600 each), `pub enum Action { None, Open, Resolve }`, `pub fn observe(value: Option<f64>, comparison: Comparison, threshold: f64, no_data: NoDataMode) -> Observation`, `pub fn collapse(observations: impl IntoIterator<Item = Observation>) -> Observation`, `pub fn step(state: SeriesState, observation: Observation, periods: Periods, now: DateTime<Utc>) -> (SeriesState, Action)`, `pub fn series_key(labels: &[(String, String)]) -> String`. Constants `MAX_PERIOD_SECONDS = 3600`, `MAX_OPEN_INCIDENTS_PER_ALERT = 50`, `MAX_EVALUATED_SERIES = 1_000`, `MAX_SERIES_STATES_PER_ALERT = 1_000`, `MAX_ALERT_EVALUATIONS = 8`.
- `mod.rs` and `model.rs`: `pub enum AlertRef { Log(Uuid) }` (T20 adds `Metric(Uuid)`), `pub struct AlertSettings { alert: AlertRef, project_id, name, mode: IncidentMode, periods: Periods, escalation_policy_id: Option<Uuid>, severity_id: Option<Uuid> }`, `pub struct SeriesObservation { key: String, labels: Vec<(String, String)>, observation: Observation, cause: String }`, `pub struct Applied { opened: usize, resolved: usize, overflow_series: usize, overflow_sample: Vec<String> }`, `pub static ALERT_EVALUATIONS: Semaphore`, and:

```rust
/// Loads the alert's series states (locked), applies R-M2-17/18 to every observation, writes the
/// states, and opens and resolves incidents through the seam, all in the caller's transaction.
pub async fn apply(conn: &mut PgConnection, settings: &AlertSettings, observations: Vec<SeriesObservation>,
    now: DateTime<Utc>) -> Result<Applied, sqlx::Error>;
/// The seam (R-M2-20): the only code that opens or resolves an alert incident.
pub async fn open_series_incident(conn: &mut PgConnection, settings: &AlertSettings, series: &SeriesObservation,
    now: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>;
pub async fn resolve_series_incident(conn: &mut PgConnection, alert: AlertRef, series_key: &str,
    now: DateTime<Utc>, reason: Option<&str>) -> Result<Option<Uuid>, sqlx::Error>;
/// Disable and edit (R-M2-21): resolves every unresolved incident of the alert and deletes its states.
pub async fn reset(conn: &mut PgConnection, alert: AlertRef, now: DateTime<Utc>, reason: &str) -> Result<usize, sqlx::Error>;
pub async fn list_series(pool: &PgPool, alert: AlertRef) -> Result<Vec<SeriesStateRow>, sqlx::Error>;
```

**API:** none in this task (T20 and T21 expose the series lists).

**Behaviour:** R-M2-16 to R-M2-20. `apply` takes `per_alert` observations already collapsed by the caller (one, key `''`). Titles and causes per R-M2-19, cleaned with `is_unsafe_in_text`. `apply` never calls `kick`; the caller does once after commit.

**Tests:**
- Unit (`machine.rs`): `a_breach_with_no_confirmation_opens_at_once`, `a_breach_must_hold_for_the_confirmation_period` (at `c - 1 s` still pending, at `c` open), `a_clear_while_pending_returns_to_ok`, `a_clear_must_hold_for_the_recovery_period`, `a_breach_while_recovering_returns_to_alerting_without_a_new_incident`, `no_data_zero_evaluates_as_zero`, `no_data_keep_changes_nothing`, `no_data_alert_counts_as_a_breach`, `collapse_breaches_when_any_series_breaches_and_has_no_data_when_empty`, `comparisons_at_the_threshold` (`gt` and `lt` false, `gte` and `lte` true), `periods_of_0_and_3600_pass_and_3601_fails`, `series_keys_do_not_depend_on_label_order`.
- `tests/incidents/alert_series.rs`: `per_series_opens_one_incident_per_breaching_series`, `at_most_50_incidents_are_open_per_alert_and_the_rest_are_noted` (60 breaching: 50 incidents, overflow 10; after 5 recover, the next apply opens 5 more), `series_states_never_exceed_1000_rows`, `an_absent_pending_series_is_dropped`, `an_absent_alerting_series_is_kept_under_keep`, `a_manually_resolved_incident_lets_a_still_breaching_series_alert_again`, `the_database_allows_one_unresolved_incident_per_series` (a second insert fails with a unique violation), `titles_carry_series_labels_cleaned_and_capped_at_200_chars`, `reset_resolves_with_the_reason_and_clears_the_states`, `the_incident_takes_the_alerts_escalation_policy_and_severity`.
- `tests/core/migrations.rs`: `alert_series_states` in `all_tables_exist`; `an_alerting_log_alert_is_backfilled_with_its_open_incident`.
- Every NAMI-LOG-14 and NAMI-INC test stays green (`cargo test --test logs log_alerts::`, `cargo test --test incidents`).

**Acceptance:** the pure machine covers every row of R-M2-17; per-series incidents are capped and bounded; nothing outside `incidents::alerting` opens or resolves an alert incident (checked in review by grepping for the lifecycle calls with alert sources).

### Task 20 (NAMI-MET-20): Add metric alerts and their evaluation worker

**Covers:** R6.11, R6.12  **Depends on:** NAMI-MET-19, NAMI-MET-5, NAMI-MET-7

**Files:**
- Create: `nami-engine/src/metrics/alert/{mod,model,routes}.rs`, `nami-engine/tests/metrics/metric_alerts.rs`.
- Modify: `nami-engine/migrations/NNNN_metrics_part2.sql` (append `metric_alerts`), `nami-engine/migrations/NNNN_incidents_alert_series.sql` (append: `incidents.metric_alert_id`, its unique series index, the `alert_series_states` metric column), `nami-engine/src/metrics/sql.rs` (`evaluate_window`), `nami-engine/src/incidents/lifecycle.rs` (`MetricAlert { alert_id, series_key }`), `nami-engine/src/incidents/alerting/mod.rs` (`AlertRef::Metric`), `nami-engine/src/incidents/model.rs` (list item `metric_alert_id`, source name joins `metric_alerts.name`), `nami-engine/src/main.rs` (spawn), `nami-engine/src/core/router.rs`, `nami-engine/tests/metrics/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`.

**Data shape:**
- Table `metric_alerts` (constraints named `metric_alerts_<rule>`): `id uuid primary key default gen_random_uuid()`, `project_id uuid not null references projects(id) on delete cascade`, `name text not null`, `query jsonb not null` (an object; Part 1's `QueryDef`), `window_seconds int not null` (60-86400), `interval_seconds int not null` (60-86400), `comparison text not null` (`gt|gte|lt|lte`), `threshold double precision not null` (`abs(threshold) <= 1e15`), `no_data text not null default 'keep'` (`zero|keep|alert`), `confirmation_seconds int not null default 0` (0-3600), `recovery_seconds int not null default 0` (0-3600), `incident_mode text not null default 'per_alert'` (`per_alert|per_series`), `escalation_policy_id` and `severity_id` (references and `on delete` as NAMI-INC-10 gives them for log alerts), `enabled boolean not null default true`, `state text not null default 'ok'` (`ok|alerting`), `last_evaluated_at timestamptz`, `last_series int`, `last_error text`, `last_error_at timestamptz`, `overflow_series int not null default 0`, `overflow_sample jsonb not null default '[]'`, `next_evaluation_at timestamptz not null default now()`, `created_by uuid references users(id) on delete set null`, `created_at`, `updated_at`. Indexes on `project_id` and `(next_evaluation_at) where enabled`.
- Incidents file append: `incidents.metric_alert_id uuid references metric_alerts(id) on delete cascade`, unique `(metric_alert_id, alert_series_key) where metric_alert_id is not null and status <> 'resolved'`, an index on `metric_alert_id`; `alert_series_states`: `log_alert_id` drops `not null`, `metric_alert_id uuid references metric_alerts(id) on delete cascade` is added with unique `(metric_alert_id, series_key)` and the check `alert_series_states_one_alert` (`num_nonnulls(log_alert_id, metric_alert_id) = 1`).
- Rust: `MetricAlert` (the row; enums for comparison, no-data and mode), `MetricAlertConfig` and `pub fn validate(config) -> Result<ValidMetricAlert, String>` (R-M2-23), `CreateMetricAlertRequest` and `PatchMetricAlertRequest` (`deny_unknown_fields`; nullable policy and severity), constants `MAX_METRIC_ALERTS_PER_PROJECT`, `WINDOW_RANGE`, `INTERVAL_RANGE`, `MAX_ABS_THRESHOLD`. `pub async fn evaluate_window(ch: &core::clickhouse::ClickHouse, project_id: Uuid, query: &ValidQuery, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<WindowResult, AppError>` with `WindowResult { groups: Vec<(BTreeMap<String, String>, Option<f64>)>, too_many: bool }` (asks for 1,001 groups). Worker functions as LOG-14's: `claim_due_alerts`, `run_pass_with(state, budget)`, `evaluate_due_alerts(state)` (tests), `run_forever`.

**API:**

| Method and path | Role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/metric-alerts` | viewer | 200 `[MetricAlert]` | 403 |
| `POST /projects/{id}/metric-alerts` | member | 201 | 400 (validation, foreign policy or severity), 403, 409 (quota) |
| `GET /projects/{id}/metric-alerts/{alert_id}` | viewer | 200 | 403, 404 |
| `PATCH /projects/{id}/metric-alerts/{alert_id}` | member | 200 | 400, 403, 404 |
| `DELETE /projects/{id}/metric-alerts/{alert_id}` | admin | 204 | 403, 404 |
| `GET /projects/{id}/metric-alerts/{alert_id}/series` | viewer | 200 `[SeriesStateRow]` | 403, 404 |
| `POST /projects/{id}/metric-alerts/preview` (`{query, window_seconds}`) | viewer | 200 `{series: [{tags, value}], truncated}` (at most 50) | 400, 403, 429 (the user's query slots, Part 1 R10) |

**Behaviour:** R-M2-21, R-M2-22, R-M2-23. One evaluation: run `evaluate_window` (holding an `ALERT_EVALUATIONS` permit), build observations (`observe`, then `collapse` for `per_alert`), then in one transaction re-read the alert (drop if `updated_at` changed or a project-wide maintenance window holds it), `incidents::alerting::apply`, write `state`, `last_*`, `overflow_*` and `next_evaluation_at`; commit; `kick` when anything opened or resolved. A failed query only sets `last_error` and `last_error_at`.

**Tests** (`tests/metrics/metric_alerts.rs`):
- Roles: `a_member_creates_an_alert_and_a_viewer_reads_it`, `a_viewer_cannot_create_or_edit_an_alert`, `a_member_cannot_delete_an_alert_but_an_admin_can`.
- Limits: `window_and_interval_limits_are_60_and_86400`, `periods_are_0_to_3600`, `a_non_finite_or_too_large_threshold_is_400`, `dashboard_variables_are_refused_in_alert_queries`, `per_series_needs_a_group_by`, `a_foreign_escalation_policy_or_severity_is_400`, `the_101st_alert_is_409`.
- Evaluation: `a_breaching_alert_opens_an_incident_with_its_metric_alert_id_and_resolves_on_recovery`, `each_comparison_at_the_threshold`, `rate_alerts_count_a_counter_reset_from_zero`, `a_built_in_metric_can_be_alerted_on`, `a_metric_of_another_project_never_matches`.
- Improvements: `no_data_zero_keep_and_alert_on_a_metric_that_stops`, `confirmation_and_recovery_periods_delay_open_and_resolve`, `per_series_alerts_open_one_incident_per_group_and_stop_at_50`, `a_query_with_more_than_1000_series_fails_the_evaluation_and_keeps_state`.
- Worker: `overlapping_passes_evaluate_each_alert_once`, `an_evaluation_of_an_edited_alert_is_dropped`, `a_failed_query_records_last_error_and_changes_nothing`, `a_project_wide_maintenance_window_holds_the_alert_state`, `the_incident_takes_the_alerts_escalation_policy_and_severity`.
- Edits: `disabling_an_alerting_alert_resolves_its_incidents_with_the_reason`, `changing_the_query_resolves_open_incidents_as_alert_changed`, `changing_the_threshold_keeps_the_series_states`.
- `preview_returns_the_current_value_per_group`; `tests/core/json_bodies.rs` and `tests/core/migrations.rs` additions.

**Acceptance:** a metric alert opens, escalates through its policy and resolves its incident; every no-data mode, period and incident mode behaves as R-M2-17 and R-M2-18 say.

### Task 21 (NAMI-MET-21): Move log alerts onto the shared evaluator

**Covers:** R6.12  **Depends on:** NAMI-MET-19

**Files:**
- Create: `nami-engine/migrations/NNNN_logs_alert_series.sql`.
- Modify: `nami-engine/src/logs/alert/{mod,model,routes}.rs` (new fields, grouped count, `apply_evaluation` calls `incidents::alerting::apply`, the R-P2-27 seam functions removed, `ALERT_EVALUATIONS` shared), `nami-engine/tests/logs/log_alerts.rs`, `nami-engine/tests/core/migrations.rs`.

**Data shape:**
- Migration (logs domain): `log_alerts` gets `confirmation_seconds int not null default 0` (0-3600), `recovery_seconds int not null default 0` (0-3600), `incident_mode text not null default 'per_alert'` (`per_alert|per_series`), `series_fields jsonb not null default '[]'` (an array of at most 3 field paths), `overflow_series int not null default 0`, `overflow_sample jsonb not null default '[]'`, and the check `log_alerts_per_series_needs_fields` (`incident_mode = 'per_alert' or jsonb_array_length(series_fields) >= 1`).
- Rust: `LogAlertConfig` and both request bodies gain `confirmation_seconds`, `recovery_seconds`, `incident_mode`, `series_fields` (validated with Logs' field-path rules, NAMI-LOG-13); `pub const MAX_SERIES_FIELDS: usize = 3;` `pub async fn count_by(ch: &core::clickhouse::ClickHouse, project_id: Uuid, filters: &LogFilters, window: &Window, fields: &[FieldPath]) -> Result<Groups, AppError>` (at most 1,001 groups, every path segment a bound parameter, a missing field grouped as `''`).

**API:** LOG-14's routes, bodies extended as above; new `GET /projects/{id}/log-alerts/{alert_id}/series` (viewer, 403/404).

**Behaviour:** R-M2-17 to R-M2-22 with log specifics: a count is never missing, so there is no `no_data` setting; a series known in the states but absent from the grouped count is observed with count 0; comparisons stay `gt|lt`. Defaults reproduce LOG-14 exactly (open on the first breaching evaluation, resolve on the first clear one, one incident per alert).

**Tests** (`tests/logs/log_alerts.rs`):
- Every existing LOG-14 test passes unchanged, apart from the seam names they import.
- `log_alerts_default_to_per_alert_with_no_periods`, `confirmation_and_recovery_periods_apply_to_log_alerts`, `per_series_log_alerts_group_by_up_to_three_fields` (3 pass, 4 is 400), `per_series_needs_series_fields`, `an_absent_group_counts_as_zero`, `field_paths_are_bound_as_parameters` (Part 1's injection strings as paths match literally), `a_count_with_more_than_1000_groups_fails_the_evaluation`, `log_and_metric_alerts_share_one_evaluation_budget`.
- `tests/core/migrations.rs`: `log_alert_series_fields_are_capped_at_three`.

**Acceptance:** log alerts gain the three improvements; with defaults their behaviour, tests and notifications are what NAMI-LOG-14 and NAMI-INC-10 left.

### Task 22 (NAMI-MET-22): Ship and validate the collector bundle

**Covers:** R6.13  **Depends on:** NAMI-MET-14

**Files:**
- Create: `deploy/collectors/vector/{vector.yaml, vector.docker.yaml, exporters.yaml, compose.yaml}`, `deploy/collectors/otelcol/{config.yaml, config.docker.yaml, exporters.yaml, compose.yaml}`, `deploy/collectors/exporters/compose.yaml`, `deploy/collectors/validate.sh`, `nami-engine/tests/metrics/collector_e2e.rs`.
- Modify: `nami-engine/tests/metrics/main.rs`.

**Data shape** (what each file holds; every file starts with a comment naming its purpose, the two variables it reads and, for the Docker-socket and cAdvisor files, the root-equivalence warning of R-M2-28):
- `vector/vector.yaml`: sources `container_logs` (`file`, `/var/lib/docker/containers/*/*-json.log`), `journald`, `host` (`host_metrics`: cpu, memory, disk, filesystem, network, load; 15 s); a `remap` transform that unwraps Docker's JSON log lines (message, time, stream, container id from the path); sinks `nami_logs` (`http` to `${NAMI_URL}/api/v1/logs`, `encoding.codec: json`, `framing.method: newline_delimited`, `compression: gzip`, bearer `${NAMI_TOKEN}`, batches under Logs' 10 MiB body limit, at most 40 requests per second, below the 50/s per-source limit) and `nami_metrics` (`prometheus_remote_write` to `${NAMI_URL}/api/v1/metrics/prometheus/remote_write`, bearer, batches of at most 10,000 events, below the 50,000-sample cap).
- `vector/vector.docker.yaml`: the same with `docker_logs` instead of the `file` source.
- `vector/exporters.yaml`: a `prometheus_scrape` source for `node-exporter:9100` and `cadvisor:8080` with its own remote-write sink (Vector merges several config files).
- `otelcol/config.yaml`: receivers `hostmetrics` (`root_path: /hostfs`), `otlp` (gRPC 4317 and HTTP 4318), `filelog` (container log files); processors `memory_limiter`, `batch`; exporters `prometheusremotewrite` (endpoint, `Authorization: Bearer ${env:NAMI_TOKEN}` header, resource attributes to labels) and `otlphttp` (for the Traces sub-project); pipeline `metrics: [hostmetrics, otlp] -> [memory_limiter, batch] -> [prometheusremotewrite]`; logs and traces per R-M2-27.
- `otelcol/config.docker.yaml`: adds `docker_stats`. `otelcol/exporters.yaml`: a `prometheus` receiver scraping `node-exporter:9100` and `cadvisor:8080` in its own metrics pipeline.
- `*/compose.yaml`: the collector service, image pinned (R-M2-26), `env_file: .env` (`NAMI_URL`, `NAMI_TOKEN`), read-only mounts (`/var/lib/docker/containers`, `/var/log/journal`, `/etc/machine-id`, `/proc` and `/sys` as `/host/...` with `PROCFS_ROOT`/`SYSFS_ROOT` for Vector, `/` as `/hostfs` for the OpenTelemetry Collector), OTLP ports published on `127.0.0.1` only, `restart: unless-stopped`.
- `exporters/compose.yaml`: `node-exporter` (`--path.rootfs=/host`, `/:/host:ro,rslave`) and `cadvisor` (its documented read-only mounts; root-equivalent warning, since it reads the Docker runtime directory).
- `validate.sh` (R-M2-29): `set -euo pipefail`; renders every config into `mktemp -d` by plain replacement of the four placeholder strings with the URL-safe dummy values; `docker run --rm` `vector validate --no-environment` for `vector.yaml`, `vector.docker.yaml` and `vector.yaml` plus `exporters.yaml`; `otelcol-contrib validate --config=...` for each OpenTelemetry combination; `docker compose -f <file> config -q` for each compose file. Exits non-zero on the first failure.

**API:** none.

**Behaviour:** R-M2-25 to R-M2-29. Placeholders are exactly `${NAMI_URL}` and `${NAMI_TOKEN}` (Vector) and `${env:NAMI_URL}` and `${env:NAMI_TOKEN}` (OpenTelemetry Collector), nothing else, each inside an already double-quoted scalar (R-M2-25), so T26 renders by replacing those strings with escaped values and validate.sh by plain replacement.

**Tests:**
- `deploy/collectors/validate.sh` exits 0.
- `tests/metrics/collector_e2e.rs` (`#[ignore]`): `vector_ships_logs_and_metrics_to_a_test_engine` (R-M2-29): a test public listener on `127.0.0.1:<port>` with a seeded source; a temporary directory with one Docker JSON log file mounted at `/var/lib/docker/containers`; the rendered `vector.yaml` in a pinned Vector container with `--network host`; within 60 s, the log line is in `logs` and `host_*` series are in `metrics` for that source.

**Acceptance:** both commands pass on the phase machine and their output goes in the PR (no CI).

### Task 23 (NAMI-MET-23): Build the scrape targets UI

**Covers:** R6.02, R6.14  **Depends on:** NAMI-MET-16

**Files:**
- Create: `nami-ui/src/lib/metrics/scrape.ts` (+ `scrape.test.ts`), `nami-ui/src/lib/server/metrics/scrape-form.ts` (+ test), `nami-ui/src/routes/(app)/projects/[id=uuid]/metrics/scrape-targets/{+page.server.ts, +page.svelte}`, `.../scrape-targets/[targetId=uuid]/{+page.server.ts, +page.svelte}`, `.../scrape-targets/scrape-targets.test.ts`.
- Modify: Part 1's metrics section navigation (a "Scrape targets" tab).

**Data shape:** `ScrapeTarget` (the engine's JSON), `TargetHealth`; `parseTargetForm(form: FormData): {body} | {error}` builds the create and patch bodies: labels from repeated `label_key`/`label_value` rows (at most 20), `auth` absent when the credential inputs are empty (keep), `null` when "Remove credentials" is checked, an object otherwise.

**API** (SvelteKit form actions under `(app)/projects/[id=uuid]/metrics/scrape-targets`; each calls the T15 route and answers `engineFailure(...)` with the engine's status on error):

| Page and action | Role (engine-enforced) | Success | Errors shown |
|---|---|---|---|
| list `+page` load | viewer | the list | 403 |
| list `?/create` | member | redirect to the target | 400, 403, 409 |
| `[targetId=uuid]` load | viewer | the target | 403, 404 |
| `[targetId=uuid]` `?/update` | member | the saved target | 400, 403, 404, 409 |
| `[targetId=uuid]` `?/delete` (`confirm=yes`) | member | redirect to the list | 403, 404 |

**Behaviour:**
1. List: name, URL, health badge (`up` uses `status-up`, `down` `status-down`, `unknown` `status-pending`, disabled `status-paused`), last scrape (UTC), duration, samples, last error as text. EmptyState with a create link.
2. Create and edit forms: name, source select (the project's sources), URL, interval (15-3600), timeout (1-30, below the interval), labels editor, credentials (`type="password"`, `autocomplete="off"`, never prefilled; the page shows only "Bearer token set" or "Basic auth set"), enabled. Help text: scrapes run from the engine host, private addresses are allowed, link-local and metadata addresses are refused, redirects are not followed.
3. Delete is two-step. Viewers see the list and details without forms (`data.role`).

**Tests** (vitest): `the form refuses a 21st label before calling the engine`, `empty credential inputs keep the stored credentials`, `remove credentials sends null`, `a timeout not below the interval is refused`, `a viewer sees no create, edit or delete controls`, `health badges use the status tokens`, `last error renders as text`.

**Acceptance:** `npm run check` has 0 errors and 0 warnings; a member creates, edits and deletes a target and sees its health after a scrape.

### Task 24 (NAMI-MET-24): Build the template gallery, share dialog and public share page

**Covers:** R6.08, R6.09, R6.14  **Depends on:** NAMI-MET-17, NAMI-MET-18

**Files:**
- Create: `nami-ui/src/lib/metrics/share.ts` (+ test), `nami-ui/src/lib/components/metrics/{ShareDialog,TemplateGallery}.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/dashboards/templates/{+page.server.ts, +page.svelte}` (+ test), `nami-ui/src/routes/share/dashboards/[token]/{+page.server.ts, +page.svelte}` (+ test), `.../[token]/panels/[panelId=uuid]/query/+server.ts` (+ test), `.../[token]/variables/[name]/values/+server.ts` (+ test).
- Modify: Part 1's dashboard list (a "From template" action) and dashboard view (`share`, `revoke` form actions and the dialog), `nami-ui/src/lib/server/core/engine.ts` (`callPublicEngine`, only when NAMI-SP-13 has not merged).

**Data shape:** `isShareToken(s): boolean` (`^[A-Za-z0-9_-]{43}$`), the `SharedDashboard` DTO type, `ShareStatus`, `CreatedShare`.

**API** (form actions and BFF endpoints):

| Path and action | Session and role | Success | Errors |
|---|---|---|---|
| `(app)/projects/[id=uuid]/dashboards/templates` load | viewer | template list | 403 |
| same, `?/create` (`template`, `name?`) | member | redirect to the new dashboard | 400, 403, 409 |
| `(app)/.../dashboards/[dashboardId=uuid]` `?/share` | member | the token once, in the action result | 403, 404, 409 |
| same, `?/revoke` (`confirm=yes`) | member | status inactive | 403, 404 |
| `share/dashboards/[token]` load | none | the read-only page | 404 page, 429 message |
| `POST share/dashboards/[token]/panels/[panelId=uuid]/query` (JSON relay, same origin) | none; token in `x-nami-share-token` to the engine | the engine's JSON, `no-store` | 400, 404, 429 relayed |
| `GET share/dashboards/[token]/variables/[name]/values?q=` | none; token in the header | the engine's JSON, `no-store` | 400, 404, 429 relayed |

**Behaviour:**
1. Gallery: one Card per template (name, description, panel count, variables) with "Create" (member); viewers see the cards without buttons. Creating redirects to the new dashboard.
2. Share dialog (dashboard view, members): before creating, the warning "Anyone with this link can see every series these panels can show, including every value of this dashboard's variables, without signing in, until you revoke it." After creating, the full link (page origin plus `path`) once, with CopyButton, and the note that it cannot be shown again. With a link active: "Shared since <UTC>" and a two-step Revoke. Viewers see the status only.
3. Public page (R-M2-15): outside `(app)`; an invalid token renders the 404 page without an engine call; `setHeaders` sends `x-robots-tag: noindex, nofollow`, `referrer-policy: no-referrer`, `cache-control: no-store`; the page has the `noindex` meta tag; panels, variables and time range (presets up to 30 days and a custom range up to 90 days) reuse Part 1's components read-only; text panels go through `renderMarkdown`; auto-refresh follows the dashboard setting and pauses when the tab is hidden.
4. Relays call `callPublicEngine` with the token in `x-nami-share-token`, never in the engine path, and relay the engine's status and JSON with `cache-control: no-store`.

**Tests** (vitest): `isShareToken accepts 43 url-safe chars only`, `an invalid token renders 404 without an engine call`, `the share page sends noindex, no-referrer and no-store`, `the relay sends the token in the header and not in the engine path`, `the relay forwards exactly one client address`, `the dialog shows the warning before creating and the link once after`, `a viewer sees no share or revoke controls`, `the gallery creates from a template and a viewer sees no create button`, `a text panel on the share page renders scripts and javascript links inert`.

**Acceptance:** a member shares a dashboard, opens the link in a private window and sees it read-only; after revoke the link is a 404.

### Task 25 (NAMI-MET-25): Build the metric alert UI and extend the log alert UI

**Covers:** R6.11, R6.12, R6.14  **Depends on:** NAMI-MET-20, NAMI-MET-21

**Files:**
- Create: `nami-ui/src/lib/metrics/alerts.ts` (+ test), `nami-ui/src/lib/components/metrics/AlertEvaluationFields.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/metrics/alerts/{+page.server.ts, +page.svelte}`, `.../alerts/new/{+page.server.ts, +page.svelte}`, `.../alerts/[alertId=uuid]/{+page.server.ts, +page.svelte}`, `.../alerts/alerts.test.ts`.
- Modify: NAMI-LOG-18's log alert form and detail pages (the shared fields, series fields, the series table), Part 1's metrics section navigation ("Alerts").

**Data shape:** `parseEvaluationFields(form, kind: 'metric' | 'log')` returns `{confirmation_seconds, recovery_seconds, incident_mode, no_data?, series_fields?}` or an error (periods 0-3600, `per_series` needs a group-by or 1-3 series fields, `no_data` only for `metric`).

**API** (form actions; each calls a T20 or T21 route):

| Page and action | Role (engine-enforced) | Success | Errors shown |
|---|---|---|---|
| `metrics/alerts` load, `[alertId=uuid]` load (with `/series`) | viewer | list, detail and series | 403, 404 |
| `metrics/alerts/new` `?/create`, `?/preview` | member (preview: viewer) | redirect to the alert; preview values | 400, 403, 409, 429 |
| `[alertId=uuid]` `?/update` | member | the saved alert | 400, 403, 404 |
| `[alertId=uuid]` `?/delete` (`confirm=yes`) | admin | redirect to the list | 403, 404 |
| LOG-18's log alert `?/create` and `?/update` | member | as LOG-18, with the new fields | 400, 403, 404 |

**Behaviour:**
1. List: name, metric, condition (`p95 of http_latency over 5m > 500`), state badge, last evaluated (UTC), last error, overflow note.
2. Form: Part 1's QueryEditor without variables, window, comparison (`gt|gte|lt|lte`), threshold, interval, `AlertEvaluationFields` (no-data with the three choices explained, confirmation and recovery in seconds, incident mode), escalation policy and severity selects (Incidents' list endpoints), enabled. Preview shows the current value per group.
3. Detail: settings, the series table (key labels, state, pending or recovering since, linked incident), the overflow note ("N more breaching series have no incident: the alert has 50 open"). Members edit; admins delete (two-step); viewers read.
4. Log alerts get the same fields, with series fields instead of no-data.

**Tests** (vitest): `periods outside 0-3600 are refused before the engine`, `per_series needs a group-by for metric alerts and 1 to 3 series fields for log alerts`, `no_data is offered for metric alerts only`, `the overflow note shows the count and the sample`, `roles: a viewer sees no form, a member no delete, an admin a two-step delete`, `the series table shows states and UTC times`.

**Acceptance:** a member creates a metric alert with each evaluation option and sees its series states; log alerts show the new fields.

### Task 26 (NAMI-MET-26): Build the collector setup pages

**Covers:** R6.13, R6.14  **Depends on:** NAMI-MET-22

**Files:**
- Create: `nami-ui/src/lib/server/metrics/collectors.ts` (+ test), `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/collectors/{+page.server.ts, +page.svelte}`, `.../collectors/download/+server.ts`, `.../collectors/collectors.test.ts`.
- Modify: the source detail page (a "Collector setup" link for members).

**Data shape:** `COLLECTORS: {kind, title, filename, warning?, raw}[]` for Vector, Vector with the Docker socket, Vector exporters, OpenTelemetry Collector, its Docker variant, its exporters file, and the compose files; `renderCollector(raw: string, values: {url: string, token: string}): string` (R-M2-25).

**API:**

| Path and action | Role | Success | Errors |
|---|---|---|---|
| `(app)/.../logs/sources/[sourceId=uuid]/collectors` load | member (viewers get 403 from the load) | configs with `TOKEN_PLACEHOLDER` | 403, 404 |
| same, `?/reveal` | member (engine `GET .../token`) | configs rendered with the token, in the action result only | 403, 404 |
| `POST .../collectors/download` (JSON `{kind}`) | member | 200 the rendered file, `content-disposition: attachment`, `cache-control: no-store` | 400 (unknown kind), 403, 404 |

**Behaviour:** R-M2-30. The page shows each file in a `<pre>` with CopyButton and a download button, the compose snippet next to its config, the root-equivalence warning above the Docker-socket and cAdvisor files, and a short "how to run" (`.env` with `NAMI_URL` and `NAMI_TOKEN`, then `docker compose up -d`). The public URL comes from `publicEngineUrl()`.

**Tests** (vitest): `rendering replaces every placeholder and leaves none`, `every placeholder sits inside a double-quoted scalar`, `values are escaped and no quotes are added`, `rendering url-safe values gives the same bytes as plain replacement`, `a viewer gets 403`, `the page shows the placeholder until a member reveals the token`, `the download is a POST answered with attachment and no-store headers`, `no GET response contains the token`, `the docker variants carry the root-equivalence warning`, and, when the fallback copies are used, `the copies match deploy/collectors`.

**Acceptance:** a member downloads a rendered Vector config that passes `validate.sh`'s checks when saved over the dummy one; a viewer cannot open the page.

### Task 27 (NAMI-MET-27): Run the phase security review and fix wave

**Covers:** R6.01, R6.02, R6.09, R6.11, R6.12, R6.13  **Depends on:** NAMI-MET-14..26

**Files:** fixes where the review finds them; `docs/superpowers/decisions/<date>-metrics-part2.md`; `docs/superpowers/roadmap.md` (section 6 ticks).

**Behaviour:** a whole-phase security review of the diff from `origin/main`, then a fix wave on `feat/metrics-part2` and a scoped re-review. The review checks at least:
1. Remote-write: no allocation before the length guard, budget charged before every allocation, no panic over the fuzz corpus, the stale-marker rule, 415 for remote-write 2.0.
2. Scrape: link-local and metadata refusal at DNS time and for literals (IPv4-mapped and NAT64 forms included), no redirect, no proxy, the 10 MiB and 50,000-sample caps with nothing stored on failure, credentials sealed, never returned or logged, the retarget rule, the worker surviving every failure.
3. Share links: token only as SHA-256, only in a header to the engine, 404 for unknown and revoked, panels by id within the dashboard only, variable values literal, per-IP limit and share slots, `noindex`, `no-referrer`, `no-store`, inert text panels.
4. Alerts: each no-data mode, the periods, the 50-open cap, the 1,000-series bounds, the one seam, the shared evaluation budget, edited-alert drops, maintenance holds, `last_error` as fixed text.
5. Collectors: rendered tokens only on member pages and POST downloads.
6. Every new route in the role tables and in `json_bodies`, every new table in `migrations`, every limit tested at and past its bound.

**Tests:** every finding fixed gets a test; then the whole engine suite (`cargo test`), `cargo clippy --all-targets`, `npm run check`, `npx vitest run`, `deploy/collectors/validate.sh` and the ignored end-to-end test, with results in the PR.

**Acceptance:** a clean re-review; the decisions doc records R-M2-1 to R-M2-31, the spec deviations, the image tags, the follow-ups (Platform: outbound policy settings, a share cache if measured, per-IP limits on the public listener) and the answers to the open questions below; roadmap section 6 is ticked; the phase PR lists tasks with commit ranges, the local test runs ("No CI is configured on this repo yet"), breaking changes (the R-P2-27 seam replaced, the `IncidentSource::LogAlert` shape) and risks.

## Open questions for the controller

Each has a default this plan already follows; a different answer changes the named task.

**Controller ruling (2026-10-24):** all seven defaults below are accepted. The spec is the product source, so members delete scrape targets as "Members manage" says; deleting a metric alert cascades to its incidents for parity with log alerts.

1. Members delete scrape targets (spec "Members manage"; STANDARDS would make it admin) (R-M2-7, NAMI-MET-15).
2. Staleness markers are dropped without counting as rejected (R-M2-3, NAMI-MET-14).
3. A second share link is 409 until the first is revoked, rather than a rotate endpoint (R-M2-12, NAMI-MET-18).
4. Share results are not cached (R-M2-14, NAMI-MET-18).
5. Scrape TLS is always verified, with no skip-verify option (R-M2-5, NAMI-MET-16).
6. Deleting a metric alert deletes its incidents (`on delete cascade`, mirroring LOG-14's log alerts) rather than keeping them with a null link (R-M2-21, NAMI-MET-20).
7. R-P2-27's log-alert seam is replaced by `incidents::alerting` (R-M2-20, NAMI-MET-19, NAMI-MET-21).
