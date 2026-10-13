# Uptime Monitoring — Phase 2 (BetterStack parity)

## Context

Phase 1 (`2026-10-13-uptime-monitoring-design.md`, plan NAMI-UP-1..8) ships
http/keyword/tcp monitors, heartbeats, incidents, email alerts and the UI.
This phase closes the remaining uptime items in `docs/superpowers/roadmap.md`
§2. It starts only after Phase 1 is fully merged and builds on its code.

## Goals

### 1. Request configuration (http and keyword monitors)

- Methods: `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`
  (keyword monitors still forbid `HEAD`). A request body (max 64 KiB) is
  allowed only for `POST`/`PUT`/`PATCH`.
- Custom request headers: up to 20; names must be RFC 7230 tokens, values
  max 1024 bytes, no CR/LF anywhere (header injection), and `Host`,
  `Content-Length`, `Transfer-Encoding`, `Connection` are rejected.
- Basic auth: username + password.
- `follow_redirects` (default true, max 10 hops) and `verify_ssl`
  (default true).
- Secrets at rest: header values and the basic-auth password are encrypted
  with AES-256-GCM (random 96-bit nonce per value) using
  `NAMI_ENCRYPTION_KEY` (32 bytes, base64; the engine refuses to start
  without it outside tests). The API never returns secret values:
  responses carry header names with `"value_set": true` and
  `has_auth_password`. Secrets are write-only — PATCH replaces them when
  provided and keeps them when omitted; an explicit `null` clears them.

### 2. More monitor types

- `ping` — ICMP echo via an unprivileged datagram ICMP socket (no root).
  Up when an echo reply arrives within the timeout; response time = RTT.
  Docker deployments set `net.ipv4.ping_group_range` (documented).
- `dns` — resolve `hostname` for a record type (`A`, `AAAA`, `CNAME`,
  `MX`, `TXT`, `NS`), optionally against a specific nameserver; optional
  `expected_value` that at least one answer must contain. Down on
  NXDOMAIN, SERVFAIL, timeout, empty answer, or expected value missing.
- `ssl_expiry` — TLS handshake to `host:port` (default 443) with SNI;
  down when the leaf certificate is invalid/untrusted or expires within
  `expiry_threshold_days` (default 14). The error states days left.
- `domain_expiry` — RDAP lookup (`https://rdap.org/domain/<domain>`,
  following its redirect to the registry); down when the `expiration`
  event is within `expiry_threshold_days` (default 30) or RDAP fails
  repeatedly (normal confirmation threshold applies).
- `ssl_expiry` and `domain_expiry` intervals: minimum 3600 s, default
  86400 s.

### 3. Recovery threshold

`recovery_threshold` (1-10, default 1): consecutive successes required
before a `down` monitor recovers and its incident resolves (BetterStack
"recovery period").

### 4. Maintenance windows

- Per project, optionally scoped to one monitor (null = every monitor and
  heartbeat in the project); `starts_at`, `ends_at`, `title`.
- While a window covers a monitor/heartbeat: checks still run and are
  recorded, but no incident opens, no incident resolves, no alert is sent,
  and counters do not advance toward a transition. An incident already
  open when the window starts stays open (and can resolve after the window).
- Reads expose `in_maintenance: true`; the UI shows a "maintenance" badge.
- CRUD API (member creates/edits, admin deletes) + UI list/create.

### 5. Monitor groups

- `monitor_groups` per project (name, position); `monitors.group_id`
  nullable (`on delete set null`).
- List endpoint returns monitors grouped; UI shows collapsible groups and
  a group picker in the monitor form; groups CRUD in project settings.

## Non-goals

Playwright, multi-region, screenshots.

## Data model changes

```sql
create table monitor_groups (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    position int not null default 0,
    created_at timestamptz not null default now()
);

alter table monitors
    -- widen the check_type / http_method CHECK constraints (names come
    -- from the Phase 1 migration; the plan resolves them)
    add column request_body text,
    add column request_headers_enc bytea,   -- AES-GCM(json [{name,value}])
    add column auth_username text,
    add column auth_password_enc bytea,
    add column follow_redirects boolean not null default true,
    add column verify_ssl boolean not null default true,
    add column dns_record_type text,
    add column dns_nameserver text,
    add column expected_value text,
    add column expiry_threshold_days int,
    add column recovery_threshold int not null default 1
        check (recovery_threshold between 1 and 10),
    add column consecutive_successes int not null default 0,
    add column group_id uuid references monitor_groups(id) on delete set null;

create table maintenance_windows (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    monitor_id uuid references monitors(id) on delete cascade,
    title text not null,
    starts_at timestamptz not null,
    ends_at timestamptz not null check (ends_at > starts_at),
    created_at timestamptz not null default now()
);
create index on maintenance_windows (project_id, ends_at);
```

## Security requirements (tested)

- Header injection: CR/LF or forbidden header names → 400, nothing stored.
- Secrets never appear in any API response, log line, or alert email;
  the DB column holds ciphertext (test reads the row and asserts the
  plaintext is absent).
- Tampered ciphertext fails decryption cleanly (check reports down with a
  generic config error, worker keeps running).
- Missing `NAMI_ENCRYPTION_KEY` outside tests → startup error.
- Maintenance window ids/monitor ids from another project → 404.

## Testing

Per-type checker tests (wiremock for http config; a local DNS stub or
hickory test resolver; a self-signed TLS server with a short-lived cert for
`ssl_expiry`; wiremock RDAP for `domain_expiry`; ping against 127.0.0.1),
state-machine tests for recovery threshold and maintenance suppression,
crypto round-trip/tamper tests, API redaction tests, and the UI verified
live in a browser.

## Amendments (2026-10-13)

Controller rulings made while planning (the user asked for full BetterStack
parity). They override the sections above where they differ; the plan is
`docs/superpowers/plans/2026-10-13-uptime-phase2.md`.

- **A. Mail and UDP checks.** New types `smtp`, `pop3`, `imap`: TCP connect
  (implicit TLS when `use_tls`; ports default to 25/110/143, or 465/995/993
  with TLS), read the greeting within the timeout; up when it starts with
  `220` / `+OK` / `* OK` or `* PREAUTH`. New type `udp`: send the required
  `udp_payload`, wait for a datagram within the timeout; up when one arrives
  and, if `keyword` is set, it contains it. New columns `use_tls boolean not
  null default false` and `udp_payload text`.
- **B. Weekly maintenance windows.** A window's `kind` is `once`
  (`starts_at`/`ends_at`) or `weekly`: `days_of_week` (ISO 1-7), local
  `start_time`/`end_time` that may wrap past midnight, and an IANA `timezone`
  validated with chrono-tz. Coverage is a pure function of wall-clock time
  (DST-aware) with unit tests for DST, wrap and day boundaries. Schema: `kind`,
  `days_of_week smallint[]`, `start_time time`, `end_time time`, `timezone
  text`; `starts_at`/`ends_at` become nullable and a check constraint ties
  each kind to its fields.
- **C. Groups in the API.** `GET /projects/{id}/monitors` keeps returning a
  flat array (each monitor carries `group_id`); groups come from
  `GET /projects/{id}/monitor-groups`, ordered by position then name, and the
  UI groups monitors client-side.
- **D. SSRF** stays a non-goal for monitor targets, as in Phase 1: Nami is
  self-hosted and internal hosts are legitimate targets.
