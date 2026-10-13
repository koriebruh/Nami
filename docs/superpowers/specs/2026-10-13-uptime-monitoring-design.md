# Uptime Monitoring

## Context

Second sub-project of Nami (self-hosted BetterStack), built on the completed
Foundation (auth, sessions, two-level RBAC, projects — PRs #1-#16). Mirrors
BetterStack Uptime: monitors checked on an interval, cron-job heartbeats,
automatic incidents with email alerts, uptime percentages, and response-time
history — all scoped to a project.

## Goals

- Monitors of three types, configured per project:
  - `http` — request a URL, up if the status code is within an expected
    range (default 200-399).
  - `keyword` — request a URL, up only if the response body contains (or,
    in `not_contains` mode, does not contain) a keyword, and the status is
    within the expected range.
  - `tcp` — up if a TCP connection to `host:port` opens within the timeout.
- Per monitor: interval (>= 30 s), timeout (1-60 s), HTTP method (`GET` or
  `HEAD`), confirmation threshold (consecutive failures before an incident
  opens, default 2), paused flag.
- Heartbeats (BetterStack "cron job monitoring"): a secret ping URL; if no
  ping arrives within `period + grace`, the heartbeat goes down.
- Incidents: opened automatically when a monitor/heartbeat goes down,
  resolved automatically when it recovers. Stored in Postgres so the later
  Incident Management and Status Page sub-projects build on the same rows.
- Alerts: email (existing `Mailer`) to every member of the project on
  incident open and resolve. Routed through one function so the Incident
  Management sub-project can swap in escalation policies and other channels
  (webhook, Slack, Telegram, Discord) without touching the worker.
- Monitor detail data: uptime % for 24 h / 7 d / 30 d, average response
  time, a response-time series, recent checks, and the monitor's incidents.
- UI for all of the above, auto-refreshing like BetterStack's dashboard.

## Non-goals (deferred)

- ICMP ping (needs raw-socket privileges inside the container), DNS, SMTP,
  UDP, Playwright checks, SSL/domain-expiry checks.
- Multi-region checking — one self-hosted vantage point.
- Escalation policies, on-call schedules, acknowledgement, non-email
  channels — Incident Management sub-project.
- Public status pages — Status Page sub-project (reads this data).
- SSRF restrictions on monitor targets: this is a self-hosted tool for the
  operator's own infrastructure, which legitimately includes internal
  hosts. Targets are not restricted. Revisit before exposing to untrusted
  users.

## Architecture

### Storage split

- **Postgres**: configuration and state — `monitors`, `heartbeats`,
  `incidents`. Relational, RBAC-joined, low write volume.
- **ClickHouse**: check results — `uptime_checks`, one row per check
  (a 30 s monitor writes 2,880 rows/day). Append-only time series, queried
  with aggregates for uptime % and response-time buckets. TTL 180 days
  (status pages need 90 days of history).

### ClickHouse access

A thin client in `nami-engine/src/clickhouse.rs` over ClickHouse's HTTP
interface using the existing `reqwest` dependency — no extra driver crate,
reused later by the Logs and Metrics sub-projects:

- `execute(sql)` — DDL/statements, used by the idempotent migrator.
- `insert_rows(table, rows)` — `INSERT ... FORMAT JSONEachRow`, rows
  serialized with serde, one JSON object per line.
- `query_rows::<T>(sql, params)` — `FORMAT JSONEachRow`, deserialized with
  serde. Values are bound through ClickHouse query parameters
  (`{name:Type}` placeholders + `param_name=` URL params), never string
  interpolation.
- Requests send `date_time_input_format=best_effort` and
  `date_time_output_format=iso` so chrono `DateTime<Utc>` round-trips.
- Migrations: `CREATE DATABASE IF NOT EXISTS` + `CREATE TABLE IF NOT
  EXISTS ...` statements run at startup.
- Tests create a uniquely named database per test and drop it afterwards.

The docker-compose ClickHouse service gets `CLICKHOUSE_USER=nami`,
`CLICKHOUSE_PASSWORD=nami`, `CLICKHOUSE_DB=nami` and a data volume —
ClickHouse 26.x rejects the passwordless default user over the network.

### Background worker

In-process `tokio` task spawned from `main.rs` (disabled with
`UPTIME_WORKER_ENABLED=false`), ticking every 5 s:

1. **Claim due monitors atomically** — one statement that both selects and
   reschedules, safe across overlapping ticks or multiple engine instances:
   `UPDATE monitors SET next_check_at = now() + interval ... WHERE id IN
   (SELECT id ... WHERE NOT paused AND next_check_at <= now() ORDER BY
   next_check_at LIMIT 50 FOR UPDATE SKIP LOCKED) RETURNING *`.
2. Run the checks concurrently (bounded to 20 in flight).
3. Insert all results into ClickHouse in one batch.
4. Apply each result to the monitor's state in a Postgres transaction
   (state machine below), opening or resolving incidents.
5. Send alerts for every transition.
6. Heartbeat sweep: mark heartbeats overdue past `period + grace` as down
   (opening incidents and alerting).

### State machine (monitor)

- Success: `consecutive_failures = 0`. If status was `down`, it becomes
  `up`, the open incident is resolved, and a recovery alert is sent. If it
  was `pending`, it becomes `up` silently.
- Failure: `consecutive_failures += 1`. When it reaches the confirmation
  threshold and status is not already `down`, status becomes `down`, an
  incident opens (cause = the check's error), and a down alert is sent —
  including from `pending`, since a brand-new monitor that is down is a
  real outage.
- Heartbeats follow the same transitions: a ping on a `down` heartbeat
  resolves its incident.

## Data model

```sql
-- Postgres
create table monitors (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    check_type text not null check (check_type in ('http', 'keyword', 'tcp')),
    url text,                 -- http, keyword
    host text,                -- tcp
    port int,                 -- tcp
    http_method text not null default 'GET' check (http_method in ('GET', 'HEAD')),
    keyword text,             -- keyword
    keyword_mode text not null default 'contains'
        check (keyword_mode in ('contains', 'not_contains')),
    expected_status_min int not null default 200,
    expected_status_max int not null default 399,
    interval_seconds int not null default 60 check (interval_seconds >= 30),
    timeout_seconds int not null default 10 check (timeout_seconds between 1 and 60),
    confirmation_threshold int not null default 2 check (confirmation_threshold between 1 and 10),
    paused boolean not null default false,
    current_status text not null default 'pending'
        check (current_status in ('pending', 'up', 'down')),
    consecutive_failures int not null default 0,
    last_checked_at timestamptz,
    next_check_at timestamptz not null default now(),
    created_at timestamptz not null default now()
);

create table heartbeats (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    token text not null unique,
    period_seconds int not null check (period_seconds >= 30),
    grace_seconds int not null default 60 check (grace_seconds >= 0),
    paused boolean not null default false,
    current_status text not null default 'pending'
        check (current_status in ('pending', 'up', 'down')),
    last_ping_at timestamptz,
    created_at timestamptz not null default now()
);

create table incidents (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    monitor_id uuid references monitors(id) on delete cascade,
    heartbeat_id uuid references heartbeats(id) on delete cascade,
    title text not null,
    cause text,
    status text not null default 'open' check (status in ('open', 'resolved')),
    started_at timestamptz not null default now(),
    resolved_at timestamptz
);
```

```sql
-- ClickHouse
CREATE TABLE IF NOT EXISTS uptime_checks (
    monitor_id UUID,
    project_id UUID,
    checked_at DateTime64(3, 'UTC'),
    status Enum8('up' = 1, 'down' = 2),
    status_code Nullable(UInt16),
    response_time_ms Nullable(UInt32),
    error Nullable(String)
) ENGINE = MergeTree
ORDER BY (monitor_id, checked_at)
TTL toDateTime(checked_at) + INTERVAL 180 DAY;
```

## API (`nami-engine`)

All project-scoped routes use `require_project_role` exactly like
`/projects/{id}` (viewer to read, member to create/edit, admin to delete).
Every monitor/heartbeat lookup filters by both its id AND the path's
project id, so an id from another project is a 404 within a project the
caller can access.

- `POST|GET /projects/{id}/monitors` — create (member) / list with
  current status and 24 h uptime (viewer)
- `GET|PATCH|DELETE /projects/{id}/monitors/{monitor_id}`
- `GET /projects/{id}/monitors/{monitor_id}/checks?limit=` — recent checks
- `GET /projects/{id}/monitors/{monitor_id}/stats` — uptime 24 h/7 d/30 d,
  average response time, 24 h response-time series (48 buckets)
- `POST|GET /projects/{id}/heartbeats`,
  `GET|PATCH|DELETE /projects/{id}/heartbeats/{heartbeat_id}`
- `GET|POST /api/v1/heartbeat/{token}` — public ping (the token is the
  secret; no session). Served only by the engine's public listener
  (`PUBLIC_PORT`, default 8081), never by the private API port (`PORT`,
  which only nami-ui may reach); at most 4 pings run at once and the rest
  get `503` + `Retry-After: 1`; only 64-lowercase-hex tokens reach the
  database (amended by the Phase 1 final-review fix wave)
- `GET /projects/{id}/incidents?status=open|resolved` — list

## UI (`nami-ui`)

The project page becomes a section with sub-navigation:

- `/projects/[id]/monitors` — list (status dot, type, target, 24 h uptime,
  last checked), auto-refresh every 30 s
- `/projects/[id]/monitors/new` and `/projects/[id]/monitors/[monitorId]/edit`
- `/projects/[id]/monitors/[monitorId]` — status header, uptime tiles,
  response-time chart, recent checks, incidents, pause/resume/delete
- `/projects/[id]/heartbeats` (list + create, shows the ping URL) and
  `/projects/[id]/heartbeats/[heartbeatId]`
- `/projects/[id]/incidents` — open and resolved incidents
- `/projects/[id]/settings` — the existing member add/remove forms

## Testing

- ClickHouse client: insert/query round trip; a parameter value containing
  quotes and SQL keywords is stored and matched literally (no injection).
- Monitor CRUD + RBAC + cross-project isolation, mirroring `tests/rbac.rs`.
- Checker: http/keyword/tcp against `wiremock` and a local `TcpListener`,
  including timeout and connection-refused.
- Worker: claim semantics, every state-machine transition, exactly one
  email per transition (Mailpit), ClickHouse rows written.
- Heartbeats: ping updates state; overdue sweep opens an incident; a ping
  on a down heartbeat resolves it.
- UI: `npm run check` clean plus a live browser walkthrough against the real
  stack.
