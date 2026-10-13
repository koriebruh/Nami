# Metrics + Dashboards

## Context

Sixth sub-project (roadmap §6), mirroring BetterStack metrics and
dashboards. Reuses the Logs sub-project's sources and ingest-token auth (a
source token is accepted on every telemetry endpoint; data is separated by
endpoint/table), the ClickHouse client, the worker loop, incidents and
`alerts::notify_transition`.

## Goals

### Ingestion (source token, no session)

- `POST /api/v1/metrics` JSON: object or array of
  `{name, value, type: "gauge"|"counter", dt?, tags?: {k: v}}`.
- `POST /api/v1/metrics/prometheus` Prometheus text exposition format
  (`metric{label="v"} value [timestamp_ms]`, `# TYPE` honored).
- `POST /api/v1/metrics/prometheus/remote_write` — Prometheus remote-write
  (protobuf `WriteRequest`, snappy-compressed) so an existing Prometheus
  can forward with no exporter changes.
- Validation: metric names `^[a-zA-Z_:][a-zA-Z0-9_:.]{0,199}$`, ≤ 50 tags,
  tag keys `^[a-zA-Z_][a-zA-Z0-9_.]{0,99}$`, tag values ≤ 1 KiB, finite
  values only (NaN/Inf rejected per sample, not per request), body limits
  and per-source rate limit as in Logs.

### Storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS metrics (
    project_id UUID,
    source_id UUID,
    name LowCardinality(String),
    metric_type LowCardinality(String),
    series_id UInt64,                       -- cityHash64(name, sorted tags)
    tags Map(LowCardinality(String), String),
    dt DateTime64(3, 'UTC'),
    value Float64,
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, name, series_id, dt)
TTL toDateTime(dt) + toIntervalDay(retention_days);
```

### Built-in metrics

Virtual metrics resolved by the query engine over existing tables:
`nami.uptime.response_time_ms` / `nami.uptime.up` (tags `monitor`,
`monitor_name`) from `uptime_checks`, and `nami.logs.count` (tags `level`,
`source`) from `logs`.

### Query API

- `POST /projects/{id}/metrics/query`:
  `{metric, aggregation: avg|min|max|sum|count|p50|p95|p99|rate,
  filters: [{tag, op: eq|neq|regex, value}], group_by: [tag], from, to,
  step?}` → `{series: [{tags, points: [[t, v]]}]}`. `rate` = per-second
  increase for counters (counter resets handled). Auto step targets ~120
  points; max range 90 days; max 50 series per response (top by
  magnitude, flagged as truncated).
- Regex filters use ClickHouse `match` (RE2, linear time — no ReDoS),
  pattern ≤ 256 chars.
- Discovery: `GET .../metrics/names`, `.../metrics/tags?metric=`,
  `.../metrics/tag-values?metric=&tag=` for the editor.
- Every value bound as a ClickHouse parameter; project id from the path.

### Dashboards

- `dashboards` (project, name, description, default range) and
  `dashboard_panels` (title, `viz`: `line` | `bar` | `stat`, query JSON,
  position, width `1/3` | `1/2` | `full`).
- A "Uptime overview" dashboard template using the built-in metrics.
- Members create/edit; admins delete; viewers read.

### Metric alerts

`metric_alerts`: query (aggregation over `window_seconds`), comparison
`gt|gte|lt|lte`, threshold, interval, enabled, state. Evaluated by the
worker exactly like log alerts; incidents get `metric_alert_id`;
notifications via escalation.

### UI

Metrics explorer (metric picker with autocomplete, aggregation, filters,
group-by, time range, chart + legend), Dashboards (list, view with time
picker and auto-refresh, panel editor with live preview), Metric alerts.
Charts follow the `dataviz` skill (inline SVG, accessible, dark theme).

## Security requirements (tested)

- Token auth as Logs; cross-project isolation on query/discovery.
- Injection via metric names, tag keys/values, regex patterns, group-by
  keys → rejected or literal.
- Malformed protobuf/snappy/Prometheus text → 400 without panics (fuzz
  cases); decompression-bomb guard on remote_write.
- High-cardinality guard: >10,000 new series per source per minute → 429.

## Non-goals

StatsD/Graphite ingestion, PromQL compatibility, recording rules, anomaly
detection.
