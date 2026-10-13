# Traces + OpenTelemetry

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
- OTLP logs → `logs` table (body → message, severity → level, attributes +
  resource attributes → fields); OTLP metrics → `metrics` table (gauge,
  sum, histogram → `_count`/`_sum`/bucket series).

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
  error rate, p50/p95/p99 duration) for a time range.

### UI

Traces explorer (filters + result list), trace waterfall (span bars on a
time axis, nesting by parent, error highlighting, span detail drawer with
attributes/events), services overview table with sparklines.

## Security requirements (tested)

- Token auth; cross-project isolation (a trace id from another project is
  404); id format validation.
- Malformed protobuf/JSON, oversized payloads, decompression bombs → 4xx
  without panics; attribute/value size caps.
- Injection via filter values/attribute keys → parameters only.

## Non-goals

gRPC OTLP (HTTP only), tail sampling, service maps, profiling.
