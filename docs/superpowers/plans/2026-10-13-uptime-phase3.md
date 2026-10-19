# Uptime Monitoring — Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the last BetterStack uptime gaps: heartbeat failure pings (`/fail`, exit codes) with captured, sanitized job output attached to incidents; heartbeat groups shared with monitor groups; bulk monitor import (URL list, Uptime Kuma backup JSON, UptimeRobot CSV); per-monitor IP version, a per-check cookie jar and an HTTP(S) proxy with encrypted credentials; an availability (SLA) report over any range; a CSV export of check history; and the nami-ui screens for all of it.

**Architecture:** The public ping endpoint learns a second path segment (`fail` or an exit code), reads a body of at most 1 MiB, rate-limits per heartbeat (token bucket keyed by heartbeat id, looked up before any lock) and hands a `Ping { kind, exit_code, output }` to a new `uptime::heartbeat::record_signal`, which flips the state machine and attaches the sanitized output to the incident it opens. Every ping is appended to a new ClickHouse table `heartbeat_pings`. Heartbeats get the same composite `(group_id, project_id)` foreign key monitors have. The checker's `HttpClients` cache gains an `IpVersion` dimension (the `GuardedResolver` drops addresses of the other family and says `no IPv4 address` / `no IPv6 address`); monitors with a proxy or `remember_cookies` get a client of their own for that one check, built by the same `build_client` (still `.no_proxy()` first, the guarded resolver also resolving the proxy host). A pure `uptime::import` module turns the three formats into create-request JSON objects that go through the exact single-create path. `uptime::availability` computes downtime as the union of incident intervals minus maintenance spans produced by a new `Schedule::spans`. The CSV export streams day-sized ClickHouse slices through an mpsc channel into the response body.

**Tech Stack:** Rust 2024 (Axum 0.8.9, SQLx 0.9 runtime queries, reqwest 0.13.5 — this plan enables its `cookies` feature, tokio, tokio-stream), new crate `csv` 1.3 (RFC 4180 reader and writer), Postgres 17, ClickHouse 26.9, Mailpit, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-uptime-phase3-design.md`. Context: Phase 2 spec `docs/superpowers/specs/2026-10-13-uptime-phase2-design.md` and plan `docs/superpowers/plans/2026-10-13-uptime-phase2.md` (whose conventions this plan keeps). Roadmap checklist: `docs/superpowers/roadmap.md` §2 "Phase 3".

**When to start:** after NAMI-UP-19 (merged as `6e78618`). The Phase 2 final security review + fix wave runs before or alongside this plan. It may touch the same files; every task below lists its touch points under **Files** and the exact functions it changes under **Interfaces**, so a rebase onto the fix wave is a matter of re-applying small, named hunks. Implementers adapt such drift minimally (same behaviour, same tests) and say so in the PR; they never redesign around it.

**Verification status (2026-10-19):** written against `main` at `6e78618` by reading the merged modules named in each task. Unlike the Phase 2 plan, the code below has **not** been applied to a scratch copy and run. Treat signatures and test assertions as binding, bodies as guidance; run every step.

**Execution:** per CLAUDE.md — one worktree per task under `.worktrees/NAMI-UP-<n>/`, branch from the latest `origin/main` (name under each task heading), Conventional Commits ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, task review, PR (stating "No CI is configured on this repo yet" and the local test commands run), auto-merge on a clean review (pre-authorized), delete branch + worktree. Tasks are sequential unless a task says otherwise: each consumes the interfaces of the ones before it. NAMI-UP-25 (pure parsers) and NAMI-UP-27 (availability) have no dependency on NAMI-UP-23/24 and may run in parallel with them if the controller wants.

**Migrations:** Postgres versions used so far: `0001`-`0019` (uptime range `0012`-`0029`, Phase 1 hardening took `0019`) and `0040`-`0041` (logs). This plan uses **`0022`, `0023`, `0024`** and deliberately leaves **`0020`-`0021` free for the Phase 2 final fix wave**, so the two streams never collide on a number. If the fix wave needs more than two, it takes `0025`+ and this plan's numbers stay. ClickHouse DDL is not numbered: it is appended to `clickhouse::MIGRATIONS` (NAMI-UP-21 makes it 3 entries).

## Global Constraints

- Containers only (`nami-engine/docker-compose.yaml`); Postgres on host port **5433**. Engine tests: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <binary>`; full suite `cargo test` (binaries run one after another). Never restart Docker from a task; `docker compose up -d postgres clickhouse mailpit` only if they are down.
- SQLx runtime queries only, SQL as `&'static str` (`concat!` allowed, never `format!` SQL). ClickHouse values always bound parameters (`{name:Type}`).
- axum 0.8 `{param}` paths. Authorization failure 403, never 404; an id from another project inside an accessible project path is 404 (spec: "a `group_id` from another project → 404", "cross-project ids → 404").
- RBAC: viewer reads (pings list, availability, CSV export), member creates/edits (import, heartbeat group, proxy settings), admin deletes.
- Every API write stays JSON-only (`router::require_json_body`, 415 otherwise). The public listener keeps no JSON guard: ping bodies are raw bytes (`curl --data-binary`).
- Pings (spec §1-§2): `GET|POST /api/v1/heartbeat/{token}`, `/{token}/fail`, `/{token}/{exit_code}` (`i32`; `0` = success; non-integer → 400); unknown token → 404; token must stay the strict 64-lowercase-hex shape (`is_well_formed_token`) checked before any database work; per-heartbeat rate limit 1 ping/s, burst 10 → 429 with `Retry-After: 1`; body > 1 MiB → 413; `Cache-Control: no-store` on every reply; the `MAX_PINGS_IN_FLIGHT` load shedding (503) stays. Tokens never appear in any log line.
- Output (spec §2): first **10 KiB** (10240 bytes) kept, `…[truncated]` marker beyond; invalid UTF-8 → U+FFFD; C0/C1 controls except `\n` and `\t`, DEL, and bidi controls (U+202A-U+202E, U+2066-U+2069; this plan also strips U+061C, U+200E, U+200F) removed. `heartbeat_pings` TTL 180 days. Pings list `?limit=` default 50, clamped to 1..=100.
- Import (spec §4): body ≤ **2 MiB** → else 413; ≤ **500** entries → else 400 before anything is created; each entry validated exactly like `POST /projects/{id}/monitors` and created in its own transaction; `skip_existing` default true; `dry_run` creates nothing; secrets sealed like any create and never echoed; Kuma `notificationList` (and every other section) never stored or echoed.
- Advanced options (spec §5): `ip_version` `any` (default) | `ipv4` | `ipv6` for http, keyword, tcp, ping, ssl_expiry, no fallback; `remember_cookies` (http/keyword, default false), jar per check execution; proxy (http/keyword) `proxy_url` `http://` or `https://` host:port, userinfo → 400, `proxy_password` AES-GCM sealed with AAD `monitors.proxy_password:{id}`, write-only (`has_proxy_password`), PATCH: omitted keeps, value replaces, `null` clears.
- Outbound hardening that must survive every change: `.no_proxy()` on every check client (an explicit per-monitor proxy is added after it, never from the environment); every connection — the proxy's included — resolves through `GuardedResolver` / `allowed_addresses` (link-local, `0.0.0.0/8`, `::`, `fd00:ec2::254`, mapped/NAT64 forms refused); IP literals checked with `names_forbidden_address` (the proxy URL's host included); every check error passes `clean_error`.
- Availability (spec §6): range `from < to`, ≤ 3 years (this plan: 1096 days) else 400; total = `[from, to)` ∩ `[created_at, now)`; downtime = union of incident intervals ∩ range − maintenance; `uptime_pct` null when `total − maintenance` is 0.
- CSV export (spec §6): range ≤ 180 days else 400; columns `checked_at,status,status_code,response_time_ms,error`; RFC 4180; a cell starting with `=`, `+`, `-`, `@`, tab or CR gets a leading `'`.
- nami-ui: every engine call through `callEngine` with `enginePath`; `[id=uuid]` matchers; `engineFailure` for action failures; two-step (confirm) destructive actions; `cache-control: private, no-store` on pages that carry tokens or secrets; dark theme classes copied from neighbouring pages; times in UTC; inputs follow the `defaultValue` convention pinned by `form-inputs.test.ts`.
- UI checks per task: `cd nami-ui && npm run check` (0 errors, 0 warnings) and `npm test` green.

## Review Focus

- A cron job that keeps failing during a maintenance window, or while its heartbeat is paused, must not open an incident or send an alert — and must not leave a stale "down" when the window ends → NAMI-UP-20 (`fail_pings_during_maintenance_or_pause_change_nothing_but_last_ping_at`).
- Job output that uses a carriage return or ANSI codes to overwrite or hide text (`"rm -rf done\rall good"`, `"\x1b[8mhidden"`) must show every byte that was sent, as plain text, in the UI and in the alert email → NAMI-UP-21 (`carriage_returns_and_escape_sequences_cannot_hide_text`).
- Importing the same file twice (a retry after a timeout, a double click) must not create every monitor twice → NAMI-UP-26 (`importing_the_same_list_twice_creates_nothing_the_second_time`).
- Renaming or regrouping a monitor that has a proxy must keep the stored proxy password byte for byte, exactly like the basic-auth password → NAMI-UP-23 (`patch_keeps_omitted_proxy_password_replaces_given_and_null_clears`).
- An availability range in which an incident started before `from`, is still open, and overlaps a maintenance window must count each second at most once and never report more than 100 % or less than 0 % → NAMI-UP-27 (`overlapping_incidents_and_maintenance_count_each_second_once`).

## Decisions the spec leaves open (rulings made by this plan)

