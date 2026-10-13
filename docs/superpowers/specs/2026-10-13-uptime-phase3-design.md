# Uptime Monitoring — Phase 3 (BetterStack parity)

## Context

Phases 1 and 2 (`2026-10-13-uptime-monitoring-design.md`,
`2026-10-13-uptime-phase2-design.md`) ship every monitor type, heartbeats,
incidents, email alerts, request configuration, maintenance windows and
groups. This phase closes the uptime gaps found in the 2026-10-13 parity
review (roadmap §2, Phase 3). It starts after Phase 2 merges and reuses
its `crypto.rs` (AES-GCM, `NAMI_ENCRYPTION_KEY`), monitor groups,
maintenance windows and request-config validation.

## Goals

### 1. Heartbeat failure reporting

- `GET|POST /api/v1/heartbeat/{token}/fail` reports a failure at once.
- `GET|POST /api/v1/heartbeat/{token}/{exit_code}`: a 32-bit integer path
  segment; `0` is a success ping, anything else a failure (non-integer →
  400). Example: `out=$(job 2>&1); curl -fsS -m 10 --data-binary "$out"
  https://nami.example/api/v1/heartbeat/<token>/$?`.
- A failure ping moves the heartbeat to `down` immediately (heartbeats
  have no confirmation threshold) and opens an incident with cause
  `Heartbeat reported failure (exit code N)`; the next success ping
  recovers it and resolves the incident. Maintenance windows and pause
  suppress transitions exactly as for missed pings. Every ping, success
  or failure, updates `last_ping_at`.
- Per-token rate limit on all ping routes: 1 ping/s, burst 10 → 429.

### 2. Heartbeat output capture

- Any ping may carry a body (script output). The first 10 KiB are kept
  (`…[truncated]` marker beyond); bodies over 1 MiB → 413.
- Stored as text: invalid UTF-8 replaced with U+FFFD, then C0/C1 control
  characters (except `\n` and `\t`), DEL and bidi controls (U+202A-U+202E,
  U+2066-U+2069) removed, so ANSI escape sequences lose their ESC byte and
  show as plain text.
- Pings are recorded in ClickHouse `heartbeat_pings` (180-day TTL, like
  `uptime_checks`); `GET /projects/{id}/heartbeats/{heartbeat_id}/pings
  ?limit=` (viewer, max 100) lists time, kind, exit code and output.
- The output of the ping that opens an incident is attached to it
  (`incidents.details`); the incident page, Incident Management's
  timeline and every notification channel show it escaped, never as
  markup.

### 3. Heartbeat groups

`heartbeats.group_id` references `monitor_groups` (`on delete set null`):
one group list per project for monitors and heartbeats. The heartbeat list
renders groups like the monitor list; the heartbeat form gets the group
picker; group CRUD stays in project settings.

### 4. Monitor bulk import

`POST /projects/{id}/monitors/import` (member), body <= 2 MiB:
`{format: "urls" | "uptime_kuma" | "uptimerobot", content, defaults?,
dry_run?, skip_existing? (default true)}`.

- `urls`: one URL per line (blank lines and `#` comments ignored) →
  `http` monitors named after the host, with `defaults` (interval,
  timeout, thresholds, group) applied.
- `uptime_kuma`: Uptime Kuma 1.x backup JSON (`monitorList`). Types
  `http`, `keyword` (`invertKeyword` → `not_contains`), `port` → `tcp`,
  `ping` and `dns` are mapped; other types → per-entry error. Mapped
  fields: name, URL or hostname/port, method, body, headers, basic auth,
  accepted status codes (first range), interval (raised to >= 30 s),
  `maxretries + 1` → confirmation threshold (max 10), `ignoreTls` →
  `verify_ssl = false`, `maxredirects: 0` → no redirects, `active: false`
  → paused, DNS record type and resolver. `notificationList` and every
  other section are ignored, never stored or echoed.
- `uptimerobot`: UptimeRobot monitor CSV export with a header row,
  columns matched by name case-insensitively (the exact header mapping is
  pinned against a real export fixture in the plan). Types HTTP(s),
  Keyword, Ping and Port map to `http`, `keyword`, `ping`, `tcp`.
- Max 500 entries per request (more → 400 before anything is created).
  Each entry is validated exactly like a single create and created in its
  own transaction. The response lists every entry: `{line, status:
  created | skipped | error, monitor_id?, error?}`. `skip_existing` skips
  URLs/hosts already monitored in the project or repeated in the input;
  `dry_run` validates and returns the same list without creating.
- Imported secrets (headers, basic auth) are encrypted on create like any
  monitor's; result rows never include them.

### 5. Advanced request options

- `ip_version` (`any` default | `ipv4` | `ipv6`) for http, keyword, tcp,
  ping and ssl_expiry monitors: only A or AAAA records are used; no
  address of that family → down with `no IPv4 address` / `no IPv6
  address` (no fallback).
