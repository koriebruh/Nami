# Traces (OpenTelemetry), Part 1 Implementation Plan

> **Status:** outline. Nothing of this phase is merged; NAMI-TR-1 is next once Metrics Part 2 has merged.
> **Branch:** `feat/traces-part1` in `.worktrees/traces-part1/`.
> **Depends on:** Logs Part 2 (`feat/logs-part2`), Metrics Part 1 and Metrics Part 2 (the CLAUDE.md sequence: sub-project 6 before 7). Incidents Part 1 only for the `role` field the UI reads (NAMI-INC-7).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task by task. This is an outline plan (see `docs/superpowers/plan-conventions.md`): tasks, data shapes, endpoints, files, tests and acceptance criteria are fixed here; the step-level code is written at phase start against the then-current `main`.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R7.01 | OTLP/HTTP (JSON + protobuf) trace ingestion with source tokens | NAMI-TR-2, NAMI-TR-3, NAMI-TR-4, NAMI-TR-5, NAMI-TR-11 |
| R7.01 | Spec scope of the same item: OTLP logs and OTLP metrics on `/v1/logs`, `/v1/metrics` | NAMI-TR-6, NAMI-TR-7 |
| R7.02 | ClickHouse span storage, retention | NAMI-TR-1, NAMI-TR-4 |
| R7.03 | Trace search (service, operation, duration, status, time range) | NAMI-TR-8, NAMI-TR-12 |
| R7.04 | Trace waterfall view | NAMI-TR-9, NAMI-TR-13 |
| R7.05 | Trace-to-log linking both ways via `trace_id` | NAMI-TR-6, NAMI-TR-9, NAMI-TR-13, NAMI-TR-14 |
| R7.06 | Apdex per service (configurable threshold) | NAMI-TR-10, NAMI-TR-15 |
| R7.01 to R7.06 | Phase security review and fix wave | NAMI-TR-16 |

There is no Part 2: the spec's whole scope is in this plan. After the phase PR merges, the controller ticks R7.01 to R7.06.

**Goal:** Give Nami BetterStack-style tracing. OTLP/HTTP ingestion (protobuf and JSON, gzip) of traces, logs and metrics with source tokens; spans in ClickHouse with per-source retention; trace search; a trace waterfall; links from logs to traces and back through `trace_id`; per-service RED stats and Apdex with a configurable threshold; and the nami-ui pages for all of it.

**Architecture:** A new domain `nami-engine/src/traces/`. OTLP decoding (`traces::otlp`) is protocol code shared by the three signals: hand-written `prost` messages for the OTLP subset Nami reads, checked against vendored `.proto` files, carry both the protobuf and the OTLP JSON mapping (serde attributes on the same structs). Before either decoder allocates, a pure pre-scan walks the body and charges `logs::ingest::budget::Budget`, so a small body can never decode into gigabytes. The three endpoints sit on the public listener next to `/api/v1/logs` and share its `IngestLimits` (places, Postgres query slots, decode places). Each signal keeps its own domain's limits and storage: spans go through `traces::span` validation into the `spans` MergeTree table (rate limits in `TracesState`); OTLP log records become `logs::ingest::normalize::LogEvent`s and enter the one shared row step (`logs::ingest::store::RowBatch::push`, pipeline, patterns, usage and `daily_byte_quota`, limits in `LogsState`); OTLP metric points become `metrics::sample::Sample`s and go through `metrics::ingest::validate_body` and `SeriesGuard::admit` (limits in `MetricsState`). Reads build SQL only with `logs::query::Sql` (static fragments, bound values), project id bound from the path, under a fixed read budget and per-user plus per-instance slots. Per-service Apdex thresholds live in Postgres (`trace_service_settings`) and reach ClickHouse as bound arrays. nami-ui adds a Traces section (explorer, waterfall, services) and "View trace" / "View logs" links between the logs explorer and the waterfall.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, `flate2`, `serde`/`serde_json`, `base64`, `hex`), `prost` 0.14.4 (see R-TR-1), ClickHouse 26.9 over HTTP (`core/clickhouse.rs`), Postgres 17, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest. No new npm package.

**Spec:** `docs/superpowers/specs/2026-10-13-traces-otel-design.md`, whole (including the 2026-10-13 parity additions). Roadmap: `docs/superpowers/roadmap.md` section 7. Related: the logs spec (`docs/superpowers/specs/2026-10-13-log-management-design.md`, "Ingest pipeline" and quota), the metrics spec ("Collector bundle").

**When to start:** from the latest `origin/main` after Metrics Part 2 has merged. At phase start, check every row of "Interfaces assumed from unmerged work" against `main`, take the next free migration number, and write the step-level code.

### Interfaces assumed from unmerged work

If the merged code differs in a name, adapt minimally (same behaviour, same tests) and say so in the PR. If it differs in behaviour, stop and ask the controller.

| From | Interface | Used by |
|---|---|---|
| Logs Part 2 (NAMI-LOG-10, NAMI-LOG-11, merged) | `logs::ingest::store::RowBatch::push(event, source, pipeline)` runs caps, the source's pipeline and pattern extraction; `logs::ingest::normalize::checked_id`; `logs::ingest::pipeline::CompiledPipeline`; `LogsState.pipelines.for_source`. Logs Part 2 ruling R-P2-22: OTLP log events must reach `RowBatch::push` with `trace_id`/`span_id` already checked. | NAMI-TR-6 |
| Logs Part 2 (NAMI-LOG-15, NAMI-LOG-17, planned) | The explorer's `ExplorerState`, `parseExplorerState`, `explorerParams`, `tailParams`, `engineParams` in `nami-ui/src/lib/logs/logs.ts` and the explorer page. TR-14 adds `traceId`/`spanId` to whatever shape those tasks leave. | NAMI-TR-13, NAMI-TR-14 |
| Metrics Part 1 (NAMI-MET-1, NAMI-MET-3, NAMI-MET-4, planned) | `metrics::sample::{Sample, MetricType::{Gauge, Counter}, Rejection, is_valid_name, is_valid_tag_key}`; `metrics::ingest::{validate_body, Built}`; `metrics::series_guard::SeriesGuard::{ensure_loaded, admit, forget}`; `metrics::store::{insert_encoded, insert_series}`; `MetricsState {rate_limiter, in_flight, project_in_flight, series, ..}`; `logs::ingest::gunzip_within(compressed, limit)`; `IngestError::CardinalityExceeded`. | NAMI-TR-5, NAMI-TR-7 |
| Metrics Part 2 (`docs/superpowers/plans/2026-10-24-metrics-dashboards-part2.md`, planned) | `prost@0.14.4` with `default-features = false, features = ["std"]` (planned in NAMI-MET-14, ruling R-M2-1: wire primitives only, no `derive`, so no proc-macro record there); the collector bundle under `deploy/collectors/` (planned in NAMI-MET-22): `otelcol/config.yaml` with the `otlphttp` exporter and the `filelog` receiver defined for this phase (R-M2-27), `deploy/collectors/validate.sh`, and the ignored end-to-end test `tests/metrics/collector_e2e.rs`; the member-only setup pages (planned in NAMI-MET-26). If `prost` is absent when TR-2 starts, TR-2 adds it at that version. | NAMI-TR-2, NAMI-TR-11 |
| Incidents Part 1 (NAMI-INC-7) | `GET /projects/{id}` returns the caller's effective `role`, passed on as `data.role` by `(app)/projects/[id=uuid]/+layout.server.ts`. If it is absent, TR-15 shows the threshold form to everyone and relies on the engine's 403 (viewers see the error). | NAMI-TR-15 |

