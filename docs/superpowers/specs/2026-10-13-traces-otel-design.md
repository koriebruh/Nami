# Traces + OpenTelemetry

> Parity additions (2026-10-13): trace-to-log linking both ways via `trace_id` (logs explorer "View trace", waterfall "View logs"), Apdex per service with a configurable threshold.

## Context

Seventh sub-project (roadmap §7). Adds OpenTelemetry (OTLP/HTTP) ingestion
for traces, plus OTLP logs and metrics routed into the existing Logs and
Metrics tables, with source-token auth from the Logs sub-project.

## Goals

### OTLP/HTTP ingestion (source token via `Authorization: Bearer`)

- `POST /v1/traces`, `/v1/logs`, `/v1/metrics` — the standard OTLP/HTTP
  paths, both `application/x-protobuf` and `application/json` (OTLP JSON
  encoding: hex trace/span ids, camelCase), optional gzip, OTLP response
  bodies (`Export*ServiceResponse`, partial-success when some records are
  rejected).
- Uses the `opentelemetry-proto` crate's generated types (prost + serde).
- OTLP logs → `logs` table (body → message, severity → level, `traceId` /
  `spanId` → `trace_id` / `span_id`, attributes + resource attributes →
  fields), through the same per-source pipeline, pattern extraction and
  quota as HTTP logs; OTLP metrics → `metrics` table (gauge, sum,
  histogram → `_count`/`_sum`/bucket series).
- Switches on the collector bundle's OTLP pipelines (logs, traces) from
  the Metrics sub-project and extends its end-to-end test to spans.

### Span storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS spans (
    project_id UUID,
    source_id UUID,
    trace_id String,          -- 32 hex chars
    span_id String,           -- 16 hex chars
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
    events String,            -- JSON array
    retention_days UInt16,
    INDEX idx_trace trace_id TYPE bloom_filter(0.01) GRANULARITY 4
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(start_time)
ORDER BY (project_id, service_name, start_time)
TTL toDateTime(start_time) + toIntervalDay(retention_days);
```

### Query API

- `GET /projects/{id}/traces` — search by service, operation, min/max
  duration, error only, attribute filter, time range (max 7 days), keyset
  pagination → trace summaries (root span, service, duration, span count,
  error flag).
- `GET /projects/{id}/traces/{trace_id}` — all spans of the trace, project-
  bound (id must be 32 hex chars).
- `GET /projects/{id}/services` — per-service RED stats (request rate,
  error rate, p50/p95/p99 duration) and Apdex for a time range. Apdex
  uses the service's request spans (kind SERVER or CONSUMER, plus root
  spans): satisfied `d <= T`, tolerating `T < d <= 4T`, frustrated
  `d > 4T` or status error; Apdex = (satisfied + tolerating / 2) /
  total, null when there are no spans. `T` per service in
  `trace_service_settings` (default 500 ms, 1 ms-60 s; members edit).

```sql
-- Postgres
create table trace_service_settings (
    project_id uuid not null references projects(id) on delete cascade,
    service_name text not null,
    apdex_threshold_ms int not null default 500
        check (apdex_threshold_ms between 1 and 60000),
    primary key (project_id, service_name)
);
```

### Trace-to-log linking

- Logs carry `trace_id` / `span_id` (Logs spec: OTLP log records and the
  JSON keys of the same name).
- Logs explorer rows with a `trace_id` get "View trace" →
  `/projects/{id}/traces/{trace_id}` ("no spans stored" when the trace is
  unknown).
- Waterfall: "View logs" opens the explorer filtered by `trace_id` over
  the trace's time span ± 5 min; each span shows its log count (one
  query grouped by `span_id`) and "logs for this span".
- Both directions stay inside the path's project.

### UI

Traces explorer (filters + result list), trace waterfall (span bars on a
time axis, nesting by parent, error highlighting, span detail drawer with
attributes/events, "View logs" and per-span log counts), services
overview table with sparklines, Apdex and an editable threshold.

## Security requirements (tested)

- Token auth; cross-project isolation (a trace id from another project is
  404); id format validation.
- Malformed protobuf/JSON, oversized payloads, decompression bombs → 4xx
  without panics; attribute/value size caps.
- Injection via filter values/attribute keys → parameters only.
- Linking: a `trace_id` from another project yields neither spans nor
  logs; viewers cannot change Apdex thresholds (403).

## Non-goals

gRPC OTLP (HTTP only), tail sampling, service maps, profiling.