- `remember_cookies` (http/keyword, default false): cookies set by
  responses are sent on later redirect hops of the same check. The jar
  lives for one check execution and is never persisted or shared.
- HTTP(S) proxy (http/keyword): `proxy_url` (`http://` or `https://`
  host:port; userinfo in it → 400), `proxy_username`, `proxy_password` —
  the password AES-GCM encrypted and write-only (`has_proxy_password`),
  same PATCH rules as the basic-auth password. Check errors say whether
  the proxy or the target failed.

### 6. Availability (SLA) report and check export

- `availability(resource, from, to)` — one function, reused by Incident
  Management's SLA report:
  - total = `[from, to)` clipped to `[created_at, now)`;
  - maintenance = time covered by the resource's maintenance windows
    (one-off and recurring, as defined in Phase 2);
  - downtime = union of the resource's incident intervals (`started_at`
    to `resolved_at`, or now) clipped to the range, minus maintenance;
  - uptime % = `1 - downtime / (total - maintenance)`, null when that
    denominator is zero.
  Incident time is confirmed downtime, so it can differ from the
  check-ratio tiles (24 h / 7 d / 30 d), which stay as they are.
- `GET /projects/{id}/monitors/{monitor_id}/availability?from&to` and
  the heartbeat equivalent (viewer) → `{uptime_pct, downtime_seconds,
  maintenance_seconds, incidents, longest_incident_seconds}`; range
  <= 3 years, `from < to`, else 400.
- `GET /projects/{id}/monitors/{monitor_id}/checks/export?from&to`
  (viewer; range <= 180 days, the check TTL) streams CSV `checked_at,
  status, status_code, response_time_ms, error` (RFC 4180; a cell starting
  with `=`, `+`, `-`, `@`, tab or CR gets a leading `'`).

### UI

- Heartbeat page: ping URL with `/fail` and exit-code examples, recent
  pings with expandable output (escaped), group picker; grouped heartbeat
  list.
- `/projects/[id]/monitors/import`: format picker, paste or file upload,
  defaults, dry-run preview table, import with per-entry results.
- Monitor form "Advanced" section: IP version, remember cookies, proxy.
- Monitor and heartbeat detail: Availability card (last month, last
  quarter, year to date, custom range) and "Export checks (CSV)" on
  monitors.

## Data model changes

```sql
alter table heartbeats
    add column group_id uuid references monitor_groups(id) on delete set null;

alter table monitors
    add column ip_version text not null default 'any'
        check (ip_version in ('any', 'ipv4', 'ipv6')),
    add column remember_cookies boolean not null default false,
    add column proxy_url text,
    add column proxy_username text,
    add column proxy_password_enc bytea;

alter table incidents add column details text;  -- heartbeat output, <= 10 KiB
```

```sql
-- ClickHouse
CREATE TABLE IF NOT EXISTS heartbeat_pings (
    heartbeat_id UUID,
    project_id UUID,
    received_at DateTime64(3, 'UTC'),
    kind Enum8('success' = 1, 'fail' = 2),
    exit_code Nullable(Int32),
    output String
) ENGINE = MergeTree
ORDER BY (heartbeat_id, received_at)
TTL toDateTime(received_at) + INTERVAL 180 DAY;
```

## Security requirements (tested)

- `/fail` and exit-code routes: unknown token → 404; tokens redacted from
  request logs; rate limit → 429.
- Output: NUL, ESC sequences, `\r`, bidi controls and invalid UTF-8 are
  neutralized before storage; `<script>`/HTML output renders as text in
  the UI and in alert emails (Incident Management's channel formatters
  then carry it escaped as well); > 10 KiB truncated; > 1 MiB → 413.
- Import: viewer → 403; > 500 entries → 400 and > 2 MiB → 413, with
  nothing created; malformed JSON/CSV → 400 or per-entry errors, no
  panic; every entry passes header-injection and URL validation; secrets
  encrypted and never echoed; Kuma notification secrets never stored; a
  `group_id` from another project → 404.
- Proxy: only ciphertext of the password in the DB (test reads the row),
  absent from responses, logs and alerts; userinfo or non-http(s) proxy
  URLs → 400.
- Cookie jar isolation: a cookie set during one check is never sent by
  the next check or by another monitor.
- `ip_version`: an IPv6-only target with `ipv4` reports down (no silent
  fallback).
- CSV export: formula cells prefixed; cross-project ids → 404; range
  limits → 400.

## Non-goals

- Request phase timing (DNS / connect / TLS / first byte): reqwest and
  hyper expose no per-phase timings, so it needs a custom connector stack
  timing each phase, and Better Stack's version is unconfirmed (parity
  review). Revisit if the checker moves to its own HTTP client.
- Traceroute / MTR: needs raw sockets (`CAP_NET_RAW`, a privileged
  container) beyond the unprivileged ICMP datagram socket used by `ping`,
  and from a single vantage point the path data adds little.
- Pingdom import, SOCKS proxies, timezone-aware cron schedules for
  heartbeats, Playwright, multi-region, screenshots.