- **Path order on pings:** token shape (404) → suffix parse (400) → body size (413) → token lookup (404) → rate limit (429) → load-shedding slot (503) → record. A malformed suffix with a bogus token is a 400; that reveals nothing.
- **Rate-limit key is the heartbeat id**, taken from the one indexed lookup the endpoint already did for unknown tokens: random tokens cost no bucket, so the map is bounded by the number of heartbeats. Buckets live in the public router (`PingLimits`), not in `AppState`.
- **A failure ping on a heartbeat that is already down** only moves `last_ping_at`; the open incident keeps the output of the ping that opened it. A failure ping on `pending` or `up` opens the incident at once (title `{name} reported a failure`). Exit code `0` is a success ping and is stored with `exit_code = 0`.
- **Paused / in-maintenance heartbeats** record every ping (Postgres `last_ping_at`, ClickHouse row) and change nothing else, failure pings included.
- **`heartbeat_pings` writes are best effort:** inserted inline after the Postgres transaction commits, bounded by a 2 s timeout, failure logged without the token; the ping reply never depends on ClickHouse.
- **`incidents.details`** is set only for heartbeat failure incidents with non-empty output; the incident list returns it (viewers included, like the pings list the spec gives viewers). Alert emails for a Down transition with details carry an `Output:` block, every line prefixed `> `, between `Since:` and `Details:`.
- **Heartbeat groups use the composite FK** `(group_id, project_id) → monitor_groups (id, project_id) on delete set null (group_id)`, like monitors (stronger than the spec's single-column FK: another project's group cannot be referenced even by a bug).
- **`ip_version` with a proxy is a 400** (`ip_version cannot be combined with a proxy: the proxy resolves the target`). For types outside http/keyword/tcp/ping/ssl_expiry the field is reset to `any` (not an error), like other unused fields.
- **Proxy URL** must be `http://host:port` or `https://host:port` with an explicit port, no userinfo, no path other than `/`, no query or fragment. Removing `proxy_url` clears the username and password; a username without a URL, or a password without a username, is a 400. Viewers read `proxy_username` as `***`; `proxy_url` is shown (it cannot hold credentials).
- **Proxied target names are resolved by the proxy.** The engine guards what it connects to — the proxy host (through `GuardedResolver`, pinned by a test that proves reqwest resolves the proxy through the custom resolver) and target IP literals (`names_forbidden_address`). Errors name the side that failed: `proxy connection failed: …`, `proxy authentication failed (HTTP 407)`, `proxy tunnel failed: …`; everything else is the target's.
- **Per-check clients:** a monitor with a proxy or `remember_cookies` gets a fresh client for each check (proxy and jar never shared); all others keep the per-pass shared cache, now 18 slots (3 redirect modes × verify × 3 IP versions).
- **Import result rows** are `{line, status, name?, target?, monitor_id?, error?}`. `target` is the URL through `redact_url` or `host[:port]`; no header, password or body ever appears. In a dry run, valid entries say `created` with no `monitor_id` and the response has `"dry_run": true` (the UI says "will be created"). `line` is the 1-based input line for `urls` and CSV records, the 1-based position in `monitorList` for Kuma.
- **Import defaults** (`interval_seconds`, `timeout_seconds`, `confirmation_threshold`, `recovery_threshold`, `group_id`) apply to every format: they fill what the source does not set; `group_id` applies to every entry. A foreign `group_id` makes the whole request 404 before parsing.
- **`skip_existing` key:** the validated URL string for http/keyword; `lowercase(host)` plus `:port` when the type has a port, for the others. Compared against the project's monitors and against earlier entries of the same request.
- **Kuma specifics:** `upsideDown: true` → per-entry error (inverted semantics are not supported); `authMethod` other than empty/`basic` → per-entry error; `headers` must be a JSON object string; `timeout` (if present) clamped to 1..=min(60, interval−1); `proxyId` ignored (Kuma proxies live in `proxyList`, which is never read). `active` accepts `true`/`false`/`1`/`0`.
- **UptimeRobot interval** is read as seconds (the unit of UptimeRobot's API) and raised to ≥ 30; this needs confirmation against a real export (see Open questions).
- **Availability JSON:** `{from, to, total_seconds, maintenance_seconds, downtime_seconds, uptime_pct, incidents, longest_incident_seconds}` where `from`/`to` are the effective (clipped) bounds, `uptime_pct` is 0-100 (not 0-1), `incidents` counts incidents overlapping the effective range, and `longest_incident_seconds` is the largest per-incident downtime after maintenance is removed. Only windows that exist now count (deleted windows are gone).
- **Maintenance spans of weekly windows** follow `Schedule::covers` wall-clock semantics; a window lying wholly inside a repeated DST hour is counted as one span from its first start to its second end (over-counts maintenance by at most that window's gap, once a year). Pinned by a sampling test against `covers`.
- **CSV export** filename `checks-{monitor_id}.csv` (ids only, so no header injection); `Cache-Control: no-store`; ClickHouse is queried one UTC day at a time (`LIMIT 20000` per day) so memory stays bounded; the first slice is fetched before the response starts, so a ClickHouse outage is a JSON 500, a failure later aborts the stream (logged).
- **Request logging:** the engine has no request-logging layer (no `TraceLayer` is installed); "tokens redacted from request logs" is therefore enforced by extending `no_token_ever_reaches_a_log_line` to the new routes at TRACE level. Any future request logger must redact `/api/v1/heartbeat/*` paths.

## Open questions (flagged for the controller; the plan proceeds with the ruling given)

1. **UptimeRobot header names.** No real UptimeRobot export is in the repo. The fixture in NAMI-UP-25 is reconstructed from UptimeRobot's documented export columns (`Friendly Name`, `URL/IP`, `Type`, `Keyword Type`, `Keyword Value`, `Port`, `Interval`) and the parser matches case-insensitive aliases. If anyone can export a real CSV, replace `tests/fixtures/import/uptimerobot.csv` with it (credentials removed) and adjust the alias table; the tests are written against the alias table, so that is a one-file change.
2. **UptimeRobot interval unit** (seconds vs minutes) — ruling: seconds. Same fix path as 1.
3. **adapter body size in nami-ui:** a 2 MiB import file passes SvelteKit's dev server, but `adapter-node` defaults `BODY_SIZE_LIMIT` to 512 KiB. NAMI-UP-31 documents `BODY_SIZE_LIMIT=3M` in `nami-ui/.env.example`; the deployment adapter itself is a Platform (sub-project 11) decision.

## File Structure

```
nami-engine/
  Cargo.toml                            T24 reqwest features += cookies; T25 csv
  migrations/
    0022_incident_details.sql           T21 incidents.details
    0023_heartbeat_groups.sql           T22 heartbeats.group_id (composite FK) + index
    0024_monitor_advanced_request.sql   T23 ip_version, remember_cookies, proxy_url, proxy_username, proxy_password_enc
  src/
    clickhouse.rs                       T21 MIGRATIONS += heartbeat_pings
    models/
      incident.rs                       T21 details column, attach_details, list item field
      heartbeat.rs                      T22 group_id on row, view, config, requests, queries
      monitor.rs                        T23 IpVersion, new columns, view, config, requests, validation, write_config
      monitor_request.rs                T23 proxy password context + seal/open + validation helpers
      maintenance.rs                    T27 Span, Schedule::spans
    routes/
      heartbeats.rs                     T20 signal routes, body, rate limit; T21 pings route + CH insert; T22 group check; T27 availability route
      monitors.rs                       T23 group/proxy untouched beyond view; T26 import route; T27 availability; T28 export
      incidents.rs                      T21 (no code change: list item gains `details`)
    router.rs                           T20 public router takes PingLimits
    uptime/
      heartbeat.rs                      T20 Ping, PingKind, parse_signal, lookup_token, record_signal; T21 sanitize_output, details
      pings.rs                          T21 heartbeat_pings rows: insert + recent
      alerts.rs                         T21 Transition.details, Output block in render
      worker.rs                         T21 Transition literals gain `details: None`
      checker.rs                        T24 IpVersion in resolver/clients, resolve_target, per-check clients, proxy errors
      ping.rs, tls.rs, expiry.rs, mail.rs  T24 resolution through resolve_target / ip_version
      import/mod.rs, urls.rs, kuma.rs, uptimerobot.rs   T25 parsers; T26 run_import
      availability.rs                   T27 union/subtract/compute + queries
      export.rs                         T28 csv cells, streaming body
  tests/
    common/mod.rs                       T20 ping helpers; T24 FakeProxy
    fixtures/import/                    T25 uptime_kuma.json, uptimerobot.csv
    heartbeats.rs                       T20, T21, T22 extended
    heartbeat_pings.rs                  T21 (new; ClickHouse)
    monitor_advanced_options.rs         T23 (new)
    checker_advanced_options.rs         T24 (new)
    uptime_checker_proxy.rs             T24 explicit proxy beats env proxy
    monitor_import.rs                   T25 parser tests, T26 API tests (new)
    availability.rs                     T27 (new)
    checks_export.rs                    T28 (new; ClickHouse)
    clickhouse.rs                       T21 table list += heartbeat_pings
    migrations.rs                       T21-T23 new columns

nami-ui/src/
  lib/uptime.ts (+ .test.ts)            T29 Heartbeat.group_id, HeartbeatPing, Incident.details, ping command helpers;
                                        T30 Monitor advanced fields, hasMaskedValues
  lib/availability.ts (+ .test.ts)      T32 ranges, formatting
  lib/server/heartbeat-form.ts (+ test) T29 group_id, mode
  lib/server/monitor-form.ts (+ test)   T30 advanced section
  lib/server/import-form.ts (+ test)    T31
  lib/components/MonitorForm.svelte     T30 Advanced section
  lib/components/AvailabilityCard.svelte T32
  routes/(app)/projects/[id=uuid]/
    heartbeats/+page.*                  T29 grouped list, group picker
    heartbeats/[heartbeatId=uuid]/+page.*  T29 examples, pings, group; T32 availability card
    incidents/+page.svelte              T29 details
    monitors/+page.svelte               T31 "Import" link
    monitors/import/+page.*             T31
    monitors/[monitorId=uuid]/+page.*   T32 availability card, export link
    monitors/[monitorId=uuid]/checks.csv/+server.ts   T32 streaming BFF route
```

---

### Task 20 (NAMI-UP-20): Heartbeat `/fail` and exit-code pings, ping bodies, per-heartbeat rate limit

Branch `feat/NAMI-UP-20-heartbeat-fail-pings`, worktree `.worktrees/NAMI-UP-20/`.

**Files:**
- Modify: `nami-engine/src/uptime/heartbeat.rs` (new types and `record_signal`; `record_ping` becomes a wrapper), `nami-engine/src/routes/heartbeats.rs` (`public_router`, `ping`, new `ping_signal`), `nami-engine/src/router.rs` (`build_public_router*`)
- Test: `nami-engine/tests/heartbeats.rs` (extend), unit tests in `src/uptime/heartbeat.rs`

**Interfaces:**
- Consumes: `is_well_formed_token`, `incident::open_for_heartbeat(conn, project_id, heartbeat_id, title, cause, started_at) -> Option<Uuid>`, `incident::resolve_for_heartbeat`, `maintenance::heartbeat_in_maintenance`, `logs::rate_limit::TokenBuckets::{new(rate, burst), try_acquire(Uuid) -> bool}`, `AppError::RateLimited`.
- Produces (in `uptime::heartbeat`):
  - `pub enum PingKind { Success, Fail }` (`Debug, Clone, Copy, PartialEq, Eq, Serialize` lowercase).
  - `pub struct Ping { pub kind: PingKind, pub exit_code: Option<i32>, pub output: String }` (`Debug, Clone, PartialEq`); `Ping::success() -> Ping` (no code, empty output).
  - `pub fn parse_signal(segment: &str) -> Option<(PingKind, Option<i32>)>`: `"fail"` → `(Fail, None)`; an `i32` (`str::parse`) → `(Success, Some(0))` for 0, else `(Fail, Some(n))`; anything else `None`.
  - `pub fn failure_cause(exit_code: Option<i32>) -> String`: `Heartbeat reported failure (exit code N)` / `Heartbeat reported failure`.
  - `pub async fn lookup_token(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error>` (one indexed lookup; `None` for a malformed or unknown token, without a query for malformed).
  - `pub struct Recorded { pub heartbeat_id: Uuid, pub project_id: Uuid, pub transition: Option<Transition> }`.
  - `pub async fn record_signal(pool: &PgPool, token: &str, ping: &Ping) -> Result<Option<Recorded>, sqlx::Error>` (`None` = unknown token under the lock).
  - `record_ping(pool, token) -> Result<PingOutcome, sqlx::Error>` keeps its signature and behaviour (lookup + `record_signal(.., &Ping::success())`), so existing tests stay untouched.
- Produces (routes/router):
  - `pub const MAX_PING_BODY_BYTES: usize = 1024 * 1024;` `pub const PINGS_PER_SECOND: f64 = 1.0;` `pub const PING_BURST: f64 = 10.0;` in `routes::heartbeats`.
  - `pub fn public_router(ping_slots: Arc<Semaphore>, ping_limits: Arc<TokenBuckets>) -> Router<AppState>`; routes `/api/v1/heartbeat/{token}` and `/api/v1/heartbeat/{token}/{signal}` (GET and POST each), layer `DefaultBodyLimit::max(MAX_PING_BODY_BYTES)`.
  - `router::build_public_router_with_ping_limits(state, ping_slots: Arc<Semaphore>, ping_limits: Arc<TokenBuckets>) -> Router` (tests); the existing builders pass `default_ping_limits()` = `TokenBuckets::new(PINGS_PER_SECOND, PING_BURST)`.

- [ ] **Step 1: Write the failing unit tests** in `src/uptime/heartbeat.rs` `mod tests`:

```rust
#[test]
fn signals_parse_to_kind_and_exit_code() {
    assert_eq!(parse_signal("fail"), Some((PingKind::Fail, None)));
    assert_eq!(parse_signal("0"), Some((PingKind::Success, Some(0))));
    assert_eq!(parse_signal("1"), Some((PingKind::Fail, Some(1))));
    assert_eq!(parse_signal("137"), Some((PingKind::Fail, Some(137))));
    assert_eq!(parse_signal("-1"), Some((PingKind::Fail, Some(-1))));
    assert_eq!(parse_signal("2147483647"), Some((PingKind::Fail, Some(i32::MAX))));
    for bad in ["", "FAIL", "Fail", "fail ", "1.5", "2147483648", "0x1", "abc", "1e3", "/"] {
        assert_eq!(parse_signal(bad), None, "{bad:?}");
    }
}

#[test]
fn failure_causes_name_the_exit_code_when_there_is_one() {
    assert_eq!(failure_cause(Some(2)), "Heartbeat reported failure (exit code 2)");
    assert_eq!(failure_cause(None), "Heartbeat reported failure");
}
```

- [ ] **Step 2: Write the failing integration tests** in `tests/heartbeats.rs`. Add helpers next to `ping_raw`:

```rust
/// POST `body` to a ping path with no session; returns status, headers, JSON.
async fn ping_with(public: &Router, path: &str, body: Vec<u8>) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded") // what curl --data-binary sends
        .body(Body::from(body))
        .unwrap();
    let (parts, body) = public.clone().oneshot(request).await.unwrap().into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
```

Tests (names and assertions are binding):

```rust
#[sqlx::test]
async fn a_fail_ping_takes_the_heartbeat_down_at_once_and_a_success_ping_recovers_it(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, _, body) = ping_with(&public, &format!("/api/v1/heartbeat/{token}/fail"), vec![]).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(heartbeat_state(&pool, id).await, ("down".to_string(), true));
    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].1, "open");
    assert_eq!(incidents[0].2.as_deref(), Some("Heartbeat reported failure"));

    let (status, _, _) = ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert_eq!(incidents_of(&pool, id).await[0].1, "resolved");
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(mail[0].0, "[Nami] Nightly backup is DOWN");
    assert!(mail[0].1.contains("Cause: Heartbeat reported failure\n"), "{}", mail[0].1);
    assert_eq!(mail[1].0, "[Nami] Nightly backup is back UP");
}

#[sqlx::test]
async fn a_non_zero_exit_code_is_a_failure_naming_the_code(pool: sqlx::PgPool) {
    // ... seed, POST /api/v1/heartbeat/{token}/137
    // assert status 200, current_status "down",
    // cause == Some("Heartbeat reported failure (exit code 137)"),
    // and a second failure (/2) leaves exactly one incident whose cause still names 137.
}

#[sqlx::test]
async fn a_signal_that_is_neither_fail_nor_an_integer_is_400(pool: sqlx::PgPool) {
    // seed a heartbeat; for suffix in ["FAIL", "1.5", "2147483648", "ok", "fail%20"]:
    //   status == 400, body["error"] == "the last path segment must be fail or an exit code"
    // and heartbeat_state unchanged (pending, never pinged).
}

#[sqlx::test]
async fn fail_and_exit_code_routes_answer_404_for_unknown_or_malformed_tokens(pool: sqlx::PgPool) {
    // for path in ["/api/v1/heartbeat/{64 x 'f'}/fail", "/api/v1/heartbeat/{64 x 'f'}/1",
    //              "/api/v1/heartbeat/not-a-token/fail", "/api/v1/heartbeat/{64 x 'F'}/0"]:
    //   status == 404, body == {"error":"not found"}
}

#[sqlx::test]
async fn fail_pings_during_maintenance_or_pause_change_nothing_but_last_ping_at(pool: sqlx::PgPool) {
    // two heartbeats in one project, both status 'up':
    //   A paused; B in a project-wide one-off window covering now (insert into maintenance_windows
    //   kind 'once', starts_at now()-1h, ends_at now()+1h, monitor_id null).
    // POST /fail to both -> 200; both still 'up', last_ping_at set, no incidents,
    // settled_mail(recipient, 0) empty.
    // Delete the window, unpause A, POST /0 to both -> still 'up', still no incidents.
}

#[sqlx::test]
async fn ping_bodies_over_one_mebibyte_are_413_and_change_nothing(pool: sqlx::PgPool) {
    // POST /api/v1/heartbeat/{token}/fail with vec![b'x'; MAX_PING_BODY_BYTES + 1]
    //   -> 413, body == {"error":"ping body too large (max 1 MiB)"}, heartbeat still pending.
    // exactly MAX_PING_BODY_BYTES -> 200.
}

#[sqlx::test]
async fn more_than_the_burst_of_pings_per_heartbeat_is_429(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    // A refill rate of ~0 makes the test independent of how fast pings run.
    let limits = Arc::new(TokenBuckets::new(0.000_1, PING_BURST));
    let public = nami_engine::router::build_public_router_with_ping_limits(
        state, Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT)), limits);
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let (_, other) = seed_heartbeat(&pool, project, "B", 3600, 60, 0).await;
    for n in 0..10 {
        let path = if n % 2 == 0 { format!("/api/v1/heartbeat/{token}") } else { format!("/api/v1/heartbeat/{token}/fail") };
        assert_eq!(ping_with(&public, &path, vec![]).await.0, StatusCode::OK, "ping {n}");
    }
    let (status, headers, body) = ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers["retry-after"], "1");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(body, json!({ "error": "too many requests" }));
    // Buckets are per heartbeat: another token is unaffected; an unknown token stays 404.
    assert_eq!(ping_with(&public, &format!("/api/v1/heartbeat/{other}"), vec![]).await.0, StatusCode::OK);
    assert_eq!(ping_with(&public, &format!("/api/v1/heartbeat/{}", "e".repeat(64)), vec![]).await.0, StatusCode::NOT_FOUND);
}
```

Extend `no_token_ever_reaches_a_log_line`: after the existing calls, also `POST {ping}/fail` with body `b"backup failed: disk full"`, `POST {ping}/3`, `POST {ping}/0`, `POST /api/v1/heartbeat/{unknown}/fail` — the same final assertion (no token in any line) must hold. Extend `ping_replies_are_never_cacheable` to the `/fail`, `/1`, 400 and 429 replies.

- [ ] **Step 3: Run them to verify they fail**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeats -- fail exit_code signal 413 burst no_token cacheable`
Expected: compile errors (`parse_signal`, `build_public_router_with_ping_limits`, `MAX_PING_BODY_BYTES` not found).

- [ ] **Step 4: Implement `uptime::heartbeat`**

`lookup_token` is the existing `exists` query turned into `select id from heartbeats where token = $1`. `record_signal` is today's `record_ping` transaction (row lock, `now()`, paused/maintenance freeze) generalized:

```rust
pub async fn record_signal(pool: &PgPool, token: &str, ping: &Ping) -> Result<Option<Recorded>, sqlx::Error> {
    if !is_well_formed_token(token) { return Ok(None); }
    let mut tx = pool.begin().await?;
    // same `select id, project_id, name, current_status, paused, now() ... for update` as today
    let Some((id, project_id, name, status, paused, now)) = row else { return Ok(None) };
    let frozen = paused || maintenance::heartbeat_in_maintenance(&mut *tx, project_id, now).await?;
    let transition = match (frozen, ping.kind, status.as_str()) {
        (true, _, _) | (false, PingKind::Fail, "down") => {
            sqlx::query("update heartbeats set last_ping_at = now() where id = $1") /* ... */;
            None
        }
        (false, PingKind::Success, _) => { /* today's success branch: status 'up', resolve if was down */ }
        (false, PingKind::Fail, _) => {
            sqlx::query("update heartbeats set last_ping_at = now(), current_status = 'down' where id = $1") /* ... */;
            let cause = failure_cause(ping.exit_code);
            let opened = incident::open_for_heartbeat(&mut tx, project_id, id,
                &format!("{name} reported a failure"), Some(&cause), now).await?;
            opened.map(|_| Transition { kind: TransitionKind::Down, subject: Subject::Heartbeat(id),
                project_id, name, cause: Some(cause), at: now })
        }
    };
    tx.commit().await?;
    Ok(Some(Recorded { heartbeat_id: id, project_id, transition }))
}
```

`record_ping` = `lookup_token` (return `UnknownToken` when `None`) then `record_signal(pool, token, &Ping::success())`, mapping `None` to `UnknownToken` and `Some(r)` to `Recorded { transition: r.transition }`.

- [ ] **Step 5: Implement the routes**

In `routes::heartbeats`: `#[derive(Clone)] struct PingLimits(Arc<TokenBuckets>);` added as an `Extension` beside `PingSlots`. Both handlers delegate to one function:

```rust
async fn handle_ping(state: AppState, slots: Arc<Semaphore>, limits: Arc<TokenBuckets>,
                     token: String, signal: Option<String>,
                     body: Result<Bytes, BytesRejection>) -> Result<Response, AppError>
```

Order (each step returns before the next): `is_well_formed_token` else 404 → `signal.map(parse_signal)`: `Some(None)` → `AppError::Validation("the last path segment must be fail or an exit code")` → `body` rejection with status 413 → `(413, Json({"error":"ping body too large (max 1 MiB)"}))`, any other rejection → 400 → `lookup_token` (`None` → 404) → `limits.try_acquire(id)` else `(429, [(RETRY_AFTER, "1")], Json({"error":"too many requests"}))` → `slots.try_acquire()` else `busy()` → `record_signal(.., &Ping { kind, exit_code, output: String::new() })` (`None` → 404) → spawn `alerts::notify_transition` for a transition (as today) → `Json({"ok": true})`. The body is read but not stored yet (NAMI-UP-21 stores it). Path extraction keeps the `Result<Path<_>, PathRejection>` pattern (a rejection is a 404). The 429 and 413 replies go through the existing `no_store` layer.

In `router.rs`: `fn default_ping_limits() -> Arc<TokenBuckets>`; `build_public_router_with_limits` gains a `ping_limits` parameter; add `pub fn build_public_router_with_ping_limits(state, ping_slots, ping_limits) -> Router`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --lib uptime::heartbeat && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeats`
Expected: PASS (the whole `heartbeats` binary, old tests included).

- [ ] **Step 7: Full engine suite, then commit**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test` → all binaries green; `cargo clippy --all-targets -- -D warnings` clean.

```bash
git add nami-engine/src/uptime/heartbeat.rs nami-engine/src/routes/heartbeats.rs nami-engine/src/router.rs nami-engine/tests/heartbeats.rs
git commit -m "feat(engine): heartbeat /fail and exit-code pings with per-heartbeat rate limit (NAMI-UP-20)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 21 (NAMI-UP-21): Heartbeat output capture, `heartbeat_pings`, pings list, incident details

Branch `feat/NAMI-UP-21-heartbeat-output`, worktree `.worktrees/NAMI-UP-21/`.

**Files:**
- Create: `nami-engine/migrations/0022_incident_details.sql`, `nami-engine/src/uptime/pings.rs`, `nami-engine/tests/heartbeat_pings.rs`
- Modify: `nami-engine/src/clickhouse.rs` (`MIGRATIONS`), `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/heartbeat.rs` (`sanitize_output`, details in `record_signal`), `nami-engine/src/models/incident.rs`, `nami-engine/src/uptime/alerts.rs` (`Transition.details`, `render`), `nami-engine/src/uptime/worker.rs` (two `Transition` literals), `nami-engine/src/routes/heartbeats.rs` (store output, insert ping row, pings route)
- Test: `nami-engine/tests/heartbeat_pings.rs`, `nami-engine/tests/clickhouse.rs` (`migrate_is_idempotent` table list), `nami-engine/tests/migrations.rs`, unit tests in `heartbeat.rs`, `alerts.rs`

**Interfaces:**
- Consumes: NAMI-UP-20's `Ping`, `record_signal`, `Recorded`, `handle_ping`; `ClickHouse::{insert_rows_with, query_rows}`; `require_project_role(.., "viewer")`, `heartbeat::find_in_project`.
- Produces:
  - `uptime::heartbeat::MAX_OUTPUT_BYTES: usize = 10 * 1024`, `TRUNCATED_MARKER: &str = "\n…[truncated]"`, `pub fn sanitize_output(body: &[u8]) -> String`.
  - `models::incident::attach_details(conn: &mut PgConnection, incident_id: Uuid, details: &str) -> Result<(), sqlx::Error>`; `IncidentListItem.details: Option<String>`.
  - `alerts::Transition.details: Option<String>` (every literal sets it; only heartbeat failure transitions carry `Some`).
  - `uptime::pings::{HeartbeatPingRow, insert_ping(ch, &HeartbeatPingRow) -> Result<(), ClickHouseError>, recent_pings(ch, project_id, heartbeat_id, limit: u32) -> Result<Vec<HeartbeatPingRow>, ClickHouseError>}` where `HeartbeatPingRow { heartbeat_id: Uuid, project_id: Uuid, received_at: DateTime<Utc>, kind: PingKind, exit_code: Option<i32>, output: String }` (`Serialize, Deserialize`; also the JSON shape of the pings list).
  - `GET /projects/{id}/heartbeats/{heartbeat_id}/pings?limit=` (viewer) → `Vec<HeartbeatPingRow>` newest first; `DEFAULT_PINGS_LIMIT = 50`, clamp 1..=100.
  - `Recorded.received_at: DateTime<Utc>` (the transaction's `now()`), used for the ClickHouse row.

- [ ] **Step 1: Migration**

`nami-engine/migrations/0022_incident_details.sql`:

```sql
-- Output of the heartbeat ping that opened the incident (sanitized, at most
-- 10 KiB plus a truncation marker; see uptime::heartbeat::sanitize_output).
-- Numbers 0020-0021 are left to the Phase 2 final fix wave.
alter table incidents add column details text
    check (details is null or octet_length(details) <= 10240 + 16);
```

ClickHouse — append to `clickhouse::MIGRATIONS` (array length 3):

```sql
CREATE TABLE IF NOT EXISTS heartbeat_pings (
    heartbeat_id UUID,
    project_id UUID,
    received_at DateTime64(3, 'UTC'),
    kind Enum8('success' = 1, 'fail' = 2),
    exit_code Nullable(Int32),
    output String
) ENGINE = MergeTree
ORDER BY (heartbeat_id, received_at)
TTL toDateTime(received_at) + INTERVAL 180 DAY
```

- [ ] **Step 2: Write the failing unit tests** (`src/uptime/heartbeat.rs`):

```rust
#[test]
fn output_keeps_text_newlines_and_tabs() {
    assert_eq!(sanitize_output(b"line 1\n\tline 2\n"), "line 1\n\tline 2\n");
    assert_eq!(sanitize_output("gagal \u{2026} 日本語 🦀".as_bytes()), "gagal \u{2026} 日本語 🦀");
    assert_eq!(sanitize_output(b""), "");
}

#[test]
fn carriage_returns_and_escape_sequences_cannot_hide_text() {
    assert_eq!(sanitize_output(b"rm -rf done\rall good"), "rm -rf doneall good");
    assert_eq!(sanitize_output(b"\x1b[8mhidden\x1b[0m ok"), "[8mhidden[0m ok");
    assert_eq!(sanitize_output(b"a\0b\x07c\x7fd"), "abcd");
    let c1: String = ('\u{80}'..='\u{9f}').collect();
    assert_eq!(sanitize_output(format!("x{c1}y").as_bytes()), "xy");
}

#[test]
fn direction_controls_are_removed() {
    let text = "a\u{202A}b\u{202B}c\u{202C}d\u{202D}e\u{202E}f\u{2066}g\u{2067}h\u{2068}i\u{2069}j\u{200E}k\u{200F}l\u{061C}m";
    assert_eq!(sanitize_output(text.as_bytes()), "abcdefghijklm");
}

#[test]
fn invalid_utf8_becomes_the_replacement_character() {
    assert_eq!(sanitize_output(b"ok \xff\xfe end"), "ok \u{FFFD}\u{FFFD} end");
}

#[test]
fn output_is_cut_at_ten_kib_on_a_char_boundary_with_a_marker() {
    let exact = "a".repeat(MAX_OUTPUT_BYTES);
    assert_eq!(sanitize_output(exact.as_bytes()), exact);
    let over = sanitize_output("a".repeat(MAX_OUTPUT_BYTES + 1).as_bytes());
    assert_eq!(over, format!("{}{TRUNCATED_MARKER}", "a".repeat(MAX_OUTPUT_BYTES)));
    // 3-byte chars: the cut never splits one, and the kept part is <= 10 KiB.
    let wide = sanitize_output("€".repeat(MAX_OUTPUT_BYTES).as_bytes());
    let kept = wide.strip_suffix(TRUNCATED_MARKER).unwrap();
    assert!(kept.len() <= MAX_OUTPUT_BYTES && kept.chars().all(|c| c == '€'));
    // Replacement characters can grow the text: still capped.
    let invalid = sanitize_output(&vec![0xff; MAX_OUTPUT_BYTES]);
    assert!(invalid.strip_suffix(TRUNCATED_MARKER).unwrap().len() <= MAX_OUTPUT_BYTES);
    // Stripped characters do not count toward the cap.
    let mut noisy = b"\r".repeat(MAX_OUTPUT_BYTES);
    noisy.extend_from_slice(b"tail");
    assert_eq!(sanitize_output(&noisy), "tail");
}
```

Alert unit tests (`src/uptime/alerts.rs`): `transition()` gains `details: None`;

```rust
#[test]
fn heartbeat_output_is_quoted_line_by_line_between_since_and_details() {
    let down = Transition {
        subject: Subject::Heartbeat(Uuid::nil()),
        cause: Some("Heartbeat reported failure (exit code 2)".into()),
        details: Some("<script>alert(1)</script>\nBcc: x@evil.test\n\tdisk full".into()),
        ..transition(TransitionKind::Down, "Nightly backup")
    };
    let (subject, body) = render("http://ui.test", &down);
    assert_eq!(subject, "[Nami] Nightly backup is DOWN");
    assert!(body.contains(
        "Since: 2026-10-14 17:46:40 UTC\n\nOutput:\n> <script>alert(1)</script>\n> Bcc: x@evil.test\n> \tdisk full\n\nDetails: "
    ), "{body:?}");
    assert!(!body.lines().any(|l| l.starts_with("Bcc:")), "{body:?}");
}

#[test]
fn details_are_sanitized_again_when_rendered() {
    // A Transition built any other way: CR and ESC never reach the body.
    let down = Transition { details: Some("a\rb\u{1b}c".into()), ..transition(TransitionKind::Down, "X") };
    let (_, body) = render("http://ui.test", &down);
    assert!(body.contains("Output:\n> abc\n"), "{body:?}");
}
```

(The email is `SinglePart::plain`, so markup is inert text by construction; the test pins that nothing in the output starts a header-looking line.)

- [ ] **Step 3: Write the failing integration tests** — `tests/heartbeat_pings.rs` (needs ClickHouse; uses `common::state_with_clickhouse` and `common::public_app` / `common::app`):

```rust
#[sqlx::test]
async fn every_ping_is_recorded_with_kind_exit_code_and_sanitized_output(pool: sqlx::PgPool) {
    let (state, ch) = common::state_with_clickhouse(&pool).await;
    // seed project + member + heartbeat via the API (member token), then:
    // POST {ping}            body "started"          -> success, exit_code null
    // POST {ping}/0          body ""                 -> success, exit_code 0
    // POST {ping}/2          body "err\r\x1b[31mred" -> fail, exit_code 2, output "err[31mred"
    // POST {ping}/fail       body 11 KiB of 'x'      -> fail, null, output = 10240 'x' + TRUNCATED_MARKER
    // GET /projects/{p}/heartbeats/{h}/pings as a viewer -> 200, 4 rows newest first with exactly
    //   those (kind, exit_code, output); keys == {heartbeat_id, project_id, received_at, kind, exit_code, output}
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_output_of_the_ping_that_opens_the_incident_is_attached_to_it(pool: sqlx::PgPool) {
    // POST {ping}/1 body "<script>alert(1)</script>\nbackup failed"
    // incidents list (GET /projects/{p}/incidents, viewer) -> one item, details ==
    //   "<script>alert(1)</script>\nbackup failed", cause "Heartbeat reported failure (exit code 1)".
    // A second failure ping with other output leaves details unchanged.
    // Recovery (POST /0) resolves; a new failure with empty body opens a new incident with details null.
    // The DOWN email (stored_mail_for) contains "Output:\n> <script>alert(1)</script>\n> backup failed\n".
}

#[sqlx::test]
async fn the_pings_list_checks_access_project_and_limit(pool: sqlx::PgPool) {
    // outsider -> 403; heartbeat id of another project -> 404; unknown id -> 404;
    // ?limit=0 -> 1 row, ?limit=1000 -> at most 100 rows (seed 105 rows straight into ClickHouse
    // with insert_ping), no limit -> 50; ?limit=abc -> 400.
}

#[sqlx::test]
async fn a_clickhouse_outage_never_fails_a_ping(pool: sqlx::PgPool) {
    // AppState::for_test (ClickHouse bound to a database that does not exist):
    // POST {ping}/fail -> 200 {"ok":true}, heartbeat down, incident open.
}
```

`tests/clickhouse.rs::migrate_is_idempotent`: expected table list becomes `heartbeat_pings`, `logs`, `uptime_checks` (sorted). `tests/migrations.rs`: assert `incidents.details` exists (follow the file's existing column-check style).

- [ ] **Step 4: Run them to verify they fail**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --lib uptime:: && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeat_pings --test clickhouse --test migrations`
Expected: FAIL (missing `sanitize_output`, `details`, table).

- [ ] **Step 5: Implement**

`sanitize_output`: `String::from_utf8_lossy(body)`, drop every char with `c.is_control() && c != '\n' && c != '\t'` (C0, DEL, C1) and the bidi set (`'\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'`), then if the result is longer than `MAX_OUTPUT_BYTES` bytes cut at the last char boundary ≤ the cap and append `TRUNCATED_MARKER`. (Filter first, then cut: stripped bytes never count.)

`handle_ping` passes `output: sanitize_output(&body)` in the `Ping`. `record_signal`: when the failure branch opened an incident and `!ping.output.is_empty()`, `incident::attach_details(&mut tx, incident_id, &ping.output)` and put `details: Some(output.clone())` on the transition; `Recorded` gains `received_at: now`. After the reply data is ready and before returning, the handler inserts the ClickHouse row:

```rust
let row = HeartbeatPingRow { heartbeat_id: r.heartbeat_id, project_id: r.project_id,
    received_at: r.received_at, kind: ping.kind, exit_code: ping.exit_code, output: ping.output };
match tokio::time::timeout(Duration::from_secs(2), pings::insert_ping(&state.clickhouse, &row)).await {
    Ok(Ok(())) => {}
    // Never the token: only ids.
    Ok(Err(e)) => tracing::warn!(error = %e, heartbeat_id = %row.heartbeat_id, "heartbeat ping not recorded in ClickHouse"),
    Err(_) => tracing::warn!(heartbeat_id = %row.heartbeat_id, "heartbeat ping not recorded in ClickHouse (timeout)"),
}
```

`insert_ping` uses `insert_rows_with("heartbeat_pings", &[row], &[("async_insert", "1"), ("wait_for_async_insert", "1")])`. `recent_pings` mirrors `store::recent_checks` (`WHERE project_id = {project_id:UUID} AND heartbeat_id = {heartbeat_id:UUID} ORDER BY received_at DESC LIMIT {limit:UInt32}`).

`alerts::render` (Down only): after the `Since:` line, when `details` is `Some` and not empty, add `"\nOutput:\n"` and each line of `sanitize_output(details.as_bytes())` as `"> {line}\n"`, then the existing `"\nDetails: {link}\n"`. Add `details: None` to the literals in `worker.rs` (2) and `heartbeat.rs` (success/sweep), and the alerts tests.

`incident::list_for_project` selects `i.details`. `attach_details` is `update incidents set details = $2 where id = $1`.

Route: `.route("/projects/{id}/heartbeats/{heartbeat_id}/pings", get(list_pings))` — `require_project_role(.., "viewer")`, `find_in_project` (404), clamp, `recent_pings`.

- [ ] **Step 6: Run to verify they pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test` (full suite) — green; clippy clean.

- [ ] **Step 7: Tick the roadmap and commit**

In `docs/superpowers/roadmap.md` Phase 3, tick "Heartbeat `/fail` endpoint and exit-code path segment (0 = success)" and "Heartbeat output capture (<= 10 KiB, sanitized, attached to the incident)" (the UI for them is the "UI for all of the above" item, ticked in NAMI-UP-32).

```bash
git add nami-engine docs/superpowers/roadmap.md
git commit -m "feat(engine): heartbeat output capture, heartbeat_pings and incident details (NAMI-UP-21)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 22 (NAMI-UP-22): Heartbeat groups (shared with monitor groups)

Branch `feat/NAMI-UP-22-heartbeat-groups`, worktree `.worktrees/NAMI-UP-22/`.

**Files:**
- Create: `nami-engine/migrations/0023_heartbeat_groups.sql`
- Modify: `nami-engine/src/models/heartbeat.rs`, `nami-engine/src/routes/heartbeats.rs` (`create_heartbeat`)
- Test: `nami-engine/tests/heartbeats.rs` (new tests), `nami-engine/tests/monitor_groups.rs` (delete cascade), unit tests in `models/heartbeat.rs`

**Interfaces:**
- Consumes: `monitor_group::belongs_to(executor, project_id, group_id) -> Result<bool, sqlx::Error>`, `models::nullable`.
- Produces: `Heartbeat.group_id: Option<Uuid>`, `HeartbeatView.group_id: Option<Uuid>`, `HeartbeatConfig.group_id`, `CreateHeartbeatRequest.group_id: Option<Uuid>`, `PatchHeartbeatRequest.group_id: Option<Option<Uuid>>` (`#[serde(default, deserialize_with = "nullable")]`); `update_in_project` returns `AppError::NotFound` for a foreign group.

- [ ] **Step 1: Migration** `0023_heartbeat_groups.sql`:

```sql
-- One group list per project for monitors and heartbeats (spec §3). Same
-- composite key as monitors (0018): a group of another project cannot be
-- referenced, and deleting a group ungroups its heartbeats.
alter table heartbeats
    add column group_id uuid,
    add constraint heartbeats_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id);

create index heartbeats_group_idx on heartbeats (group_id) where group_id is not null;
```

- [ ] **Step 2: Write the failing tests**

```rust
#[sqlx::test]
async fn heartbeats_join_their_projects_groups(pool: sqlx::PgPool) {
    // member creates group G (POST /projects/{p}/monitor-groups) and a heartbeat with "group_id": G
    //   -> 201, body["group_id"] == G; GET list shows group_id for viewers too.
    // PATCH {"group_id": null} -> group_id null; PATCH {"name": "x"} keeps group_id (set again first).
    // PATCH with a group of another project -> 404 and the row unchanged; same on create -> 404, nothing inserted.
    // PATCH with a random uuid -> 404.
}

#[sqlx::test]
async fn deleting_a_group_ungroups_its_heartbeats_and_monitors(pool: sqlx::PgPool) {
    // group with one monitor and one heartbeat; admin DELETE the group -> both rows keep existing with group_id null.
}

#[sqlx::test]
async fn the_database_refuses_a_group_of_another_project(pool: sqlx::PgPool) {
    // raw `update heartbeats set group_id = <foreign group>` -> sqlx error code 23503.
}
```

Unit test: `the_view_carries_the_group_id` (serialize `into_view(false, false)` → `group_id` key present, `null` by default).

- [ ] **Step 3: Run to verify failure**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeats -- group && cargo test --lib models::heartbeat`
Expected: FAIL.

- [ ] **Step 4: Implement** — add the field to the row (exhaustive `Debug` destructuring, `into_view`), view, config, requests (`apply_to` uses `patched`-style merge: `None` keeps, `Some(v)` sets), `insert_heartbeat` (`group_id` as `$7`) and `update_in_project` (`group_id = $8`; before writing, when the group changed and is `Some`, `belongs_to(&mut *tx, ..)` or `AppError::NotFound`; a 23503 from the write → `NotFound`, like `monitor::update_in_project`). `create_heartbeat` checks `belongs_to` before inserting (404). The handler order stays: role check first.

- [ ] **Step 5: Run to verify pass** — full engine suite green.

- [ ] **Step 6: Tick "Heartbeat groups (shared with monitor groups)" in the roadmap and commit**

```bash
git add nami-engine docs/superpowers/roadmap.md
git commit -m "feat(engine): heartbeat groups shared with monitor groups (NAMI-UP-22)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 23 (NAMI-UP-23): Advanced request options — model and API (IP version, cookies, proxy with sealed password)

Branch `feat/NAMI-UP-23-advanced-request-options`, worktree `.worktrees/NAMI-UP-23/`.

**Files:**
- Create: `nami-engine/migrations/0024_monitor_advanced_request.sql`, `nami-engine/tests/monitor_advanced_options.rs`
- Modify: `nami-engine/src/models/monitor.rs` (`Monitor`, its `Debug`, `MonitorView::new`, `for_reader`, `MonitorConfig` + `From<&Monitor>` + `changes_check_of`, `CreateMonitorRequest` + `into_config`, `PatchMonitorRequest` + `apply_to`, `validate`, `clear_unused_fields`, `clear_request_config`, `write_config`), `nami-engine/src/models/monitor_request.rs`
- Test: `nami-engine/tests/monitor_advanced_options.rs`, unit tests in both model files, `nami-engine/tests/migrations.rs`

**Interfaces:**
- Consumes: `PasswordChange`, `Cipher::{encrypt, decrypt}`, `SecretString`, `monitor_request::validate_username` style.
- Produces:
  - `models::monitor::IP_VERSIONS: [&str; 3] = ["any", "ipv4", "ipv6"]`; `pub enum IpVersion { Any, V4, V6 }` (`Debug, Clone, Copy, PartialEq, Eq, Default` = `Any`) with `IpVersion::parse(&str) -> Option<IpVersion>` and `Monitor::ip_version() -> IpVersion` (unknown stored text → `Any`); `IP_VERSION_TYPES: [&str; 5] = ["http", "keyword", "tcp", "ping", "ssl_expiry"]`.
  - `Monitor` new fields: `ip_version: String`, `remember_cookies: bool`, `proxy_url: Option<String>`, `proxy_username: Option<String>`, `#[serde(skip_serializing)] proxy_password_enc: Option<Vec<u8>>`.
  - `MonitorView.has_proxy_password: bool`.
  - `MonitorConfig` new fields: `ip_version: String`, `remember_cookies: bool`, `proxy_url: Option<String>`, `proxy_username: Option<String>`, `proxy_password: PasswordChange`.
  - Create request: `ip_version` (default `"any"`), `remember_cookies` (default false), `proxy_url`, `proxy_username`, `proxy_password: Option<SecretString>`. Patch: `ip_version: Option<String>`, `remember_cookies: Option<bool>`, `proxy_url`/`proxy_username`: `Option<Option<String>>` (nullable), `proxy_password: Option<Option<SecretString>>` (nullable).
  - `monitor_request::{PROXY_PASSWORD_CONTEXT = "monitors.proxy_password", proxy_password_context(Uuid) -> String, seal_proxy_password(&Cipher, Uuid, &SecretString) -> Vec<u8>, open_proxy_password(&Cipher, Uuid, &[u8]) -> Result<SecretString, DecryptError>, validate_proxy_url(&str) -> Result<String, String>}`.

- [ ] **Step 1: Migration** `0024_monitor_advanced_request.sql`:

```sql
-- Advanced request options (Phase 3 spec §5). proxy_password_enc is AES-GCM
-- sealed with AAD 'monitors.proxy_password:<id>' (see models/monitor_request.rs).
alter table monitors
    add column ip_version text not null default 'any'
        check (ip_version in ('any', 'ipv4', 'ipv6')),
    add column remember_cookies boolean not null default false,
    add column proxy_url text,
    add column proxy_username text,
    add column proxy_password_enc bytea,
    add constraint monitors_proxy_credentials_need_url
        check (proxy_url is not null or (proxy_username is null and proxy_password_enc is null));
```

- [ ] **Step 2: Write the failing unit tests** (`monitor_request.rs`):

```rust
#[test]
fn proxy_urls_must_be_http_or_https_host_and_port_without_credentials() {
    assert_eq!(validate_proxy_url(" http://proxy.internal:3128 ").unwrap(), "http://proxy.internal:3128");
    assert_eq!(validate_proxy_url("https://10.0.0.5:8443/").unwrap(), "https://10.0.0.5:8443/");
    assert_eq!(validate_proxy_url("http://[fd00::1]:3128").unwrap(), "http://[fd00::1]:3128");
    let credentials = "proxy_url must not contain credentials; use proxy_username and proxy_password";
    assert_eq!(validate_proxy_url("http://user:pass@proxy:3128").unwrap_err(), credentials);
    assert_eq!(validate_proxy_url("http://user@proxy:3128").unwrap_err(), credentials);
    for scheme in ["socks5://proxy:1080", "ftp://proxy:21", "proxy:3128", "//proxy:3128"] {
        assert_eq!(validate_proxy_url(scheme).unwrap_err(), "proxy_url must be an http:// or https:// URL", "{scheme}");
    }
    assert_eq!(validate_proxy_url("http://proxy").unwrap_err(), "proxy_url must include a port");
    for extra in ["http://proxy:3128/path", "http://proxy:3128/?a=b", "http://proxy:3128/#x"] {
        assert_eq!(validate_proxy_url(extra).unwrap_err(), "proxy_url must be scheme://host:port with nothing after it", "{extra}");
    }
    assert!(validate_proxy_url("http://pro xy:3128").is_err());
    assert!(validate_proxy_url("http://proxy:3128\r\nX: y").is_err());
    assert!(validate_proxy_url(&format!("http://{}:1", "a".repeat(2100))).is_err());
}

#[test]
fn a_sealed_proxy_password_opens_only_for_its_monitor_and_column() {
    let cipher = Cipher::new(&TEST_KEY);
    let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
    let sealed = seal_proxy_password(&cipher, id, &SecretString::new("pr0xy"));
    assert_eq!(open_proxy_password(&cipher, id, &sealed).unwrap().expose(), "pr0xy");
    assert!(open_proxy_password(&cipher, other, &sealed).is_err());
    assert!(open_password(&cipher, id, &sealed).is_err(), "not the basic-auth password");
}
```

Unit tests in `monitor.rs` (`validate`):

```rust
// ip_version: "ipv4"/"ipv6" kept for http, keyword, tcp, ping, ssl_expiry; reset to "any" for dns,
//   domain_expiry, smtp, pop3, imap, udp; "IPv4" or "v4" -> Err("ip_version must be any, ipv4 or ipv6").
// remember_cookies: kept for http/keyword, reset to false otherwise.
// proxy_username without proxy_url -> Err("proxy_username requires proxy_url");
// Replace(password) without username -> Err("proxy_password requires proxy_username");
// proxy_url None with Keep{present:true} password and a username -> username None, password Clear;
// proxy on a tcp monitor -> cleared (url None, username None, password Clear when present);
// proxy_url + ip_version "ipv6" -> Err("ip_version cannot be combined with a proxy: the proxy resolves the target");
// proxy_username "a:b" -> Err("proxy_username must not contain ':'"); > 256 chars -> Err; control char -> Err;
// proxy password > 1024 bytes -> Err("proxy_password must be at most 1024 bytes").
// changes_check_of: each of ip_version, remember_cookies, proxy_url, proxy_username, a non-Keep proxy password -> true.
```

- [ ] **Step 3: Write the failing API tests** — `tests/monitor_advanced_options.rs`:

```rust
#[sqlx::test]
async fn the_proxy_password_is_stored_sealed_and_never_returned(pool: sqlx::PgPool) {
    // member POST http monitor with proxy_url "http://proxy.internal:3128", proxy_username "nami",
    //   proxy_password "s3cr3t-proxy-pw", ip_version "any", remember_cookies true -> 201
    // body: has_proxy_password true, proxy_username "nami", remember_cookies true, no key containing
    //   "proxy_password" except has_proxy_password; body.to_string() does not contain "s3cr3t-proxy-pw".
    // row: select proxy_password_enc -> Some(bytes), bytes do not contain the plaintext,
    //   open_proxy_password(test_cipher(), id, &bytes) == "s3cr3t-proxy-pw".
    // GET as member and as viewer: never the plaintext; viewer sees proxy_username "***".
}

#[sqlx::test]
async fn patch_keeps_omitted_proxy_password_replaces_given_and_null_clears(pool: sqlx::PgPool) {
    // create with a proxy password; read proxy_password_enc = A.
    // PATCH {"name": "renamed", "group_id": null, "paused": true} -> proxy_password_enc still == A (byte for byte).
    // PATCH {"proxy_password": "new"} -> enc != A and opens to "new".
    // PATCH {"proxy_password": null} -> enc null, has_proxy_password false, username kept.
    // PATCH {"proxy_url": null} -> url, username and password all null.
}

#[sqlx::test]
async fn invalid_proxy_settings_are_400_and_write_nothing(pool: sqlx::PgPool) {
    // for each body: userinfo url, socks5 url, no port, username without url, password without username,
    //   proxy + ip_version "ipv4", ip_version "v6"
    //   -> 400 with the exact messages from Step 2; monitors count unchanged; on PATCH the row unchanged.
}

#[sqlx::test]
async fn proxy_secrets_never_reach_a_log_line(pool: sqlx::PgPool) {
    // LogCapture::install_trace(); create + patch + get with a proxy password; the plaintext is in no line.
}
```

`tests/migrations.rs`: assert the five columns exist.

- [ ] **Step 4: Run to verify failure**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --lib models:: && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test monitor_advanced_options --test migrations`
Expected: FAIL.

- [ ] **Step 5: Implement**

- `validate_proxy_url`: reject control/whitespace before trimming (like `validate_http`), trim, ≤ 2048 bytes, `reqwest::Url::parse`, scheme `http`|`https` else the scheme message (a parse failure gets the same message), `url_has_credentials` → credentials message, `port()` must be `Some` (explicit; `port_or_known_default` is not enough) else port message, `path() == "/" && query().is_none() && fragment().is_none()` else the "nothing after it" message; return the trimmed input as typed.
- `validate_proxy(c: &mut MonitorConfig) -> Result<(), String>` in `monitor.rs`, called from `validate_request_config` (http/keyword only): normalizes empty strings to `None`; applies the rules of Step 2 in the order: url → username (`validate_username`-like with `proxy_username` in messages) → password (`PasswordChange::Replace` needs username; length) → removing url clears username/password → removing username clears password → `ip_version != "any"` with a url → error.
- `validate`: check `ip_version` against `IP_VERSIONS` before the type match; `clear_unused_fields` resets `ip_version` to `"any"` outside `IP_VERSION_TYPES`; `clear_request_config` also clears `remember_cookies`, `proxy_url`, `proxy_username`, and `proxy_password` to `Clear` unless `Keep { present: false }`.
- `write_config`: append `ip_version = $34, remember_cookies = $35, proxy_url = $36, proxy_username = $37, proxy_password_enc = case when $38 then proxy_password_enc else $39 end` (keep flag + sealed bytes via `seal_proxy_password`, same pattern as the basic-auth password).
- `for_reader`: viewer gets `proxy_username` → `***`.
- `MonitorView::new`: `has_proxy_password = monitor.proxy_password_enc.is_some()`.

- [ ] **Step 6: Run to verify pass** — full engine suite green (other test binaries that build `Monitor { .. }` use `..Monitor::default()`; fix any literal the compiler names).

- [ ] **Step 7: Commit** (the roadmap item is ticked by NAMI-UP-24, when checks use the options)

```bash
git add nami-engine
git commit -m "feat(engine): ip version, cookie and proxy settings with a sealed proxy password (NAMI-UP-23)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 24 (NAMI-UP-24): Checker — IP version, per-check cookie jar, HTTP(S) proxy

Branch `feat/NAMI-UP-24-checker-advanced-options`, worktree `.worktrees/NAMI-UP-24/`.

**Files:**
- Create: `nami-engine/tests/checker_advanced_options.rs`, `nami-engine/tests/common/proxy.rs`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/uptime/checker.rs` (`GuardedResolver`, `HttpClients`, `build_client`, `build_request`, `probe_http`, `probe_tcp`, `describe_request_error`, new `resolve_target`, `NoAddressOfFamily`), `nami-engine/src/uptime/ping.rs` (`probe`, `resolve`), `nami-engine/src/uptime/tls.rs` (`connect` gains `ip_version`), `nami-engine/src/uptime/expiry.rs` (`probe_ssl` passes it), `nami-engine/src/uptime/mail.rs` (passes `IpVersion::Any`), `nami-engine/tests/common/mod.rs` (`pub mod proxy;`), `nami-engine/tests/uptime_checker_proxy.rs`
- Test: the files above plus `checker.rs` unit tests

**Interfaces:**
- Consumes: NAMI-UP-23's `IpVersion`, `Monitor::ip_version()`, `remember_cookies`, `proxy_url`, `proxy_username`, `proxy_password_enc`, `open_proxy_password`.
- Produces (in `uptime::checker`):
  - `pub struct NoAddressOfFamily(pub IpVersion)` (`Debug, Clone, Copy, PartialEq, Eq`, `Error`; `Display`: `no IPv4 address` / `no IPv6 address`).
  - `pub(crate) fn keep_family(resolved: impl IntoIterator<Item = SocketAddr>, ip: IpVersion) -> Result<Vec<SocketAddr>, NoAddressOfFamily>` (`Any` keeps everything; empty input stays `Ok(vec![])`; non-empty input with nothing of the family → `Err`).
  - `pub(crate) async fn resolve_target(host: &str, port: u16, ip: IpVersion) -> Result<Vec<SocketAddr>, String>`: `lookup_host` (error → `describe_io_error`) → `keep_family` (error → its text) → `allowed_addresses` (error → `DESTINATION_NOT_ALLOWED`). Used by `probe_tcp`, `tls::connect`, and (with its own "could not resolve {host}" wording kept) `ping::resolve`.
  - `GuardedResolver { ip_version: IpVersion }` applies `keep_family` before `allowed_addresses`.
  - `HttpClients::select(&self, follow_redirects: bool, verify_ssl: bool, carries_secrets: bool, ip_version: IpVersion) -> &reqwest::Client`; `CLIENT_COUNT = 18`.
  - No new error constant: a proxy password that cannot be opened reuses `CONFIG_DECRYPT_ERROR`, and nothing is sent.
  - `tls::connect(config, host, port, ip_version: IpVersion)`.

- [ ] **Step 1: Enable reqwest cookies**

```bash
cd nami-engine
cargo add reqwest@0.13.5 --features json,cookies
```

- [ ] **Step 2: Add the test proxy** — `tests/common/proxy.rs`: a raw-TCP HTTP proxy that records what it receives and answers with a fixed response:

```rust
pub struct RecordedRequest { pub request_line: String, pub proxy_authorization: Option<String> }

pub struct FakeProxy { pub port: u16, requests: Arc<Mutex<Vec<RecordedRequest>>> }

impl FakeProxy {
    /// Answers every request with `reply` (a full HTTP/1.1 response, e.g.
    /// "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok") and closes.
    pub async fn start(reply: &'static str) -> FakeProxy;
    pub fn requests(&self) -> Vec<RecordedRequest>;
}
```

(Read the head up to `\r\n\r\n`, at most 64 KiB; record line 1 and the `Proxy-Authorization` value, case-insensitive.)

- [ ] **Step 3: Write the failing unit tests** (`checker.rs`):

```rust
#[test]
fn keep_family_filters_without_fallback() {
    let v4 = address("192.0.2.1");
    let v6 = address("2001:db8::1");
    assert_eq!(keep_family([v4, v6], IpVersion::Any), Ok(vec![v4, v6]));
    assert_eq!(keep_family([v4, v6], IpVersion::V4), Ok(vec![v4]));
    assert_eq!(keep_family([v4, v6], IpVersion::V6), Ok(vec![v6]));
    assert_eq!(keep_family([v6], IpVersion::V4), Err(NoAddressOfFamily(IpVersion::V4)));
    assert_eq!(keep_family([v4], IpVersion::V6), Err(NoAddressOfFamily(IpVersion::V6)));
    assert_eq!(keep_family([], IpVersion::V4), Ok(vec![]));
    assert_eq!(NoAddressOfFamily(IpVersion::V4).to_string(), "no IPv4 address");
    assert_eq!(NoAddressOfFamily(IpVersion::V6).to_string(), "no IPv6 address");
}

#[test]
fn the_client_cache_has_a_slot_per_redirect_mode_tls_setting_and_ip_version() {
    let clients = HttpClients::new();
    for ip in [IpVersion::Any, IpVersion::V4, IpVersion::V6] {
        for (follow, secrets) in [(false, false), (true, false), (true, true)] {
            for verify in [true, false] {
                let a: *const reqwest::Client = clients.select(follow, verify, secrets, ip);
                let b: *const reqwest::Client = clients.select(follow, verify, secrets, ip);
                assert_eq!(a, b);
            }
        }
    }
    assert_eq!(clients.built(), 18);
}

/// reqwest must resolve the proxy host through the client's resolver, or the
/// guard against link-local proxies would be bypassed by a name.
#[tokio::test]
async fn the_proxy_host_is_resolved_by_the_clients_resolver() {
    #[derive(Clone, Default)]
    struct Recording(Arc<Mutex<Vec<String>>>);
    impl reqwest::dns::Resolve for Recording {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            self.0.lock().unwrap().push(name.as_str().to_string());
            Box::pin(async { Err::<reqwest::dns::Addrs, _>(Box::new(DestinationNotAllowed) as _) })
        }
    }
    let seen = Recording::default();
    let client = reqwest::Client::builder().no_proxy()
        .proxy(reqwest::Proxy::all("http://proxy.test:3128").unwrap())
        .dns_resolver(seen.clone()).build().unwrap();
    let error = client.get("http://target.test/").send().await.unwrap_err();
    assert!(caused_by::<DestinationNotAllowed>(&error), "{error:?}");
    assert_eq!(*seen.0.lock().unwrap(), vec!["proxy.test".to_string()], "only the proxy is resolved, never the target");
}
```

- [ ] **Step 4: Write the failing integration tests** — `tests/checker_advanced_options.rs` (pure checker tests with `common::{check_context, unsaved_monitor, with_basic_auth}`; no database):

```rust
#[tokio::test]
async fn an_ipv6_only_target_with_ipv4_is_down_without_fallback() {
    let ctx = check_context();
    let cases = [
        Monitor { url: Some("http://[::1]:9/".into()), ip_version: "ipv4".into(), ..unsaved_monitor("http") },
        Monitor { host: Some("::1".into()), port: Some(9), ip_version: "ipv4".into(), ..unsaved_monitor("tcp") },
        Monitor { host: Some("::1".into()), ip_version: "ipv4".into(), ..unsaved_monitor("ping") },
        Monitor { host: Some("::1".into()), port: Some(443), ip_version: "ipv4".into(), ..unsaved_monitor("ssl_expiry") },
    ];
    for monitor in cases {
        let outcome = perform_check(&ctx, &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{}", monitor.check_type);
        assert_eq!(outcome.error.as_deref(), Some("no IPv4 address"), "{}", monitor.check_type);
    }
    // And the mirror image.
    let v4_only = Monitor { host: Some("127.0.0.1".into()), port: Some(9), ip_version: "ipv6".into(), ..unsaved_monitor("tcp") };
    assert_eq!(perform_check(&ctx, &v4_only).await.error.as_deref(), Some("no IPv6 address"));
}

#[tokio::test]
async fn ip_version_picks_the_family_of_a_name() {
    // wiremock listens on 127.0.0.1 only.
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let url = format!("http://localhost:{}/", server.address().port());
    let ctx = check_context();
    let v4 = Monitor { url: Some(url.clone()), ip_version: "ipv4".into(), ..unsaved_monitor("http") };
    assert_eq!(perform_check(&ctx, &v4).await.status, CheckStatus::Up);
    let v6 = Monitor { url: Some(url), ip_version: "ipv6".into(), ..unsaved_monitor("http") };
    let outcome = perform_check(&ctx, &v6).await;
    // ::1 has no listener (connection refused) or localhost has no AAAA here: down either way, never up via 127.0.0.1.
    assert_eq!(outcome.status, CheckStatus::Down, "{:?}", outcome.error);
}

#[tokio::test]
async fn cookies_are_kept_across_redirects_of_one_check_only() {
    let server = MockServer::start().await;
    Mock::given(path("/login")).respond_with(ResponseTemplate::new(302)
        .insert_header("location", "/home").insert_header("set-cookie", "session=abc; Path=/"))
        .mount(&server).await;
    Mock::given(path("/home")).and(header("cookie", "session=abc"))
        .respond_with(ResponseTemplate::new(200)).with_priority(1).mount(&server).await;
    Mock::given(path("/home")).respond_with(ResponseTemplate::new(401)).with_priority(2).mount(&server).await;
    let ctx = check_context();
    let base = server.uri();
    let login = Monitor { url: Some(format!("{base}/login")), remember_cookies: true, ..unsaved_monitor("http") };
    assert_eq!(perform_check(&ctx, &login).await.status, CheckStatus::Up, "cookie carried to /home");
    // Without the jar the redirect hop has no cookie.
    let no_jar = Monitor { remember_cookies: false, ..login.clone() };
    assert_eq!(perform_check(&ctx, &no_jar).await.status_code, Some(401));
    // The jar died with the check: the same monitor and another one send no cookie later.
    let home = Monitor { url: Some(format!("{base}/home")), remember_cookies: true, ..login.clone() };
    assert_eq!(perform_check(&ctx, &home).await.status_code, Some(401));
    let other = Monitor { id: Uuid::new_v4(), url: Some(format!("{base}/home")), remember_cookies: true, ..unsaved_monitor("http") };
    assert_eq!(perform_check(&ctx, &other).await.status_code, Some(401));
}

#[tokio::test]
async fn checks_go_through_the_monitors_proxy_with_its_credentials() {
    let proxy = FakeProxy::start("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok").await;
    let monitor = with_proxy(Monitor { url: Some("http://target.test/health".into()), ..unsaved_monitor("http") },
                             &format!("http://127.0.0.1:{}", proxy.port), Some(("nami", "pr0xy")));
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let seen = proxy.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].request_line, "GET http://target.test/health HTTP/1.1");
    assert_eq!(seen[0].proxy_authorization.as_deref(), Some("Basic bmFtaTpwcjB4eQ=="));
}

#[tokio::test]
async fn proxy_errors_say_the_proxy_failed() {
    let dead = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let target = Monitor { url: Some("http://target.test/".into()), ..unsaved_monitor("http") };
    let unreachable = with_proxy(target.clone(), &format!("http://127.0.0.1:{dead}"), None);
    let error = perform_check(&check_context(), &unreachable).await.error.unwrap();
    assert!(error.starts_with("proxy connection failed"), "{error}");
    let denied = FakeProxy::start("HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
    let refused = with_proxy(target.clone(), &format!("http://127.0.0.1:{}", denied.port), Some(("nami", "wrong")));
    assert_eq!(perform_check(&check_context(), &refused).await.error.as_deref(), Some("proxy authentication failed (HTTP 407)"));
    // HTTPS targets tunnel with CONNECT: a 407 there is the same message.
    let tunnel = with_proxy(Monitor { url: Some("https://target.test/".into()), ..target.clone() },
                            &format!("http://127.0.0.1:{}", denied.port), None);
    assert_eq!(perform_check(&check_context(), &tunnel).await.error.as_deref(), Some("proxy authentication failed (HTTP 407)"));
    // The target's own answer stays the target's.
    let upstream = FakeProxy::start("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
    let failing = with_proxy(target, &format!("http://127.0.0.1:{}", upstream.port), None);
    assert_eq!(perform_check(&check_context(), &failing).await.error.as_deref(), Some("expected status 200-399, got 503"));
}

#[tokio::test]
async fn a_link_local_proxy_is_refused_before_any_connection() {
    for proxy in ["http://169.254.169.254:3128", "http://[fe80::1]:3128", "http://0.0.0.0:3128", "http://[::ffff:169.254.169.254]:80"] {
        let monitor = with_proxy(Monitor { url: Some("http://example.test/".into()), ..unsaved_monitor("http") }, proxy, None);
        assert_eq!(perform_check(&check_context(), &monitor).await.error.as_deref(), Some(DESTINATION_NOT_ALLOWED), "{proxy}");
    }
    // A target IP literal that is forbidden stays refused behind a proxy.
    let monitor = with_proxy(Monitor { url: Some("http://169.254.169.254/latest/".into()), ..unsaved_monitor("http") },
                             "http://127.0.0.1:9", None);
    assert_eq!(perform_check(&check_context(), &monitor).await.error.as_deref(), Some(DESTINATION_NOT_ALLOWED));
}

#[tokio::test]
async fn an_undecryptable_proxy_password_sends_nothing() {
    let proxy = FakeProxy::start("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
    let mut monitor = with_proxy(Monitor { url: Some("http://target.test/".into()), ..unsaved_monitor("http") },
                                 &format!("http://127.0.0.1:{}", proxy.port), Some(("nami", "pw")));
    monitor.id = Uuid::new_v4(); // sealed for another id
    assert_eq!(perform_check(&check_context(), &monitor).await.error.as_deref(), Some(CONFIG_DECRYPT_ERROR));
    assert!(proxy.requests().is_empty());
}
```

`with_proxy(monitor, url, credentials: Option<(&str, &str)>) -> Monitor` goes into `tests/common/mod.rs` next to `with_basic_auth` (password sealed with `seal_proxy_password` for `monitor.id`).

`tests/uptime_checker_proxy.rs`: inside the single test, after the existing loop, add one monitor with an explicit proxy (`FakeProxy` answering 200) while `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` point at the dead proxy → up, and the fake proxy saw the request (explicit proxy wins, environment still ignored).

- [ ] **Step 5: Run to verify failure**

Run: `cd nami-engine && cargo test --lib uptime::checker && cargo test --test checker_advanced_options --test uptime_checker_proxy`
Expected: FAIL (compile errors on `IpVersion` params, `with_proxy`).

- [ ] **Step 6: Implement**

- `ClientOptions { redirects: RedirectMode, verify_ssl: bool, ip_version: IpVersion, proxy: Option<reqwest::Proxy>, cookies: bool }`; `build_client(&ClientOptions) -> reqwest::Client` keeps every existing builder call in the same order (`.no_proxy()` first, then `.proxy(p)` only when `Some`), `.dns_resolver(GuardedResolver { ip_version })`, and `.cookie_provider(Arc::new(reqwest::cookie::Jar::default()))` when `cookies`. The cache calls it with `proxy: None, cookies: false`.
- `build_request`: after opening headers and the basic-auth password, if `monitor.proxy_url` is `Some`: parse it; `names_forbidden_address(&proxy)` → `Err(DESTINATION_NOT_ALLOWED)`; open the proxy password (error → `CONFIG_DECRYPT_ERROR`); `reqwest::Proxy::all(url)` with `.basic_auth(user, pass)` when a username is set. If a proxy or `remember_cookies` is set → `build_client(&ClientOptions { …, proxy, cookies: monitor.remember_cookies })` owned by the request (return the client with the builder, e.g. `Result<(Option<reqwest::Client>, reqwest::RequestBuilder), String>`, so the per-check client lives as long as the request); otherwise `ctx.http.select(.., monitor.ip_version())`.
- `probe_http`: when the monitor has a proxy, a response with status 407 → `Observation::down(Some(407), …, "proxy authentication failed (HTTP 407)")`; send errors go through `describe_proxied_error(&e)`:

```rust
/// A send error of a check that went through a proxy: which side failed.
fn describe_proxied_error(error: &reqwest::Error) -> String {
    if caused_by::<DestinationNotAllowed>(error) { return DESTINATION_NOT_ALLOWED.to_string(); }
    // hyper-util's tunnel errors display as "tunnel error: <reason>".
    match chain_message_with_prefix(error, "tunnel error: ") {
        Some(reason) if reason == "proxy authorization required" => "proxy authentication failed (HTTP 407)".to_string(),
        Some(reason) if reason == "failed to create underlying connection" =>
            format!("proxy connection failed: {}", root_cause(error)),
        Some(reason) => format!("proxy tunnel failed: {reason}"),
        None if error.is_connect() && !caused_by::<rustls::Error>(error) =>
            format!("proxy connection failed: {}", root_cause(error)),
        None => describe_request_error(error),
    }
}
```

  (`chain_message_with_prefix` walks `source()` and returns the remainder of the first `to_string()` that starts with the prefix.)
- `describe_request_error`: first `if let Some(e) = find_cause::<NoAddressOfFamily>(error) { return e.to_string(); }` (generic `find_cause` beside `caused_by`).
- `probe_tcp` → `resolve_target(host, port, monitor.ip_version())`; `tls::connect(.., ip_version)` → `resolve_target`; `expiry::probe_ssl` passes `monitor.ip_version()`, `mail::probe` passes `IpVersion::Any`; `ping::probe` → `resolve(host, monitor.ip_version())` applying `keep_family` (literals included) before `allowed_addresses`, error text from `NoAddressOfFamily`.
- Update the doc comments of `HttpClients` and `build_client` (the "Proxy settings … are ignored" bullet now reads: environment proxies are ignored; a monitor's own proxy is used only for that monitor's checks, on a client built for that one check).

- [ ] **Step 7: Run to verify pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test` (full suite; `uptime_checker_proxy` stays its own binary) and `cargo clippy --all-targets -- -D warnings`.
Expected: green.

- [ ] **Step 8: Tick "Advanced request options: IP version (any/IPv4/IPv6), remember cookies within a check, HTTP(S) proxy with encrypted credentials" and commit**

```bash
git add nami-engine docs/superpowers/roadmap.md
git commit -m "feat(engine): checks honour ip version, a per-check cookie jar and the monitor's proxy (NAMI-UP-24)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 25 (NAMI-UP-25): Import parsers — URL list, Uptime Kuma backup, UptimeRobot CSV

Branch `feat/NAMI-UP-25-import-parsers`, worktree `.worktrees/NAMI-UP-25/`. Pure functions, no database; may run in parallel with NAMI-UP-23/24.

**Files:**
- Create: `nami-engine/src/uptime/import/mod.rs`, `urls.rs`, `kuma.rs`, `uptimerobot.rs`, `nami-engine/tests/fixtures/import/uptime_kuma.json`, `nami-engine/tests/fixtures/import/uptimerobot.csv`, `nami-engine/tests/monitor_import.rs`
- Modify: `nami-engine/Cargo.toml` (+ lock), `nami-engine/src/uptime/mod.rs`

**Interfaces:**
- Produces (`uptime::import`):
  - `pub const MAX_ENTRIES: usize = 500;` `pub const MAX_IMPORT_BODY_BYTES: usize = 2 * 1024 * 1024;`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)] #[serde(rename_all = "snake_case")] pub enum ImportFormat { Urls, UptimeKuma, Uptimerobot }`
  - `#[derive(Debug, Clone, Default, PartialEq, Deserialize)] pub struct ImportDefaults { pub interval_seconds: Option<i32>, pub timeout_seconds: Option<i32>, pub confirmation_threshold: Option<i32>, pub recovery_threshold: Option<i32>, pub group_id: Option<Uuid> }`
  - `#[derive(Debug, Clone, PartialEq)] pub struct ParsedEntry { pub line: usize, pub body: Result<serde_json::Map<String, Value>, String> }` — `body` is exactly a `POST /projects/{id}/monitors` JSON object (secrets included, in memory only) or a per-entry error.
  - `#[derive(Debug, PartialEq)] pub enum ParseError { TooManyEntries(usize), Malformed(String) }` (`Display`: `at most 500 entries per import, got N` / the message).
  - `pub fn parse(format: ImportFormat, content: &str, defaults: &ImportDefaults) -> Result<Vec<ParsedEntry>, ParseError>` — dispatches to `urls::parse`, `kuma::parse`, `uptimerobot::parse` (each `fn parse(content: &str) -> Result<Vec<ParsedEntry>, ParseError>`), then applies defaults to every `Ok` body (`entry.entry(key).or_insert(value)` for the numbers; `group_id` always set when given).

- [ ] **Step 1: Add the crate**

```bash
cd nami-engine
cargo add csv@1.3
```

- [ ] **Step 2: Add the fixtures**

`tests/fixtures/import/uptime_kuma.json` — shape of an Uptime Kuma 1.23 backup (secrets are dummies):

```json
{
  "version": "1.23.16",
  "notificationList": [
    { "id": 1, "name": "Ops Slack", "config": "{\"slackwebhookURL\":\"https://hooks.slack.com/services/KUMA-NOTIFY-SECRET\"}", "active": true }
  ],
  "proxyList": [],
  "monitorList": [
    { "id": 1, "name": "Website", "type": "http", "url": "https://example.com/", "method": "GET",
      "interval": 20, "maxretries": 2, "ignoreTls": false, "maxredirects": 10, "active": true,
      "accepted_statuscodes": ["200-299"], "timeout": 16, "upsideDown": false,
      "headers": "{\"X-Api-Key\": \"kuma-header-secret\"}", "body": null,
      "authMethod": "basic", "basic_auth_user": "ops", "basic_auth_pass": "kuma-basic-secret",
      "notificationIDList": { "1": true } },
    { "id": 2, "name": "Pricing keyword", "type": "keyword", "url": "https://example.com/pricing",
      "method": "GET", "keyword": "Sold out", "invertKeyword": true, "interval": 60, "maxretries": 0,
      "ignoreTls": true, "maxredirects": 0, "active": 0, "accepted_statuscodes": ["200-299", "300-399"] },
    { "id": 3, "name": "Postgres", "type": "port", "hostname": "db.internal", "port": 5432, "interval": 30, "maxretries": 20, "active": 1 },
    { "id": 4, "name": "Gateway", "type": "ping", "hostname": "10.0.0.1", "interval": 45, "maxretries": 1, "active": true },
    { "id": 5, "name": "MX", "type": "dns", "hostname": "example.com", "dns_resolve_type": "MX",
      "dns_resolve_server": "1.1.1.1", "port": 53, "interval": 300, "maxretries": 0, "active": true },
    { "id": 6, "name": "Container", "type": "docker", "docker_container": "web", "interval": 60, "active": true },
    { "id": 7, "name": "Upside", "type": "http", "url": "https://example.org/", "upsideDown": true, "interval": 60, "active": true },
    { "id": 8, "name": "NTLM", "type": "http", "url": "https://intranet.test/", "authMethod": "ntlm", "interval": 60, "active": true }
  ]
}
```

`tests/fixtures/import/uptimerobot.csv` (reconstructed; see Open questions 1-2):

```csv
Friendly Name,URL/IP,Type,Keyword Type,Keyword Value,Port,Interval
Website,https://example.com/,HTTP(s),,,,300
Login page,https://example.com/login,Keyword,not exists,Error,,60
"Mail, primary",mail.example.com,Port,,,25,120
Router,192.0.2.1,Ping,,,,10
Heartbeat job,https://heartbeat.uptimerobot.com/x,Heartbeat,,,,300
=HYPERLINK("http://evil.test"),https://example.net/,HTTP(s),,,,300
```

- [ ] **Step 3: Write the failing parser tests** — `tests/monitor_import.rs` (no `#[sqlx::test]` yet; NAMI-UP-26 adds API tests to the same file):

```rust
use nami_engine::uptime::import::{parse, ImportDefaults, ImportFormat, ParseError, MAX_ENTRIES};
use serde_json::{json, Value};

fn bodies(format: ImportFormat, content: &str) -> Vec<(usize, Result<Value, String>)> {
    parse(format, content, &ImportDefaults::default()).unwrap().into_iter()
        .map(|e| (e.line, e.body.map(Value::Object))).collect()
}

#[test]
fn url_lists_skip_blanks_and_comments_and_name_monitors_after_the_host() {
    let got = bodies(ImportFormat::Urls, "# prod\nhttps://example.com/health\n\n  http://api.example.com:8080/v1  \nnot a url\nftp://x.test/\n");
    assert_eq!(got, vec![
        (2, Ok(json!({"check_type": "http", "name": "example.com", "url": "https://example.com/health"}))),
        (4, Ok(json!({"check_type": "http", "name": "api.example.com", "url": "http://api.example.com:8080/v1"}))),
        (5, Err("line is not a valid http(s) URL".to_string())),
        (6, Err("line is not a valid http(s) URL".to_string())),
    ]);
}

#[test]
fn more_than_500_entries_are_refused_before_anything_else() {
    let many: String = (0..=MAX_ENTRIES).map(|i| format!("https://h{i}.test/\n")).collect();
    assert_eq!(parse(ImportFormat::Urls, &many, &ImportDefaults::default()), Err(ParseError::TooManyEntries(501)));
    let exactly: String = (0..MAX_ENTRIES).map(|i| format!("https://h{i}.test/\n# c\n\n")).collect();
    assert_eq!(parse(ImportFormat::Urls, &exactly, &ImportDefaults::default()).unwrap().len(), 500);
}

#[test]
fn defaults_fill_what_the_source_leaves_out() {
    let group = uuid::Uuid::new_v4();
    let defaults = ImportDefaults { interval_seconds: Some(120), timeout_seconds: Some(5), confirmation_threshold: Some(3), recovery_threshold: Some(2), group_id: Some(group) };
    let entries = parse(ImportFormat::Urls, "https://a.test/", &defaults).unwrap();
    assert_eq!(Value::Object(entries[0].body.clone().unwrap()), json!({
        "check_type": "http", "name": "a.test", "url": "https://a.test/", "interval_seconds": 120,
        "timeout_seconds": 5, "confirmation_threshold": 3, "recovery_threshold": 2, "group_id": group }));
}

#[test]
fn uptime_kuma_backups_map_every_supported_type() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    let got = bodies(ImportFormat::UptimeKuma, &content);
    assert_eq!(got[0], (1, Ok(json!({
        "name": "Website", "check_type": "http", "url": "https://example.com/", "http_method": "GET",
        "interval_seconds": 30, "timeout_seconds": 16, "confirmation_threshold": 3,
        "verify_ssl": true, "follow_redirects": true, "paused": false,
        "expected_status_min": 200, "expected_status_max": 299,
        "request_headers": [{"name": "X-Api-Key", "value": "kuma-header-secret"}],
        "auth_username": "ops", "auth_password": "kuma-basic-secret" }))));
    assert_eq!(got[1], (2, Ok(json!({
        "name": "Pricing keyword", "check_type": "keyword", "url": "https://example.com/pricing", "http_method": "GET",
        "keyword": "Sold out", "keyword_mode": "not_contains", "interval_seconds": 60, "confirmation_threshold": 1,
        "verify_ssl": false, "follow_redirects": false, "paused": true,
        "expected_status_min": 200, "expected_status_max": 299 }))));
    assert_eq!(got[2], (3, Ok(json!({ "name": "Postgres", "check_type": "tcp", "host": "db.internal", "port": 5432,
        "interval_seconds": 30, "confirmation_threshold": 10, "paused": false }))));
    assert_eq!(got[3], (4, Ok(json!({ "name": "Gateway", "check_type": "ping", "host": "10.0.0.1",
        "interval_seconds": 45, "confirmation_threshold": 2, "paused": false }))));
    assert_eq!(got[4], (5, Ok(json!({ "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX",
        "dns_nameserver": "1.1.1.1:53", "interval_seconds": 300, "confirmation_threshold": 1, "paused": false }))));
    assert_eq!(got[5], (6, Err("Uptime Kuma monitor type \"docker\" is not supported".to_string())));
    assert_eq!(got[6], (7, Err("upside-down mode is not supported".to_string())));
    assert_eq!(got[7], (8, Err("authMethod \"ntlm\" is not supported (only basic)".to_string())));
}

#[test]
fn kuma_notification_secrets_never_leave_the_parser() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptime_kuma.json").unwrap();
    let rendered = format!("{:?}", parse(ImportFormat::UptimeKuma, &content, &ImportDefaults::default()));
    assert!(!rendered.contains("KUMA-NOTIFY-SECRET") && !rendered.contains("hooks.slack.com"));
}

#[test]
fn malformed_kuma_documents_are_one_error_and_bad_entries_their_own() {
    assert_eq!(parse(ImportFormat::UptimeKuma, "{not json", &ImportDefaults::default()),
               Err(ParseError::Malformed("content is not valid JSON (line 1, column 2)".into())));
    assert_eq!(parse(ImportFormat::UptimeKuma, "{\"version\":\"1.23\"}", &ImportDefaults::default()),
               Err(ParseError::Malformed("content has no monitorList array (is it an Uptime Kuma backup?)".into())));
    // A secret in a malformed document is never quoted back (serde would quote a wrong-typed value).
    let err = parse(ImportFormat::UptimeKuma, "{\"monitorList\": \"hunter2\"}", &ImportDefaults::default()).unwrap_err();
    assert!(!err.to_string().contains("hunter2"));
    let got = bodies(ImportFormat::UptimeKuma, r#"{"monitorList":[42, {"type":"http"}, {"type":"http","name":"x","url":"https://x.test","headers":"[1]"}]}"#);
    assert_eq!(got[0].1, Err("entry is not a JSON object".to_string()));
    assert_eq!(got[1].1, Err("entry has no name".to_string()));
    assert_eq!(got[2].1, Err("headers is not a JSON object of strings".to_string()));
    let many = format!("{{\"monitorList\":[{}]}}", vec!["{}"; 501].join(","));
    assert_eq!(parse(ImportFormat::UptimeKuma, &many, &ImportDefaults::default()), Err(ParseError::TooManyEntries(501)));
}

#[test]
fn uptimerobot_csv_maps_by_header_name() {
    let content = std::fs::read_to_string("tests/fixtures/import/uptimerobot.csv").unwrap();
    let got = bodies(ImportFormat::Uptimerobot, &content);
    assert_eq!(got[0], (2, Ok(json!({"name": "Website", "check_type": "http", "url": "https://example.com/", "interval_seconds": 300}))));
    assert_eq!(got[1], (3, Ok(json!({"name": "Login page", "check_type": "keyword", "url": "https://example.com/login",
        "keyword": "Error", "keyword_mode": "not_contains", "interval_seconds": 60}))));
    assert_eq!(got[2], (4, Ok(json!({"name": "Mail, primary", "check_type": "tcp", "host": "mail.example.com", "port": 25, "interval_seconds": 120}))));
    assert_eq!(got[3], (5, Ok(json!({"name": "Router", "check_type": "ping", "host": "192.0.2.1", "interval_seconds": 30}))));
    assert_eq!(got[4], (6, Err("UptimeRobot monitor type \"Heartbeat\" is not supported".to_string())));
    // A formula-looking name is just a name; validation (NAMI-UP-26) judges it like any other.
    assert_eq!(got[5].1.as_ref().unwrap()["name"], "=HYPERLINK(\"http://evil.test\")");
}

#[test]
fn uptimerobot_headers_match_case_insensitively_and_unknown_columns_are_ignored() {
    let got = bodies(ImportFormat::Uptimerobot, "friendly name,url/ip,TYPE,Notes\nA,https://a.test/,1,ignored\n");
    assert_eq!(got, vec![(2, Ok(json!({"name": "A", "check_type": "http", "url": "https://a.test/"})))]);
    assert_eq!(parse(ImportFormat::Uptimerobot, "Name,Something\nx,y\n", &ImportDefaults::default()),
               Err(ParseError::Malformed("CSV header must name the columns Friendly Name, URL/IP and Type".into())));
    assert!(matches!(parse(ImportFormat::Uptimerobot, "", &ImportDefaults::default()), Err(ParseError::Malformed(_))));
    // An unterminated quote is a malformed document, not a panic.
    assert!(matches!(parse(ImportFormat::Uptimerobot, "Friendly Name,URL/IP,Type\n\"a,https://a.test,1\n", &ImportDefaults::default()), Err(ParseError::Malformed(_))));
}
```

- [ ] **Step 4: Run to verify failure**

Run: `cd nami-engine && cargo test --test monitor_import`
Expected: FAIL (module missing).

- [ ] **Step 5: Implement**

- `urls::parse`: `content.lines().enumerate()`; skip `trim().is_empty()` and `trim_start().starts_with('#')`; count kept lines first and return `TooManyEntries(n)` when `n > MAX_ENTRIES`; for each: `reqwest::Url::parse(trimmed)` with scheme http/https and a host, else `Err("line is not a valid http(s) URL")`; body `{check_type: "http", name: host_str, url: trimmed}` (the URL text as typed, so validation judges the same string).
- `kuma::parse`: `serde_json::from_str::<Value>` → on error `Malformed(format!("content is not valid JSON (line {}, column {})", e.line(), e.column()))`; `monitorList` must be an array (else the "no monitorList" message); length > 500 → `TooManyEntries`; per entry (`line = index + 1`), with a `fn str_field(&Map, &str) -> Option<&str>` helper that never echoes values in errors:
  - not an object → "entry is not a JSON object"; no non-empty `name` → "entry has no name"; `upsideDown == true` → upside-down error; type mapping: `http`, `keyword` (needs `keyword`), `port` → `tcp`, `ping`, `dns` (record type must be in `DNS_RECORD_TYPES`, else `Uptime Kuma DNS record type "X" is not supported`), any other → the "type … is not supported" error.
  - http/keyword: `url`, `http_method` = uppercase `method` (default GET), `body` when a non-empty string, `headers` (string parsed as an object of strings → `[{name, value}]`, sorted by name; anything else the headers error), basic auth only when `authMethod` is null/""/"basic" and `basic_auth_user` non-empty (other `authMethod` with credentials → the authMethod error), `accepted_statuscodes[0]` as `"a-b"` or `"a"` → `expected_status_min/max` (unparseable → per-entry error `accepted_statuscodes "X" is not a status range`), `ignoreTls` → `verify_ssl = !ignoreTls`, `maxredirects == 0` → `follow_redirects = false` (else true), `invertKeyword` → `keyword_mode: not_contains`.
  - all: `interval_seconds = max(30, interval)` (absent → omitted, defaults apply), `confirmation_threshold = min(10, maxretries + 1)` (absent → omitted), `timeout_seconds` when `timeout` is a number: `clamp(round(timeout), 1, min(60, interval - 1))`, `paused = !active` (`active` bool or 0/1; absent → `false`).
  - tcp/ping/dns: `host` from `hostname`; tcp `port`; dns `dns_nameserver = "{server}:{port}"` when `dns_resolve_server` is set (`port` default 53).
  - Read nothing else from the document.
- `uptimerobot::parse`: `csv::ReaderBuilder::new().flexible(true).from_reader(content.as_bytes())`; header aliases (lowercased, trimmed): name ← `friendly name`|`name`; target ← `url/ip`|`url`|`ip`|`host`; type ← `type`|`monitor type`; keyword type ← `keyword type`; keyword ← `keyword value`|`keyword`; port ← `port`; interval ← `interval`|`monitoring interval`. Name/target/type required else the header message. Count records before mapping (>500 → `TooManyEntries`). `line = record.position().line()`. Type: `http(s)`|`http`|`https`|`1` → http (target as `url`), `keyword`|`2` → keyword (`keyword type` `not exists`|`2` → `not_contains`, else `contains`), `ping`|`3` → ping (target as `host`), `port`|`4` → tcp (target as `host`, `port` required → per-entry error `port is required for Port monitors`); other → the type error quoting the cell through `printable(cell, 40)`. Interval cell → `max(30, n)` when it parses as an integer. A csv error → `Malformed(format!("CSV could not be read: {e}"))` (csv errors carry positions, not cell contents).

- [ ] **Step 6: Run to verify pass** — `cargo test --test monitor_import` green; `cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 7: Commit**

```bash
git add nami-engine
git commit -m "feat(engine): parsers for URL lists, Uptime Kuma backups and UptimeRobot CSV (NAMI-UP-25)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 26 (NAMI-UP-26): `POST /projects/{id}/monitors/import`

Branch `feat/NAMI-UP-26-monitor-import`, worktree `.worktrees/NAMI-UP-26/`.

**Files:**
- Modify: `nami-engine/src/uptime/import/mod.rs` (`run_import`), `nami-engine/src/routes/monitors.rs` (route + handler)
- Test: `nami-engine/tests/monitor_import.rs` (API tests)

**Interfaces:**
- Consumes: NAMI-UP-25 `parse`, `ImportFormat`, `ImportDefaults`, `ParseError`, `MAX_IMPORT_BODY_BYTES`; `CreateMonitorRequest` (deserialized from each body), `monitor::{validate, insert_monitor, list_for_project, redact_url}`, `monitor_group::belongs_to`.
- Produces:
  - `#[derive(Deserialize)] pub struct ImportRequest { pub format: ImportFormat, pub content: String, #[serde(default)] pub defaults: ImportDefaults, #[serde(default)] pub dry_run: bool, #[serde(default = "default_true")] pub skip_existing: bool }`
  - `#[derive(Debug, Serialize, PartialEq)] #[serde(rename_all = "lowercase")] pub enum EntryStatus { Created, Skipped, Error }`
  - `#[derive(Debug, Serialize)] pub struct ImportResultRow { pub line: usize, pub status: EntryStatus, #[serde(skip_serializing_if = "Option::is_none")] pub name: Option<String>, … target, monitor_id: Option<Uuid>, error: Option<String> }`
  - `#[derive(Debug, Serialize)] pub struct ImportReport { pub dry_run: bool, pub created: usize, pub skipped: usize, pub errors: usize, pub results: Vec<ImportResultRow> }`
  - `pub async fn run_import(pool: &PgPool, cipher: &Cipher, project_id: Uuid, request: ImportRequest) -> Result<ImportReport, AppError>`
  - `pub fn existing_key(c: &MonitorConfig) -> String` (the skip key from the rulings).
  - Route `POST /projects/{id}/monitors/import` with `DefaultBodyLimit::max(MAX_IMPORT_BODY_BYTES)` on that route only.

- [ ] **Step 1: Write the failing API tests** (append to `tests/monitor_import.rs`, `mod common;` at the top):

```rust
async fn import(app: &Router, project: Uuid, cookie: &HeaderValue, body: Value) -> (StatusCode, Value) {
    call(app, "POST", &format!("/projects/{project}/monitors/import"), Some(cookie), Some(body)).await
}

async fn monitor_count(pool: &sqlx::PgPool, project: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from monitors where project_id = $1").bind(project).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn a_url_list_creates_monitors_and_reports_every_line(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, body) = import(&app, project, &member, json!({
        "format": "urls",
        "content": "https://a.test/\n# skip me\nhttps://b.test/x?token=abc\nnot-a-url\nhttps://a.test/\n",
        "defaults": { "interval_seconds": 120 }
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], false);
    assert_eq!((body["created"].as_u64(), body["skipped"].as_u64(), body["errors"].as_u64()), (Some(2), Some(1), Some(1)));
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["line"], 1);
    assert_eq!(results[0]["status"], "created");
    assert!(results[0]["monitor_id"].is_string());
    assert_eq!(results[1]["target"], "https://b.test/x?token=***", "targets are redacted");
    assert_eq!(results[2], json!({ "line": 4, "status": "error", "error": "line is not a valid http(s) URL" }));
    assert_eq!(results[3]["status"], "skipped");
    assert_eq!(results[3]["error"], "already monitored");
    assert_eq!(monitor_count(&pool, project).await, 2);
    let interval: i32 = sqlx::query_scalar("select interval_seconds from monitors where project_id = $1 limit 1")
        .bind(project).fetch_one(&pool).await.unwrap();
    assert_eq!(interval, 120);
}

#[sqlx::test]
async fn importing_the_same_list_twice_creates_nothing_the_second_time(pool: sqlx::PgPool) {
    // import "https://a.test/\nhttps://b.test/" twice -> second report created 0, skipped 2; count stays 2.
    // with "skip_existing": false the third import creates 2 more (count 4).
}

#[sqlx::test]
async fn a_dry_run_validates_everything_and_creates_nothing(pool: sqlx::PgPool) {
    // dry_run true on the Kuma fixture -> dry_run true, statuses for lines 1-5 "created" without monitor_id,
    // lines 6-8 "error"; monitor_count == 0.
}

#[sqlx::test]
async fn kuma_secrets_are_sealed_never_echoed_and_notifications_never_stored(pool: sqlx::PgPool) {
    // import the Kuma fixture -> 200; response text contains none of "kuma-header-secret",
    //   "kuma-basic-secret", "KUMA-NOTIFY-SECRET", "hooks.slack.com".
    // the "Website" row: request_header_names == ["X-Api-Key"], request_headers_enc / auth_password_enc open
    //   (open_headers / open_password with test_cipher) to the fixture values, raw bytes don't contain them.
    // A full-text dump of every monitors row (select to_jsonb(m)::text from monitors m where project_id = $1)
    //   contains neither notification string.
}

#[sqlx::test]
async fn every_entry_is_validated_like_a_single_create(pool: sqlx::PgPool) {
    // urls content with a header-injection attempt is impossible in a URL list, so use Kuma:
    // monitorList with headers {"X-Bad\r\nInjected": "v"}, headers {"Host": "evil"}, url "javascript:alert(1)",
    //   name "\u{202E}gnp.exe", interval 10 (raised to 30), method "TRACE"
    // -> each of the bad ones "error" with the exact single-create message
    //    (copy from POST /projects/{id}/monitors for the same body), the interval one "created" with interval 30.
}

#[sqlx::test]
async fn import_limits_are_enforced_before_anything_is_created(pool: sqlx::PgPool) {
    // 501 URLs -> 400 {"error": "at most 500 entries per import, got 501"}, count 0.
    // content of 2 MiB + 1 byte (body over the route limit) -> 413, count 0.
    // malformed Kuma JSON -> 400 with the parser message; unknown format "pingdom" -> 400 (axum rejection), count 0.
}

#[sqlx::test]
async fn import_rbac_and_foreign_groups(pool: sqlx::PgPool) {
    // viewer -> 403, outsider -> 403 (count 0 both); defaults.group_id of another project -> 404, count 0;
    // own group -> every created monitor has that group_id.
}

#[sqlx::test]
async fn one_failing_entry_never_rolls_back_the_others(pool: sqlx::PgPool) {
    // 3 URLs; delete the default group between parse and insert is hard to arrange, so instead:
    // entries 1 and 3 valid, entry 2 a URL longer than 2048 bytes -> created, error, created; count 2.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test monitor_import`
Expected: FAIL (404 from the router for the new path / compile errors).

- [ ] **Step 3: Implement `run_import`**

```rust
pub async fn run_import(pool: &PgPool, cipher: &Cipher, project_id: Uuid, request: ImportRequest)
    -> Result<ImportReport, AppError>
{
    if let Some(group) = request.defaults.group_id {
        if !monitor_group::belongs_to(pool, project_id, group).await? { return Err(AppError::NotFound); }
    }
    let entries = parse(request.format, &request.content, &request.defaults)
        .map_err(|e| AppError::Validation(e.to_string()))?;
    let mut seen: HashSet<String> = if request.skip_existing {
        monitor::list_for_project(pool, project_id).await?.iter()
            .map(|m| existing_key(&MonitorConfig::from(m))).collect()
    } else { HashSet::new() };
    let mut report = ImportReport { dry_run: request.dry_run, created: 0, skipped: 0, errors: 0, results: Vec::new() };
    for entry in entries {
        // body -> CreateMonitorRequest (serde_json::from_value; its error text is replaced by
        // "entry could not be read as a monitor" so no value is quoted) -> into_config -> validate
        // -> skip check (insert the key into `seen` when not skipping, so repeats inside the input skip)
        // -> dry_run ? row Created without id : insert_monitor (own transaction) -> Created with id.
        // A 23503 from insert (group deleted meanwhile) -> row Error "group not found".
    }
    Ok(report)
}
```

`name` = validated name; `target` = `redact_url(url)` for http/keyword, `host` or `host:port` otherwise. Errors from `validate` are used verbatim (they never repeat secrets — Phase 2 property). Handler: `require_project_role(.., "member")` first, then `Json<ImportRequest>`... note the extractor runs before the handler body, so put `Json` last in the argument list and accept that a malformed body is a 400 even for a viewer (same as every other route; documented in `routes/monitors.rs`'s header comment).

- [ ] **Step 4: Run to verify pass** — the binary and then the full suite green.

- [ ] **Step 5: Tick "Monitor bulk import: URL list (<= 500, per-line results), Uptime Kuma JSON, UptimeRobot CSV" and commit**

```bash
git add nami-engine docs/superpowers/roadmap.md
git commit -m "feat(engine): bulk monitor import with dry run and skip-existing (NAMI-UP-26)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 27 (NAMI-UP-27): Availability (SLA) report

Branch `feat/NAMI-UP-27-availability`, worktree `.worktrees/NAMI-UP-27/`. Independent of NAMI-UP-23..26.

**Files:**
- Create: `nami-engine/src/uptime/availability.rs`, `nami-engine/tests/availability.rs`
- Modify: `nami-engine/src/models/maintenance.rs` (`Span`, `Schedule::spans`), `nami-engine/src/uptime/mod.rs`, `nami-engine/src/routes/monitors.rs`, `nami-engine/src/routes/heartbeats.rs`

**Interfaces:**
- Produces:
  - `models::maintenance::Span { pub start: DateTime<Utc>, pub end: DateTime<Utc> }` (`Debug, Clone, Copy, PartialEq, Eq`; half-open); `Schedule::spans(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span>` (sorted, clipped to `[from, to)`, possibly overlapping); `MaintenanceWindow::spans(&self, from, to) -> Vec<Span>` (empty when `schedule()` is `None`).
  - `uptime::availability::{MAX_RANGE_DAYS: i64 = 1096, union(spans: Vec<Span>) -> Vec<Span>, subtract(spans: &[Span], minus: &[Span]) -> Vec<Span>, seconds(spans: &[Span]) -> i64}`.
  - `#[derive(Debug, Clone, PartialEq, Serialize)] pub struct Availability { pub from: DateTime<Utc>, pub to: DateTime<Utc>, pub total_seconds: i64, pub maintenance_seconds: i64, pub downtime_seconds: i64, pub uptime_pct: Option<f64>, pub incidents: u32, pub longest_incident_seconds: i64 }`.
  - `pub fn compute(total: Span, incidents: &[Span], maintenance: &[Span]) -> Availability` (pure; `total` already clipped; an empty or inverted `total` → zeros, `uptime_pct: None`).
  - `pub fn check_range(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<(), String>` (`"from must be before to"`, `"the range must be at most 1096 days"`).
  - `pub async fn for_monitor(pool, project_id, monitor_id, from, to, now) -> Result<Option<Availability>, sqlx::Error>` and `for_heartbeat(...)` (`None` = not in project). **This is the one function Incident Management's SLA report reuses.**
  - Routes (viewer): `GET /projects/{id}/monitors/{monitor_id}/availability?from&to`, `GET /projects/{id}/heartbeats/{heartbeat_id}/availability?from&to`; `from`/`to` RFC 3339 (`Query<RangeQuery { from: DateTime<Utc>, to: DateTime<Utc> }>`).

- [ ] **Step 1: Write the failing unit tests** (`availability.rs` `mod tests`, using `fn t(h: i64) -> DateTime<Utc>` = a fixed epoch + h hours and `fn s(a, b) -> Span`):

```rust
#[test]
fn union_merges_overlapping_and_touching_spans() {
    assert_eq!(union(vec![s(5, 7), s(0, 2), s(1, 3), s(3, 4)]), vec![s(0, 4), s(5, 7)]);
    assert_eq!(union(vec![]), vec![]);
}

#[test]
fn subtract_cuts_holes() {
    assert_eq!(subtract(&[s(0, 10)], &[s(2, 3), s(5, 12)]), vec![s(0, 2), s(3, 5)]);
    assert_eq!(subtract(&[s(0, 1)], &[s(0, 1)]), vec![]);
}

#[test]
fn overlapping_incidents_and_maintenance_count_each_second_once() {
    // range 0..100h; incidents 10-30, 20-40 (overlap) and one still open from -50 to "now" 100;
    // maintenance 25-35 and 90-200.
    let a = compute(s(0, 100), &[s(10, 30), s(20, 40), s(-50, 100)], &[s(25, 35), s(90, 200)]);
    assert_eq!(a.total_seconds, 100 * 3600);
    assert_eq!(a.maintenance_seconds, 20 * 3600);           // 25-35, 90-100
    assert_eq!(a.downtime_seconds, 80 * 3600);               // 0-100 minus maintenance
    assert_eq!(a.uptime_pct, Some(0.0));
    assert_eq!(a.incidents, 3);
    assert_eq!(a.longest_incident_seconds, 80 * 3600);
    let none = compute(s(0, 10), &[], &[]);
    assert_eq!((none.downtime_seconds, none.uptime_pct), (0, Some(100.0)));
    let all_maintenance = compute(s(0, 10), &[s(0, 10)], &[s(0, 10)]);
    assert_eq!((all_maintenance.downtime_seconds, all_maintenance.uptime_pct), (0, None));
    let empty = compute(s(5, 5), &[s(0, 10)], &[]);
    assert_eq!((empty.total_seconds, empty.uptime_pct, empty.incidents), (0, None, 0));
    let half = compute(s(0, 10), &[s(-5, 5)], &[]);
    assert_eq!((half.downtime_seconds, half.uptime_pct, half.incidents), (5 * 3600, Some(50.0), 1));
}

#[test]
fn ranges_are_bounded() {
    assert_eq!(check_range(t(1), t(1)), Err("from must be before to".into()));
    assert_eq!(check_range(t(2), t(1)), Err("from must be before to".into()));
    assert!(check_range(t(0), t(24 * MAX_RANGE_DAYS)).is_ok());
    assert_eq!(check_range(t(0), t(24 * MAX_RANGE_DAYS + 1)), Err("the range must be at most 1096 days".into()));
}
```

`maintenance.rs` tests:

```rust
#[test]
fn one_off_spans_are_clipped_to_the_range() {
    let w = Schedule::Once { starts_at: utc(2026, 1, 1, 10, 0, 0), ends_at: utc(2026, 1, 1, 12, 0, 0) };
    assert_eq!(w.spans(utc(2026, 1, 1, 11, 0, 0), utc(2026, 1, 2, 0, 0, 0)),
               vec![Span { start: utc(2026, 1, 1, 11, 0, 0), end: utc(2026, 1, 1, 12, 0, 0) }]);
    assert!(w.spans(utc(2026, 1, 2, 0, 0, 0), utc(2026, 1, 3, 0, 0, 0)).is_empty());
}

#[test]
fn weekly_spans_agree_with_covers_every_five_minutes_for_two_years() {
    for (days, start, end, zone) in [
        (&[1, 3, 5][..], "02:00", "04:00", "Europe/Berlin"),
        (&[7][..], "23:00", "01:00", "America/New_York"),   // wraps midnight into Monday
        (&[7][..], "01:30", "03:30", "Europe/Berlin"),       // Sundays: crosses the March gap and the October repeated hour
        (&[1, 2, 3, 4, 5, 6, 7][..], "00:00", "00:00", "Asia/Jakarta"), // full days
    ] {
        let w = weekly(days, start, end, zone);
        let (from, to) = (utc(2026, 1, 1, 0, 0, 0), utc(2028, 1, 1, 0, 0, 0));
        let spans = w.spans(from, to);
        let mut at = from;
        while at < to {
            let in_span = spans.iter().any(|sp| sp.start <= at && at < sp.end);
            assert_eq!(in_span, w.covers(at), "{zone} {start}-{end} at {at}");
            at += TimeDelta::minutes(5);
        }
    }
}
```

(Windows wholly inside a repeated DST hour are excluded from the sampling test by construction; the ruling above documents them.)

- [ ] **Step 2: Write the failing API tests** — `tests/availability.rs`:

```rust
#[sqlx::test]
async fn monitor_availability_counts_incidents_minus_maintenance(pool: sqlx::PgPool) {
    // monitor created 10 days ago (update created_at); incidents (raw inserts):
    //   resolved one 3 days ago lasting 2 h; open one started 1 h ago;
    // project-wide one-off window covering 30 min of the resolved incident;
    // window scoped to ANOTHER monitor covering the open incident (must not count).
    // GET .../availability?from=<7 days ago>&to=<now + 1 day> (viewer)
    //   -> 200; to == now (clipped, within 5 s), from == 7 days ago;
    //   downtime_seconds ≈ 1.5 h + 1 h (±5 s), maintenance_seconds == 1800, incidents == 2,
    //   longest_incident_seconds ≈ 5400; uptime_pct ≈ 100 * (1 - 9000/(7d - 1800)) (±0.001).
}

#[sqlx::test]
async fn a_range_before_creation_has_no_uptime(pool: sqlx::PgPool) {
    // range entirely before created_at -> total_seconds 0, uptime_pct null, incidents 0.
}

#[sqlx::test]
async fn heartbeat_availability_uses_project_wide_windows_only(pool: sqlx::PgPool) {
    // heartbeat incident 2 h; a monitor-scoped window over it does not count, a project-wide one does.
}

#[sqlx::test]
async fn availability_checks_ranges_access_and_project(pool: sqlx::PgPool) {
    // from == to -> 400 "from must be before to"; 1097-day range -> 400; from=garbage -> 400;
    // outsider -> 403; monitor of another project -> 404; same for the heartbeat route.
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cd nami-engine && cargo test --lib uptime::availability models::maintenance && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test availability`
Expected: FAIL.

- [ ] **Step 4: Implement**

`Schedule::spans` for `Weekly`: for each local date `d` from `(from in tz).date() - 1 day` to `(to in tz).date()` inclusive with `d.weekday()` in `days`: local start `d + start`, local end `d + end` or `(d + 1) + end` when `end <= start`; convert each with

```rust
/// The first instant whose wall-clock time in `tz` is at or after `local`:
/// the earlier of two on a repeated hour (`earliest`), the end of the gap for
/// a skipped one.
fn instant_at_or_after(tz: Tz, local: NaiveDateTime, prefer_latest: bool) -> DateTime<Utc>
```

(start: `prefer_latest = false`; end: `prefer_latest = true`, so a window across a repeated hour lasts an hour longer, matching `covers`; for `LocalResult::None` step forward one minute at a time until a valid local time exists, at most 180 steps). Keep spans with `start < end`, clip to `[from, to)`. `Once` → its interval clipped.

`for_monitor`: `find_in_project` (`None` → `Ok(None)`); `total = Span { start: max(from, created_at), end: min(to, now) }`; incidents: `select started_at, resolved_at from incidents where monitor_id = $1 and started_at < $3 and (resolved_at is null or resolved_at > $2)` mapped to spans with `resolved_at.unwrap_or(now)`, clipped to `total`; windows: `maintenance::list_for_project(pool, project_id)` filtered by `applies_to_monitor(monitor_id)` (`for_heartbeat`: `applies_to_heartbeats()`), `spans(total.start, total.end)`; `compute`.

`compute`: `maintenance = union(maintenance ∩ total)`, `down = subtract(&union(incidents ∩ total), &maintenance)`, `uptime_pct = (total − maint > 0).then(|| 100.0 * (1.0 − down / (total − maint)))`; per incident `seconds(subtract(&[clipped], &maintenance))` for the longest; `incidents` = count of clipped incidents with `start < end`.

Routes call `check_range` (400) after the role check (`viewer`), then `for_monitor`/`for_heartbeat` with `Utc::now()`.

- [ ] **Step 5: Run to verify pass** — full suite green.

- [ ] **Step 6: Commit** (the roadmap line "Availability (SLA) report …; check-history CSV export" is ticked by NAMI-UP-28)

```bash
git add nami-engine
git commit -m "feat(engine): availability (SLA) report for monitors and heartbeats (NAMI-UP-27)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 28 (NAMI-UP-28): Check-history CSV export

Branch `feat/NAMI-UP-28-checks-export`, worktree `.worktrees/NAMI-UP-28/`.

**Files:**
- Create: `nami-engine/src/uptime/export.rs`, `nami-engine/tests/checks_export.rs`
- Modify: `nami-engine/src/uptime/store.rs` (`checks_between`), `nami-engine/src/uptime/mod.rs`, `nami-engine/src/routes/monitors.rs`

**Interfaces:**
- Consumes: NAMI-UP-25's `csv` crate; `UptimeCheckRow`; `tokio_stream::wrappers::ReceiverStream`; `axum::body::Body::from_stream`.
- Produces:
  - `store::checks_between(ch, project_id, monitor_id, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<UptimeCheckRow>, ClickHouseError>` (ascending, `LIMIT 20000`).
  - `export::{MAX_EXPORT_DAYS: i64 = 180, HEADER: &str = "checked_at,status,status_code,response_time_ms,error\r\n", neutralize(cell: &str) -> Cow<str>, csv_line(row: &UptimeCheckRow) -> String, check_range(from, to) -> Result<(), String>}`.
  - Route `GET /projects/{id}/monitors/{monitor_id}/checks/export?from&to` (viewer) → `text/csv; charset=utf-8`, `Content-Disposition: attachment; filename="checks-{monitor_id}.csv"`, `Cache-Control: no-store`.

- [ ] **Step 1: Write the failing unit tests** (`export.rs`):

```rust
#[test]
fn formula_cells_get_a_leading_quote() {
    for (raw, safe) in [("=1+1", "'=1+1"), ("+cmd", "'+cmd"), ("-2", "'-2"), ("@SUM(A1)", "'@SUM(A1)"),
                        ("\tx", "'\tx"), ("\rx", "'\rx"), ("ok", "ok"), ("", ""), (" =x", " =x")] {
        assert_eq!(neutralize(raw), safe, "{raw:?}");
    }
}

#[test]
fn rows_are_rfc4180_lines() {
    let row = |error: Option<&str>| UptimeCheckRow { monitor_id: Uuid::nil(), project_id: Uuid::nil(),
        checked_at: DateTime::from_timestamp_millis(1_792_000_000_123).unwrap(),
        status: CheckStatus::Down, status_code: Some(503), response_time_ms: Some(42), error: error.map(String::from) };
    assert_eq!(csv_line(&row(None)), "2026-10-14T17:46:40.123Z,down,503,42,\r\n");
    assert_eq!(csv_line(&row(Some("expected status 200-399, got 503"))),
               "2026-10-14T17:46:40.123Z,down,503,42,\"expected status 200-399, got 503\"\r\n");
    assert_eq!(csv_line(&row(Some("=HYPERLINK(\"http://evil.test\")"))),
               "2026-10-14T17:46:40.123Z,down,503,42,\"'=HYPERLINK(\"\"http://evil.test\"\")\"\r\n");
    let up = UptimeCheckRow { status: CheckStatus::Up, status_code: None, response_time_ms: None, error: None, ..row(None) };
    assert_eq!(csv_line(&up), "2026-10-14T17:46:40.123Z,up,,,\r\n");
}

#[test]
fn export_ranges_are_capped_at_180_days() {
    // from == to and from > to -> "from must be before to"; 180 days ok; 180 days + 1 s -> "the range must be at most 180 days"
}
```

- [ ] **Step 2: Write the failing API tests** — `tests/checks_export.rs` (ClickHouse):

```rust
#[sqlx::test]
async fn the_export_streams_every_check_in_the_range_as_csv(pool: sqlx::PgPool) {
    // seed monitor; insert_checks: 3 days x 2000 rows (1 every 43.2 s), one row with error "=cmd|' /C calc'!A0",
    //   one outside the range, and one row of ANOTHER monitor inside the range.
    // GET .../checks/export?from&to (viewer) -> 200, headers content-type "text/csv; charset=utf-8",
    //   content-disposition `attachment; filename="checks-{id}.csv"`, cache-control "no-store";
    // body: first line == HEADER, then exactly 6000 lines in ascending checked_at, the formula cell begins with "'=",
    //   no line of the other monitor, no line outside the range. Parse it back with csv::Reader: 5 columns everywhere.
}

#[sqlx::test]
async fn export_checks_access_project_and_range(pool: sqlx::PgPool) {
    // outsider 403; monitor of another project 404; 181-day range 400; from >= to 400; missing to 400.
}

#[sqlx::test]
async fn a_clickhouse_outage_is_a_json_500_before_any_csv(pool: sqlx::PgPool) {
    // AppState::for_test (no ClickHouse db) -> 500 {"error":"internal error"}, content-type JSON.
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cd nami-engine && cargo test --lib uptime::export && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test checks_export`
Expected: FAIL.

- [ ] **Step 4: Implement**

- `neutralize`: `Cow::Owned(format!("'{cell}"))` when the first char is one of `=`, `+`, `-`, `@`, `\t`, `\r`; else borrowed.
- `csv_line`: `csv::WriterBuilder::new().terminator(csv::Terminator::CRLF).quote_style(csv::QuoteStyle::Necessary).from_writer(vec![])`, write the five cells (`checked_at` as `%Y-%m-%dT%H:%M:%S%.3fZ`, status lowercase, empty for `None`), each through `neutralize`, flush, `String::from_utf8`.
- Handler: role `viewer` → `export::check_range` (400) → `find_in_project` (404) → query the first UTC-day slice `[from, min(from + 1 day, to))` with `checks_between` (error → `AppError::ClickHouse`, the JSON 500) → spawn a task that sends `HEADER`, the first slice's lines, then each further slice, over `tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8)`; a ClickHouse error mid-stream logs (`monitor_id`, no SQL) and sends `Err(io::Error::other("export aborted"))`, which aborts the body → `Body::from_stream(ReceiverStream::new(rx))` with the three headers.

- [ ] **Step 5: Run to verify pass** — full suite green.

- [ ] **Step 6: Tick "Availability (SLA) report for a monitor over an arbitrary range; check-history CSV export" and commit**

```bash
git add nami-engine docs/superpowers/roadmap.md
git commit -m "feat(engine): streaming CSV export of check history (NAMI-UP-28)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 29 (NAMI-UP-29): UI — heartbeat ping examples, recent pings with output, groups; incident details

Branch `feat/NAMI-UP-29-ui-heartbeats`, worktree `.worktrees/NAMI-UP-29/`.

**Files:**
- Modify: `nami-ui/src/lib/uptime.ts` (+ `uptime.test.ts`), `nami-ui/src/lib/server/heartbeat-form.ts` (+ test), `src/routes/(app)/projects/[id=uuid]/heartbeats/+page.server.ts`, `+page.svelte`, `heartbeat-list*.test.ts`, `heartbeats/[heartbeatId=uuid]/+page.server.ts`, `+page.svelte`, `heartbeat-detail*.test.ts`, `incidents/+page.svelte`, `incidents/incidents-page.test.ts`

**Interfaces:**
- Consumes: engine `GET .../heartbeats/{id}/pings?limit=50`, `GET /projects/{id}/monitor-groups`, `group_id` on heartbeats, `Incident.details`.
- Produces (`lib/uptime.ts`): `Heartbeat.group_id: string | null`; `Incident.details: string | null`; `export type HeartbeatPing = { heartbeat_id: string; project_id: string; received_at: string; kind: 'success' | 'fail'; exit_code: number | null; output: string }`; `export function failUrl(url: string): string` (`${url}/fail`); `export function exitCodeCommand(url: string): string` returning exactly `'out=$(your-job 2>&1); curl -fsS -m 10 --data-binary "$out" ' + shellQuote(url) + '/$?'` (the assignment's status is the job's, so `$?` is its exit code). `heartbeatPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult` gains `group_id` (create: omitted when blank; edit: `null` when blank).

- [ ] **Step 1: Write the failing tests**

`uptime.test.ts`:

```ts
it('builds the fail URL and an exit-code command that reports output', () => {
	const url = 'https://engine.example.com/api/v1/heartbeat/' + 'a'.repeat(64);
	expect(failUrl(url)).toBe(url + '/fail');
	expect(exitCodeCommand(url)).toBe(`out=$(your-job 2>&1); curl -fsS -m 10 --data-binary "$out" '${url}'/$?`);
});
it('groups heartbeats like monitors', () => {
	// groupMonitors([{group_id: 'g1'}, {group_id: null}], [group g1]) -> [{group: g1, ...}, {group: null, ...}]
});
```

`heartbeat-form.test.ts`: `group_id` blank on create → absent; set → sent; blank on edit → `null`.

`heartbeat-detail.test.ts` (load): calls are exactly `[GET heartbeatPath, GET heartbeatPath/pings?limit=50, GET /projects/{p}/monitor-groups]`; a pings 500 still renders with `pings: []`; groups 403 → `groups: []`; the `update` action forwards `group_id`.

`heartbeat-detail-page.test.ts` (render):

```ts
it('shows recent pings with their output as text, never as markup', () => {
	const pings = [{ heartbeat_id: HEARTBEAT_ID, project_id: PROJECT_ID, received_at: '2026-10-13T10:00:00Z',
		kind: 'fail', exit_code: 2, output: '<script>alert(1)</script>\nbackup failed' }];
	const { body } = renderPage(heartbeat(), null, pings);
	expect(body).toContain('&lt;script>alert(1)&lt;/script>');
	expect(body).not.toContain('<script>alert(1)</script>');
	expect(body).toContain('exit code 2');
});
it('shows /fail and exit-code examples to members only', () => {
	// member: body contains PING + '/fail' and 'curl -fsS -m 10 --data-binary'; viewer (token null): neither.
});
```

(The renderer escapes `<` in text as `&lt;`; assert on whatever `svelte/server` produces for text nodes — it never emits a raw `<script>` from a text expression.)

`heartbeat-list-page.test.ts`: heartbeats in group "Backups" render under a "Backups" heading, ungrouped ones last; no groups → no headings (looks as before).

`incidents-page.test.ts`: an incident with `details` renders a `<details>` with the text escaped; `details: null` renders no `<details>`.

- [ ] **Step 2: Run to verify failure**

Run: `cd nami-ui && npm test -- heartbeat incidents uptime`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Detail load: `Promise.all` of the three calls (`enginePath`), keep `cache-control: private, no-store` set first; `pings`/`groups` degrade to `[]` on failure (a 401 on the heartbeat call still redirects).
- Detail page: under the ping URL (members only), a "Report a failure" block: `failUrl(ping)` with a `CopyButton`, and `exitCodeCommand(ping)` in a `<pre>` with a `CopyButton`; a "Recent pings" table (time UTC via `formatUtc`, kind badge, exit code) where rows with output have `<details><summary>Output</summary><pre class="whitespace-pre-wrap break-all">{ping.output}</pre></details>` — text interpolation only, never `{@html}`; the settings form gets a group `<select name="group_id">` (option "No group" = `""`) using the `defaultValue` convention.
- List page: load also fetches `/monitor-groups` (failure → `[]`), renders `groupMonitors(heartbeats, groups)` sections with the same heading markup as the monitor list; the create form gets the same group select.
- Incidents page: render `details` in `<details><summary>Job output</summary><pre …>{incident.details}</pre></details>`.

- [ ] **Step 4: Run to verify pass**

Run: `cd nami-ui && npm run check && npm test`
Expected: 0 errors, 0 warnings; all green.

- [ ] **Step 5: Commit**

```bash
git add nami-ui
git commit -m "feat(ui): heartbeat fail/exit-code examples, recent pings with output, heartbeat groups (NAMI-UP-29)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 30 (NAMI-UP-30): UI — monitor form "Advanced" section (IP version, cookies, proxy)

Branch `feat/NAMI-UP-30-ui-advanced-options`, worktree `.worktrees/NAMI-UP-30/`.

**Files:**
- Modify: `nami-ui/src/lib/uptime.ts` (+ test), `nami-ui/src/lib/server/monitor-form.ts` (+ test), `nami-ui/src/lib/components/MonitorForm.svelte`, `nami-ui/src/lib/components/monitor-form-render.test.ts`, `nami-ui/src/lib/components/form-inputs.test.ts` (if it enumerates components), `src/routes/(app)/projects/[id=uuid]/monitors/[monitorId=uuid]/+page.svelte` (show the settings read-only)

**Interfaces:**
- Produces: `Monitor` type gains `ip_version: 'any' | 'ipv4' | 'ipv6'`, `remember_cookies: boolean`, `proxy_url: string | null`, `proxy_username: string | null`, `has_proxy_password: boolean`; `export const IP_VERSION_TYPES: CheckType[] = ['http', 'keyword', 'tcp', 'ping', 'ssl_expiry']`; `hasMaskedValues` also true when `proxy_username === '***'` (its `Pick` gains `proxy_username`). `monitorPayloadFromForm`: `ip_version` sent for `IP_VERSION_TYPES` (`formText || 'any'`); for http/keyword `remember_cookies = form.has('remember_cookies')`, `optional('proxy_url', …)`, `optional('proxy_username', …)`, and the proxy password with `proxy_password_action` keep/replace/clear exactly like `password_action` (create: replace when filled).

- [ ] **Step 1: Write the failing tests** (`monitor-form.test.ts`):

```ts
it('sends ip version for the types that use it and never for the others', () => {
	expect(payload({ check_type: 'tcp', host: 'db', port: '5432', ip_version: 'ipv6' }).ip_version).toBe('ipv6');
	expect(payload({ check_type: 'dns', host: 'x.test', ip_version: 'ipv6' })).not.toHaveProperty('ip_version');
	expect(payload({ check_type: 'http', url: 'https://a.test' }).ip_version).toBe('any');
});
it('sends proxy settings with keep, replace and clear for the password', () => {
	const base = { check_type: 'http', url: 'https://a.test', proxy_url: 'http://proxy:3128', proxy_username: 'nami' };
	expect(payload({ ...base, proxy_password: 'pw' })).toMatchObject({ proxy_url: 'http://proxy:3128', proxy_username: 'nami', proxy_password: 'pw' });
	expect(payload({ ...base }, 'edit')).not.toHaveProperty('proxy_password');               // keep (no action)
	expect(payload({ ...base, proxy_password_action: 'keep' }, 'edit')).not.toHaveProperty('proxy_password');
	expect(payload({ ...base, proxy_password_action: 'clear' }, 'edit').proxy_password).toBeNull();
	expect(payload({ ...base, proxy_password_action: 'replace', proxy_password: '' }, 'edit'))
		.toEqual({ ok: false, message: 'Enter the new proxy password, or keep the current one' }); // via monitorPayloadFromForm
	expect(payload({ check_type: 'http', url: 'https://a.test', proxy_url: '' }, 'edit').proxy_url).toBeNull();
	expect(payload({ check_type: 'http', url: 'https://a.test', remember_cookies: 'on' }).remember_cookies).toBe(true);
});
```

(`payload` = a local helper that builds `FormData` and returns `monitorPayloadFromForm(...).payload`, or the whole result for the error case — follow the file's existing helper.)

`monitor-form-render.test.ts`: the Advanced `<details>` contains `name="ip_version"` for tcp, none for dns; proxy inputs only for http/keyword; edit mode with `has_proxy_password: true` renders the keep/replace/clear radios and never a value in `name="proxy_password"`. `uptime.test.ts`: `hasMaskedValues({ url: 'https://a', request_body: null, auth_username: null, proxy_username: '***' })` is true.

- [ ] **Step 2: Run to verify failure** — `cd nami-ui && npm test -- monitor-form uptime` → FAIL.

- [ ] **Step 3: Implement** — `MonitorForm.svelte`: a collapsed `<details>` "Advanced" after the request section: IP version select (shown when the selected type is in `IP_VERSION_TYPES`), "Remember cookies during a check" checkbox and the proxy fieldset (http/keyword); help text: "Only for this check's redirects; never stored." and "The proxy resolves the target, so IP version must be Any." Inputs use the `defaultValue`/`defaultChecked` convention. Detail page: show "IP version", "Cookies", "Proxy" rows only when they differ from defaults (proxy password shown as "set" / nothing).

- [ ] **Step 4: Run to verify pass** — `npm run check && npm test` (0/0, green).

- [ ] **Step 5: Commit**

```bash
git add nami-ui
git commit -m "feat(ui): advanced monitor options — ip version, cookies, proxy (NAMI-UP-30)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 31 (NAMI-UP-31): UI — monitor import page

Branch `feat/NAMI-UP-31-ui-import`, worktree `.worktrees/NAMI-UP-31/`.

**Files:**
- Create: `nami-ui/src/lib/server/import-form.ts` (+ `import-form.test.ts`), `src/routes/(app)/projects/[id=uuid]/monitors/import/+page.server.ts`, `+page.svelte`, `import.test.ts`
- Modify: `src/routes/(app)/projects/[id=uuid]/monitors/+page.svelte` ("Import" link beside "New monitor", members only), `nami-ui/.env.example` (`BODY_SIZE_LIMIT=3M` with a comment, see Open question 3)

**Interfaces:**
- Produces:
  - `export const MAX_IMPORT_BYTES = 2 * 1024 * 1024;` `export const IMPORT_FORMATS = ['urls', 'uptime_kuma', 'uptimerobot'] as const;`
  - `export async function importPayloadFromForm(form: FormData, dryRun: boolean): Promise<FormResult>` — `format` must be one of `IMPORT_FORMATS` (else `Choose a format`); content from the `file` field when it is a non-empty `File` (`await file.text()`), else the `content` textarea; empty → `Paste the list or choose a file`; `new TextEncoder().encode(content).length > MAX_IMPORT_BYTES` → `The file is larger than 2 MiB`; `defaults` from `interval_seconds`, `timeout_seconds`, `confirmation_threshold`, `group_id` (whole numbers via `formWholeNumber`, blank omitted); `skip_existing: form.has('skip_existing')`; `dry_run`.
  - Page actions `preview` (dry run) and `import`; both return `{ report, format, content }` so the textarea keeps its text (passed back through `form`, rendered with `defaultValue`), or `engineFailure(...)`.
  - `type ImportReport = { dry_run: boolean; created: number; skipped: number; errors: number; results: { line: number; status: 'created' | 'skipped' | 'error'; name?: string; target?: string; monitor_id?: string; error?: string }[] }` in `lib/uptime.ts`.

- [ ] **Step 1: Write the failing tests**

`import-form.test.ts`: each validation message above; a `File` wins over the textarea; defaults parsed (`interval_seconds: 'abc'` → `interval_seconds must be a whole number`); `skip_existing` unchecked → `false`.

`import.test.ts` (actions with `fakeEvent`):

```ts
it('previews with dry_run and imports without it, posting JSON to the engine', async () => {
	const report = { dry_run: true, created: 1, skipped: 0, errors: 0, results: [{ line: 1, status: 'created', name: 'a.test', target: 'https://a.test/' }] };
	const { event, calls } = fakeEvent({ body: JSON.stringify(report), form: { format: 'urls', content: 'https://a.test/', skip_existing: 'on' } });
	expect(await actions.preview(event)).toEqual({ report, format: 'urls', content: 'https://a.test/' });
	expect(calls()).toEqual([{ method: 'POST', path: `/projects/${PROJECT_ID}/monitors/import`, search: '',
		body: JSON.stringify({ format: 'urls', content: 'https://a.test/', defaults: {}, skip_existing: true, dry_run: true }) }]);
});
it('passes engine failures through engineFailure (413, 400, 403)', async () => { /* status kept, message shown */ });
it('never calls the engine when the form is invalid', async () => { /* empty content -> fail(400), calls() == [] */ });
```

Render test: results table shows "will be created" for `dry_run: true` rows and a link to `/projects/{p}/monitors/{monitor_id}` for created rows; a `target` like `<img src=x onerror=alert(1)>` is escaped.

- [ ] **Step 2: Run to verify failure** — `cd nami-ui && npm test -- import` → FAIL.

- [ ] **Step 3: Implement** — page: format radio group with a one-line description per format ("One URL per line", "Uptime Kuma 1.x backup (Settings → Backup → Export)", "UptimeRobot monitors CSV export"), textarea + `<input type="file" name="file" accept=".txt,.json,.csv">`, defaults row (interval, timeout, confirmations, group select from `/monitor-groups` loaded in `load`), "Skip monitors that already exist" checked by default, buttons `formaction="?/preview"` and `formaction="?/import"`, `enctype="multipart/form-data"`, `use:enhance`. Results: summary counts and a table (line, status badge, name, target, error/link). Load is viewer-safe but the page shows "Only members can import monitors" when the engine answers 403 on preview.

- [ ] **Step 4: Run to verify pass** — `npm run check && npm test`.

- [ ] **Step 5: Commit**

```bash
git add nami-ui
git commit -m "feat(ui): bulk monitor import with preview (NAMI-UP-31)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 32 (NAMI-UP-32): UI — availability card and CSV export

Branch `feat/NAMI-UP-32-ui-availability-export`, worktree `.worktrees/NAMI-UP-32/`.

**Files:**
- Create: `nami-ui/src/lib/availability.ts` (+ `availability.test.ts`), `nami-ui/src/lib/components/AvailabilityCard.svelte`, `src/routes/(app)/projects/[id=uuid]/monitors/[monitorId=uuid]/checks.csv/+server.ts` (+ `server.test.ts`)
- Modify: `monitors/[monitorId=uuid]/+page.server.ts`, `+page.svelte`, `monitor-load.test.ts`; `heartbeats/[heartbeatId=uuid]/+page.server.ts`, `+page.svelte`, `heartbeat-detail.test.ts`; `docs/superpowers/roadmap.md`

**Interfaces:**
- Produces (`lib/availability.ts`):
  - `export type Availability = { from: string; to: string; total_seconds: number; maintenance_seconds: number; downtime_seconds: number; uptime_pct: number | null; incidents: number; longest_incident_seconds: number }`
  - `export type RangeKey = 'last_month' | 'last_quarter' | 'year_to_date' | 'custom'`
  - `export function standardRanges(now: Date): { key: RangeKey; label: string; from: string; to: string }[]` (UTC calendar: last month = first of previous month → first of this month; last quarter = first day of the previous calendar quarter → first day of this quarter; YTD = Jan 1 → `now`), ISO strings.
  - `export function customRange(from: string | null, to: string | null): { from: string; to: string } | null` — `YYYY-MM-DD` dates (UTC midnight; `to` inclusive → next midnight), `from < to`, span ≤ 1096 days, else `null`.
  - `export function exportRange(custom: {from: string; to: string} | null, now: Date): { from: string; to: string }` — the custom range clipped to its last 180 days, else the last 30 days.
  - `export function formatPct(pct: number | null): string` — `'—'` for null, 3 decimals (`'99.950 %'`), `'100 %'` exactly when 100.
- Monitor/heartbeat loads return `availability: { key: RangeKey; label: string; data: Availability | null }[]` (one engine call per range, `enginePath` + `?from=…&to=…` with `encodeURIComponent`), a failed call → `data: null` (card shows "unavailable").
- `checks.csv/+server.ts` `GET`: `isUuid` both params else 404; `from`/`to` must parse as ISO dates (else 400 text); calls `enginePath\`/projects/${id}/monitors/${monitorId}/checks/export\`` + query; on success returns `new Response(engine.body, { status: 200, headers: { 'content-type': 'text/csv; charset=utf-8', 'content-disposition': \`attachment; filename="checks-${monitorId}.csv"\`, 'cache-control': 'no-store' } })` (stream passed through, never buffered); a 401 → `redirect(303, '/login')`; other failures → the engine status with a plain-text message.

- [ ] **Step 1: Write the failing tests**

```ts
// availability.test.ts
it('computes calendar ranges in UTC', () => {
	const now = new Date('2026-05-17T12:00:00Z');
	expect(standardRanges(now)).toEqual([
		{ key: 'last_month', label: 'Last month', from: '2026-04-01T00:00:00.000Z', to: '2026-05-01T00:00:00.000Z' },
		{ key: 'last_quarter', label: 'Last quarter', from: '2026-01-01T00:00:00.000Z', to: '2026-04-01T00:00:00.000Z' },
		{ key: 'year_to_date', label: 'Year to date', from: '2026-01-01T00:00:00.000Z', to: '2026-05-17T12:00:00.000Z' }
	]);
	expect(standardRanges(new Date('2026-01-10T00:00:00Z'))[0].from).toBe('2025-12-01T00:00:00.000Z');
	expect(standardRanges(new Date('2026-02-10T00:00:00Z'))[1]).toMatchObject({ from: '2025-10-01T00:00:00.000Z', to: '2026-01-01T00:00:00.000Z' });
});
it('accepts only sane custom ranges', () => {
	expect(customRange('2026-01-01', '2026-01-31')).toEqual({ from: '2026-01-01T00:00:00.000Z', to: '2026-02-01T00:00:00.000Z' });
	for (const [f, t] of [['2026-02-01', '2026-01-01'], ['2026-13-01', '2026-01-01'], ['x', 'y'], [null, '2026-01-01'], ['2020-01-01', '2026-01-01']]) {
		expect(customRange(f, t)).toBeNull();
	}
});
it('formats percentages', () => {
	expect(formatPct(null)).toBe('—'); expect(formatPct(100)).toBe('100 %'); expect(formatPct(99.95)).toBe('99.950 %');
});
```

`server.test.ts` (CSV route): forwards `from`/`to`, passes the body through, sets the three headers; a non-UUID id → 404 without an engine call; `from=abc` → 400 without an engine call; engine 404 → 404; engine 401 → redirect to `/login`. `monitor-load.test.ts`: the load now makes 4 + 3 calls (the existing four plus the three availability ranges) and 4 when `?from=2026-01-01&to=2026-01-31` adds the custom range; an availability 500 → `data: null` and the page still loads. Same for the heartbeat load (+3 calls).

- [ ] **Step 2: Run to verify failure** — `cd nami-ui && npm test -- availability checks.csv monitor-load heartbeat-detail` → FAIL.

- [ ] **Step 3: Implement** — `AvailabilityCard.svelte` (props `availability`, optional `exportHref`): a row per range with uptime %, downtime (`formatSeconds`), incidents, longest; a custom-range GET form (`<input type="date" name="from">`, `name="to"`) that reloads the page with the query; "Export checks (CSV)" link (`href={exportHref}`, `download`) on monitors only, built from `exportRange(...)`. Explain under the card: "Incident time is confirmed downtime, so it can differ from the check-based uptime tiles."

- [ ] **Step 4: Run to verify pass** — `npm run check && npm test`.

- [ ] **Step 5: Tick "UI for all of the above" (Phase 3) in the roadmap and commit**

```bash
git add nami-ui docs/superpowers/roadmap.md
git commit -m "feat(ui): availability card and check-history CSV export (NAMI-UP-32)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Final review (after NAMI-UP-32): Phase 3 security review + fix wave

Not a numbered task: per CLAUDE.md every sub-project phase ends with a dedicated security review and fix wave. Branch `fix/NAMI-UP-phase3-final-review`, worktree `.worktrees/NAMI-UP-p3-review/`, from `origin/main` after NAMI-UP-32 merges. The reviewer reads the merged diff `6e78618..origin/main` for `nami-engine/` and `nami-ui/` and checks at least:

- Public ping surface: token shape before any query; 404/400/413/429/503 ordering; no token in any log line (TRACE capture); bucket map bounded; `heartbeat_pings` insert cannot block or fail a ping; 1 MiB limit applies to chunked bodies too (send one without `Content-Length`).
- Output: every path that shows `incidents.details` or ping output (UI pages, incident list, email, later Incident Management formatters) renders it as text; no `{@html}` anywhere in the diff.
- Import: 2 MiB / 500 limits before any write; no serde error text that could quote a secret; Kuma `notificationList`/`proxyList` never read into a stored or returned value; each entry re-validated by the single-create path; foreign groups 404.
- Proxy SSRF: `.no_proxy()` still first on every client; the proxy host resolved through `GuardedResolver` (the unit test from NAMI-UP-24 still passes against the locked reqwest version); IP-literal proxies and targets refused; proxy password only as ciphertext (AAD `monitors.proxy_password:{id}`), absent from responses, logs, alerts and `Debug`.
- Cookie jar: never shared between checks or monitors (per-check client), never persisted.
- CSV: formula neutralization on every cell; filename built from ids only; range caps; `no-store`.
- SLA: range caps; DST/maintenance math cannot produce values outside 0-100; cross-project ids 404.
- BFF: every new engine path built with `enginePath`; `[id=uuid]` matchers on the new routes; `no-store` where tokens or secrets are shown; import file size capped in the BFF.

Findings go into the review PR's description; fixes land on the same branch with tests that pin them, then the PR merges once clean (pre-authorized auto-merge).
