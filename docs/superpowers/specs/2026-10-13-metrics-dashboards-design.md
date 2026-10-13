# Metrics + Dashboards

> Parity additions (2026-10-13): Prometheus scrape targets, dashboard variables, markdown text panels, dashboard template library, read-only public share links, metric/log alert improvements (no-data handling, confirmation/recovery periods, per-series incidents), collector bundle (OpenTelemetry Collector + Vector configs and compose snippets).

## Context

Sixth sub-project (roadmap §6), mirroring BetterStack metrics and
dashboards. Reuses the Logs sub-project's sources and ingest-token auth (a
source token is accepted on every telemetry endpoint; data is separated by
endpoint/table), the ClickHouse client, the worker loop, incidents and
`alerts::notify_transition`, severities and escalation policies from
Incident Management.

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

### Prometheus scrape targets (pull)

- `metric_scrape_targets` per project: name, `source_id` (the data is
  attributed to it and takes its retention), `url` (http/https),
  `interval_seconds` (15-3600, default 60), `timeout_seconds` (1-30, below
  the interval), `labels` (<= 20, added to every sample; `job` and
  `instance` default to the target name and the URL's host:port),
  optional bearer token or basic auth (encrypted, write-only, like
  monitor secrets), `enabled`. Members manage.
- The worker claims due targets (same atomic claim as monitors), GETs
  the URL with `Accept: text/plain;version=0.0.4`, parses the body with
  the push parser and inserts through the push path (validation,
  high-cardinality guard). Limits: 10 MiB response, 50,000 samples per
  scrape (more → the scrape fails), redirects not followed.
- Target health: `last_scrape_at`, `last_duration_ms`, `last_samples`,
  `last_error`, `health` (`up` | `down` | `unknown`). Every scrape also
  writes `up`, `scrape_duration_seconds` and `scrape_samples_scraped`
  series with the target's labels, as Prometheus does.
- Requests use the shared outbound HTTP client; the outbound (SSRF)
  policy defined in the Platform + hardening spec governs it once that
  lands — default allow (exporters usually sit on private networks),
  operator can restrict.

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

- `dashboards` (project, name, description, default range, `variables`)
  and `dashboard_panels` (title, `viz`: `line` | `bar` | `stat` | `text`,
  query JSON or markdown, position, width `1/3` | `1/2` | `full`).
- Variables (<= 10 per dashboard): `{name: ^[a-z][a-z0-9_]{0,31}$, label,
  metric, tag, multi, default}`, rendered as dropdowns fed by the
  tag-values discovery endpoint. A panel filter value may be `$name`;
  the engine substitutes the selected value(s) into filter values only —
  never metric names, aggregations or group-by keys — before the usual
  validation and parameter binding; "All" removes that filter.
- Text panels: markdown <= 10 KiB, rendered with raw HTML disabled, links
  limited to http/https/mailto, images not rendered.
- Template library (bundled JSON, "Create from template"): Uptime
  overview (built-in metrics), Host (node_exporter), PostgreSQL
  (postgres_exporter), Redis (redis_exporter), Nginx
  (nginx-prometheus-exporter), Docker (cAdvisor). Templates use variables
  (e.g. `instance`) and each exporter's standard metric names; a test
  instantiates every template and runs each panel query against fixture
  series from that exporter.
- Public share link: `dashboard_shares` (one active link per dashboard;
  token 32 random bytes, stored SHA-256; revoke = delete). nami-ui renders
  `/share/dashboards/[token]` read-only without a session (`noindex`).
  The engine's public endpoints return the dashboard definition and run
  only that dashboard's own panel queries, by panel id, with a range
  <= 90 days and variable values — never an arbitrary query. Rate
  limited per IP. The share dialog warns that every series those panels
  can show becomes public.
- Members create/edit/share; admins delete; viewers read.

### Metric alerts

`metric_alerts`: query (aggregation over `window_seconds`), comparison
`gt|gte|lt|lte`, threshold, interval, enabled, state, optional
`escalation_policy_id` and `severity_id`. Evaluated by the worker exactly
like log alerts; incidents get `metric_alert_id`; notifications via
escalation.

### Alert improvements (metric and log alerts)

One shared evaluator for both alert kinds:

- `no_data` (metric alerts; a log count is never missing): `zero`
  (evaluate as 0), `keep` (keep the current state; default), `alert`
  (missing data counts as breaching).
- `confirmation_seconds` / `recovery_seconds` (0-3600, default 0): the
  condition must hold continuously that long before alerting and stay
  clear that long before recovering (`pending_since` /
  `recovering_since` per series).
- `incident_mode`: `per_alert` (default; one incident while any series
  breaches) or `per_series` (series = the query's `group_by` tags, or up
  to 3 field paths for log alerts; one incident per breaching series,
  titled with its labels; at most 50 open per alert, further breaching
  series noted on the alert). Series state lives in
  `alert_series_states (alert_kind, alert_id, series_key, state,
  pending_since, recovering_since, incident_id)`.

### Collector bundle

- `deploy/collectors/` ships ready-to-use configs plus docker-compose
  snippets for each: Vector (`file` source on container log files,
  `journald`, `host_metrics`) → `/api/v1/logs` (NDJSON) and
  `/api/v1/metrics/prometheus/remote_write`; OpenTelemetry Collector
  (contrib) with `hostmetrics`, `filelog` and `otlp` receivers — metrics
  via `prometheusremotewrite` now, its `otlphttp` logs and traces
  pipelines switched on by the Traces sub-project, which adds the OTLP
  endpoints; optional node_exporter and cAdvisor services with matching
  scrape configs.
- Container logs are read from `/var/lib/docker/containers` read-only.
  Sources that need the Docker socket (Vector `docker_logs`, OTel
  `docker_stats`) are opt-in variants documented as root-equivalent (a
  `:ro` socket mount does not restrict the API).
- UI setup pages (per source; members only, since viewers never see
  tokens) render each config with the source token and Nami's public URL
  filled in, with copy and download.
- Tests: `vector validate` and `otelcol validate` (containers) pass on
  every rendered config; an end-to-end test runs Vector against a test
  engine and asserts that logs and metrics arrive.

### UI

Metrics explorer (metric picker with autocomplete, aggregation, filters,
group-by, time range, chart + legend), Dashboards (list, view with time
picker, variable dropdowns and auto-refresh, panel editor with live
preview, text panels, template gallery, share dialog with revoke),
Scrape targets (list with health, last error, create/edit), Metric alerts
(incl. no-data, confirmation/recovery and incident mode), collector setup
pages. Charts follow the `dataviz` skill (inline SVG, accessible, dark
theme).

## Security requirements (tested)

- Token auth as Logs; cross-project isolation on query/discovery.
- Injection via metric names, tag keys/values, regex patterns, group-by
  keys → rejected or literal.
- Malformed protobuf/snappy/Prometheus text → 400 without panics (fuzz
  cases); decompression-bomb guard on remote_write.
- High-cardinality guard: >10,000 new series per source per minute → 429.
- Scrape: oversized responses, > 50,000 samples and malformed exposition
  text fail the scrape cleanly (health `down`, worker keeps running);
  target credentials encrypted, never returned or logged; redirects never
  followed.
- Variables: a `$var` value containing quotes, regex syntax or SQL is
  bound literally; variables cannot reach metric names or group-by keys.
- Text panels: `<script>`, `<iframe>`, `javascript:` links and raw HTML
  render inert.
- Share links: unknown or revoked token → 404; a share token cannot run
  a query outside its dashboard's panels, read another dashboard or
  project, or write anything; per-IP rate limit → 429.
- Alert evaluator: state-machine tests for each no-data mode,
  confirmation/recovery periods and the 50-series cap.
- Collector configs: rendered tokens appear only on member-visible pages
  and downloads.

## Non-goals

StatsD/Graphite ingestion, PromQL compatibility, Grafana dashboard import,
recording rules, anomaly detection, Prometheus service discovery (static
scrape targets only), percentage-change alerts.