Merged on `main` and reused by this plan (paths checked against `56916be`): `core::clickhouse::{ClickHouse::{query_rows_with, insert_rows_with, insert_encoded_with, create_test_database, drop_database}, encode_rows_within, MAX_RESPONSE_BYTES, QUERY_MAX_EXECUTION_SECS}` and the private `MIGRATIONS` / `RESERVED_SETTINGS`; `logs::query::{Sql, push_where, LogFilters {trace_id, span_id, ..}}`; `logs::query::search::{READ_SETTINGS, user_error}`; `logs::query::slots::{UserSlots, UserPermit}`; `logs::ingest::{bearer_token, read_body, BodyPace, IngestError, DecodePlace, MAX_BODY_BYTES, MAX_EVENTS}`; `logs::ingest::budget::{Budget, MAX_DECODED_BYTES}`; `logs::ingest::rate_limit::{TokenBuckets, InFlight, InFlightGuard, DecodeShares, REQUESTS_PER_SECOND, BURST, MAX_IN_FLIGHT_PER_SOURCE, MAX_IN_FLIGHT_PER_PROJECT}`; `logs::ingest::store::INSERT_SETTINGS`; `logs::ingest::normalize::{LogEvent, LEVELS, clamp_to_accepted_range}`; `IngestLimits {places, queries, decodes, body_pace}` and the `Work` pattern of `src/logs/ingest/routes.rs`; `logs::source::model::{find_by_token, is_well_formed_token, IngestSource}`; `core::middleware::rbac::{CurrentUser, Role, require_project_role}`; `core::validate::{has_invisible_or_bidi, validate_label}`; tests' `common::{app, public_app, call, login, seed_user, seed_project, add_member, user_with_role, state_with_clickhouse}` and `common::logs::{seed_source, seed_source_with, stored_rows, eventually, post_bytes_to}`; nami-ui `#lib/server/core/engine` (`callEngine`, `enginePath`, `engineFailure`), `#lib/core/uuid` (`isUuid`), `#lib/server/core/fake-event`, `#lib/server/logs/log-proxy` (`pickLogParams` pattern), `#lib/logs/logs` (`TOKEN_PLACEHOLDER`, `formatLogTime`), `src/params.ts`, the design system in `#lib/components/ui`.

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**). Never install natively. Engine tests need `docker compose up -d postgres clickhouse`.
- SQLx runtime queries only, SQL as string literals, never `format!`. No ORM. Every constraint has a name, every foreign key states `on delete`.
- ClickHouse SQL is assembled only with `logs::query::Sql` (`push` takes `&'static str`). Every value is a bound parameter: ids, service and operation names, attribute keys (`attributes[{pN:String}]`), attribute values, durations, times, thresholds and cursor values. The project id is bound from the request path (ingest: from the token's source), never from a body. Extra settings are code constants and never a name in `RESERVED_SETTINGS`.
- Ingest is token only (`Authorization: Bearer`), on the public listener only (`core::router::build_public_router`). The token is hashed for lookup, never logged and never echoed.
- Telemetry is untrusted data (STANDARDS 9): span names, attribute keys and values, event payloads and log bodies are stored as data, bound as parameters, and rendered as text nodes in nami-ui (no `{@html}` anywhere in this feature).
- Spec values, verbatim: OTLP/HTTP paths `POST /v1/traces`, `/v1/logs`, `/v1/metrics`; `application/x-protobuf` and `application/json`; optional gzip; OTLP response bodies with partial success; trace id 32 hex characters, span id 16; search range at most **7 days**; Apdex request spans are kind `SERVER` or `CONSUMER` plus root spans; satisfied `d <= T`, tolerating `T < d <= 4T`, frustrated `d > 4T` or status error; `Apdex = (satisfied + tolerating / 2) / total`, `null` with no spans; `T` default **500 ms**, range **1 ms to 60 s**, members edit; "View logs" window is the trace's time span **plus or minus 5 minutes**; viewers cannot change thresholds (403).
- Plan limits (rulings below give the reasons; each is a named constant with a test at the bound and one past it): `OTLP_MAX_DECOMPRESSED_BYTES = 16 MiB`; every decode charges `Budget::for_request()` (192 MiB estimated) in a pre-scan before it allocates; `MAX_ANY_VALUE_DEPTH = 8`; `MAX_SPANS_PER_REQUEST = 50_000`; `MAX_ATTRIBUTES = 128` per map; `MAX_ATTRIBUTE_KEY_BYTES = 256`; `MAX_ATTRIBUTE_VALUE_BYTES = 4_096`; `MAX_SPAN_NAME_BYTES = 1_024`; `MAX_SERVICE_NAME_BYTES = 256`; `MAX_STATUS_MESSAGE_BYTES = 4_096`; `MAX_EVENTS_PER_SPAN = 128`; `MAX_EVENT_ATTRIBUTES = 32`; `MAX_EVENTS_JSON_BYTES = 65_536`; `MAX_SPAN_DURATION = 7 days`; `MAX_FUTURE = 10 minutes`; `MAX_INSERT_BYTES = 64 MiB`; `MAX_REPORTED_ERRORS = 10`; search `DEFAULT_LIMIT = 20`, `MAX_LIMIT = 100`, at most **10** attribute filters, value at most **1,024** bytes; `MAX_TRACE_SPANS = 5_000`; `TRACE_SUMMARY_PAD = 1 hour`; `TRACE_LINK_PAD = 1 hour`; `MAX_SERVICES = 200`; `SERVICE_BUCKETS = 30`; `MAX_SERVICE_SETTINGS = 1_000` per project; `MAX_TRACE_QUERIES_PER_USER = 6`; `MAX_TRACE_QUERIES_PER_INSTANCE = 32`.
- RBAC: two levels, as in CLAUDE.md. Viewers read traces and services; members change Apdex thresholds; nothing in this phase deletes. Authorization failure is **403**, never 404. A trace id with no spans in the path's project is **404**, whether it exists in another project or nowhere.
- Every state-changing API route takes a JSON body (`require_json_body`, already global) parsed by a `#[serde(deny_unknown_fields)]` struct; validation errors are 400. The OTLP endpoints are the exception: they are on the public listener, and OTLP requires receivers to ignore unknown fields, so `deny_unknown_fields` applies to Nami's API bodies only (R-TR-5). New API routes are added to `tests/core/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json`, the new Postgres table to `tests/core/migrations.rs::all_tables_exist`, the ClickHouse table to `tests/core/clickhouse.rs::migrate_is_idempotent`.
- nami-ui: every engine call goes through `callEngine` and `enginePath`; uuid params use `[...=uuid]` and `isUuid`, trace ids use the new `[traceId=traceid]` matcher and `isTraceId` before they reach an engine path; modules under `#lib/server` are never imported by components; times are shown in UTC; tokens never appear in a URL; design-system tokens and components only.
- Commits: Conventional Commits with the task ID, message ending with the attribution line from the session's system reminder.

## Review Focus

- **A tiny body must not decode into gigabytes.** 16 MiB of empty repeated messages (`0x4a 0x00` is one empty span attribute) or `[{},{},...]` in JSON must stop with 413 before prost or serde allocate the structs → Task 2 (`empty_attributes_are_charged_before_decoding`), Task 3 (`a_json_body_of_empty_objects_stops_at_the_budget`), the standalone `traces_decode_memory` binary.
- **One bad span must not lose the batch.** A request of 100 spans with one all-zero trace id stores 99 and answers 200 with `partialSuccess.rejectedSpans = 1` → Task 4, Task 5 (`one_bad_span_is_a_partial_success`).
- **A trace id is not a capability.** Project A's trace id under project B's path yields 404 from the trace route, no rows from search, and no log counts → Task 9 (`a_trace_of_another_project_is_not_found`), Task 16.
- **A trace whose root span is missing still renders.** Spans whose parent is not in the result are drawn at depth 0 and marked, and a parent cycle never loops → Task 13 (`layout survives a missing root and a parent cycle`).
- **Apdex counts errors as frustrated whatever their duration**, and a service with no request spans has `apdex: null`, never 0 or 1 → Task 10 (`a_fast_error_is_frustrated`, `no_request_spans_is_null_apdex`).
- **OTLP clients retry only what is retryable.** 429 and 503 carry `Retry-After`; 400, 413 and 415 do not, so a misconfigured exporter fails instead of retrying forever → Task 5.

## Decisions the spec leaves open (rulings)

- **R-TR-1 Protobuf crate: `prost` 0.14.4 with hand-written OTLP messages, not `opentelemetry-proto`.** The spec names the `opentelemetry-proto` crate. Its current release (0.33.1) has non-optional dependencies on `opentelemetry` and `opentelemetry_sdk`, and its `trace`, `logs` and `metrics` features switch on SDK features: a telemetry SDK the engine does not use (it logs through `tracing`), released in lockstep with breaking changes about every two months. Nami needs only the receiving side of about 30 messages, and the decode pre-scan (R-TR-4) needs a schema table that matches the Rust types field by field anyway, so owning the structs keeps the types, the table and their source in one reviewed place. Concretely:
  - `nami-engine/proto/opentelemetry/proto/` vendors the `.proto` files Nami reads (`common/v1/common.proto`, `resource/v1/resource.proto`, `trace/v1/trace.proto`, `logs/v1/logs.proto`, `metrics/v1/metrics.proto`, `collector/{trace,logs,metrics}/v1/*_service.proto`) from the latest `open-telemetry/opentelemetry-proto` release tag at phase start, with its Apache-2.0 `LICENSE` and a `VERSION` file naming the tag. They are reference data, never compiled.
  - `traces::otlp::proto` holds `#[derive(Clone, PartialEq, prost::Message)]` structs and `prost::Oneof` enums for that subset, with `serde::Deserialize` derived (hand-written for the three messages with a oneof, R-TR-5). No build script, no `protoc`, no `prost-build`, no `tonic`.
  - A unit test, `rust_messages_match_the_vendored_protos`, reads the vendored files with a small regex (`regex` is already a dependency) and checks that every field in `traces::otlp::wire::SCHEMA` has the field number, scalar type and `repeated` flag of the `.proto` declaration. A proto bump is then a diff of the vendored files plus a failing test.
  - `prost`'s `derive` feature brings `prost-derive`, a proc-macro crate, so STANDARDS 8 requires a decision record. Metrics Part 2 adds `prost` without `derive` for remote-write (R-M2-1: about five messages, decoded by hand with `prost::encoding` primitives that charge the budget as they go). OTLP's subset is about 30 messages, and the pre-scan below bounds memory before prost allocates, so derive is the smaller and more reviewable code here. TR-2 therefore turns on the feature (`prost = { version = "0.14.4", default-features = false, features = ["std", "derive"] }`, the same version) and writes the record as the first section of this phase's decisions doc before it does. The fallback, if the controller wants one protobuf style in the engine, is hand decoders on the same primitives (open question 1).
- **R-TR-2 Names.** Domain `traces`. Engine modules: `traces::{otlp::{proto, wire, json, response}, span, store, ingest::{logs, metrics}, query::{search, trace}, service}`; `TracesState` in `traces/mod.rs`. Route files: `src/traces/ingest/routes.rs` (public listener, all three OTLP paths), `src/traces/query/routes.rs` (search and trace), `src/traces/service/routes.rs` (services and thresholds). ClickHouse table `spans`. Postgres table `trace_service_settings` in migration `NNNN_traces_service_settings.sql`. Test target `tests/traces/main.rs`. UI: `#lib/traces/traces.ts`, `#lib/traces/waterfall.ts`, `#lib/server/traces/trace-proxy.ts`, `#lib/components/traces/*`, routes under `(app)/projects/[id=uuid]/traces/`.
- **R-TR-3 Content types and bodies.** `Content-Type` `application/x-protobuf` or `application/json`, compared case-insensitively without parameters; anything else is 415. `Content-Encoding` absent, `identity` or `gzip`; anything else is 415. The body as sent is capped by `logs::ingest::read_body` (`MAX_BODY_BYTES`, 10 MiB, and the `BodyPace` deadlines). A gzip body inflates through `gunzip_within(.., OTLP_MAX_DECOMPRESSED_BYTES)` (16 MiB, the metrics value: an OpenTelemetry Collector batch of 8,192 spans is typically 4 to 8 MiB). An empty protobuf body is a valid empty request (200, empty response); a JSON body that is empty or only whitespace is 400 `request body is empty`.
- **R-TR-4 Decode memory: measure, then decode.** prost and serde allocate as they go and cannot charge `logs::ingest::budget::Budget`, and an OTLP message costs far more in memory than on the wire (an empty span attribute is 2 bytes of protobuf and a `KeyValue` of about 60 bytes, inside a `Vec` that may double). So every body is measured first by a pure pass that allocates nothing:
  - Protobuf: `traces::otlp::wire::measure(bytes, root, &mut Budget) -> Result<(), WireError>` walks the wire format against `SCHEMA` (a `const` table of `MessageSpec { name, cost, fields: &[FieldSpec { number, kind, repeated }] }`, where `kind` is `Varint | Fixed64 | Fixed32 | Bytes | Text | Message(index)` and `cost` is `size_of::<T>()` of the Rust struct). It charges `2 x cost` for every message (a `Vec` element may have as much slack again) and `len + STRING_BYTES` for every string or bytes field, skips unknown fields by wire type, and refuses (400) malformed varints, lengths past the end, groups (wire types 3 and 4) and `AnyValue` nesting deeper than `MAX_ANY_VALUE_DEPTH` (8; prost's own recursion limit is 100). A spent budget is 413 with logs' `decoded_too_large` text. Only then does `prost::Message::decode` run.
  - JSON: `traces::otlp::json::measure(bytes, root, &mut Budget)` is a dry run through a `serde_json::Deserializer` with a visitor that keeps nothing (borrowed keys and strings). It walks the same `SCHEMA` by field name (each `FieldSpec` carries its lowerCamelCase name and its snake_case alias): a known key selects the nested message, which is charged `2 x cost` exactly as the protobuf walker charges it, and every string or bytes value `len + STRING_BYTES`; an unknown key is skipped at no cost, as serde skips it. It refuses nesting deeper than `MAX_JSON_DEPTH = 32` and `AnyValue` depth past `MAX_ANY_VALUE_DEPTH`. Charging every object at the largest message's cost instead would price each attribute's `KeyValue` and `AnyValue` like a `Span` and refuse an ordinary collector batch. Then the typed decode runs (R-TR-5).
  - Both passes run inside the decode place on the blocking thread, so their CPU never blocks the runtime. A standalone binary `traces_decode_memory` (the counting-allocator pattern of `logs_decode_memory`) asserts that the measured estimate is at least what the decode actually holds for the worst shapes (empty attributes, empty spans, nested empty `ArrayValue`s, `[{}]` in JSON). See open question 2 for its name in `scripts/check-plans.py`.
- **R-TR-5 OTLP JSON mapping** (OTLP/HTTP specification): field names in lowerCamelCase, with the proto's snake_case names accepted as serde aliases; `traceId`, `spanId`, `parentSpanId` as hex strings (case-insensitive), never base64; 64-bit integers (`timeUnixNano`, `intValue`, counts) as a decimal string or a JSON number; enums as integers only (an unknown integer maps to the unspecified value); `bytesValue` as standard base64; `doubleValue` as a number or one of `"NaN"`, `"Infinity"`, `"-Infinity"`; `null` means the default; unknown fields are ignored, as OTLP requires of receivers. No `#[serde(flatten)]` anywhere: flatten buffers the remaining keys in serde's internal `Content` tree, an allocation the budget never sees (for `Metric.data` it would hold the whole data-point array twice). The messages with a oneof (`AnyValue`, `Metric`, `NumberDataPoint`) get a hand-written `Deserialize` whose map visitor dispatches on the key, and `{}` decodes as an empty `AnyValue`. An id string with a non-hex character makes the whole body a 400 (the sender's encoder is broken, and keeping its bytes would let a 16-character string pass as an 8-byte id); valid hex of the wrong length decodes and is rejected per span by R7.
- **R-TR-6 Responses.**
  - Success is 200 with the signal's `Export*ServiceResponse`, in the request's encoding and `Content-Type`. `partial_success` is set only when something was rejected: `rejected_spans` / `rejected_log_records` / `rejected_data_points` and an `error_message` joining the first `MAX_REPORTED_ERRORS` reasons (`span 12: trace_id is all zeros; span 40: end before start`).
  - A request with at least one record and none accepted is 400, so a misconfigured exporter fails visibly (OTLP exporters do not retry 400). A request with no records is 200 with an empty response.
  - Errors keep the status, `Retry-After` and `WWW-Authenticate` of `logs::ingest::IngestError`, but the body is a `google.rpc.Status { code, message }` (hand-written in `traces::otlp::proto`, `details` never set) in the request's encoding, and JSON when the content type is missing or unsupported. gRPC codes: 400, 413 and 415 are `3` (INVALID_ARGUMENT), 401 is `16`, 408 is `4`, 429 is `8` (RESOURCE_EXHAUSTED), 503 is `14` (UNAVAILABLE), 500 is `13`. The `message` starts with the machine code where Nami has one (`rate_limited: ...`, `quota_exceeded: ...`, `cardinality_exceeded: ...`, `busy: ...`).
- **R-TR-7 Span mapping and caps** (`traces::span::from_otlp`, pure). Per span, in this order; a rejection drops that span only and is counted:
  1. `trace_id` must be 16 bytes and not all zeros; `span_id` 8 bytes and not all zeros; `parent_span_id` empty or 8 bytes, where 8 zero bytes count as empty (a root). Otherwise rejected (`trace_id must be 16 bytes`, `span_id is all zeros`, ...). Stored as lowercase hex.
  2. `start_time_unix_nano > 0`, `end_time_unix_nano >= start_time_unix_nano`, duration at most `MAX_SPAN_DURATION` (7 days, the search range), and the start within `[received - source.retention_days, received + MAX_FUTURE]`. Otherwise rejected (`timestamp out of range`, `end before start`, `span longer than 7 days`). Spans are not clamped like log lines: a span moved in time would draw a wrong waterfall (the metrics R7 reasoning).
  3. `service_name` is the resource's `service.name` string attribute; missing, empty or not a string becomes `unknown_service` (the OpenTelemetry SDK default). `name` is kept as sent, also empty. Both are truncated at a character boundary (`MAX_SERVICE_NAME_BYTES`, `MAX_SPAN_NAME_BYTES`).
  4. `kind` maps to `unspecified | internal | server | client | producer | consumer`, `status_code` to `unset | ok | error` (unknown integers to `unspecified` / `unset`); `status_message` is truncated to `MAX_STATUS_MESSAGE_BYTES`.
  5. `attributes` = the span's attributes, then `otel.scope.name` and `otel.scope.version` when the scope has them (the OpenTelemetry convention for non-OTLP backends). `resource_attributes` = the resource's attributes. Per map: a key that is empty, longer than `MAX_ATTRIBUTE_KEY_BYTES` or holds a control character is dropped; a repeated key keeps the first value; at most `MAX_ATTRIBUTES` entries are kept in order; values are truncated at a character boundary to `MAX_ATTRIBUTE_VALUE_BYTES`. What was dropped is added to the sender's `dropped_attributes_count` and stored as `otel.dropped_attributes_count` when non-zero (it counts against nothing).
  6. Values become strings: string as is; bool `true`/`false`; int in decimal; double with Rust's shortest round-trip `Display` (`NaN`, `inf`, `-inf` as written); bytes as standard base64; array and kvlist as compact JSON with the same rules inside (a kvlist's repeated key keeps the first); an empty `AnyValue` as `""`.
  7. `events` (JSON array, spec): at most `MAX_EVENTS_PER_SPAN`, each `{"time_unix_nano": "<decimal>", "name": "...", "attributes": {...}}` with at most `MAX_EVENT_ATTRIBUTES` attributes under the rules of step 5; events past the count or past `MAX_EVENTS_JSON_BYTES` of serialized JSON are dropped, and the number dropped (plus the sender's `dropped_events_count`) is stored as `otel.dropped_events_count`.
  8. `links` are not stored (no column in the spec; service maps are a non-goal). Their count goes to `otel.links_count` when non-zero, so the drawer can say a span had links.
  9. More than `MAX_SPANS_PER_REQUEST` spans in one request is 413 for the whole request; an insert body over `MAX_INSERT_BYTES` is 413.
  - Spans count against no `daily_byte_quota` (that quota is defined on stored log bytes; metrics R8 ruled the same) and have no usage stats (the spec has none).
- **R-TR-8 Limits and the shared listener.** All three endpoints hold a place of `IngestLimits.places`, look up the token under `IngestLimits.queries`, and decode under `IngestLimits.decodes` plus `LogsState.decode_shares` and `LogsState.project_limits.decode_shares` (the order `logs::ingest::prepare` uses), so the listener's memory and Postgres bounds stay the ones Logs Part 1 proved. Rate limits and in-flight counts stay per signal, so a source's traces never eat its log or metric budget: `/v1/traces` uses `TracesState` (50 requests/s, burst 100, 4 in flight per source, 8 per project, the logs constants); `/v1/logs` uses `LogsState` (rate limit, in flight, usage, quota) exactly as `/api/v1/logs`; `/v1/metrics` uses `MetricsState` and the `SeriesGuard` exactly as `/api/v1/metrics`.
- **R-TR-9 OTLP logs** (`traces::ingest::logs::to_events`, pure; spec: through the same per-source pipeline, pattern extraction and quota as HTTP logs). Per log record:
  - `dt` = `time_unix_nano`, else `observed_time_unix_nano`, else the receive time, then `normalize::clamp_to_accepted_range` (log lines are clamped, as on every other log path).
  - `level` = `severity_text` lowercased when it is one of `normalize::LEVELS`, else from `severity_number`: 1-4 `trace`, 5-8 `debug`, 9-12 `info`, 13-16 `warn`, 17-20 `error`, 21-24 `fatal`, else `info`.
  - `message` = the body: a string as is, any other `AnyValue` as the compact JSON of R-TR-7 step 6, empty body `""`.
  - `trace_id` / `span_id` = the record's ids as lowercase hex through `normalize::checked_id` before the event is built (Logs Part 2 ruling R-P2-22), so an invalid id never becomes a column. All-zero or wrongly sized ids are kept in `fields` as `otel.trace_id` / `otel.span_id` hex text.
  - `fields` = the record's attributes at the root, then the resource's attributes under `resource`, then `scope.name` / `scope.version` under `scope`. Dotted keys expand into nested objects (`http.method` becomes `{"http": {"method": ...}}`), so explorer paths such as `resource.service.name` address them. When two keys collide (a scalar `http` and `http.method`, or a record attribute `resource`), the first value written wins and the later one is kept under its full dotted key as a literal key: nothing is lost. Values keep their JSON types (int, double, bool, string, arrays and objects), unlike span attributes.
  - The events then go through `RowBatch::push` (caps, pipeline, pattern), the encoded insert, usage (`record_accepted`, pipeline counts) and the `daily_byte_quota` check before the body is read, exactly as `/api/v1/logs`. More than `logs::ingest::MAX_EVENTS` (10,000) records is 413. A record is never rejected one by one (logs accept every event), so `/v1/logs` never sends `partial_success`.
- **R-TR-10 OTLP metrics** (`traces::ingest::metrics::to_samples`, pure; spec: gauge, sum, histogram into the `metrics` table). Per data point, one or more `metrics::sample::Sample`s with `dt` = `time_unix_nano` and tags = the point's attributes plus the resource's `service.name`, `service.namespace`, `service.instance.id`, `deployment.environment.name` and `host.name` (the attributes Prometheus' OTLP translation promotes; all other resource attributes are left out, since `process.pid` and friends would multiply series and pass the 50-tag cap). Tag values are AnyValue strings (R-TR-7 step 6). Names and tag keys follow Prometheus' OTLP translation: every character outside `[a-zA-Z0-9_:.]` (names) or `[a-zA-Z0-9_.]` (keys) becomes `_`, and a leading digit gets a `_` prefix; a key that collides after that is the existing `DuplicateTag` rejection.
  - Gauge: `gauge`, value `as_double` or `as_int`.
  - Sum, cumulative and monotonic: `counter`. Sum, not monotonic: `gauge`. Sum with delta temporality: rejected (`delta temporality is not supported: use the collector's deltatocumulative processor`), since `rate` (metrics R11) reads successive cumulative values.
  - Histogram, cumulative: `<name>_count` and `<name>_sum` as `counter`, and `<name>_bucket` with tag `le` (the explicit bound in shortest `Display`, `+Inf` for the last bucket) as `counter` with the cumulative bucket counts, as Prometheus stores them. Histogram with delta temporality: rejected as above. `min`/`max` are not stored.
  - Exponential histogram and summary: rejected per point (`unsupported metric type`); the spec lists gauge, sum and histogram.
  - A point with neither value, or flagged `FLAG_NO_RECORDED_VALUE`, is skipped (not rejected: it is a staleness marker).
  - The samples then go through `metrics::ingest::validate_body` and `SeriesGuard::admit` (cardinality 429 `cardinality_exceeded` for the whole request) and the existing inserts. `rejected_data_points` counts points, a histogram point counting once however many samples it became. More than `MAX_SAMPLES_PER_REQUEST` (50,000) samples after expansion is 413.
- **R-TR-11 Span storage.** The spec's DDL, plus skip indexes for the search filters and a fixed time encoding:

```sql
CREATE TABLE IF NOT EXISTS spans (
    project_id UUID,
    source_id UUID,
    trace_id String,
    span_id String,
    parent_span_id String,
    service_name LowCardinality(String),
    name LowCardinality(String),
    kind LowCardinality(String),
    start_time DateTime64(9, 'UTC'),
    duration_ns UInt64,
    status_code LowCardinality(String),
    status_message String,
    attributes Map(String, String),
    resource_attributes Map(String, String),
    events String,
    retention_days UInt16,
    INDEX idx_trace trace_id TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_duration duration_ns TYPE minmax GRANULARITY 1,
    INDEX idx_status status_code TYPE set(3) GRANULARITY 4,
    INDEX idx_attr_keys mapKeys(attributes) TYPE bloom_filter(0.01) GRANULARITY 4,
    INDEX idx_attr_values mapValues(attributes) TYPE bloom_filter(0.01) GRANULARITY 4
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(start_time)
ORDER BY (project_id, service_name, start_time)
TTL toDateTime(start_time) + toIntervalDay(retention_days)
```

  - `retention_days` is copied from the source onto each row (logs and metrics do the same), so the TTL is per row and a retention change applies to new spans only. Spans older than the retention are rejected at ingest (R-TR-7), so an insert touches at most `MAX_RETENTION_DAYS + 1` daily partitions, below the 400 of `INSERT_SETTINGS`.
  - `start_time` is serialized as the decimal text `<seconds>.<9-digit nanoseconds>`, which `DateTime64(9)` parses exactly under the client's pinned input format; `span_times_keep_nanoseconds` round-trips one through ClickHouse.
  - No trace-id index table and no projection: the trace route reads a bounded window (R-TR-13) with partition pruning plus `idx_trace`. A table keyed on `(project_id, trace_id)` is a follow-up if TR-16's measurements show slow lookups (recorded in the decisions doc).
- **R-TR-12 Trace search semantics** (`GET /projects/{id}/traces`).
  - Window `from`/`to` (RFC 3339): default the last hour, `from < to`, `to - from <= 7 days`, `to <= now + 5 min`; otherwise 400.
  - Span filters, all optional, all applied to the same span: `service` (exact, at most 256 bytes), `operation` (exact span name, at most 1,024 bytes), `min_duration_ms` / `max_duration_ms` (integers, `0 ..= 7 days`, min not above max), `error=true` (`status_code = 'error'`), and up to 10 repeatable `attr` filters as JSON `{"key": "...", "op": "eq"|"neq"|"exists", "value": "..."}` (key 1-256 bytes, value at most 1,024 bytes, `value` required except for `exists`). An attribute filter matches the span's `attributes` or its `resource_attributes` (`eq`: either map holds the value; `neq`: neither does; `exists`: either map has the key). A trace matches when at least one of its spans in the window matches every filter (the Jaeger semantics).
  - Two statements, so the work never multiplies traces by spans. Phase 1 groups the matching spans by `trace_id` with `t = min(start_time)`, ordered `t DESC, trace_id DESC`, keyset `(t, trace_id) < cursor`, `LIMIT limit + 1` (the extra row sets `next_cursor`). Phase 2 summarizes those trace ids over all their spans within `[from - TRACE_SUMMARY_PAD, to + TRACE_SUMMARY_PAD]`: `start` (min start), `duration_ms` (max end minus min start), `span_count`, `error_count`, up to 10 distinct `services`, and the root: the earliest span with an empty `parent_span_id`, else the earliest span with `root_missing: true`. A trace longer than the pad shows partial counts (documented). Known limit: phase 1 groups every matching span by `trace_id` under `max_rows_to_group_by = 1000000`, so an unfiltered multi-day search over a busy project answers 400 "too expensive" (R-TR-15); TR-16 measures where that starts (open question 6).
  - Cursor: base64url of `<t in ns>:<trace_id>`, invalid is 400. `limit` `1 ..= MAX_LIMIT`, default `DEFAULT_LIMIT`.
  - Response: `{"traces": [{"trace_id", "start", "duration_ms", "span_count", "error_count", "services", "root": {"service", "name", "span_id"}, "root_missing"}], "next_cursor": "..." | null}`.
- **R-TR-13 Trace detail semantics** (`GET /projects/{id}/traces/{trace_id}`).
  - `trace_id` 32 hex characters (lowercased), not all zeros; otherwise 400.
  - Optional `from`/`to` with the search rules; the default is `[now - 7 days, now + 5 min]`. The UI always passes a window: from search results `start - TRACE_SUMMARY_PAD` to `start + duration + TRACE_SUMMARY_PAD`, from a log row its `dt` plus or minus `TRACE_LINK_PAD` (spec deviation: the spec's route takes no window, and without one the lookup reads the project's whole retention).
  - Spans ordered by `start_time, span_id`, at most `MAX_TRACE_SPANS` with `truncated: true` past it. No spans in the window is 404 `no spans stored for this trace` (the same answer for another project's trace).
  - Per-span log counts (spec: one query grouped by `span_id`): `SELECT span_id, count() FROM logs WHERE project_id = {p} AND trace_id = {t} AND dt BETWEEN {start - 5 min} AND {end + 5 min} GROUP BY span_id LIMIT 5001`. Counts for an empty `span_id` are `logs_without_span`.
  - Response: `{"trace_id", "start", "end", "truncated", "spans": [{"span_id", "parent_span_id", "service_name", "name", "kind", "start", "duration_ns", "status_code", "status_message", "attributes", "resource_attributes", "events"}], "log_counts": {"<span_id>": n}, "logs_without_span": n, "logs_window": {"from", "to"}}`. `duration_ns` is a JSON string (it can pass 2^53); `events` is the parsed JSON array.
- **R-TR-14 Services, RED and Apdex** (`GET /projects/{id}/services`).
  - Window rules as R-TR-12, default the last 24 hours. Services: every `service_name` with spans in the window, at most `MAX_SERVICES` (ordered by request count, `truncated` past it).
  - Request spans: `kind IN ('server', 'consumer') OR parent_span_id = ''` (spec). RED over request spans: `requests`, `request_rate` (per second over the window), `errors` (`status_code = 'error'`), `error_rate` (errors / requests, `null` with no requests), `p50_ms`, `p95_ms`, `p99_ms` (`quantilesTDigest`, approximate, as metrics R11).
  - Apdex per the spec with `T` from `trace_service_settings` (default 500 ms): an error span is frustrated whatever its duration; satisfied and tolerating count only non-error spans. The thresholds reach ClickHouse as two bound arrays, `transform(service_name, {pNames:Array(String)}, {pThresholdsNs:Array(UInt64)}, {pDefaultNs:UInt64})`, never formatted.
  - Sparkline: request counts in `SERVICE_BUCKETS` equal buckets of `ceil(window / 30)` seconds (at least 60), empty buckets `0` (a count, not a gap).
  - Response: `{"from", "to", "step_seconds", "truncated", "services": [{"name", "requests", "request_rate", "errors", "error_rate", "p50_ms", "p95_ms", "p99_ms", "apdex", "apdex_threshold_ms", "threshold_is_default", "spark": [n, ...]}]}`.
  - `PUT /projects/{id}/services/settings` (member) with `{"service_name", "apdex_threshold_ms"}` upserts one row. `service_name` 1-256 bytes, no control, invisible or bidi characters (`core::validate::has_invisible_or_bidi`); `apdex_threshold_ms` `1 ..= 60_000`. The service need not have spans yet. At most `MAX_SERVICE_SETTINGS` rows per project (409 past it, a new name only). There is no delete: setting 500 restores the default value.
- **R-TR-15 Query budget.** Search, trace, services and their log-count query each hold one of the user's `MAX_TRACE_QUERIES_PER_USER` slots (`UserSlots`) and one of `MAX_TRACE_QUERIES_PER_INSTANCE` (a `Semaphore`, `try_acquire`), else 429, in `TracesState`. Reads run with `TRACE_READ_SETTINGS` = metrics' `METRIC_READ_SETTINGS` values (`max_result_bytes`, `max_result_rows = 100000`, `max_rows_to_group_by = 1000000` with `group_by_overflow_mode = throw`, `max_rows_to_read`, `max_bytes_to_read`, `read_overflow_mode = throw`, `max_memory_usage = 2147483648`, `max_threads = 4`), plus the client's own time limit and cancel-on-disconnect. `traces::query::user_error` maps the limit errors to 400 `the query is too expensive: narrow the time range or add filters`, and everything else to `logs::query::search::user_error`.
- **R-TR-16 Linking windows.** Logs explorer to trace: rows with a `trace_id` link to `/projects/{id}/traces/{trace_id}?from=<dt - 1 h>&to=<dt + 1 h>`; the page shows "No spans stored for this trace" on 404. Waterfall to logs: "View logs" opens the explorer with `range=custom`, `from = trace start - 5 min`, `to = trace end + 5 min` and `trace_id`; "Logs for this span" adds `span_id`. Both stay in the path's project: the engine binds the project id in every statement, and the UI builds links only from the current project's path.
- **R-TR-17 Waterfall rendering.** In-house, like the existing charts (metrics R18): one row per span, a bar positioned by `(start - trace start) / trace duration` and sized by duration (at least 2 px), nesting by `parent_span_id` computed in the pure `#lib/traces/waterfall.ts` (`layoutSpans(spans)`: depth per span, children sorted by start, spans whose parent is absent at depth 0 with `orphan: true`, a visited set so a parent cycle cannot loop). Error spans carry the `danger` token and an "error" text badge, so colour never carries the state alone. Subtrees collapse with a toggle button (`aria-expanded`). Rows are buttons opening the span drawer; arrow keys move between rows. The drawer is a modal surface: it traps focus with `inert` on the rest of the page and returns focus to the row (CLAUDE.md accessibility floor). Up to `MAX_TRACE_SPANS` rows are rendered without virtualization; TR-13 measures the 5,000-span render and records the number.
- **R-TR-18 Collector bundle.** The OpenTelemetry Collector config of Metrics Part 2 (`deploy/collectors/otelcol/config.yaml`, planned in NAMI-MET-22, which defines the exporter and `filelog` for this phase per R-M2-27) gets its `otlphttp` exporter pointed at the public URL (`traces_endpoint`, `logs_endpoint`; metrics stay on remote-write), with `headers: {Authorization: "Bearer <token>"}` and `compression: gzip`, and its `traces` and `logs` pipelines switched on. The source page's setup snippets gain "OpenTelemetry SDK (OTLP)": `OTEL_EXPORTER_OTLP_ENDPOINT=<public url>`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`, `OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<token>` (the space is percent-encoded, as the SDK environment spec requires), built like the existing snippets with `form?.token ?? TOKEN_PLACEHOLDER`.
- **R-TR-19 Not in this phase.** gRPC OTLP, tail sampling, service maps and profiling (spec non-goals); stored span links; exponential histograms and summaries; trace-based alerts; per-source span usage stats; a trace-id index table (R-TR-11). Per-IP limits and graceful shutdown stay with Platform (Logs Part 1 ruling).

## Spec deviations (recorded in the decisions doc)

- `prost` with hand-written messages and vendored protos instead of the `opentelemetry-proto` crate (R-TR-1).
- The trace route takes an optional window, default the last 7 days (R-TR-13).
- Span links are not stored; only their count (R-TR-7 step 8).
- Delta sums and histograms, exponential histograms and summaries are rejected per point (R-TR-10).
- OTLP metric names and tag keys are sanitized the Prometheus way before the metrics validator (R-TR-10).

## File Structure

```
nami-engine/
  proto/opentelemetry/proto/...          vendored .proto files, LICENSE, VERSION (TR-2)
  src/lib.rs                             pub mod traces; (TR-1)
  src/traces/mod.rs                      TracesState, slots, constants (TR-1, TR-5, TR-8)
  src/traces/otlp/mod.rs                 Signal, Encoding, decode entry points (TR-2, TR-3)
  src/traces/otlp/proto.rs               prost + serde messages, google.rpc.Status (TR-2, TR-3)
  src/traces/otlp/wire.rs                SCHEMA and the protobuf pre-scan (TR-2)
  src/traces/otlp/json.rs                JSON pre-scan and the OTLP JSON helpers (TR-3)
  src/traces/otlp/response.rs            Export*ServiceResponse, partial success, OtlpError (TR-5)
  src/traces/span.rs                     SpanKind, SpanStatus, SpanRow, from_otlp (TR-1, TR-4)
  src/traces/store.rs                    encode and insert spans (TR-1)
  src/traces/ingest/mod.rs               shared admission and decode steps of the three paths (TR-5)
  src/traces/ingest/routes.rs            POST /v1/traces, /v1/logs, /v1/metrics (TR-5, TR-6, TR-7)
  src/traces/ingest/logs.rs              OTLP log records to LogEvents (TR-6)
  src/traces/ingest/metrics.rs           OTLP data points to Samples (TR-7)
  src/traces/query/mod.rs                window, filters, cursor, TRACE_READ_SETTINGS, user_error (TR-8)
  src/traces/query/search.rs             trace search (TR-8)
  src/traces/query/trace.rs              one trace and its log counts (TR-9)
  src/traces/query/routes.rs             GET /projects/{id}/traces[/{trace_id}] (TR-8, TR-9)
  src/traces/service/mod.rs              RED and Apdex SQL (TR-10)
  src/traces/service/model.rs            trace_service_settings (TR-10)
  src/traces/service/routes.rs           GET /services, PUT /services/settings (TR-10)
  src/core/clickhouse.rs                 MIGRATIONS gains a traces group: spans (TR-1)
  src/core/state.rs                      AppState.traces (TR-1)
  src/core/router.rs                     public and API routes of the domain (TR-5, TR-8, TR-10)
  src/logs/ingest/mod.rs                 prepare_events (a decoder-agnostic prepare) (TR-6)
  src/logs/ingest/routes.rs              admission steps shared with /v1/logs (TR-6)
  migrations/NNNN_traces_service_settings.sql (TR-10)
  tests/traces/main.rs                   new target, mounts tests/common (TR-1)
  tests/traces/{spans_store, otlp_decode, otlp_traces, otlp_logs, otlp_metrics,
                trace_search, trace_detail, services, isolation}.rs
  tests/common/traces.rs                 OTLP request builders, post helpers, stored spans (TR-5)
  tests/fixtures/otlp/                   trace.json, logs.json, metrics.json from the proto repo's examples (TR-3)
  traces_decode_memory                   standalone binary, counting allocator (TR-3)
deploy/collectors/                       OTel Collector OTLP pipelines, e2e spans test (TR-11, from Metrics Part 2)
nami-ui/
  src/params.ts                          traceid matcher (TR-12)
  src/lib/traces/trace-id.ts             isTraceId, no imports (loaded by src/params.ts under plain Node) (TR-12)
  src/lib/traces/traces.ts               types, search state, formatting (TR-12)
  src/lib/traces/waterfall.ts            layoutSpans, bar geometry (TR-13)
  src/lib/server/traces/trace-proxy.ts   allowed query params (TR-12)
  src/lib/components/traces/{TraceList, Waterfall, SpanDrawer, ServicesTable}.svelte
  src/lib/logs/logs.ts                   traceId / spanId in ExplorerState (TR-14)
  src/lib/logs/logs.ts                   otlpSnippet (TR-11)
  src/routes/(app)/projects/[id=uuid]/+layout.svelte          Traces tab (TR-12)
  src/routes/(app)/projects/[id=uuid]/traces/+page.{server.ts,svelte}             explorer (TR-12)
  src/routes/(app)/projects/[id=uuid]/traces/[traceId=traceid]/+page.{server.ts,svelte}  waterfall (TR-13)
  src/routes/(app)/projects/[id=uuid]/traces/services/+page.{server.ts,svelte}    services (TR-15)
  src/routes/(app)/projects/[id=uuid]/logs/+page.svelte       View trace, trace filters (TR-14)
  src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/+page.svelte  OTLP snippet (TR-11)
```

Order: TR-1 → TR-2 → TR-3 → TR-4 → TR-5 → TR-6 → TR-7 → TR-8 → TR-9 → TR-10 → TR-11 → TR-12 → TR-13 → TR-14 → TR-15 → TR-16. Engine tasks TR-8 to TR-10 depend only on TR-1 (rows to read) and may run in a parallel worktree after TR-1, against the response shapes fixed here (CLAUDE.md rule 5). UI tasks need the engine routes they call. Each task ends with its task review and fix commits on the phase branch.

## Tasks

### Task 1 (NAMI-TR-1): Add the traces domain, the span row and the `spans` table

**Covers:** R7.02  **Depends on:** none (phase start)

**Files:**
- Create: `nami-engine/src/traces/mod.rs`, `nami-engine/src/traces/span.rs`, `nami-engine/src/traces/store.rs`, `nami-engine/tests/traces/main.rs`, `nami-engine/tests/traces/spans_store.rs`.
- Modify: `nami-engine/src/lib.rs` (`pub mod traces;`), `nami-engine/src/core/state.rs` (`pub traces: Arc<TracesState>`, `for_test`), `nami-engine/src/main.rs` (build it), `nami-engine/src/core/clickhouse.rs` (`MIGRATIONS` gains a `---- Traces domain ----` group with the R-TR-11 DDL; the array length grows by one), `nami-engine/tests/core/clickhouse.rs` (`migrate_is_idempotent` lists `spans`).

**Data shape:**
- ClickHouse `spans` exactly as R11.
- Rust (`traces::span`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind { Unspecified, Internal, Server, Client, Producer, Consumer }
impl SpanKind { pub fn from_otlp(raw: i32) -> SpanKind; pub fn as_str(self) -> &'static str; }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus { Unset, Ok, Error }
impl SpanStatus { pub fn from_otlp(raw: i32) -> SpanStatus; pub fn as_str(self) -> &'static str; }

/// One row of `spans`, valid by construction (only `from_otlp` and tests build it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpanRow {
    pub project_id: Uuid, pub source_id: Uuid,
    pub trace_id: String, pub span_id: String, pub parent_span_id: String,   // lowercase hex, parent may be ""
    pub service_name: String, pub name: String,
    pub kind: SpanKind, pub status_code: SpanStatus, pub status_message: String,
    #[serde(serialize_with = "nanos_text")] pub start_time_ns: u64,           // column start_time
    pub duration_ns: u64,
    pub attributes: BTreeMap<String, String>, pub resource_attributes: BTreeMap<String, String>,
    pub events: String,                                                       // JSON array text
    pub retention_days: u16,
}
```

- `traces::store`: `pub const SPANS_TABLE: &str = "spans"; pub const MAX_INSERT_BYTES: usize = 64 * 1024 * 1024; pub fn encode_spans(rows: &[SpanRow]) -> Result<String, ClickHouseError>` (`encode_rows_within(rows, MAX_INSERT_BYTES)`); `pub async fn insert_encoded(ch: &ClickHouse, body: String) -> Result<(), ClickHouseError>` (`insert_encoded_with`, `logs::ingest::store::INSERT_SETTINGS`).

**Behaviour:**
1. `start_time` is written as `<seconds>.<9 digits>` text (R-TR-11); the serde rename maps `start_time_ns` to the column `start_time`.
2. `SpanKind::from_otlp` maps 0..=5 and anything else to `Unspecified`; `SpanStatus::from_otlp` maps 0..=2 and anything else to `Unset`.
3. `TracesState` starts empty here (`#[derive(Debug, Default)] pub struct TracesState {}`) and is wired into `AppState` now, so TR-5 and TR-8 only add fields (and TR-8 to TR-10 can run in parallel after TR-1).

**Tests:**
- Unit (`traces::span`): `kinds_and_statuses_map_every_otlp_value`, `unknown_kind_is_unspecified`, `start_time_is_written_with_nine_digits`.
- `tests/traces/spans_store.rs` (own `nami_test_*` database): `migrate_creates_the_spans_table`, `span_times_keep_nanoseconds` (insert `1760000000123456789`, read back `toUnixTimestamp64Nano(start_time)`), `each_row_carries_its_source_retention` (TTL expression reads the row's `retention_days`), `an_insert_over_the_cap_is_refused` (`encode_spans` at `MAX_INSERT_BYTES` and past it).
- `tests/core/clickhouse.rs::migrate_is_idempotent` with `spans` (twice in a row, no error).

**Acceptance:** `cargo test --test traces spans_store::` and `cargo test --test core clickhouse::` pass; `SHOW CREATE TABLE spans` on a fresh test database matches R11.

### Task 2 (NAMI-TR-2): Vendor the OTLP protos, write the prost messages and the protobuf pre-scan

**Covers:** R7.01  **Depends on:** NAMI-TR-1

**Files:**
- Create: `nami-engine/proto/opentelemetry/proto/**` (R-TR-1 list), `nami-engine/proto/LICENSE`, `nami-engine/proto/VERSION`, `nami-engine/src/traces/otlp/mod.rs`, `nami-engine/src/traces/otlp/proto.rs`, `nami-engine/src/traces/otlp/wire.rs`, `nami-engine/tests/traces/otlp_decode.rs`.
- Modify: `nami-engine/Cargo.toml` (`prost` gains the `derive` feature, R-TR-1; `cargo add prost@0.14.4` with `std` and `derive` if Metrics Part 2 has not added it), `nami-engine/src/traces/mod.rs` (`pub mod otlp;`), `docs/superpowers/decisions/<date>-traces-part1.md` (created here with the R-TR-1 decision record for `prost-derive`; the rest is written after TR-16).

**Data shape:**
- `traces::otlp::proto`, field numbers and types exactly as the vendored files (checked by test): `AnyValue` (`oneof value`: `string_value = 1`, `bool_value = 2`, `int_value = 3`, `double_value = 4`, `array_value = 5`, `kvlist_value = 6`, `bytes_value = 7`), `ArrayValue`, `KeyValueList`, `KeyValue`, `InstrumentationScope`, `Resource`; traces: `ExportTraceServiceRequest`, `ResourceSpans`, `ScopeSpans`, `Span` (ids as `bytes`, times `fixed64`, `kind`, `attributes`, `dropped_attributes_count`, `events`, `dropped_events_count`, `links`, `dropped_links_count`, `status`), `span::Event`, `span::Link` (no fields decoded: only counted), `Status`, `ExportTraceServiceResponse`, `ExportTracePartialSuccess`; logs: `ExportLogsServiceRequest`, `ResourceLogs`, `ScopeLogs`, `LogRecord`, `ExportLogsServiceResponse`, `ExportLogsPartialSuccess`; metrics: `ExportMetricsServiceRequest`, `ResourceMetrics`, `ScopeMetrics`, `Metric` (`oneof data`: `gauge = 5`, `sum = 7`, `histogram = 9`, `exponential_histogram = 10`, `summary = 11`), `Gauge`, `Sum`, `Histogram`, `ExponentialHistogram` and `Summary` (data points counted, not decoded), `NumberDataPoint`, `HistogramDataPoint` (packed `bucket_counts`, `explicit_bounds`), `ExportMetricsServiceResponse`, `ExportMetricsPartialSuccess`; and `RpcStatus` (`google.rpc.Status`: `code = 1`, `message = 2`, `details` not declared). Exemplars, `trace_state`, `schema_url`, `flags` other than the data point's are not declared (prost skips them).
- `traces::otlp`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Signal { Traces, Logs, Metrics }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Encoding { Protobuf, Json }
pub const OTLP_MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ANY_VALUE_DEPTH: usize = 8;
/// Pre-scan, then decode. 400 for a malformed body, 413 once `budget` is spent.
pub fn decode_traces(raw: &[u8], encoding: Encoding, budget: &mut Budget) -> Result<ExportTraceServiceRequest, DecodeError>;
pub fn decode_logs(..) -> Result<ExportLogsServiceRequest, DecodeError>;
pub fn decode_metrics(..) -> Result<ExportMetricsServiceRequest, DecodeError>;
#[derive(Debug, thiserror::Error)] pub enum DecodeError { Malformed(String), TooDeep, TooLarge }
```

- `traces::otlp::wire`: `pub enum Kind { Varint, Fixed64, Fixed32, Bytes, Text, Packed { elem: usize }, Message(usize) }`, `pub struct FieldSpec { pub number: u32, pub name: &'static str, pub kind: Kind, pub repeated: bool }`, `pub struct MessageSpec { pub name: &'static str, pub cost: usize, pub fields: &'static [FieldSpec] }`, `pub const SCHEMA: &[MessageSpec]`, `pub const MAX_MESSAGE_COST: usize`, `pub fn measure(raw: &[u8], root: usize, budget: &mut Budget) -> Result<(), DecodeError>`.

**Behaviour:**
1. `measure` follows R-TR-4: charges `2 x cost` per message and `len + STRING_BYTES` per string or bytes field, `len` per packed field, skips unknown fields by wire type, refuses malformed varints, lengths past the end, wire types 3 and 4, and `AnyValue` depth past `MAX_ANY_VALUE_DEPTH`. It never allocates (it takes `&[u8]` and keeps a fixed-size depth counter).
2. `decode_*` with `Encoding::Protobuf` calls `measure` and then `prost::Message::decode`. prost's own errors map to `Malformed` with a fixed text (`invalid protobuf body`), never prost's message text with byte offsets.
3. `cost` of each `MessageSpec` is `std::mem::size_of::<T>()` written as a `const` expression, so a struct change moves the estimate with it.

**Tests:**
- Unit (`traces::otlp::wire`): `rust_messages_match_the_vendored_protos` (R-TR-1), `every_schema_entry_points_at_a_real_message`, `malformed_varint_is_refused`, `length_past_the_end_is_refused`, `groups_are_refused`, `unknown_fields_are_skipped_and_cost_nothing`, `any_value_nesting_at_the_depth_passes_and_one_past_is_refused` (`MAX_ANY_VALUE_DEPTH` and `+ 1`), `packed_bucket_counts_are_charged`.
- `tests/traces/otlp_decode.rs`: `a_request_round_trips_through_prost` (encode a request with every field, decode, compare), `empty_attributes_are_charged_before_decoding` (16 MiB of `0x4a 0x00` inside one span: `TooLarge`, and the test's peak RSS check is the standalone binary of TR-3), `a_collector_sized_batch_fits_the_budget` (8,192 spans of 20 attributes: measured below half of `MAX_DECODED_BYTES`), `a_body_at_the_inflated_cap_decodes_and_one_byte_more_is_refused` (`OTLP_MAX_DECOMPRESSED_BYTES`, through `gunzip_within`).

**Acceptance:** the three OTLP requests decode from protobuf; the vendored-proto test passes and fails when one field number in `SCHEMA` is changed; `cargo tree` shows one `prost` and no `tonic`, `opentelemetry` or `opentelemetry_sdk`.

### Task 3 (NAMI-TR-3): Decode OTLP JSON on the same messages, with its pre-scan

**Covers:** R7.01  **Depends on:** NAMI-TR-2

**Files:**
- Create: `nami-engine/src/traces/otlp/json.rs`, `nami-engine/tests/fixtures/otlp/{trace,logs,metrics}.json` (the `examples/` files of the vendored proto tag), the standalone counting-allocator test binary `traces_decode_memory` in `nami-engine/tests/` (pattern of `logs_decode_memory`, see open question 2).
- Modify: `nami-engine/src/traces/otlp/proto.rs` (serde attributes), `nami-engine/src/traces/otlp/mod.rs` (`Encoding::Json` branches), `nami-engine/tests/traces/otlp_decode.rs`, `scripts/check-plans.py` (`traces_decode_memory` added to `STANDALONE_TESTS`, open question 2).

**Data shape:** serde on the TR-2 structs: `#[serde(rename_all = "camelCase", default)]` with `alias` for each snake_case name; hand-written `Deserialize` for `AnyValue`, `Metric` and `NumberDataPoint` (a map visitor dispatching on the oneof key, `{}` as an empty `AnyValue`, no `#[serde(flatten)]`, R-TR-5); helpers in `traces::otlp::json`: `hex_bytes` (hex string, case-insensitive, `null` or `""` as empty; a non-hex character is a deserialization error), `u64_text_or_number`, `i64_text_or_number`, `f64_or_special` (`"NaN"`, `"Infinity"`, `"-Infinity"`), `base64_bytes`, `enum_int` (integers only); `pub const MAX_JSON_DEPTH: usize = 32; pub fn measure(raw: &[u8], root: usize, budget: &mut Budget) -> Result<(), DecodeError>`; `FieldSpec` (TR-2) gains `json: &'static str` (lowerCamelCase) next to `name` (snake_case).

**Behaviour:**
1. R-TR-5 mapping. Unknown fields are ignored. A JSON syntax error, a type mismatch or a non-hex id is 400 `invalid OTLP JSON body: <serde's message without the input>` (serde_json's line and column are kept; never a fragment of the body).
2. `measure` charges per R-TR-4 and refuses nesting past `MAX_JSON_DEPTH`. Then `serde_json::from_slice`.
3. Whitespace-only body: 400 `request body is empty` (R-TR-3).

**Tests:**
- `tests/traces/otlp_decode.rs`: `the_proto_repo_examples_decode` (all three fixtures), `json_and_protobuf_decode_to_the_same_request` (one request encoded both ways), `ids_are_hex_not_base64`, `a_non_hex_id_is_a_bad_request`, `wide_integers_accept_strings_and_numbers`, `doubles_accept_nan_and_infinity_strings`, `snake_case_names_are_accepted`, `unknown_fields_are_ignored`, `enum_names_are_refused`, `a_json_body_of_empty_objects_stops_at_the_budget`, `a_collector_sized_json_batch_fits_the_budget` (8,192 spans of 20 attributes: measured below half of `MAX_DECODED_BYTES`, the bar of TR-2), `unknown_keys_cost_nothing_in_the_json_pre_scan`, `an_empty_any_value_object_decodes`, `nesting_at_the_depth_passes_and_one_past_is_refused` (`MAX_JSON_DEPTH`).
- `traces_decode_memory` (standalone, counting allocator, one test per shape): the peak live bytes of `decode_traces` for 16 MiB of empty attributes, empty spans, nested empty `ArrayValue`s and empty histogram data points (protobuf), and `{"resourceSpans":[{"scopeSpans":[{"spans":[{},...]}]}]}`, `{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"m","gauge":{"dataPoints":[{},...]}}]}]}]}` and `[{}]` attribute values (JSON) stay below `MAX_DECODED_BYTES` plus the body, and the measured estimate is at least the decode's peak for each.

**Acceptance:** every OTLP JSON example of the proto release decodes; the memory binary passes with `cargo test --test traces_decode_memory`.

### Task 4 (NAMI-TR-4): Map OTLP spans to rows, with the caps and per-span rejections

**Covers:** R7.01, R7.02  **Depends on:** NAMI-TR-3

**Files:**
- Modify: `nami-engine/src/traces/span.rs` (`from_otlp`, the cap constants, `any_value_text`).

**Data shape:**

```rust
pub const MAX_SPANS_PER_REQUEST: usize = 50_000;
pub const MAX_ATTRIBUTES: usize = 128;
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 256;
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 4_096;
pub const MAX_SPAN_NAME_BYTES: usize = 1_024;
pub const MAX_SERVICE_NAME_BYTES: usize = 256;
pub const MAX_STATUS_MESSAGE_BYTES: usize = 4_096;
pub const MAX_EVENTS_PER_SPAN: usize = 128;
pub const MAX_EVENT_ATTRIBUTES: usize = 32;
pub const MAX_EVENTS_JSON_BYTES: usize = 65_536;
pub const MAX_SPAN_DURATION: chrono::Duration = chrono::Duration::days(7);
pub const MAX_FUTURE: chrono::Duration = chrono::Duration::minutes(10);
pub const UNKNOWN_SERVICE: &str = "unknown_service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanRejection { TraceIdLength, TraceIdZero, SpanIdLength, SpanIdZero, ParentIdLength,
    MissingStart, EndBeforeStart, TooLong, TimestampOutOfRange }
impl SpanRejection { pub fn message(&self) -> &'static str }

pub struct Mapped { pub rows: Vec<SpanRow>, pub rejected: usize, pub errors: Vec<String> }  // errors: first MAX_REPORTED_ERRORS, "span <n>: <message>"
/// Pure. 413 (`Err`) only for more than MAX_SPANS_PER_REQUEST spans.
pub fn from_otlp(request: ExportTraceServiceRequest, source: &IngestSource, received_at: DateTime<Utc>)
    -> Result<Mapped, TooManySpans>;
/// R-TR-7 step 6, shared with TR-6 (message text) and TR-7 (tag values).
pub fn any_value_text(value: &AnyValue) -> String;
```

**Behaviour:** R-TR-7 steps 1 to 9, in that order, spans numbered from 1 across the whole request in decode order. The request is consumed (`into_iter`), so each span's protobuf strings move into the row instead of being copied. The span count is checked before any row is built.

**Tests (unit, `traces::span`):** one per rule, at and past each bound: `an_all_zero_trace_id_is_rejected`, `a_short_span_id_is_rejected`, `an_all_zero_parent_is_a_root`, `a_parent_of_the_wrong_length_is_rejected`, `end_before_start_is_rejected`, `a_span_of_seven_days_is_kept_and_one_nanosecond_more_is_rejected`, `a_start_older_than_the_retention_is_rejected`, `a_start_ten_minutes_ahead_is_kept_and_later_is_rejected`, `missing_service_name_is_unknown_service`, `names_are_truncated_at_a_character_boundary` (multi-byte character across `MAX_SPAN_NAME_BYTES`), `attribute_number_128_is_kept_and_129_is_dropped_and_counted`, `a_key_with_a_control_character_is_dropped`, `a_repeated_key_keeps_the_first_value`, `values_over_4096_bytes_are_truncated`, `scope_name_and_version_become_attributes`, `any_values_become_text` (every variant, nested arrays and kvlists), `event_128_is_kept_and_129_is_dropped_and_counted`, `events_past_64_kib_of_json_are_dropped_and_counted`, `links_are_counted_not_stored`, `the_first_ten_rejections_are_reported`, `more_than_50000_spans_is_too_many`.

**Acceptance:** every rule of R-TR-7 has its test at the bound and past it; `from_otlp` has no I/O.

### Task 5 (NAMI-TR-5): Serve `POST /v1/traces` on the public listener

**Covers:** R7.01  **Depends on:** NAMI-TR-4

**Files:**
- Create: `nami-engine/src/traces/otlp/response.rs`, `nami-engine/src/traces/ingest/mod.rs`, `nami-engine/src/traces/ingest/routes.rs`, `nami-engine/tests/common/traces.rs`, `nami-engine/tests/traces/otlp_traces.rs`.
- Modify: `nami-engine/src/traces/mod.rs` (`TracesState` ingest fields), `nami-engine/src/core/router.rs` (`build_public_router_with_limits` merges the public router of `src/traces/ingest/routes.rs` with `ingest_limits.clone()`), `nami-engine/tests/common/mod.rs` (`pub mod traces;`).

**Data shape:**

```rust
// traces/mod.rs
pub struct TracesState {
    pub rate_limiter: TokenBuckets,          // REQUESTS_PER_SECOND, BURST per source
    pub in_flight: InFlight,                 // MAX_IN_FLIGHT_PER_SOURCE
    pub project_in_flight: InFlight,         // MAX_IN_FLIGHT_PER_PROJECT
}
// traces/otlp/response.rs
pub struct OtlpError { pub error: IngestError, pub encoding: Option<Encoding> }  // IntoResponse per R-TR-6
pub fn export_response(signal: Signal, encoding: Encoding, rejected: usize, errors: &[String]) -> Response;
```

- `tests/common/traces.rs`: `span(trace: [u8; 16], span: [u8; 8], parent: Option<[u8; 8]>, name, start_ns, end_ns) -> Span`, `request(service: &str, spans: Vec<Span>) -> ExportTraceServiceRequest`, `post_protobuf(public, path, token, bytes, gzip) -> (StatusCode, HeaderMap, Bytes)`, `post_otlp_json(public, path, token, json) -> (StatusCode, HeaderMap, Value)`, `stored_spans(ch, project_id) -> Vec<Value>`.

**API:** `POST /v1/traces`, token only (no session, no role). Statuses: 200 (all or some stored), 400 (malformed, nothing accepted), 401 (token), 408 (slow body), 413 (inflated, budget, spans, insert), 415 (content type or encoding), 429 (`rate_limited`, `too_many_in_flight`; `Retry-After`), 503 (`busy`, `Retry-After: 1`), 500 (logged, generic).

**Behaviour:** order of checks, each cheaper than the next, as `/api/v1/logs`: token shape (401, no query) → a place of `limits.places` (503) → token lookup under `limits.queries` (401) → `traces.rate_limiter` (429) → `traces.in_flight`, then `traces.project_in_flight` (429) → content type and encoding (415) → `read_body` (413, 408) → a decode place (R-TR-8 order) → `spawn_blocking`: inflate, `decode_traces`, `span::from_otlp`, `store::encode_spans` → insert → `export_response`. The work after `read_body` runs in a spawned task holding the place and the in-flight guards (the `Work` pattern of `src/logs/ingest/routes.rs`), so a client that hangs up cannot free the place while its decode still holds memory. The response encoding is the request's (R-TR-6); errors before the content type is known are JSON `RpcStatus`.

**Tests (`tests/traces/otlp_traces.rs`):** `protobuf_spans_are_stored`, `json_spans_are_stored`, `gzip_bodies_are_accepted`, `the_response_has_the_request_encoding`, `one_bad_span_is_a_partial_success` (99 stored, `rejectedSpans: 1`, the reason in `errorMessage`), `a_request_of_only_bad_spans_is_a_bad_request`, `an_empty_request_is_ok_and_stores_nothing`, `a_missing_or_unknown_token_is_unauthorized` (`WWW-Authenticate: Bearer`, the body a `Status` with code 16), `an_unsupported_content_type_is_415`, `an_unsupported_encoding_is_415`, `a_body_over_10_mib_is_413`, `a_gzip_bomb_is_413` (a few KiB inflating past `OTLP_MAX_DECOMPRESSED_BYTES`), `malformed_protobuf_is_400_without_panicking` (truncated message, random bytes, a group), `the_rate_limit_is_429_with_retry_after`, `a_source_with_four_requests_in_flight_gets_429`, `a_full_listener_is_503`, `errors_are_status_messages_in_the_request_encoding`, `rows_carry_the_source_project_and_retention`, `the_token_is_never_logged` (captured `tracing` output of a rejected request).

**Acceptance:** an OpenTelemetry SDK exporter (`http/protobuf` and `http/json`) pointed at the public listener stores spans (manual check with the collector of TR-11, recorded in the PR); all tests pass.

### Task 6 (NAMI-TR-6): Serve `POST /v1/logs` through the logs pipeline

**Covers:** R7.01 (OTLP logs), R7.05 (log records keep their `trace_id`/`span_id`)  **Depends on:** NAMI-TR-5

**Files:**
- Create: `nami-engine/src/traces/ingest/logs.rs`, `nami-engine/tests/traces/otlp_logs.rs`.
- Modify: `nami-engine/src/logs/ingest/mod.rs` (`prepare_events`), `nami-engine/src/logs/ingest/routes.rs` (the admission steps of `ingest_logs` move into `pub(crate) async fn admit_log_request`, and `Work` takes the decoder; `/api/v1/logs` keeps its behaviour), `nami-engine/src/traces/ingest/routes.rs` (`/v1/logs`).

**Data shape:**

```rust
// logs/ingest/mod.rs
/// `prepare` with the body's decoder as a parameter: the decoder turns the (inflated) body into
/// events on the blocking thread; the rest (RowBatch::push, encoding, places) is prepare's.
/// `prepare` becomes `prepare_events(body, http decoder, ..)`.
pub async fn prepare_events(body: Bytes, decode: EventDecoder, source: &IngestSource,
    pipeline: Arc<CompiledPipeline>, received_at: DateTime<Utc>, decodes: &Arc<Semaphore>,
    shares: &DecodeShares, project_shares: &DecodeShares) -> Result<Prepared, IngestError>;
pub type EventDecoder = Box<dyn FnOnce(Bytes, DateTime<Utc>) -> Result<Vec<LogEvent>, IngestError> + Send>;

// traces/ingest/logs.rs (pure)
pub fn to_events(request: ExportLogsServiceRequest, received_at: DateTime<Utc>) -> Result<Vec<LogEvent>, IngestError>;
pub fn level_of(severity_text: &str, severity_number: i32) -> &'static str;
pub fn expand_fields(entries: impl Iterator<Item = (String, Value)>, into: &mut Map<String, Value>);
```

**API:** `POST /v1/logs`, token only. Statuses as `/v1/traces`, plus 429 `quota_exceeded` with `Retry-After` until the next UTC midnight (from `admit_log_request`, as `/api/v1/logs`).

**Behaviour:**
1. Admission is `/api/v1/logs`'s, in its order and with its state (`LogsState` rate limit, in-flight counts, quota, usage counters for rate-limited and over-quota requests), then the OTLP content type and encoding (415), `read_body`, and `prepare_events` with a decoder that inflates (`OTLP_MAX_DECOMPRESSED_BYTES`), calls `decode_logs` (R-TR-4 budget) and `to_events` (R-TR-9).
2. `to_events` follows R-TR-9; more than `MAX_EVENTS` records is 413 before any event is built.
3. Storage, usage (`record_accepted`, pipeline counts) and the response are `Work`'s; the response is an `ExportLogsServiceResponse` in the request's encoding with no partial success (R-TR-9).

**Tests:**
- Unit (`traces::ingest::logs`): `severity_text_wins_when_it_is_a_level`, `severity_numbers_map_to_levels` (every band edge: 0, 1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21, 24, 25), `observed_time_is_used_without_a_time`, `a_non_string_body_is_compact_json`, `dotted_keys_expand_into_objects`, `a_colliding_key_is_kept_literally`, `resource_and_scope_go_under_their_keys`, `invalid_ids_stay_in_fields_as_otel_keys`.
- `tests/traces/otlp_logs.rs`: `otlp_log_records_are_stored`, `json_and_protobuf_store_the_same_rows`, `trace_and_span_ids_are_columns`, `the_source_pipeline_runs_on_otlp_logs` (a redact rule hides a value in `fields`), `patterns_are_extracted`, `usage_counts_otlp_logs`, `the_daily_quota_is_429_quota_exceeded`, `the_rate_limit_is_shared_with_http_logs` (draining the source's bucket through `/api/v1/logs` makes `/v1/logs` 429), `record_10000_is_stored_and_10001_records_are_413`.
- `cargo test --test logs` passes unchanged (no `/api/v1/logs` behaviour moved).

**Acceptance:** an OTLP log record appears in the explorer with its level, message, `trace_id` and nested fields; the existing logs suite passes.

### Task 7 (NAMI-TR-7): Serve `POST /v1/metrics` into the metrics table

**Covers:** R7.01 (OTLP metrics)  **Depends on:** NAMI-TR-5

**Files:**
- Create: `nami-engine/src/traces/ingest/metrics.rs`, `nami-engine/tests/traces/otlp_metrics.rs`.
- Modify: `nami-engine/src/traces/ingest/routes.rs` (`/v1/metrics`), `nami-engine/src/metrics/sample.rs` (the two `Rejection` variants, if absent).

**Data shape:**

```rust
// traces/ingest/metrics.rs (pure)
pub const PROMOTED_RESOURCE_ATTRIBUTES: [&str; 5] =
    ["service.name", "service.namespace", "service.instance.id", "deployment.environment.name", "host.name"];
pub struct Points { pub samples: Vec<Result<Sample, Rejection>>, pub point_of_sample: Vec<u32>, pub points: usize, pub skipped: usize }
pub fn to_samples(request: ExportMetricsServiceRequest, received_at: DateTime<Utc>) -> Result<Points, IngestError>;
pub fn sanitize_name(raw: &str) -> String;
pub fn sanitize_key(raw: &str) -> String;
```

`Rejection` gains `DeltaTemporality` and `UnsupportedType` (in `metrics::sample`, messages from R-TR-10) if Metrics Part 1 has no equivalent.

**API:** `POST /v1/metrics`, token only. Statuses as `/v1/traces`, plus 429 `cardinality_exceeded` with `Retry-After` (whole request, from `SeriesGuard::admit`).

**Behaviour:**
1. Admission is `/api/v1/metrics`'s (`MetricsState` rate limit and in-flight counts), then content type and encoding, `read_body`, `series.ensure_loaded(project)`, a decode place, and on the blocking thread: inflate, `decode_metrics`, `to_samples` (R-TR-10), `metrics::ingest::validate_body`. Then the existing series and sample inserts (`forget` on an insert error).
2. `rejected_data_points` counts distinct points with at least one rejected sample (through `point_of_sample`), so a histogram point counts once; skipped no-value points are neither accepted nor rejected.
3. More than `MAX_SAMPLES_PER_REQUEST` samples after expansion is 413 before validation.

**Tests:**
- Unit (`traces::ingest::metrics`): `gauge_points_take_double_or_int`, `cumulative_monotonic_sums_are_counters`, `non_monotonic_sums_are_gauges`, `delta_sums_and_histograms_are_rejected`, `histograms_become_count_sum_and_buckets`, `bucket_bounds_use_shortest_display_and_inf`, `exponential_histograms_and_summaries_are_rejected`, `no_recorded_value_points_are_skipped`, `names_and_keys_are_sanitized_the_prometheus_way`, `only_promoted_resource_attributes_become_tags`.
- `tests/traces/otlp_metrics.rs`: `otlp_metrics_are_stored_and_queryable` (through `POST /projects/{id}/metrics/query`), `rejected_data_points_count_points_not_samples`, `too_many_new_series_is_429_cardinality_exceeded`, `sample_50000_is_stored_and_50001_samples_are_413`, `the_rate_limit_is_shared_with_http_metrics`.

**Acceptance:** a collector's `otlphttp` metrics (a gauge, a counter and a histogram) chart in the metrics explorer with the same values the Prometheus exporter of the same collector shows.

### Task 8 (NAMI-TR-8): Trace search API

**Covers:** R7.03  **Depends on:** NAMI-TR-1 (TR-5 for fixtures through ingest; tests may insert rows directly)

**Files:**
- Create: `nami-engine/src/traces/query/mod.rs`, `nami-engine/src/traces/query/search.rs`, `nami-engine/src/traces/query/routes.rs`, `nami-engine/tests/traces/trace_search.rs`.
- Modify: `nami-engine/src/traces/mod.rs` (`query_slots: Arc<UserSlots>`, `query_total: Arc<Semaphore>`, the two constants), `nami-engine/src/core/router.rs` (mount the query routes).

**Data shape:**

```rust
// traces/query/mod.rs
pub const DEFAULT_WINDOW: Duration = Duration::hours(1);
pub const MAX_WINDOW: Duration = Duration::days(7);
pub const MAX_ATTRIBUTE_FILTERS: usize = 10;
pub const MAX_FILTER_VALUE_BYTES: usize = 1_024;
pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 100;
pub const TRACE_SUMMARY_PAD: Duration = Duration::hours(1);
pub const TRACE_READ_SETTINGS: [(&str, &str); 10] = [/* R-TR-15 */];
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum AttrOp { Eq, Neq, Exists }
pub struct AttrFilter { pub key: String, pub op: AttrOp, pub value: Option<String> }   // value is Some iff op != Exists
pub struct SpanFilters { pub service: Option<String>, pub operation: Option<String>,
    pub min_duration_ns: Option<u64>, pub max_duration_ns: Option<u64>, pub error_only: bool,
    pub attributes: Vec<AttrFilter> }
pub struct TraceCursor { pub start_ns: u64, pub trace_id: String }       // encode/decode, base64url
pub fn parse_window(pairs: &[(String, String)], now: DateTime<Utc>, default: Duration) -> Result<Window, String>;
pub fn parse_search(pairs: &[(String, String)], now: DateTime<Utc>) -> Result<SearchParams, String>;
pub fn user_error(error: ClickHouseError) -> AppError;
// traces/query/search.rs
pub fn build_candidates(project_id: Uuid, params: &SearchParams) -> Sql;   // phase 1
pub fn build_summaries(project_id: Uuid, window: &Window, trace_ids: &[String]) -> Sql;  // phase 2
pub async fn search(ch: &ClickHouse, project_id: Uuid, params: &SearchParams) -> Result<SearchPage, AppError>;
```

**API:** `GET /projects/{id}/traces?from&to&service&operation&min_duration_ms&max_duration_ms&error&attr&limit&cursor`, viewer and up. 200 with the R-TR-12 response; 400 for any invalid parameter (message names it); 403 for a non-member; 429 for a full slot.

**Behaviour:** R-TR-12 and R15. Query parameters use `logs::query::query_pairs`; an unknown parameter is ignored (the logs search does the same). Phase 2 binds the trace ids as one `Array(String)` parameter. Both phases hold one user slot and one instance slot for the whole request.

**Tests:**
- Unit (`traces::query`): `the_window_defaults_to_an_hour_and_is_capped_at_seven_days` (7 days passes, 7 days and 1 s is 400), `to_may_not_be_more_than_five_minutes_ahead`, `durations_are_validated` (negative, min above max, past 7 days), `attribute_filters_are_validated` (10 pass, 11 is 400; key 256 bytes passes, 257 is 400; value 1,024 bytes passes, 1,025 is 400; `exists` takes no value), `the_cursor_round_trips_and_garbage_is_400`, `user_input_never_reaches_the_sql_text` (a service named `x' OR 1=1 --`, an attribute key `{p0:String}`, a value with a backslash: the SQL text has only placeholders).
- `tests/traces/trace_search.rs`: `traces_are_listed_newest_first`, `filters_apply_to_one_span` (service A on one span and an error on another span of the same trace: `service=A&error=true` does not match), `service_operation_and_duration_filters`, `error_only_finds_traces_with_an_error_span`, `attribute_filters_match_span_or_resource_attributes`, `neq_and_exists`, `keyset_pages_do_not_overlap_or_skip`, `the_root_span_names_the_trace`, `a_trace_without_its_root_is_root_missing`, `a_viewer_can_search_and_a_non_member_cannot` (403), `another_projects_spans_are_never_listed`, `a_fifth_concurrent_search_beyond_the_user_slots_is_429` (slots held by the test).

**Acceptance:** searches for each roadmap filter (service, operation, duration, status, time range) return the expected traces in a seeded project.

### Task 9 (NAMI-TR-9): Trace detail API with per-span log counts

**Covers:** R7.04, R7.05  **Depends on:** NAMI-TR-8

**Files:**
- Create: `nami-engine/src/traces/query/trace.rs`, `nami-engine/tests/traces/trace_detail.rs`, `nami-engine/tests/traces/isolation.rs`.
- Modify: `nami-engine/src/traces/query/routes.rs`.

**Data shape:**

```rust
pub const MAX_TRACE_SPANS: usize = 5_000;
pub const LOGS_PAD: Duration = Duration::minutes(5);          // spec: plus or minus 5 min
pub const MAX_LOG_COUNT_ROWS: usize = 5_000;
pub fn parse_trace_id(raw: &str) -> Result<String, String>;    // 32 hex, lowercased, not all zeros
pub async fn load(ch: &ClickHouse, project_id: Uuid, trace_id: &str, window: &Window) -> Result<Option<TraceView>, AppError>;
```

**API:** `GET /projects/{id}/traces/{trace_id}?from&to`, viewer and up. 200 with the R-TR-13 response; 400 for a malformed trace id or window; 403 for a non-member; 404 `no spans stored for this trace`; 429 for a full slot.

**Behaviour:** R13. The spans statement selects `WHERE project_id = {p} AND trace_id = {t} AND start_time BETWEEN {from} AND {to} ORDER BY start_time, span_id LIMIT 5001`. The log-count statement runs only when spans were found, over `[min start - 5 min, max end + 5 min]` and the same project id; with more than `MAX_LOG_COUNT_ROWS` distinct span ids the counts are cut and `log_counts_truncated: true` is set. Both statements share one user slot and one instance slot. `events` text is parsed with `serde_json` (it was written by TR-4, but a row that fails to parse yields `[]` and a `warn` log with the project id, never a 500).

**Tests:**
- Unit: `trace_ids_are_32_hex_characters` (31 and 33 are 400, uppercase is lowercased, all zeros is 400, a uuid with dashes is 400).
- `tests/traces/trace_detail.rs`: `all_spans_of_a_trace_are_returned_in_start_order`, `span_5000_is_returned_and_more_are_truncated`, `durations_are_strings`, `events_and_attributes_are_returned`, `log_counts_are_grouped_by_span`, `logs_without_a_span_are_counted_apart`, `logs_outside_the_five_minute_pad_are_not_counted`, `an_unknown_trace_is_404`, `the_window_defaults_to_seven_days_and_is_capped`.
- `tests/traces/isolation.rs`: `a_trace_of_another_project_is_not_found` (same trace id stored in project A, requested under project B where the caller is admin: 404), `log_counts_never_include_another_projects_logs` (project B's logs carry project A's trace id), `search_never_lists_another_projects_traces`, `a_non_member_is_403_before_the_trace_is_looked_up`.

**Acceptance:** the waterfall's data for a seeded 3-service trace, with log counts per span, comes back in one request.

### Task 10 (NAMI-TR-10): Services API with RED stats, Apdex and thresholds

**Covers:** R7.06  **Depends on:** NAMI-TR-8

**Files:**
- Create: `nami-engine/migrations/NNNN_traces_service_settings.sql`, `nami-engine/src/traces/service/mod.rs`, `nami-engine/src/traces/service/model.rs`, `nami-engine/src/traces/service/routes.rs`, `nami-engine/tests/traces/services.rs`.
- Modify: `nami-engine/src/traces/mod.rs` (`pub mod service;`), `nami-engine/src/core/router.rs`, `nami-engine/tests/core/migrations.rs` (`all_tables_exist`), `nami-engine/tests/core/json_bodies.rs` (the PUT route).

**Data shape:**

```sql
create table trace_service_settings (
    project_id uuid not null,
    service_name text not null,
    apdex_threshold_ms int not null default 500,
    constraint trace_service_settings_pkey primary key (project_id, service_name),
    constraint trace_service_settings_project_id_fkey foreign key (project_id)
        references projects (id) on delete cascade,
    constraint trace_service_settings_threshold_check
        check (apdex_threshold_ms between 1 and 60000),
    constraint trace_service_settings_name_check
        check (octet_length(service_name) between 1 and 256)
);
```

```rust
pub const DEFAULT_APDEX_THRESHOLD_MS: i32 = 500;
pub const MIN_APDEX_THRESHOLD_MS: i32 = 1;
pub const MAX_APDEX_THRESHOLD_MS: i32 = 60_000;
pub const MAX_SERVICE_SETTINGS: i64 = 1_000;
pub const MAX_SERVICES: usize = 200;
pub const SERVICE_BUCKETS: u32 = 30;
pub const MIN_BUCKET_SECONDS: i64 = 60;
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
pub struct SetThreshold { pub service_name: String, pub apdex_threshold_ms: i32 }
pub struct ValidThreshold { service_name: String, apdex_threshold_ms: i32 }   // only validate() builds it
pub fn validate(body: SetThreshold) -> Result<ValidThreshold, String>;
pub async fn thresholds(pool: &PgPool, project_id: Uuid) -> Result<Vec<(String, i32)>, sqlx::Error>;
/// Upsert inside one transaction with the count check (409 at MAX_SERVICE_SETTINGS for a new name).
pub async fn set_threshold(pool: &PgPool, project_id: Uuid, value: ValidThreshold) -> Result<(), AppError>;
pub fn build_stats(project_id: Uuid, window: &Window, thresholds: &[(String, i32)]) -> Sql;
pub fn build_sparks(project_id: Uuid, window: &Window, step_seconds: i64, services: &[String]) -> Sql;
```

**API:**
- `GET /projects/{id}/services?from&to`, viewer and up: 200 with the R-TR-14 response; 400 for the window; 403; 429.
- `PUT /projects/{id}/services/settings`, member and up: 200 `{"service_name", "apdex_threshold_ms"}`; 400 for validation; 403 for a viewer; 409 past `MAX_SERVICE_SETTINGS`; 422 for unknown fields (axum, before the handler).

**Behaviour:** R-TR-14 and R15. `set_threshold` runs in one transaction: it takes the project's row lock with `select ... from projects where id = $1 for no key update` (the lock `uptime::quota::ProjectQuota` uses, so two concurrent new names cannot both pass at 999), counts the project's rows only when the name is new, then upserts (`on conflict on constraint trace_service_settings_pkey do update`). The cap answers 409 (STANDARDS 3; `ProjectQuota`'s 400 is not reused because it is uptime's). Stats are one statement grouped by `service_name`; sparks one statement grouped by service and bucket, only for the listed services (bound `Array(String)`).

**Tests:**
- Unit (`traces::service`): `apdex_counts_satisfied_tolerating_and_frustrated` (pure helper mirrored by the SQL test), `the_threshold_bounds_are_1_and_60000` (0 and 60,001 are 400), `service_names_are_validated` (256 bytes passes, 257 and an invisible character are 400), `thresholds_are_bound_never_formatted` (SQL text has placeholders only).
- `tests/traces/services.rs`: `red_stats_count_request_spans_only` (internal child spans are not requests; a root of kind internal is), `a_fast_error_is_frustrated`, `durations_at_t_and_4t_are_satisfied_and_tolerating`, `no_request_spans_is_null_apdex`, `a_custom_threshold_changes_apdex`, `the_default_threshold_is_500_ms_and_marked`, `sparks_have_thirty_buckets_with_zeros`, `services_past_200_are_truncated`, `a_member_sets_a_threshold_and_a_viewer_gets_403`, `setting_a_threshold_twice_updates_it`, `the_1000th_setting_passes_and_a_new_1001st_is_409`, `an_existing_name_at_the_cap_can_still_change`, `another_projects_thresholds_do_not_apply`.
- `tests/core/migrations.rs::all_tables_exist` and `tests/core/json_bodies.rs` list the new table and route.

**Acceptance:** the services API for a seeded project gives the hand-computed RED and Apdex numbers for two services with different thresholds.

### Task 11 (NAMI-TR-11): Switch on the collector bundle's OTLP pipelines and add the SDK snippet

**Covers:** R7.01  **Depends on:** NAMI-TR-6 (and the collector bundle of Metrics Part 2)

**Files:**
- Modify: `deploy/collectors/otelcol/config.yaml` and its compose snippet (planned in NAMI-MET-22: `traces` and `logs` pipelines on, `otlphttp` pointed at `${env:NAMI_URL}`), `deploy/collectors/validate.sh` (unchanged if it already validates every rendered config), `nami-engine/tests/metrics/collector_e2e.rs` (one more ignored test), the collector setup page of NAMI-MET-26 only if its rendering needs a new placeholder (it should not), `nami-ui/src/lib/logs/logs.ts` (`otlpSnippet(url, token)`), `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/+page.svelte` (the snippet kind "OpenTelemetry SDK (OTLP)"), `nami-ui/src/lib/logs/logs.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/source-detail.test.ts`.

**Behaviour:**
1. R18. The collector's `otlphttp` exporter gets `traces_endpoint` and `logs_endpoint` on the public URL, `Authorization: Bearer <token>` and gzip; the `traces` and `logs` pipelines go from commented-out to on. Metrics keep remote-write (the spec's split).
2. `tests/metrics/collector_e2e.rs` gains `otelcol_ships_spans_and_logs_to_a_test_engine` (`#[ignore]`, the R-M2-29 setup: a test public listener, `--network host`): the rendered `otelcol/config.yaml` runs in its pinned container, a `telemetrygen` container (`ghcr.io/open-telemetry/opentelemetry-collector-contrib/telemetrygen`, pinned to the collector's version) sends 10 traces of 3 spans and 10 log records to the collector's OTLP receiver, and within 60 s `spans` holds 30 rows and `logs` 10 rows of the test source. `deploy/collectors/validate.sh` passes on the changed config.
3. `otlpSnippet` renders the three `OTEL_EXPORTER_OTLP_*` variables of R18. The token is `form?.token ?? TOKEN_PLACEHOLDER`, as the other snippets: viewers see the placeholder.

**Tests:** the extended container test (`spans_and_logs_arrive_through_the_collector`); vitest `otlpSnippet percent-encodes the space of the bearer header`, `the OTLP snippet uses the placeholder until a member reveals the token`.

**Acceptance:** `otelcol validate` and the end-to-end test pass; the source page shows the OTLP snippet.

### Task 12 (NAMI-TR-12): UI, Traces tab and trace explorer

**Covers:** R7.03  **Depends on:** NAMI-TR-8

**Files:**
- Create: `nami-ui/src/lib/traces/trace-id.ts` (+ `trace-id.test.ts`), `nami-ui/src/lib/traces/traces.ts` (+ `traces.test.ts`), `nami-ui/src/lib/server/traces/trace-proxy.ts` (+ test), `nami-ui/src/lib/components/traces/TraceList.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/+page.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/explorer.test.ts`.
- Modify: `nami-ui/src/params.ts` (`traceid: (param) => (isTraceId(param) ? param : undefined)`, imported from `./lib/traces/trace-id.ts` with its extension, as `uuid.ts` is: `vite build` loads `params.ts` with plain Node, so `trace-id.ts` imports nothing), `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` (a "Traces" tab after "Logs"), `nami-ui/src/routes/(app)/projects/[id=uuid]/section.test.ts` if it pins the tabs.

**Data shape:** `#lib/traces/trace-id.ts`: `isTraceId(value): boolean` (32 hex, any case), dependency-free. `#lib/traces/traces.ts`: `type TraceSummary` (R-TR-12 shape); `type TraceSearchState = { range: TimePreset | 'custom'; from; to; service; operation; minMs; maxMs; errorOnly: boolean; attrs: {key, op, value?}[] }`; `parseTraceSearch(params)`, `traceSearchParams(state)`, `engineSearchParams(state, window)` (anything invalid is dropped, as `parseExplorerState` does); `formatDuration(ms)` (`850 µs`, `12.3 ms`, `4.1 s`, `2 m 5 s`); `traceHref(projectId, summary)` (window `start - TRACE_SUMMARY_PAD` to `start + duration + TRACE_SUMMARY_PAD`, R-TR-13, so a trace longer than the pad is not cut). `trace-proxy.ts`: `SEARCH_PARAMS` allow-list and `pickTraceParams` (the `pickLogParams` pattern).

**Behaviour:**
1. The page load reads the URL state, calls `GET /projects/{id}/traces` through `callEngine(enginePath(...))` with only allowed parameters, and renders a filter bar (time presets and custom range, service, operation, min/max duration, "errors only", up to 10 attribute filters) and the result list: start (UTC), root service and operation, duration, span count, error badge, services; "Load more" follows `next_cursor`.
2. Each row links to the waterfall with `traceHref`. Engine 400 messages show in an `.alert-danger`; 429 shows "Too many trace queries, try again in a moment".
3. State lives in the URL (shareable, like the logs explorer); no token, no secret in it.
4. Empty state: `EmptyState` "No traces in this time range" with a link to the source setup snippet.

**Tests (vitest):** `isTraceId accepts 32 hex and refuses 31, 33, dashes and non-hex`, `the traceid matcher refuses non-trace ids`, `search state round-trips through the URL and drops invalid values`, `only allowed params reach the engine`, `formatDuration covers each unit`, `the explorer renders summaries and a load-more link`, `an engine 400 is shown as an alert`, `the project layout shows a Traces tab`.

**Acceptance:** `npm run check` (0 errors, 0 warnings), `npx vitest run` and `npm run build` pass (the build is what loads `params.ts` under plain Node); the explorer lists seeded traces with each filter.

### Task 13 (NAMI-TR-13): UI, trace waterfall, span drawer and "View logs"

**Covers:** R7.04, R7.05  **Depends on:** NAMI-TR-9, NAMI-TR-12

**Files:**
- Create: `nami-ui/src/lib/traces/waterfall.ts` (+ `waterfall.test.ts`), `nami-ui/src/lib/components/traces/Waterfall.svelte`, `nami-ui/src/lib/components/traces/SpanDrawer.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/[traceId=traceid]/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/[traceId=traceid]/+page.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/[traceId=traceid]/waterfall-page.test.ts`.

**Data shape (`#lib/traces/waterfall.ts`):** `type LaidOutSpan = Span & { depth: number; orphan: boolean; hasChildren: boolean; offsetPct: number; widthPct: number }`; `layoutSpans(spans: Span[], traceStart: string, traceEnd: string): LaidOutSpan[]` (depth-first order, children by start then span id, orphans at depth 0, a visited set against cycles, `widthPct` at least the 2 px floor applied by the component); `visibleRows(rows, collapsed: Set<string>)`; `logsHref(projectId, trace, spanId?)` (R-TR-16: `range=custom`, `from = start - 5 min`, `to = end + 5 min`, `trace_id`, optional `span_id`).

**Behaviour:**
1. The load validates `params.traceId` (matcher plus `isTraceId`), forwards only `from`/`to` from the URL, and calls `GET /projects/{id}/traces/{trace_id}`. A 404 renders `EmptyState` "No spans stored for this trace" (spec), with a link back to the explorer; it is not an error page.
2. Header: root service and operation, start (UTC), duration, span count, error count, "View logs" (`logsHref` without a span) and the total log count. A `truncated` trace shows "Showing the first 5,000 spans".
3. Waterfall (R-TR-17): a time axis with tick labels, one row per visible span (service, name, duration, the log count as "N logs" when non-zero), bars on the axis, error spans in the `danger` token plus an "error" badge, orphans marked "parent not in trace", collapse toggles with `aria-expanded`, arrow keys between rows.
4. Span drawer (modal, focus trap via `inert`, Escape closes, focus returns to the row): ids, kind, status and message, start, duration, attributes and resource attributes as key/value tables (text only), events with their times and attributes (the `exception` event's `exception.stacktrace` in a `<pre>`), `otel.links_count` and the dropped counts when present, "Logs for this span" (`logsHref` with the span id) and the span's log count.
5. Every engine string reaches the DOM as a text node (no `{@html}`); attribute values with control or bidi characters render as text with them escaped by Svelte.

**Tests (vitest):** `layout nests children under their parent in start order`, `layout survives a missing root and a parent cycle`, `bars are positioned on the trace's time axis`, `collapsing hides a subtree`, `logsHref pads the trace by five minutes and adds the span`, `a 404 shows no spans stored`, `the drawer shows attributes and events as text` (a value of `<img src=x onerror=alert(1)>` renders literally), `the drawer traps focus and returns it`, `5000 spans render` (time recorded in the PR, R-TR-17).

**Acceptance:** a seeded 3-service trace with an error span renders with nesting, highlighting, per-span log counts and working links into the logs explorer.

### Task 14 (NAMI-TR-14): UI, "View trace" in the logs explorer and trace filters there

**Covers:** R7.05  **Depends on:** NAMI-TR-13

**Files:**
- Modify: `nami-ui/src/lib/logs/logs.ts` (`ExplorerState` gains `traceId: string | null` and `spanId: string | null`, URL keys `trace_id` / `span_id`, checked as 32 / 16 hex, forwarded by `tailParams` and so by `engineParams`), `nami-ui/src/lib/logs/logs.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/+page.svelte` (the "View trace" link, the two filter chips), `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/explorer.test.ts`.

**Behaviour:**
1. Rows with a `trace_id` show "View trace", linking to `/projects/{id}/traces/{trace_id}?from=<dt - 1 h>&to=<dt + 1 h>` (R-TR-16), built from the current project's path only.
2. A URL with `trace_id` (and optionally `span_id`) filters search, histogram, export and live tail through the engine's existing `trace_id` / `span_id` filters (`logs::query::LogFilters`, merged); the filter bar shows them as removable chips ("trace 4bf92f35...", "span 00f067aa...").
3. The BFF allow-lists already pass `trace_id` and `span_id` (`FILTER_PARAMS` in `#lib/server/logs/log-proxy.ts`); nothing else changes in the BFF.

**Tests (vitest):** `trace_id and span_id round-trip through the explorer URL and invalid ones are dropped`, `the trace filters reach search, export and tail params`, `rows with a trace id link to the trace with a one-hour window`, `rows without a trace id have no link`, `removing the trace chip removes the filter`.

**Acceptance:** from a log row to its trace and from the waterfall back to exactly that trace's logs, both ways, in a seeded project.

### Task 15 (NAMI-TR-15): UI, services overview with Apdex and the threshold editor

**Covers:** R7.06  **Depends on:** NAMI-TR-10, NAMI-TR-12

**Files:**
- Create: `nami-ui/src/lib/components/traces/ServicesTable.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/services/+page.server.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/services/+page.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/services/services-page.test.ts`.
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/traces/+page.svelte` (`SegmentedTabs` link mode: "Traces" / "Services"), `nami-ui/src/lib/traces/traces.ts` (`apdexTone(apdex)`, `formatApdex`).

**Behaviour:**
1. The load calls `GET /projects/{id}/services` with the time preset; the table (`.table`) lists service, request rate, error rate, p50/p95/p99, Apdex with its threshold, and a `Sparkline` of requests. Apdex shows as a number with two decimals and a `Badge` (tone from `apdexTone`: at least 0.94 `success`, at least 0.85 `info`, at least 0.70 `warning`, below `danger`; the conventional Apdex bands) plus its label text ("Excellent", "Good", "Fair", "Poor"), so colour never carries it alone; `null` shows "No requests".
2. Each service name links to the explorer filtered by that service.
3. Members (`data.role` member or admin, NAMI-INC-7) get an inline form per row (`Input type="number" min="1" max="60000"`, "Save") whose action posts `PUT /projects/{id}/services/settings` and returns `engineFailure(...)` on error; viewers see the threshold as text. A default threshold is marked "default".
4. `truncated` shows "Showing the 200 busiest services".

**Tests (vitest):** `apdexTone covers every band and null`, `the table renders RED stats, Apdex and sparklines`, `a member sees the threshold form and a viewer does not`, `the threshold action sends the engine body and shows its 400`, `the services tab is active on the services page`.

**Acceptance:** a member changes a service's threshold and the Apdex column changes on reload; a viewer cannot (no form, and the engine's 403 if forced).

### Task 16 (NAMI-TR-16): Phase security review and fix wave

**Covers:** R7.01, R7.02, R7.03, R7.04, R7.05, R7.06  **Depends on:** NAMI-TR-1 to NAMI-TR-15

**Files:** fix commits wherever the review finds something; `docs/superpowers/decisions/<date>-traces-part1.md`; `docs/superpowers/roadmap.md` (R7.01 to R7.06 ticked); `CLAUDE.md` (current sub-project).

**Behaviour:** a whole-phase security review against the spec's tested requirements and STANDARDS 14, then fixes on the branch until a re-review is clean. Checklist:
1. Token auth on all three OTLP paths; the token never logged or echoed; 401 bodies identical for a malformed, unknown and deleted token.
2. Cross-project isolation: a trace id from another project is 404 on the trace route, absent from search, services and log counts; the "View trace" / "View logs" links carry no project id but the path's.
3. Id format validation on every route and in ingest (all-zero, wrong length, non-hex).
4. Malformed protobuf and JSON, oversized bodies, gzip bombs and decode amplification answer 4xx without panics: a fuzz pass of `decode_traces`, `decode_logs`, `decode_metrics` (protobuf and JSON) over the fixtures' mutations (truncations, bit flips, length-field inflation, deep nesting) for at least 10 minutes each, run as an ignored test (`cargo test --test traces otlp_decode:: -- --ignored`), with the counts in the PR.
5. Attribute, value, name, event and span caps hold at and past each bound (TR-4's tests), including multi-byte truncation.
6. Injection: every filter value, attribute key, service name, threshold and cursor reaches ClickHouse as a parameter (the SQL-text tests of TR-8 and TR-10 plus a review of every `Sql::push`).
7. RBAC: viewers read and cannot change thresholds (403); non-members get 403 before any ClickHouse work.
8. Resource bounds: the listener's 2 GiB argument of `src/logs/ingest/routes.rs` still holds with the OTLP paths sharing its places and decodes (restated with the 16 MiB inflate cap and the measured `traces_decode_memory` numbers); slots and read settings on every read.
9. UI: no `{@html}`; engine strings as text; the drawer's focus trap; trace ids checked before they reach an engine path.
10. Measurements: the trace lookup over a 7-day window, a 5,000-span render, and the span volume at which an unfiltered 7-day search reaches the group-by cap, recorded (R-TR-11, R-TR-12, R-TR-17).

**Acceptance:** the re-review is clean; the decisions doc records the rulings R-TR-1 to R-TR-19, the spec deviations, the measured numbers and the deferred items (R-TR-19, the trace-id index table, open questions left open); the roadmap ticks are committed; the phase PR follows CLAUDE.md (purpose and scope, task list with commit ranges, implementation summary, the local test runs because no CI is configured, breaking changes: none, risks and follow-ups) and auto-merges once review is clean.

## Open questions

1. **Two protobuf styles in the engine.** Metrics Part 2 (R-M2-1, NAMI-MET-14) decodes remote-write by hand on `prost::encoding` primitives without `derive`; this plan derives the OTLP messages and bounds memory with a pre-scan (R-TR-1, R-TR-4). Recommendation: keep both (five messages by hand there, about thirty declared here), with the `prost-derive` decision record of TR-2. If the controller wants one style, TR-2 instead hand-writes the OTLP decoders on the same primitives, charging the budget as they build (an estimated 400 more lines), and the JSON pre-scan keeps `SCHEMA`. Also: the Metrics Part 2 plan is unmerged and was written alongside this one; at phase start re-check NAMI-MET-14, NAMI-MET-22 and NAMI-MET-26 against what merged.
2. **The standalone `traces_decode_memory` binary** needs a counting global allocator, so it cannot live in the `traces` target. `scripts/check-plans.py` allows only its known standalone names (`STANDALONE_TESTS`); TR-3 adds `traces_decode_memory` there in the same commit (a one-line change to the checker). Recommendation: accept; the alternative is a unit-level estimate test without real allocation numbers, which is what R-TR-4 must not rely on.
3. **Trace lookups without a window.** R-TR-13 defaults to the last 7 days. If links from alerts or other places (Error tracking, AI SRE) later need traces older than that without a time, the trace-id index table of R-TR-11 becomes necessary. Recommendation: decide in Error tracking's plan.
4. **Delta temporality.** R-TR-10 rejects delta sums and histograms with a hint at the collector's `deltatocumulative` processor. SDKs that default to delta (for example some .NET and JavaScript configurations) will see rejected points until configured. Recommendation: keep the rejection (a wrong rate is worse than a visible rejection) and document it on the source page's OTLP snippet.
5. **Span names as `LowCardinality(String)`** follow the spec. Instrumentation that puts ids into span names would degrade it. Recommendation: keep the spec's type and record it as a risk; no cap on distinct names in this phase.
6. **Unfiltered long searches hit the group-by cap.** R-TR-12's phase 1 groups all matching spans by trace, so a busy project's unfiltered 7-day search answers 400 "too expensive" once more than a million distinct traces match. Recommendation: accept for this phase (the error tells the user to narrow the range), let TR-16 measure the threshold, and add a per-trace root-span table only if the measured threshold is low for single-org installs.
