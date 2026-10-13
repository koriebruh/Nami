# Log Management

## Context

Third sub-project of Nami (roadmap §3), mirroring BetterStack Logs. Builds
on Foundation (auth/RBAC/projects), Uptime Phase 1 (ClickHouse client,
incidents, `alerts::notify_transition`, worker loop) and Uptime Phase 2
(`crypto.rs` AES-GCM with `NAMI_ENCRYPTION_KEY`). Starts after both uptime
phases merge.

## Goals

### Sources

- Per project: `name`, `platform` label (`http`, `syslog`, `vector`,
  `docker`, `other`), `retention_days` (1-365, default 30).
- Each source has an ingest token: 32 random bytes, base64url. Stored as
  SHA-256 (lookup) + AES-GCM ciphertext (so the UI can keep showing it, as
  BetterStack does). Rotate = new token, old one stops working
  immediately. Viewers can see a source but not its token; members and
  admins can.
- CRUD: member creates/edits/rotates, admin deletes (deleting a source
  stops ingestion; its rows age out by TTL).

### HTTP ingestion

- `POST /api/v1/logs`, `Authorization: Bearer <source token>`, no session.
- Body: a JSON object, a JSON array of objects, or NDJSON
  (`application/x-ndjson`); optional `Content-Encoding: gzip`.
- Field mapping per event: timestamp from `dt` | `timestamp` | `time`
  (RFC 3339, or unix seconds / milliseconds) else receive time; message
  from `message` | `msg` | `log`; level from `level` | `severity` | `lvl`,
  normalized lowercase (unknown → `info`); every other key goes into
  `fields` (JSON object, kept as-is).
- Limits: 10 MiB compressed body, 50 MiB decompressed (gzip-bomb guard →
  413), 10,000 events per request (→ 413), message truncated at 64 KiB
  with a `…[truncated]` marker, `fields` capped at 256 KiB per event.
- Per-source rate limit (in-memory token bucket, keyed by source id):
  50 requests/s, burst 100 → 429 with the service's JSON error shape.
- Response `202 {"accepted": N}`. Inserts use ClickHouse
  `async_insert=1, wait_for_async_insert=1` so the server batches small
  inserts and 202 still means durable.

### Syslog ingestion

- RFC 5424 over TCP (newline or octet-counting framing) and UDP on
  configurable ports (default 5140 TCP/UDP; `SYSLOG_ENABLED`).
- Auth: structured-data element `[nami token="<source token>"]`; frames
  without a valid token are dropped and counted (log line, no crash).
- Mapping: severity → level, `HOSTNAME`/`APP-NAME`/`PROCID`/`MSGID` and
  other SD params → `fields`.
- Malformed frames never crash the listener; TCP connections idle > 5 min
  are closed; max frame 64 KiB.

### Storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS logs (
    id UUID,                          -- uuid v7, time-ordered tiebreaker
    project_id UUID,
    source_id UUID,
    dt DateTime64(3, 'UTC'),
    level LowCardinality(String),
    message String,
    fields String,                    -- JSON object
    retention_days UInt16,
    INDEX idx_message message TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 4,
    INDEX idx_level level TYPE set(16) GRANULARITY 4
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, dt, id)
TTL toDateTime(dt) + toIntervalDay(retention_days);
```

Retention is per row (set from the source at ingest); changing a source's
retention affects new rows only (documented in the UI).

### Search

`GET /projects/{id}/logs` with:

- `from`, `to` (required window, default last hour, max 31 days → 400),
- `source_id` (repeatable), `level` (repeatable),
- `q` — case-insensitive substring over `message` (and `fields` when
  `in_fields=true`),
- `filter` (repeatable JSON `{"path":"a.b","op":"eq|neq|contains|exists",
  "value":"…"}`; max 10 filters; path segments match
  `^[A-Za-z0-9_-]{1,64}$`, max depth 5),
- `limit` (default 100, max 500), `cursor` (opaque `(dt, id)` keyset
  cursor, newest first).

The project id is always bound from the path. Every value — including
JSON path segments — is a ClickHouse query parameter; the SQL text is
assembled only from fixed fragments with numbered placeholders.

Also `GET /projects/{id}/logs/histogram` (same filters) → counts per
bucket (auto bucket size targeting ~60 buckets) for the explorer's volume
chart.

### Live tail

`GET /projects/{id}/logs/tail` (same filters minus window/cursor) →
Server-Sent Events. The engine polls ClickHouse every 1 s for rows newer
than the last `(dt, id)` sent, emits them as `event: log`, sends a comment
keep-alive every 15 s, re-checks the session every 60 s (ends the stream
if the session became invalid), and closes after 30 min (client
reconnects). nami-ui proxies the stream through a `+server.ts` endpoint
without buffering.

### Saved views

Postgres `log_views` (project, name, query JSON with the search
parameters, created_by). Members create/edit/delete; viewers read.

### Log alerts

Postgres `log_alerts`: project, name, query JSON (same shape as a view),
`threshold` (count), `comparison` (`gt` | `lt`), `window_seconds`
(60-86400), `interval_seconds` (>= 60), `enabled`, `state`
(`ok` | `alerting`), `last_evaluated_at`, `next_evaluation_at`.
The worker evaluates due alerts (atomic claim, same pattern as monitors):
condition met while `ok` → `alerting`, opens an incident
(`incidents.log_alert_id`), notifies through `alerts::notify_transition`;
condition clears while `alerting` → `ok`, resolves the incident.

### UI

Logs section in the project nav: Sources (list, create, token reveal +
copy, rotate, delete, setup snippets for curl / Vector / syslog), Explorer
(search bar, level chips, source picker, time-range presets 15 m / 1 h /
6 h / 24 h / 7 d + custom, volume histogram, results table with level
colors and expandable pretty-printed fields, load more), Live tail toggle,
Saved views, Alerts (list, create/edit with a live preview count).

## Security requirements (tested)

- Missing/invalid/rotated/deleted-source token → 401; tokens never logged.
- Search, histogram and tail are always confined to the path's project;
  a `source_id` from another project yields nothing (never other
  projects' rows).
- Injection: `q`, filter values and paths containing quotes, backslashes,
  `'; DROP TABLE logs; --`, ClickHouse function syntax, and newlines are
  matched literally or rejected — never executed.
- Oversized body, gzip bomb, too many events → 413; rate limit → 429.
- Syslog fuzz: truncated frames, huge octet counts, invalid UTF-8, missing
  SD — listener survives and keeps serving.
- SSE endpoint requires a session and stops when it is invalidated.

## Non-goals

SQL query console, log-to-metrics transforms, anomaly detection, archive to
S3, Fluentd/Logstash-specific endpoints (the HTTP endpoint covers them).
