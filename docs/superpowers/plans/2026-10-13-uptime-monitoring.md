# Uptime Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add uptime monitoring to Nami — http/keyword/tcp monitors checked on an interval, cron-job heartbeats, automatic incidents with email alerts, and uptime/response-time history — plus the nami-ui pages to manage and watch them.

**Architecture:** nami-engine gains a thin ClickHouse HTTP client (existing `reqwest`, every value a bound query parameter) for check history; Postgres tables for monitors, heartbeats and incidents; and an in-process tokio worker that atomically claims due monitors, runs checks (at most 20 in flight), batch-inserts results into ClickHouse, applies each monitor's state machine in one row-locked transaction, and emails project members through a single `alerts::notify_transition` entry point. nami-ui turns `/projects/[id]` into a section (Monitors, Heartbeats, Incidents, Settings) that reads everything through the existing `callEngine` BFF; heartbeat pings go straight from cron jobs to the engine's public `/api/v1/heartbeat/{token}`.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, reqwest 0.13, tokio, lettre, wiremock), Postgres 17, ClickHouse 26.9 over HTTP, Mailpit, SvelteKit 3.0.1 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-uptime-monitoring-design.md`

**Verification status (2026-10-13):** every Rust, SQL, TypeScript and Svelte block in this plan was compiled and its tests were run in a scratch copy of `main`: all engine test binaries green against ClickHouse 26.9, Postgres and Mailpit, plus a live engine run that checked https://example.com; UI `npm run check` 0 errors / 0 warnings, vitest green, and a browser walkthrough of every page. Still run every step — the scratch copy is not the repo.

**Execution:** per CLAUDE.md — one worktree per task under `.worktrees/NAMI-UP-<n>/`, branch from the latest `origin/main`, the branch names given in each task heading, Conventional Commits, task review, PR, and auto-merge on a clean review (pre-authorized for sub-projects 2-6). Tasks are sequential: each consumes the interfaces of the ones before it.

## Global Constraints

- All infra via docker-compose containers (`nami-engine/docker-compose.yaml`), never native installs. The Docker Postgres is published on host port **5433** (Task 1 moves it off 5432): a native Homebrew `postgresql@16` on this machine listens on `127.0.0.1:5432`/`[::1]:5432` and captured every `localhost:5432` connection — app and tests had silently been running against it, not the container. Controller ruling 2026-10-13: remap the container port rather than stopping the user's native service.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`), no compile-time macros, no ORM. SQLx 0.9 only accepts `&'static str` SQL, so every statement is a literal — never `format!` SQL.
- ClickHouse values always bound via query parameters (`{name:Type}` + `param_name=`), never interpolated; identifiers travel as `Identifier` parameters.
- No new crate for ClickHouse: reuse `reqwest` 0.13 (its `query` feature is off, so URLs are built with `Url::query_pairs_mut`).
- axum 0.8 path syntax `{param}`.
- Authorization failure is 403, never 404; a resource id from another project inside an accessible project path is 404.
- Project-scoped RBAC exactly like `routes/projects.rs`: viewer reads, member creates/edits, admin deletes.
- Monitor limits (spec): interval >= 30 s, timeout 1-60 s, method `GET`|`HEAD`, confirmation threshold 1-10 (default 2), expected status default 200-399.
- Worker (spec): tick 5 s, claim at most 50 due monitors per tick, at most 20 checks in flight; ClickHouse `uptime_checks` TTL 180 days.
- Every engine call from nami-ui goes through `callEngine`; the browser never calls nami-engine directly (heartbeat pings are machine-to-engine, not browser).
- nami-ui is SvelteKit 3.0.1: `#lib` alias (not `$lib`), `process.env` in server code, Svelte 5 runes, `page` from `$app/state` (`$app/stores` is removed), `refreshAll` from `$app/navigation` (`invalidateAll` is deprecated in 3.x), named form actions never mixed with a `default` action, `use:enhance` on forms that depend on client state.
- Dark theme only: page `#0a0a0a`, cards `bg-[#141414]` + `border-[#262626]`, teal-500 accent, red-400 / teal-300 alert text — copy classes from `nami-ui/src/routes/login/+page.svelte` and `(app)/+layout.svelte`.
- Tests need `DATABASE_URL=postgres://nami:nami@localhost:5433/nami` set; Mailpit assertions filter by recipient (parallel tests share one inbox) and new tests never delete messages. The existing `register.rs`/`email.rs` wipe the inbox, which is safe only because `cargo test` runs test binaries one after another — do not switch to a cross-binary parallel runner (e.g. nextest) without changing them.
- Monitor targets are not SSRF-restricted (spec non-goal: self-hosted, internal targets are legitimate).
- Commits: Conventional Commits whose message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- A monitor that stays down across many checks must keep exactly one open incident and send no further DOWN email until it recovers → Task 4 (`reaching_the_threshold_opens_one_incident_and_alerts_once`).
- An SMTP outage during a transition must not abort the worker pass: the incident is still recorded and `run_due_checks` returns normally → Task 4 (`an_smtp_outage_does_not_abort_the_pass`).
- With ClickHouse unavailable, `GET /projects/{id}/monitors` must still return every monitor with its Postgres status and `uptime_24h: null`, not a 500 → Task 5 (`the_monitor_list_still_loads_when_clickhouse_is_unavailable`).
- Uptime that is not exactly 100% must never display as `100.00%` — truncate, never round (0.99999 → `99.99%`) → Task 7 (`formatUptime` vitest "truncates instead of rounding").
- A keyword monitor configured with `HEAD` (no body to search) must be rejected with 400 at create/edit time instead of silently reporting DOWN forever → Task 2 (`invalid_configs_are_rejected_with_400_and_nothing_is_written`).

## Decisions the spec leaves open

- **Migration order:** `0010_create_incidents.sql` creates incidents without `heartbeat_id`; `0011_create_heartbeats.sql` creates `heartbeats`, then adds `incidents.heartbeat_id` (FK, `on delete cascade`) and its one-open-incident index. The final schema equals the spec's.
- **Keyword monitors require `GET`:** a `HEAD` response has no body to search.
- **ClickHouse outage:** the worker still applies state changes and sends alerts (the insert error is logged); the monitor list degrades to `uptime_24h: null`; `/checks` and `/stats` return a generic 500.
- **Results for a monitor paused or deleted while its check was in flight** are discarded (no state change, no alert).
- **Paused heartbeats** accept pings and record `last_ping_at`, but their status and incidents are frozen and they never alert.
- **Heartbeat recovery emails** are sent in a background task, so the cron job's ping returns immediately; worker alerts are awaited inline.
- **Heartbeat tokens** are stored in clear (the UI keeps showing the ping URL), unlike hashed auth tokens, and every project role that can read a heartbeat — viewers included — sees its token.
- **Editing check config** (type, target, method, keyword, status range, interval, timeout) sets `next_check_at = now()`; renaming, pausing and the threshold do not.
- **Response time** is measured to the response headers for `http`, to the end of the capped body for `keyword`, to connection setup for `tcp`. Keyword matching is case-sensitive; only the first 1 MiB of the body is searched.
- **"Since" on the monitor page** is derived from incidents (no state-change column in the spec): open incident start while down, latest recovery while up, creation otherwise.
- **Times in the UI render in UTC**, matching the alert emails and keeping server- and client-rendered markup identical.
- **Sub-nav tabs** are added as their pages land: Monitors + Settings in Task 7, Heartbeats + Incidents in Task 8, so `main` never has dead links.

## File Structure

```
nami-engine/
  docker-compose.yaml                 T1  ClickHouse credentials, volume, ulimits, pinned 26.9
  .env.example                        T1  CLICKHOUSE_*, UI_BASE_URL, UPTIME_WORKER_ENABLED
  migrations/
    0009_create_monitors.sql          T2  monitors (+ due/project indexes)
    0010_create_incidents.sql         T4  incidents (monitor part) + one-open-per-monitor index
    0011_create_heartbeats.sql        T6  heartbeats + incidents.heartbeat_id + index
  src/
    clickhouse.rs                     T1  HTTP client: execute / insert_rows / query_rows / migrate / test dbs
    config.rs                         T1  new env-driven fields
    error.rs                          T1  AppError::ClickHouse -> generic 500
    state.rs                          T1  AppState.clickhouse + AppState::for_test
    main.rs                           T1  ClickHouse migrate at startup; T4 spawn the worker
    lib.rs                            T1  mod clickhouse; T3 mod uptime
    router.rs                         T2/T5/T6  merge monitors, incidents, heartbeats routers
    models/
      mod.rs                          T2  validate_name (shared); module list
      monitor.rs                      T2  Monitor, MonitorConfig, validation, CRUD queries
      incident.rs                     T4  open/resolve; T5 list; T6 heartbeat variants + join
      heartbeat.rs                    T6  Heartbeat, validation, CRUD queries
    routes/
      mod.rs                          T2/T5/T6
      monitors.rs                     T2  CRUD; T5 list with uptime, /checks, /stats
      incidents.rs                    T5  GET /projects/{id}/incidents
      heartbeats.rs                   T6  CRUD + public ping router
    uptime/
      mod.rs                          T3/T4/T6
      checker.rs                      T3  perform_check (http/keyword/tcp)
      store.rs                        T4  UptimeCheckRow + insert; T5 read queries
      alerts.rs                       T4  Transition + notify_transition (single entry point)
      worker.rs                       T4  claim / apply_outcome / run_due_checks / run_forever; T6 sweep
      heartbeat.rs                    T6  record_ping / sweep_overdue
  tests/
    common/mod.rs                     T2  shared helpers; T4 Mailpit + ClickHouse helpers
    clickhouse.rs                     T1
    monitors_api.rs                   T2
    uptime_checker.rs                 T3
    uptime_worker.rs                  T4
    uptime_read_api.rs                T5
    heartbeats.rs                     T6
    migrations.rs                     T2/T4/T6  expected tables
    health.rs rbac.rs register.rs login.rs totp.rs oauth.rs password_reset.rs rate_limit.rs
                                      T1  AppState::for_test refactor

nami-ui/
  .env.example                        T8  PUBLIC_ENGINE_URL
  src/lib/
    uptime.ts (+ .test.ts)            T7  engine JSON types + display helpers; T8 heartbeat helpers
    server/engine.ts (+ .test.ts)     T7  engineErrorMessage; T8 publicEngineUrl
    server/monitor-form.ts (+ .test)  T7  monitor FormData -> engine JSON
    server/heartbeat-form.ts (+ .test) T8 heartbeat FormData -> engine JSON
    components/StatusBadge.svelte     T7
    components/MonitorForm.svelte     T7  shared by new/edit
    components/ResponseTimeChart.svelte T7  inline SVG chart
    components/CopyButton.svelte      T8
  src/routes/(app)/projects/[id]/
    +layout.server.ts, +layout.svelte T7  project load (403) + sub-nav; T8 more tabs
    +page.server.ts                   T7  redirect to ./monitors (+page.svelte moves to settings/)
    settings/+page.server.ts, +page.svelte              T7  member forms, moved unchanged
    monitors/+page.server.ts, +page.svelte              T7  list, auto-refresh
    monitors/new/+page.server.ts, +page.svelte          T7
    monitors/[monitorId]/+page.server.ts, +page.svelte  T7  detail
    monitors/[monitorId]/edit/+page.server.ts, +page.svelte T7
    heartbeats/+page.server.ts, +page.svelte            T8  list + create
    heartbeats/[heartbeatId]/+page.server.ts, +page.svelte T8 detail
    incidents/+page.server.ts, +page.svelte             T8  open/resolved tabs
```

---

### Task 1 (NAMI-UP-1): ClickHouse infrastructure + shared test state

Branch `feat/NAMI-UP-1-clickhouse-client`, worktree `.worktrees/NAMI-UP-1/`.

**Files:**
- Modify: `nami-engine/docker-compose.yaml`, `nami-engine/.env.example`
- Create: `nami-engine/src/clickhouse.rs`
- Modify: `nami-engine/src/config.rs`, `nami-engine/src/lib.rs`, `nami-engine/src/error.rs`, `nami-engine/src/state.rs`, `nami-engine/src/main.rs`
- Modify (AppState refactor only): `nami-engine/tests/health.rs`, `tests/rbac.rs`, `tests/register.rs`, `tests/login.rs`, `tests/rate_limit.rs`, `tests/totp.rs`, `tests/oauth.rs`, `tests/password_reset.rs`
- Test: `nami-engine/tests/clickhouse.rs`

**Interfaces:**
- Consumes: `Config::from_env_for_test() -> Config`, `email::Mailer::new(&str, u16) -> Mailer` (existing).
- Produces:
  - `clickhouse::ClickHouse` (`Clone`): `ClickHouse::new(url: &str, user: &str, password: &str, database: &str) -> ClickHouse` (panics on an unparsable URL); `database(&self) -> &str`; `async execute(&self, sql: &str) -> Result<(), ClickHouseError>`; `async insert_rows<T: Serialize>(&self, table: &str, rows: &[T]) -> Result<(), ClickHouseError>`; `async query_rows<T: DeserializeOwned>(&self, sql: &str, params: &[(&str, String)]) -> Result<Vec<T>, ClickHouseError>`; `async migrate(&self) -> Result<(), ClickHouseError>`; test-only `async ClickHouse::create_test_database() -> Result<ClickHouse, ClickHouseError>` and `async drop_database(&self) -> Result<(), ClickHouseError>`.
  - `clickhouse::ClickHouseError` — `Transport(reqwest::Error)`, `Server { status: u16, body: String }`, `Encode(serde_json::Error)`, `Decode { line: String, source: serde_json::Error }`; `clickhouse::TEST_DATABASE_PREFIX = "nami_test_"`.
  - `AppError::ClickHouse(ClickHouseError)` (`From` impl; responds `500 {"error":"internal error"}`, logs the detail).
  - `AppState { db: PgPool, mailer: Mailer, clickhouse: ClickHouse, config: Config }` and `AppState::for_test(pool: PgPool) -> AppState`.
  - `Config` fields `clickhouse_url`, `clickhouse_user`, `clickhouse_password`, `clickhouse_database`, `ui_base_url: String`, `uptime_worker_enabled: bool`.

Facts this task relies on (verified against ClickHouse 26.9.17 on 2026-10-13): GET requests are read-only, so every request is a POST; query-parameter values are parsed in the TSV "escaped" format (a raw tab/newline is rejected and `\n`-style sequences are decoded), hence `escape_param_value`; `DateTime64` parameters do not accept a trailing `Z`, so later tasks pass times as epoch milliseconds through `fromUnixTimestamp64Milli({x:Int64}, 'UTC')`; `{db:Identifier}` works in `CREATE`/`DROP DATABASE` and `INSERT INTO`; `+` in URL parameters decodes to a space (compatible with `form_urlencoded`).

- [ ] **Step 1: Give the ClickHouse container credentials, a volume and file limits**

First, in the `postgres:` service of `nami-engine/docker-compose.yaml`, change the port mapping from `"5432:5432"` to `"5433:5432"` (a native Homebrew Postgres on this machine owns host port 5432 and silently captured every `localhost:5432` connection), and in `nami-engine/.env.example` change `DATABASE_URL` to `postgres://nami:nami@localhost:5433/nami`. Also update the one `DATABASE_URL`/port mention in `/CLAUDE.md`'s Conventions (add a line: Docker Postgres listens on host port 5433; tests use `DATABASE_URL=postgres://nami:nami@localhost:5433/nami`). Verify with `lsof -nP -iTCP:5433 -sTCP:LISTEN` that only Docker owns 5433.

Then replace the whole `clickhouse:` service and the `volumes:` block of `nami-engine/docker-compose.yaml` (mailpit stays as it is):

```yaml
  clickhouse:
    # Pinned: the HTTP query-parameter escaping and DateTime handling in
    # src/clickhouse.rs were verified against 26.9.
    image: clickhouse/clickhouse-server:26.9
    environment:
      # ClickHouse 26.x rejects the passwordless `default` user over the
      # network, so nami-engine authenticates as this user.
      CLICKHOUSE_USER: nami
      CLICKHOUSE_PASSWORD: nami
      CLICKHOUSE_DB: nami
    ports:
      - "8123:8123" # HTTP interface (nami-engine)
      - "9000:9000" # native protocol (clickhouse-client)
    ulimits:
      nofile:
        soft: 262144
        hard: 262144
    volumes:
      - nami_clickhouse_data:/var/lib/clickhouse

volumes:
  nami_postgres_data:
  nami_clickhouse_data:
```

Append to `nami-engine/.env.example`:

```dotenv
CLICKHOUSE_URL=http://localhost:8123
CLICKHOUSE_USER=nami
CLICKHOUSE_PASSWORD=nami
CLICKHOUSE_DATABASE=nami
# Base URL of nami-ui, used for the links in alert emails.
UI_BASE_URL=http://localhost:5173
# Set to false to serve the API without the background uptime checker.
UPTIME_WORKER_ENABLED=true
```

Recreate the container (the old one had no credentials and no named volume, so it starts fresh) and wait until it answers:

```bash
cd nami-engine
docker compose up -d clickhouse
until curl -sf -H 'X-ClickHouse-User: nami' -H 'X-ClickHouse-Key: nami' 'http://localhost:8123/?query=SELECT%201'; do sleep 1; done
```

Expected: `1`.

- [ ] **Step 2: Write the failing test `tests/clickhouse.rs`**

```rust
// Requires `docker compose up -d clickhouse` (user/password nami/nami).
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    clickhouse::{ClickHouse, ClickHouseError},
    state::AppState,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CheckRow {
    monitor_id: Uuid,
    project_id: Uuid,
    checked_at: DateTime<Utc>,
    status: String,
    status_code: Option<u16>,
    response_time_ms: Option<u32>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Count {
    n: u64,
}

/// `checked_at` is `DateTime64(3)`: sub-millisecond precision is truncated
/// on insert, so build values that are already whole milliseconds.
fn now_millis() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

fn row(monitor_id: Uuid, checked_at: DateTime<Utc>, error: Option<&str>) -> CheckRow {
    CheckRow {
        monitor_id,
        project_id: Uuid::new_v4(),
        checked_at,
        status: if error.is_some() { "down" } else { "up" }.to_string(),
        status_code: if error.is_some() { None } else { Some(200) },
        response_time_ms: if error.is_some() { None } else { Some(42) },
        error: error.map(str::to_string),
    }
}

#[tokio::test]
async fn insert_then_query_round_trips_uuids_datetimes_and_nulls() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let monitor_id = Uuid::new_v4();
    let t0 = now_millis();
    let rows = vec![
        row(monitor_id, t0, None),
        row(monitor_id, t0 + Duration::seconds(30), Some("timeout after 10s")),
        row(Uuid::new_v4(), t0, None), // another monitor: must not come back
    ];
    ch.insert_rows("uptime_checks", &rows).await.unwrap();

    let fetched: Vec<CheckRow> = ch
        .query_rows(
            "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms, error
             FROM uptime_checks
             WHERE monitor_id = {monitor_id:UUID}
             ORDER BY checked_at",
            &[("monitor_id", monitor_id.to_string())],
        )
        .await
        .unwrap();

    assert_eq!(fetched, rows[..2].to_vec());
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn parameter_values_are_matched_literally_never_executed() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    // Quotes, a backslash sequence that TSV-unescaping would mangle
    // (`\n`, `\f`), a raw tab and newline, an embedded statement, and a
    // placeholder-looking fragment.
    let hostile = "it's \"quoted\" C:\\new\\folder\ttab\nline'; DROP TABLE uptime_checks; -- {error:String}";
    let stored = row(Uuid::new_v4(), now_millis(), Some(hostile));
    ch.insert_rows("uptime_checks", &[stored]).await.unwrap();

    let count = |value: String| {
        let ch = ch.clone();
        async move {
            let rows: Vec<Count> = ch
                .query_rows(
                    "SELECT count() AS n FROM uptime_checks WHERE error = {error:String}",
                    &[("error", value)],
                )
                .await
                .unwrap();
            rows[0].n
        }
    };

    assert_eq!(count(hostile.to_string()).await, 1, "value must round-trip byte-for-byte");
    assert_eq!(
        count(hostile.replace("C:\\new", "C:new")).await,
        0,
        "a near-miss must not match: the value is compared, not reinterpreted"
    );
    let all: Vec<Count> = ch
        .query_rows("SELECT count() AS n FROM uptime_checks", &[])
        .await
        .unwrap();
    assert_eq!(all[0].n, 1, "the embedded DROP TABLE never ran");
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn non_2xx_responses_carry_the_clickhouse_error_text() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let error = ch
        .query_rows::<serde_json::Value>("SELEC 1", &[])
        .await
        .unwrap_err();
    match error {
        ClickHouseError::Server { status, body } => {
            assert!((400..600).contains(&status), "status was {status}");
            assert!(body.contains("SYNTAX_ERROR"), "body was {body:?}");
        }
        other => panic!("expected ClickHouseError::Server, got {other:?}"),
    }
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn migrate_is_idempotent() {
    let ch = ClickHouse::create_test_database().await.unwrap(); // first run
    ch.migrate().await.unwrap(); // second run must be a no-op, not an error
    let tables: Vec<serde_json::Value> = ch
        .query_rows(
            "SELECT name FROM system.tables WHERE database = {db:String}",
            &[("db", ch.database().to_string())],
        )
        .await
        .unwrap();
    assert_eq!(tables, vec![serde_json::json!({ "name": "uptime_checks" })]);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
#[should_panic(expected = "drop_database only drops")]
async fn drop_database_refuses_anything_but_test_databases() {
    let ch = ClickHouse::new("http://localhost:8123", "nami", "nami", "nami");
    let _ = ch.drop_database().await;
}

#[sqlx::test]
async fn app_state_for_test_never_points_at_the_real_database(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool);
    assert_ne!(state.clickhouse.database(), state.config.clickhouse_database);
    assert!(!state.clickhouse.database().starts_with("nami_test_"));
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test clickhouse`
Expected: compile error ``unresolved import `nami_engine::clickhouse` ``.

- [ ] **Step 4: Add the config fields**

In `nami-engine/src/config.rs`, add to the `Config` struct after `github_userinfo_url`:

```rust
    pub clickhouse_url: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,
    pub clickhouse_database: String,
    /// Base URL of nami-ui, used to build links in alert emails.
    pub ui_base_url: String,
    /// `UPTIME_WORKER_ENABLED=false` serves the API without the background
    /// uptime checker (e.g. a second, API-only replica).
    pub uptime_worker_enabled: bool,
```

and in `from_env_defaults_only()` (shared by `from_env` and `from_env_for_test`), after the `github_userinfo_url: ...` initializer:

```rust
            clickhouse_url: std::env::var("CLICKHOUSE_URL")
                .unwrap_or_else(|_| "http://localhost:8123".to_string()),
            clickhouse_user: std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "nami".to_string()),
            clickhouse_password: std::env::var("CLICKHOUSE_PASSWORD")
                .unwrap_or_else(|_| "nami".to_string()),
            clickhouse_database: std::env::var("CLICKHOUSE_DATABASE")
                .unwrap_or_else(|_| "nami".to_string()),
            ui_base_url: std::env::var("UI_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            uptime_worker_enabled: std::env::var("UPTIME_WORKER_ENABLED")
                .map(|v| !(v == "false" || v == "0"))
                .unwrap_or(true),
```

- [ ] **Step 5: Write `src/clickhouse.rs` and register it**

```rust
//! Thin client for ClickHouse's HTTP interface, built on the existing
//! `reqwest` dependency (no driver crate). Shared by the uptime sub-project
//! now and the logs/metrics sub-projects later.
//!
//! Every value reaches ClickHouse as a query parameter — a `{name:Type}`
//! placeholder in the SQL plus a `param_name=value` URL parameter — never by
//! string interpolation. Database and table names travel the same way, as
//! `Identifier` parameters, so no SQL text in this module is built with
//! `format!`.

use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

/// Settings sent with every request. ClickHouse 26.x already defaults to
/// these, but sending them pins the wire format this client relies on:
/// - `date_time_input_format=best_effort`: accept chrono's RFC 3339 output
///   (`2026-10-13T10:00:00.123456789Z`) in inserted rows.
/// - `date_time_output_format=iso`: emit `2026-10-13T10:00:00.123Z`, which
///   chrono's `DateTime<Utc>` deserializer accepts.
/// - `output_format_json_quote_64bit_integers=0`: emit `count()` (UInt64)
///   as a JSON number, not a string (older servers quote by default).
const BASE_SETTINGS: [(&str, &str); 3] = [
    ("date_time_input_format", "best_effort"),
    ("date_time_output_format", "iso"),
    ("output_format_json_quote_64bit_integers", "0"),
];

/// Idempotent DDL applied by [`ClickHouse::migrate`], in order. Table names
/// are unqualified because every request carries the `database` setting.
const MIGRATIONS: [&str; 1] = ["CREATE TABLE IF NOT EXISTS uptime_checks (
    monitor_id UUID,
    project_id UUID,
    checked_at DateTime64(3, 'UTC'),
    status Enum8('up' = 1, 'down' = 2),
    status_code Nullable(UInt16),
    response_time_ms Nullable(UInt32),
    error Nullable(String)
) ENGINE = MergeTree
ORDER BY (monitor_id, checked_at)
TTL toDateTime(checked_at) + INTERVAL 180 DAY"];

/// Prefix of the throwaway databases created by
/// [`ClickHouse::create_test_database`].
pub const TEST_DATABASE_PREFIX: &str = "nami_test_";

#[derive(Debug, thiserror::Error)]
pub enum ClickHouseError {
    #[error("clickhouse request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// Non-2xx response; `body` is ClickHouse's own error text.
    #[error("clickhouse returned HTTP {status}: {body}")]
    Server { status: u16, body: String },
    #[error("could not encode a row for clickhouse: {0}")]
    Encode(serde_json::Error),
    #[error("could not decode clickhouse row {line:?}: {source}")]
    Decode {
        line: String,
        source: serde_json::Error,
    },
}

#[derive(Clone)]
pub struct ClickHouse {
    http: reqwest::Client,
    url: reqwest::Url,
    user: String,
    password: String,
    database: String,
}

impl ClickHouse {
    /// Panics on an unparsable `url`: like a non-numeric `PORT` in
    /// `Config::from_env`, a broken `CLICKHOUSE_URL` is a startup error.
    pub fn new(url: &str, user: &str, password: &str, database: &str) -> Self {
        let url = reqwest::Url::parse(url).expect("CLICKHOUSE_URL must be a valid URL");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build the ClickHouse HTTP client");
        Self {
            http,
            url,
            user: user.to_string(),
            password: password.to_string(),
            database: database.to_string(),
        }
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// Runs one statement (DDL, `OPTIMIZE`, ...) against this client's database.
    pub async fn execute(&self, sql: &str) -> Result<(), ClickHouseError> {
        self.post(self.settings(true), sql.to_string()).await.map(|_| ())
    }

    /// `INSERT INTO <database>.<table> FORMAT JSONEachRow`, one serde-encoded
    /// JSON object per line. An empty slice is a no-op.
    pub async fn insert_rows<T: Serialize>(
        &self,
        table: &str,
        rows: &[T],
    ) -> Result<(), ClickHouseError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for row in rows {
            body.push_str(&serde_json::to_string(row).map_err(ClickHouseError::Encode)?);
            body.push('\n');
        }
        let mut pairs = self.settings(true);
        pairs.push((
            "query".to_string(),
            "INSERT INTO {db:Identifier}.{table:Identifier} FORMAT JSONEachRow".to_string(),
        ));
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        pairs.push(("param_table".to_string(), escape_param_value(table)));
        self.post(pairs, body).await.map(|_| ())
    }

    /// Runs a `SELECT` and deserializes each `JSONEachRow` line into `T`.
    /// Write placeholders as `{name:Type}` in `sql` and pass the values in
    /// `params` as `("name", value)` — never interpolate values into `sql`.
    pub async fn query_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<T>, ClickHouseError> {
        let mut pairs = self.settings(true);
        pairs.push(("default_format".to_string(), "JSONEachRow".to_string()));
        // Buffer the whole result server-side, so an error raised mid-query
        // becomes a non-2xx status instead of trailing text in a 200 body.
        pairs.push(("wait_end_of_query".to_string(), "1".to_string()));
        for (name, value) in params {
            pairs.push((format!("param_{name}"), escape_param_value(value)));
        }
        let text = self.post(pairs, sql.to_string()).await?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|source| ClickHouseError::Decode {
                    line: line.to_string(),
                    source,
                })
            })
            .collect()
    }

    /// Creates the database if needed, then applies every migration.
    /// Idempotent: safe to run on every startup.
    pub async fn migrate(&self) -> Result<(), ClickHouseError> {
        // Not `settings(true)`: the `database` setting would make ClickHouse
        // reject the request while the database does not exist yet.
        let mut pairs = self.settings(false);
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        self.post(pairs, "CREATE DATABASE IF NOT EXISTS {db:Identifier}".to_string())
            .await?;
        for ddl in MIGRATIONS {
            self.execute(ddl).await?;
        }
        Ok(())
    }

    /// Test-only (same precedent as `router::with_fallback_connect_info` and
    /// `AppState::for_test`): creates a uniquely named `nami_test_<uuid>`
    /// database on the server named by `CLICKHOUSE_URL`/`CLICKHOUSE_USER`/
    /// `CLICKHOUSE_PASSWORD` (defaults match docker-compose), applies the
    /// migrations, and returns a client bound to it. Tests run in parallel,
    /// so each one gets its own database. Call [`Self::drop_database`] at the
    /// end of the test; a test that panics first leaves its database behind,
    /// which is harmless.
    pub async fn create_test_database() -> Result<ClickHouse, ClickHouseError> {
        let config = crate::config::Config::from_env_for_test();
        let name = format!("{TEST_DATABASE_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let client = ClickHouse::new(
            &config.clickhouse_url,
            &config.clickhouse_user,
            &config.clickhouse_password,
            &name,
        );
        client.migrate().await?;
        Ok(client)
    }

    /// Test-only counterpart to [`Self::create_test_database`]. Panics instead
    /// of dropping anything not named `nami_test_*`, so a test can never drop
    /// the real `nami` database.
    pub async fn drop_database(&self) -> Result<(), ClickHouseError> {
        assert!(
            self.database.starts_with(TEST_DATABASE_PREFIX),
            "drop_database only drops {TEST_DATABASE_PREFIX}* databases, not {:?}",
            self.database
        );
        let mut pairs = self.settings(false);
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        self.post(pairs, "DROP DATABASE IF EXISTS {db:Identifier} SYNC".to_string())
            .await
            .map(|_| ())
    }

    fn settings(&self, with_database: bool) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = BASE_SETTINGS
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        if with_database {
            pairs.push(("database".to_string(), self.database.clone()));
        }
        pairs
    }

    /// POSTs `body` with `pairs` as URL parameters. Always POST: ClickHouse
    /// treats GET as read-only. Credentials go in headers, never the URL.
    async fn post(
        &self,
        pairs: Vec<(String, String)>,
        body: String,
    ) -> Result<String, ClickHouseError> {
        let mut url = self.url.clone();
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in &pairs {
                query.append_pair(name, value);
            }
        }
        let response = self
            .http
            .post(url)
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ClickHouseError::Server {
                status: status.as_u16(),
                body: text.trim().to_string(),
            });
        }
        Ok(text)
    }
}

/// ClickHouse parses query-parameter values in its "escaped" (TSV) text
/// format: a raw tab or newline is a parse error, and backslash sequences are
/// decoded (`C:\new` would arrive as `C:` + newline + `ew`). Escaping these
/// four characters makes every value arrive byte-for-byte (verified against
/// ClickHouse 26.9). Quotes need no escaping — the value is never parsed as
/// SQL.
fn escape_param_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_param_value;

    #[test]
    fn escapes_exactly_the_tsv_special_characters() {
        assert_eq!(escape_param_value("it's \"fine\""), "it's \"fine\"");
        assert_eq!(escape_param_value("C:\\new"), "C:\\\\new");
        assert_eq!(escape_param_value("a\tb\nc\rd"), "a\\tb\\nc\\rd");
    }
}
```

In `nami-engine/src/lib.rs`, add `pub mod clickhouse;` after `pub mod auth;`.

- [ ] **Step 6: Map ClickHouse errors to a generic 500**

In `nami-engine/src/error.rs`, add the variant after `Conflict(String),`:

```rust
    #[error("clickhouse error")]
    ClickHouse(#[from] crate::clickhouse::ClickHouseError),
```

add the match arm after the `AppError::Conflict(msg) => ...` arm:

```rust
            AppError::ClickHouse(e) => {
                // ClickHouse's error text can include SQL and schema details:
                // log it, never return it.
                tracing::error!(error = %e, "clickhouse error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
```

and append this unit test at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clickhouse_errors_become_a_generic_500() {
        let error = AppError::ClickHouse(crate::clickhouse::ClickHouseError::Server {
            status: 404,
            body: "Code: 60. DB::Exception: Unknown table expression identifier 'uptime_checks'"
                .to_string(),
        });
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, json!({ "error": "internal error" }));
    }
}
```

- [ ] **Step 7: Add `clickhouse` to `AppState`, plus the shared test constructor**

Replace `nami-engine/src/state.rs` with:

```rust
use sqlx::PgPool;

use crate::{clickhouse::ClickHouse, config::Config, email};

/// A ClickHouse database name that is never created. `AppState::for_test`
/// binds its client to it, so a test that touches ClickHouse without opting
/// into `ClickHouse::create_test_database()` fails loudly (UNKNOWN_DATABASE)
/// instead of reading or writing the developer's real `nami` database.
const UNCONFIGURED_TEST_CLICKHOUSE_DATABASE: &str = "nami_unconfigured_in_tests";

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: email::Mailer,
    pub clickhouse: ClickHouse,
    pub config: Config,
}

impl AppState {
    /// Test-only constructor (same precedent as
    /// `router::with_fallback_connect_info`): every field gets the
    /// defaults tests expect — `Config::from_env_for_test()`, the Mailpit
    /// mailer, and a ClickHouse client bound to a database that does not
    /// exist. Tests override single fields with struct-update syntax, e.g.
    /// `AppState { mailer: Mailer::new("localhost", 1), ..AppState::for_test(pool) }`,
    /// so adding a field to `AppState` never touches test files again.
    pub fn for_test(pool: PgPool) -> AppState {
        let config = Config::from_env_for_test();
        AppState {
            db: pool,
            mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
            clickhouse: ClickHouse::new(
                &config.clickhouse_url,
                &config.clickhouse_user,
                &config.clickhouse_password,
                UNCONFIGURED_TEST_CLICKHOUSE_DATABASE,
            ),
            config,
        }
    }
}
```

In `nami-engine/src/main.rs`, change the import line to

```rust
use nami_engine::{clickhouse::ClickHouse, config::Config, db, email, router, state::AppState};
```

insert after the `sqlx::migrate!(...)...expect("failed to run migrations");` statement:

```rust

    let clickhouse = ClickHouse::new(
        &config.clickhouse_url,
        &config.clickhouse_user,
        &config.clickhouse_password,
        &config.clickhouse_database,
    );
    clickhouse
        .migrate()
        .await
        .expect("failed to run ClickHouse migrations");
```

and add `clickhouse,` to the `AppState { ... }` literal, between `mailer: ...,` and `config,`.

- [ ] **Step 8: Refactor every existing test file to `AppState::for_test`**

From now on no test file builds `AppState` field by field, so later fields never touch them. Make exactly these edits:

- `tests/health.rs`: replace the `let mailer = ...;` line and the 5-line `let state = nami_engine::state::AppState { ... };` with `let state = nami_engine::state::AppState::for_test(pool);`
- `tests/rbac.rs`: import line → `use nami_engine::state::AppState;`; the 5-line `let state = AppState { ... };` → `let state = AppState::for_test(pool);`
- `tests/register.rs`, `tests/login.rs`, `tests/rate_limit.rs`: import line → `use nami_engine::state::AppState;`; `let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };` → `let state = AppState::for_test(pool);`
- `tests/totp.rs`: import line → `use nami_engine::{state::AppState, auth};`; same `let state` replacement.
- `tests/oauth.rs`: import line → `use nami_engine::{state::AppState, config::Config};`; `let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config };` → `let state = AppState { config, ..AppState::for_test(pool) };`
- `tests/password_reset.rs`: import line → `use nami_engine::{state::AppState, email::Mailer};`; the `test_app` helper's `let state = ...` → `let state = AppState::for_test(pool);`; both port-1 mailer constructions → `let state = AppState { mailer: Mailer::new("localhost", 1), ..AppState::for_test(pool) };`

Then `grep -rn "AppState {" nami-engine/tests` must list only the three struct-update lines (`oauth.rs` once, `password_reset.rs` twice).

- [ ] **Step 9: Run the new tests and the whole suite**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test clickhouse && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: `tests/clickhouse.rs` 6 passed; every other binary passes as before, with no new warnings. The literal-binding test fails if `escape_param_value` is bypassed (verified: ClickHouse answers `BAD_QUERY_PARAMETER`), so it pins the escaping.

- [ ] **Step 10: Commit**

```bash
git add nami-engine/docker-compose.yaml nami-engine/.env.example nami-engine/src nami-engine/tests
git commit -F - <<'EOF'
feat(engine): add ClickHouse HTTP client and shared test state (NAMI-UP-1)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 2 (NAMI-UP-2): Monitors model + CRUD API

Branch `feat/NAMI-UP-2-monitors-crud`, worktree `.worktrees/NAMI-UP-2/`.

**Files:**
- Create: `nami-engine/migrations/0009_create_monitors.sql`
- Create: `nami-engine/src/models/monitor.rs`, `nami-engine/src/routes/monitors.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/common/mod.rs` (new shared helpers), `nami-engine/tests/monitors_api.rs`

**Interfaces:**
- Consumes: `AppState::for_test`, `require_project_role(&PgPool, &CurrentUser, Uuid, &str)`, `CurrentUser` extractor, `AppError::{Validation, NotFound, Database}`.
- Produces:
  - `models::validate_name(raw: &str) -> Result<String, String>` (trimmed, 1-200 chars, no control characters).
  - `models::monitor::Monitor` — `#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]`, every column of `monitors` as a `pub` field: `id: Uuid, project_id: Uuid, name: String, check_type: String, url: Option<String>, host: Option<String>, port: Option<i32>, http_method: String, keyword: Option<String>, keyword_mode: String, expected_status_min: i32, expected_status_max: i32, interval_seconds: i32, timeout_seconds: i32, confirmation_threshold: i32, paused: bool, current_status: String, consecutive_failures: i32, last_checked_at: Option<DateTime<Utc>>, next_check_at: DateTime<Utc>, created_at: DateTime<Utc>`.
  - `MonitorConfig` (editable fields), `CreateMonitorRequest::into_config(self) -> MonitorConfig`, `PatchMonitorRequest::apply_to(self, MonitorConfig) -> MonitorConfig`, `validate(MonitorConfig) -> Result<MonitorConfig, String>`, `MonitorConfig::changes_check_of(&self, &Monitor) -> bool`.
  - Queries: `insert_monitor(&PgPool, Uuid, &MonitorConfig) -> Result<Monitor, sqlx::Error>`, `list_for_project(&PgPool, Uuid) -> Result<Vec<Monitor>, sqlx::Error>`, `find_in_project(&PgPool, project_id: Uuid, monitor_id: Uuid) -> Result<Option<Monitor>, sqlx::Error>`, `update_in_project(&PgPool, Uuid, Uuid, PatchMonitorRequest) -> Result<Option<Monitor>, AppError>`, `delete_in_project(&PgPool, Uuid, Uuid) -> Result<bool, sqlx::Error>`.
  - Routes: `POST|GET /projects/{id}/monitors` (create → `201` + monitor; list → monitors ordered by name), `GET|PATCH|DELETE /projects/{id}/monitors/{monitor_id}` (delete → `{"deleted": true}`).
  - Test helpers in `tests/common/mod.rs`: `app(AppState) -> Router`, `test_app(PgPool) -> Router`, `unique_email(&str) -> String`, `seed_user(&PgPool, email, global_role) -> Uuid`, `seed_project(&PgPool) -> Uuid`, `add_member(&PgPool, project, user, role)`, `login(&Router, email) -> HeaderValue`, `user_with_role(&Router, &PgPool, project, Option<&str>) -> (Uuid, String, HeaderValue)`, `call(&Router, method, uri, Option<&HeaderValue>, Option<Value>) -> (StatusCode, Value)`, `seed_http_monitor(&PgPool, project, name, url, confirmation_threshold) -> Uuid`.

- [ ] **Step 1: Write the shared test helpers `tests/common/mod.rs`**

`login` sends a fresh `X-Forwarded-For` per call: `/auth/login` is rate-limited per IP with a burst of 5, and a test may log in several users.

```rust
//! Helpers shared by the uptime-monitoring integration tests. Every test
//! binary compiles this module separately and uses a different subset of
//! it, hence the blanket `dead_code` allowance.
#![allow(dead_code)]

use std::sync::OnceLock;

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, StatusCode},
    Router,
};
use nami_engine::state::AppState;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "password123";

/// argon2 is deliberately slow: hash the shared fixture password once per
/// test binary instead of once per seeded user.
fn password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| nami_engine::auth::password::hash_password(PASSWORD).unwrap())
}

/// The full production router (same layers as `main.rs`) over `state`.
pub fn app(state: AppState) -> Router {
    nami_engine::router::with_fallback_connect_info(nami_engine::router::build_router(state)).layer(
        nami_engine::router::configure_session_layer(tower_sessions::MemoryStore::default(), false),
    )
}

pub fn test_app(pool: sqlx::PgPool) -> Router {
    app(AppState::for_test(pool))
}

/// Unique per call: Mailpit keeps messages across test runs and tests run
/// in parallel, so mail assertions always filter by their own recipient.
pub fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::new_v4().simple())
}

pub async fn seed_user(pool: &sqlx::PgPool, email: &str, global_role: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into users (email, password_hash, email_verified_at, global_role)
         values ($1, $2, now(), $3) returning id",
    )
    .bind(email)
    .bind(password_hash())
    .bind(global_role)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub async fn seed_project(pool: &sqlx::PgPool) -> Uuid {
    let slug = format!("p-{}", Uuid::new_v4().simple());
    sqlx::query_scalar("insert into projects (name, slug) values ($1, $1) returning id")
        .bind(&slug)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn add_member(pool: &sqlx::PgPool, project_id: Uuid, user_id: Uuid, role: &str) {
    sqlx::query("insert into project_members (project_id, user_id, project_role) values ($1, $2, $3)")
        .bind(project_id)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

/// Logs in through the real `/auth/login` route and returns the session
/// cookie. Each call uses its own `X-Forwarded-For` address because the
/// login route is rate-limited per IP (burst of 5) and a test may log in
/// several users.
pub async fn login(app: &Router, email: &str) -> HeaderValue {
    let ip = Uuid::new_v4().as_bytes()[..3].to_vec();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-for", format!("10.{}.{}.{}", ip[0], ip[1], ip[2]))
                .body(Body::from(json!({ "email": email, "password": PASSWORD }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "login failed for {email}");
    response.headers().get(header::SET_COOKIE).unwrap().clone()
}

/// A global-`member` user holding `role` on `project_id` (or no membership
/// at all when `role` is `None`), logged in. Returns (user id, email, cookie).
pub async fn user_with_role(
    app: &Router,
    pool: &sqlx::PgPool,
    project_id: Uuid,
    role: Option<&str>,
) -> (Uuid, String, HeaderValue) {
    let email = unique_email(role.unwrap_or("outsider"));
    let user_id = seed_user(pool, &email, "member").await;
    if let Some(role) = role {
        add_member(pool, project_id, user_id, role).await;
    }
    let cookie = login(app, &email).await;
    (user_id, email, cookie)
}

/// Sends one request through the router. Returns the status and the JSON
/// body (`Value::Null` when the body is empty or not JSON).
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&HeaderValue>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Inserts an http monitor directly (bypassing the API).
pub async fn seed_http_monitor(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    name: &str,
    url: &str,
    confirmation_threshold: i32,
) -> Uuid {
    sqlx::query_scalar(
        "insert into monitors (project_id, name, check_type, url, confirmation_threshold, timeout_seconds)
         values ($1, $2, 'http', $3, $4, 5) returning id",
    )
    .bind(project_id)
    .bind(name)
    .bind(url)
    .bind(confirmation_threshold)
    .fetch_one(pool)
    .await
    .unwrap()
}
```

- [ ] **Step 2: Write the failing API test `tests/monitors_api.rs`**

```rust
mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

fn http_monitor(name: &str) -> Value {
    json!({ "name": name, "check_type": "http", "url": "https://example.com/health" })
}

#[sqlx::test]
async fn member_creates_reads_and_updates_admin_deletes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(&app, "POST", &collection, Some(&member), Some(http_monitor("API"))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["http_method"], "GET", "defaults are applied");
    assert_eq!(created["expected_status_min"], 200);
    assert_eq!(created["expected_status_max"], 399);
    assert_eq!(created["interval_seconds"], 60);
    assert_eq!(created["timeout_seconds"], 10);
    assert_eq!(created["confirmation_threshold"], 2);
    assert_eq!(created["current_status"], "pending");
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("{collection}/{id}");

    let (status, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());

    let (status, patched) =
        call(&app, "PATCH", &item, Some(&member), Some(json!({ "name": "API v2", "paused": true }))).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"], "API v2");
    assert_eq!(patched["paused"], true);
    assert_eq!(patched["url"], "https://example.com/health", "fields absent from a PATCH are kept");

    let (status, _) = call(&app, "DELETE", &item, Some(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "members cannot delete");

    let (status, body) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": true }));
    let (status, _) = call(&app, "GET", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewer_can_read_but_not_write(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/monitors");
    let item = format!("{collection}/{monitor}");

    assert_eq!(call(&app, "GET", &collection, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &item, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(
        call(&app, "POST", &collection, Some(&viewer), Some(http_monitor("x"))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "PATCH", &item, Some(&viewer), Some(json!({ "paused": true }))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&app, "DELETE", &item, Some(&viewer), None).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn non_member_gets_403_never_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitors");
    let existing = format!("{collection}/{monitor}");
    let missing = format!("{collection}/{}", Uuid::new_v4());

    assert_eq!(call(&app, "GET", &collection, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        call(&app, "POST", &collection, Some(&outsider), Some(http_monitor("x"))).await.0,
        StatusCode::FORBIDDEN
    );
    for uri in [&existing, &missing] {
        assert_eq!(call(&app, "GET", uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            call(&app, "PATCH", uri, Some(&outsider), Some(json!({ "paused": true }))).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(call(&app, "DELETE", uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    }
}

#[sqlx::test]
async fn monitor_of_another_project_is_404_inside_an_accessible_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project_a = seed_project(&pool).await;
    let project_b = seed_project(&pool).await;
    let foreign = seed_http_monitor(&pool, project_b, "B's monitor", "https://example.com", 2).await;
    let (_, _, admin_a) = user_with_role(&app, &pool, project_a, Some("admin")).await;
    let uri = format!("/projects/{project_a}/monitors/{foreign}");

    assert_eq!(call(&app, "GET", &uri, Some(&admin_a), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&app, "PATCH", &uri, Some(&admin_a), Some(json!({ "name": "hijacked" }))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "DELETE", &uri, Some(&admin_a), None).await.0, StatusCode::NOT_FOUND);
    let (_, list) = call(&app, "GET", &format!("/projects/{project_a}/monitors"), Some(&admin_a), None).await;
    assert_eq!(list, json!([]));

    let (name, project): (String, Uuid) =
        sqlx::query_as("select name, project_id from monitors where id = $1")
            .bind(foreign)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((name.as_str(), project), ("B's monitor", project_b), "B's monitor is untouched");
}

/// Review Focus: a keyword monitor with HEAD (no body to search) is
/// rejected instead of silently reporting DOWN forever.
#[sqlx::test]
async fn invalid_configs_are_rejected_with_400_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let url = "https://example.com";
    let cases = [
        (json!({ "name": "x", "check_type": "icmp", "url": url }), "check_type"),
        (json!({ "name": "  ", "check_type": "http", "url": url }), "name"),
        (json!({ "name": "a\r\nBcc: x@evil.test", "check_type": "http", "url": url }), "control"),
        (json!({ "name": "x", "check_type": "http" }), "url"),
        (json!({ "name": "x", "check_type": "http", "url": "ftp://example.com" }), "url"),
        (json!({ "name": "x", "check_type": "http", "url": "not a url" }), "url"),
        (json!({ "name": "x", "check_type": "http", "url": url, "http_method": "POST" }), "http_method"),
        (json!({ "name": "x", "check_type": "keyword", "url": url }), "keyword"),
        (json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "   " }), "keyword"),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "ok", "http_method": "HEAD" }),
            "GET",
        ),
        (json!({ "name": "x", "check_type": "tcp", "port": 5432 }), "host"),
        (json!({ "name": "x", "check_type": "tcp", "host": "db" }), "port"),
        (json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 0 }), "port"),
        (json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 65536 }), "port"),
        (json!({ "name": "x", "check_type": "http", "url": url, "interval_seconds": 29 }), "interval_seconds"),
        (json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 0 }), "timeout_seconds"),
        (json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 61 }), "timeout_seconds"),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "expected_status_min": 400, "expected_status_max": 300 }),
            "expected_status_min",
        ),
        (json!({ "name": "x", "check_type": "http", "url": url, "expected_status_max": 600 }), "between 100 and 599"),
        (json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 0 }), "confirmation_threshold"),
        (json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 11 }), "confirmation_threshold"),
    ];
    for (body, fragment) in cases {
        let (status, json) =
            call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(message.contains(fragment), "{body}: error {message:?} should mention {fragment:?}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0, "no invalid monitor was written");
}

#[sqlx::test]
async fn invalid_patch_is_rejected_and_leaves_the_monitor_unchanged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(&app, "PATCH", &uri, Some(&member), Some(json!({ "check_type": "tcp" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("host"));
    let (status, _) = call(&app, "PATCH", &uri, Some(&member), Some(json!({ "interval_seconds": 10 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, current) = call(&app, "GET", &uri, Some(&member), None).await;
    assert_eq!(current["check_type"], "http");
    assert_eq!(current["url"], "https://example.com");
    assert_eq!(current["interval_seconds"], 60);
}

#[sqlx::test]
async fn switching_check_type_clears_fields_of_the_old_type(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "DB", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "check_type": "tcp", "host": " db.internal ", "port": 5432 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "db.internal", "host is trimmed");
    assert_eq!(body["port"], 5432);
    assert_eq!(body["url"], Value::Null, "the http url no longer applies");
}

#[sqlx::test]
async fn editing_check_config_reschedules_now_but_renaming_does_not(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let due_now = || async {
        sqlx::query_scalar::<_, bool>("select next_check_at <= now() from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    call(&app, "PATCH", &uri, Some(&member), Some(json!({ "name": "renamed" }))).await;
    assert!(!due_now().await, "a rename keeps the schedule");

    let (status, _) = call(&app, "PATCH", &uri, Some(&member), Some(json!({ "url": "https://example.org/" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(due_now().await, "a new url is checked on the next tick");
}

#[sqlx::test]
async fn server_owned_fields_in_the_body_are_ignored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let mut body = http_monitor("API");
    body["project_id"] = json!(other_project);
    body["current_status"] = json!("up");
    body["consecutive_failures"] = json!(7);
    let (status, created) = call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["current_status"], "pending");

    let uri = format!("/projects/{project}/monitors/{}", created["id"].as_str().unwrap());
    let (status, patched) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "name": "x", "project_id": other_project, "current_status": "down", "consecutive_failures": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["project_id"], project.to_string());
    assert_eq!(patched["current_status"], "pending");
    assert_eq!(patched["consecutive_failures"], 0);
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test monitors_api`
Expected: FAIL — tests panic with `relation "monitors" does not exist` (from `seed_http_monitor`) or get `404` where they expect `201`.

- [ ] **Step 4: Write the migration and extend the migrations test**

`nami-engine/migrations/0009_create_monitors.sql`:

```sql
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

-- The worker's claim query scans unpaused monitors in next_check_at order.
create index monitors_due_idx on monitors (next_check_at) where not paused;
create index monitors_project_idx on monitors (project_id);
```

In `nami-engine/tests/migrations.rs`, add `"monitors",` after `"password_reset_tokens",` in the expected-table list.

- [ ] **Step 5: Write the model**

Replace `nami-engine/src/models/mod.rs` with:

```rust
pub mod user;
pub mod token;
pub mod project;
pub mod monitor;

/// Name rule shared by monitors and heartbeats: trimmed, 1-200 characters,
/// and no control characters (names end up in alert email subjects).
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.chars().count() > 200 {
        return Err("name must be at most 200 characters".into());
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".into());
    }
    Ok(name.to_string())
}
```

`nami-engine/src/models/monitor.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::validate_name;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
    pub current_status: String,
    pub consecutive_failures: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// The user-editable fields of a monitor. Create and PATCH both build one,
/// run it through [`validate`], and only then write it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorConfig {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
}

impl From<&Monitor> for MonitorConfig {
    fn from(m: &Monitor) -> Self {
        MonitorConfig {
            name: m.name.clone(),
            check_type: m.check_type.clone(),
            url: m.url.clone(),
            host: m.host.clone(),
            port: m.port,
            http_method: m.http_method.clone(),
            keyword: m.keyword.clone(),
            keyword_mode: m.keyword_mode.clone(),
            expected_status_min: m.expected_status_min,
            expected_status_max: m.expected_status_max,
            interval_seconds: m.interval_seconds,
            timeout_seconds: m.timeout_seconds,
            confirmation_threshold: m.confirmation_threshold,
            paused: m.paused,
        }
    }
}

impl MonitorConfig {
    /// True when a field that changes how or when the monitor is checked
    /// differs from `current`. Renaming, pausing and the confirmation
    /// threshold do not count.
    pub fn changes_check_of(&self, current: &Monitor) -> bool {
        self.check_type != current.check_type
            || self.url != current.url
            || self.host != current.host
            || self.port != current.port
            || self.http_method != current.http_method
            || self.keyword != current.keyword
            || self.keyword_mode != current.keyword_mode
            || self.expected_status_min != current.expected_status_min
            || self.expected_status_max != current.expected_status_max
            || self.interval_seconds != current.interval_seconds
            || self.timeout_seconds != current.timeout_seconds
    }
}

/// `POST /projects/{id}/monitors` body. Server-owned fields (`id`,
/// `project_id`, `current_status`, ...) are not part of it, so a client that
/// sends them is ignored rather than obeyed.
#[derive(Debug, Deserialize)]
pub struct CreateMonitorRequest {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    #[serde(default = "default_http_method")]
    pub http_method: String,
    pub keyword: Option<String>,
    #[serde(default = "default_keyword_mode")]
    pub keyword_mode: String,
    #[serde(default = "default_expected_status_min")]
    pub expected_status_min: i32,
    #[serde(default = "default_expected_status_max")]
    pub expected_status_max: i32,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: i32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: i32,
    #[serde(default = "default_confirmation_threshold")]
    pub confirmation_threshold: i32,
    #[serde(default)]
    pub paused: bool,
}

fn default_http_method() -> String {
    "GET".to_string()
}
fn default_keyword_mode() -> String {
    "contains".to_string()
}
fn default_expected_status_min() -> i32 {
    200
}
fn default_expected_status_max() -> i32 {
    399
}
fn default_interval_seconds() -> i32 {
    60
}
fn default_timeout_seconds() -> i32 {
    10
}
fn default_confirmation_threshold() -> i32 {
    2
}

impl CreateMonitorRequest {
    pub fn into_config(self) -> MonitorConfig {
        MonitorConfig {
            name: self.name,
            check_type: self.check_type,
            url: self.url,
            host: self.host,
            port: self.port,
            http_method: self.http_method,
            keyword: self.keyword,
            keyword_mode: self.keyword_mode,
            expected_status_min: self.expected_status_min,
            expected_status_max: self.expected_status_max,
            interval_seconds: self.interval_seconds,
            timeout_seconds: self.timeout_seconds,
            confirmation_threshold: self.confirmation_threshold,
            paused: self.paused,
        }
    }
}

/// `PATCH` body: any subset of the editable fields. Absent fields keep
/// their current value; unknown and server-owned fields are ignored.
#[derive(Debug, Default, Deserialize)]
pub struct PatchMonitorRequest {
    pub name: Option<String>,
    pub check_type: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: Option<String>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    pub expected_status_min: Option<i32>,
    pub expected_status_max: Option<i32>,
    pub interval_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub confirmation_threshold: Option<i32>,
    pub paused: Option<bool>,
}

impl PatchMonitorRequest {
    pub fn apply_to(self, base: MonitorConfig) -> MonitorConfig {
        MonitorConfig {
            name: self.name.unwrap_or(base.name),
            check_type: self.check_type.unwrap_or(base.check_type),
            url: self.url.or(base.url),
            host: self.host.or(base.host),
            port: self.port.or(base.port),
            http_method: self.http_method.unwrap_or(base.http_method),
            keyword: self.keyword.or(base.keyword),
            keyword_mode: self.keyword_mode.unwrap_or(base.keyword_mode),
            expected_status_min: self.expected_status_min.unwrap_or(base.expected_status_min),
            expected_status_max: self.expected_status_max.unwrap_or(base.expected_status_max),
            interval_seconds: self.interval_seconds.unwrap_or(base.interval_seconds),
            timeout_seconds: self.timeout_seconds.unwrap_or(base.timeout_seconds),
            confirmation_threshold: self
                .confirmation_threshold
                .unwrap_or(base.confirmation_threshold),
            paused: self.paused.unwrap_or(base.paused),
        }
    }
}

/// Fail-closed validation of a complete config. Returns the normalized
/// config (trimmed name/url/host; fields irrelevant to the check type
/// cleared) or a message for a 400.
pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
    c.name = validate_name(&c.name)?;
    if !matches!(c.http_method.as_str(), "GET" | "HEAD") {
        return Err("http_method must be GET or HEAD".into());
    }
    if !matches!(c.keyword_mode.as_str(), "contains" | "not_contains") {
        return Err("keyword_mode must be contains or not_contains".into());
    }
    match c.check_type.as_str() {
        "http" | "keyword" => {
            let url = c.url.as_deref().map(str::trim).unwrap_or_default().to_string();
            let parsed = reqwest::Url::parse(&url)
                .map_err(|_| "url must be a valid http(s) URL".to_string())?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err("url must be a valid http(s) URL".into());
            }
            c.url = Some(url);
            c.host = None;
            c.port = None;
            if c.check_type == "keyword" {
                if c.keyword.as_deref().is_none_or(|k| k.trim().is_empty()) {
                    return Err("keyword is required for keyword monitors".into());
                }
                if c.http_method != "GET" {
                    return Err("keyword monitors must use GET (HEAD responses have no body)".into());
                }
            } else {
                c.keyword = None;
            }
        }
        "tcp" => {
            let host = c.host.as_deref().map(str::trim).unwrap_or_default().to_string();
            if host.is_empty() {
                return Err("host is required for tcp monitors".into());
            }
            if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
                return Err("port must be between 1 and 65535".into());
            }
            c.host = Some(host);
            c.url = None;
            c.keyword = None;
        }
        _ => return Err("check_type must be http, keyword or tcp".into()),
    }
    if c.interval_seconds < 30 {
        return Err("interval_seconds must be at least 30".into());
    }
    if !(1..=60).contains(&c.timeout_seconds) {
        return Err("timeout_seconds must be between 1 and 60".into());
    }
    if !(100..=599).contains(&c.expected_status_min) || !(100..=599).contains(&c.expected_status_max)
    {
        return Err("expected status codes must be between 100 and 599".into());
    }
    if c.expected_status_min > c.expected_status_max {
        return Err("expected_status_min must not exceed expected_status_max".into());
    }
    if !(1..=10).contains(&c.confirmation_threshold) {
        return Err("confirmation_threshold must be between 1 and 10".into());
    }
    Ok(c)
}

pub async fn insert_monitor(
    pool: &PgPool,
    project_id: Uuid,
    c: &MonitorConfig,
) -> Result<Monitor, sqlx::Error> {
    sqlx::query_as(
        "insert into monitors (project_id, name, check_type, url, host, port, http_method,
             keyword, keyword_mode, expected_status_min, expected_status_max,
             interval_seconds, timeout_seconds, confirmation_threshold, paused)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         returning *",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.check_type)
    .bind(&c.url)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.http_method)
    .bind(&c.keyword)
    .bind(&c.keyword_mode)
    .bind(c.expected_status_min)
    .bind(c.expected_status_max)
    .bind(c.interval_seconds)
    .bind(c.timeout_seconds)
    .bind(c.confirmation_threshold)
    .bind(c.paused)
    .fetch_one(pool)
    .await
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Every lookup is scoped by BOTH ids: a monitor id from another project is
/// simply not found here (404), even for a caller who can see that project.
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<Option<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write in one transaction. The `for update` row lock makes
/// concurrent PATCHes apply one after the other instead of the second one
/// overwriting the first with values it read before the first committed.
/// Editing any check-config field reschedules the monitor to run now.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
    patch: PatchMonitorRequest,
) -> Result<Option<Monitor>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Monitor> =
        sqlx::query_as("select * from monitors where id = $1 and project_id = $2 for update")
            .bind(monitor_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let config = validate(patch.apply_to(MonitorConfig::from(&current))).map_err(AppError::Validation)?;
    let reschedule = config.changes_check_of(&current);
    let updated: Monitor = sqlx::query_as(
        "update monitors set
             name = $3, check_type = $4, url = $5, host = $6, port = $7, http_method = $8,
             keyword = $9, keyword_mode = $10, expected_status_min = $11,
             expected_status_max = $12, interval_seconds = $13, timeout_seconds = $14,
             confirmation_threshold = $15, paused = $16,
             next_check_at = case when $17 then now() else next_check_at end
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(monitor_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(&config.check_type)
    .bind(&config.url)
    .bind(&config.host)
    .bind(config.port)
    .bind(&config.http_method)
    .bind(&config.keyword)
    .bind(&config.keyword_mode)
    .bind(config.expected_status_min)
    .bind(config.expected_status_max)
    .bind(config.interval_seconds)
    .bind(config.timeout_seconds)
    .bind(config.confirmation_threshold)
    .bind(config.paused)
    .bind(reschedule)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Returns false when no monitor with this id exists in this project.
pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
```

- [ ] **Step 6: Write the routes and mount them**

`nami-engine/src/routes/monitors.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::monitor::{self, CreateMonitorRequest, Monitor, PatchMonitorRequest},
    state::AppState,
};

// RBAC mirrors routes/projects.rs: viewer reads, member creates/edits,
// admin deletes. Authorization runs before any lookup, so a caller without
// access gets 403 whether or not the monitor exists.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/monitors", get(list_monitors).post(create_monitor))
        .route(
            "/projects/{id}/monitors/{monitor_id}",
            get(get_monitor).patch(patch_monitor).delete(delete_monitor),
        )
}

async fn list_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Monitor>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(monitor::list_for_project(&state.db, project_id).await?))
}

async fn create_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateMonitorRequest>,
) -> Result<(StatusCode, Json<Monitor>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = monitor::insert_monitor(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // A global admin bypasses membership, so the project itself may
            // not exist: SQLSTATE 23503 (foreign_key_violation) -> 404.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchMonitorRequest>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = monitor::update_in_project(&state.db, project_id, monitor_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !monitor::delete_in_project(&state.db, project_id, monitor_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}
```

In `nami-engine/src/routes/mod.rs` add `pub mod monitors;` after `pub mod projects;`. In `nami-engine/src/router.rs::build_router`, add `.merge(routes::monitors::router())` after `.merge(routes::projects::router())`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test monitors_api --test migrations && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: `monitors_api` 9 passed, `migrations` 1 passed, everything else unchanged, no warnings.

- [ ] **Step 8: Commit**

```bash
git add nami-engine/migrations/0009_create_monitors.sql nami-engine/src nami-engine/tests
git commit -F - <<'EOF'
feat(engine): add monitors model and CRUD API (NAMI-UP-2)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 3 (NAMI-UP-3): Checker

Branch `feat/NAMI-UP-3-checker`, worktree `.worktrees/NAMI-UP-3/`.

**Files:**
- Create: `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/checker.rs`
- Modify: `nami-engine/src/lib.rs`
- Test: `nami-engine/tests/uptime_checker.rs`

**Interfaces:**
- Consumes: `models::monitor::Monitor` (Task 2; all fields `pub`).
- Produces:
  - `uptime::checker::CheckStatus` — `Up | Down`, `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]`, serde `"up"`/`"down"` (matches the ClickHouse `Enum8`).
  - `uptime::checker::CheckOutcome { status: CheckStatus, status_code: Option<u16>, response_time_ms: Option<u32>, error: Option<String>, checked_at: DateTime<Utc> }` (`Debug, Clone, PartialEq`).
  - `build_http_client() -> reqwest::Client` (redirects followed, no idle connection reuse).
  - `async perform_check(http: &reqwest::Client, monitor: &Monitor) -> CheckOutcome` — never fails; errors become `Down` with short messages: `timeout after {n}s`, `connection refused`, `connection reset`, `connection failed: <root cause>`, `too many redirects`, `expected status {min}-{max}, got {code}`, `keyword "{k}" not found`, `keyword "{k}" found`.
  - `MAX_BODY_BYTES: usize = 1 MiB`.

- [ ] **Step 1: Write the failing test `tests/uptime_checker.rs`**

```rust
use std::time::{Duration, Instant};

use chrono::Utc;
use nami_engine::{
    models::monitor::Monitor,
    uptime::checker::{build_http_client, perform_check, CheckStatus, MAX_BODY_BYTES},
};
use uuid::Uuid;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn monitor(check_type: &str) -> Monitor {
    Monitor {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        name: "test".to_string(),
        check_type: check_type.to_string(),
        url: None,
        host: None,
        port: None,
        http_method: "GET".to_string(),
        keyword: None,
        keyword_mode: "contains".to_string(),
        expected_status_min: 200,
        expected_status_max: 399,
        interval_seconds: 60,
        timeout_seconds: 5,
        confirmation_threshold: 2,
        paused: false,
        current_status: "pending".to_string(),
        consecutive_failures: 0,
        last_checked_at: None,
        next_check_at: Utc::now(),
        created_at: Utc::now(),
    }
}

fn http_monitor(url: String) -> Monitor {
    Monitor { url: Some(url), ..monitor("http") }
}

fn keyword_monitor(url: String, keyword: &str, mode: &str) -> Monitor {
    Monitor {
        url: Some(url),
        keyword: Some(keyword.to_string()),
        keyword_mode: mode.to_string(),
        ..monitor("keyword")
    }
}

async fn server_returning(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/")).respond_with(template).mount(&server).await;
    server
}

/// A local port with nothing listening on it.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn status_inside_the_expected_range_is_up() {
    let server = server_returning(ResponseTemplate::new(204)).await;
    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/", server.uri()))).await;
    assert_eq!(outcome.status, CheckStatus::Up);
    assert_eq!(outcome.status_code, Some(204));
    assert!(outcome.response_time_ms.is_some());
    assert_eq!(outcome.error, None);
}

#[tokio::test]
async fn status_outside_the_expected_range_is_down_with_the_reason() {
    let server = server_returning(ResponseTemplate::new(503)).await;
    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/", server.uri()))).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.status_code, Some(503));
    assert_eq!(outcome.error.as_deref(), Some("expected status 200-399, got 503"));
}

#[tokio::test]
async fn a_custom_expected_range_is_honoured() {
    let server = server_returning(ResponseTemplate::new(204)).await;
    let only_200 = Monitor {
        expected_status_min: 200,
        expected_status_max: 200,
        ..http_monitor(format!("{}/", server.uri()))
    };
    let outcome = perform_check(&build_http_client(), &only_200).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("expected status 200-200, got 204"));
}

#[tokio::test]
async fn redirects_are_followed_to_the_final_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", format!("{}/new", server.uri())))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/new")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/old", server.uri()))).await;
    assert_eq!(outcome.status, CheckStatus::Up);
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn head_monitors_send_head_requests() {
    let server = MockServer::start().await;
    // Only HEAD is mocked: a GET would get wiremock's default 404.
    Mock::given(method("HEAD")).and(path("/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let head = Monitor { http_method: "HEAD".to_string(), ..http_monitor(format!("{}/", server.uri())) };
    let outcome = perform_check(&build_http_client(), &head).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn keyword_present_is_up_and_absent_is_down() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("status: all systems go")).await;
    let url = format!("{}/", server.uri());

    let present = perform_check(&build_http_client(), &keyword_monitor(url.clone(), "systems go", "contains")).await;
    assert_eq!(present.status, CheckStatus::Up, "{:?}", present.error);

    let absent = perform_check(&build_http_client(), &keyword_monitor(url, "maintenance", "contains")).await;
    assert_eq!(absent.status, CheckStatus::Down);
    assert_eq!(absent.error.as_deref(), Some("keyword \"maintenance\" not found"));
    assert_eq!(absent.status_code, Some(200));
}

#[tokio::test]
async fn keyword_matching_is_case_sensitive() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("Status: OK")).await;
    let outcome =
        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "ok", "contains")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("keyword \"ok\" not found"));
}

#[tokio::test]
async fn not_contains_mode_inverts_the_keyword_check() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("database error")).await;
    let url = format!("{}/", server.uri());

    let found = perform_check(&build_http_client(), &keyword_monitor(url.clone(), "error", "not_contains")).await;
    assert_eq!(found.status, CheckStatus::Down);
    assert_eq!(found.error.as_deref(), Some("keyword \"error\" found"));

    let missing = perform_check(&build_http_client(), &keyword_monitor(url, "panic", "not_contains")).await;
    assert_eq!(missing.status, CheckStatus::Up, "{:?}", missing.error);
}

#[tokio::test]
async fn a_keyword_beyond_the_body_cap_is_not_found() {
    let body = format!("{}NEEDLE", "a".repeat(MAX_BODY_BYTES));
    let server = server_returning(ResponseTemplate::new(200).set_body_string(body)).await;
    let outcome =
        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
}

#[tokio::test]
async fn a_keyword_match_still_needs_an_expected_status() {
    let server = server_returning(ResponseTemplate::new(500).set_body_string("all systems go")).await;
    let outcome =
        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "systems go", "contains"))
            .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("expected status 200-399, got 500"));
}

#[tokio::test]
async fn a_slow_response_times_out_within_the_monitor_timeout() {
    let server = server_returning(ResponseTemplate::new(200).set_delay(Duration::from_secs(5))).await;
    let slow = Monitor { timeout_seconds: 1, ..http_monitor(format!("{}/", server.uri())) };
    let started = Instant::now();
    let outcome = perform_check(&build_http_client(), &slow).await;
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
    assert_eq!(outcome.response_time_ms, None);
}

#[tokio::test]
async fn http_connection_refused_is_down() {
    let outcome =
        perform_check(&build_http_client(), &http_monitor(format!("http://127.0.0.1:{}/", closed_port()))).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn tcp_open_port_is_up() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tcp = Monitor { host: Some("127.0.0.1".to_string()), port: Some(i32::from(port)), ..monitor("tcp") };
    let outcome = perform_check(&build_http_client(), &tcp).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
    drop(listener);
}

#[tokio::test]
async fn tcp_closed_port_is_down_with_connection_refused() {
    let tcp = Monitor { host: Some("127.0.0.1".to_string()), port: Some(i32::from(closed_port())), ..monitor("tcp") };
    let outcome = perform_check(&build_http_client(), &tcp).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd nami-engine && cargo test --test uptime_checker`
Expected: compile error ``unresolved import `nami_engine::uptime` ``.

- [ ] **Step 3: Write the checker**

`nami-engine/src/uptime/mod.rs`:

```rust
//! Uptime monitoring: performing checks (`checker`).
pub mod checker;
```

`nami-engine/src/uptime/checker.rs`:

```rust
//! Performs one check of one monitor. Never fails: every problem (timeout,
//! refused connection, wrong status, missing keyword) becomes a `Down`
//! outcome with a short human-readable `error`.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::monitor::Monitor;

/// Keyword checks read at most this much of the body; a keyword that only
/// appears after the first MiB counts as not found.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub status: CheckStatus,
    pub status_code: Option<u16>,
    pub response_time_ms: Option<u32>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// The client used for checks. Redirects are followed (reqwest's default
/// policy, up to 10 hops). Idle connections are never kept, so every check
/// opens a fresh connection: a pooled keep-alive socket would hide a server
/// that stopped accepting new connections, and would leave connection setup
/// out of the measured response time.
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .user_agent(concat!("Nami-Uptime/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build the uptime HTTP client")
}

/// Runs the monitor's check within its timeout. Response time is measured
/// to the response headers for `http` checks, to the end of the (capped)
/// body for `keyword` checks, and to connection setup for `tcp` checks.
/// Keyword matching is case-sensitive.
pub async fn perform_check(http: &reqwest::Client, monitor: &Monitor) -> CheckOutcome {
    let checked_at = Utc::now();
    let timeout_secs = u64::try_from(monitor.timeout_seconds.clamp(1, 60)).unwrap_or(10);
    let timeout = Duration::from_secs(timeout_secs);
    let started = Instant::now();
    let probe = async {
        match monitor.check_type.as_str() {
            "http" | "keyword" => probe_http(http, monitor, timeout, started).await,
            "tcp" => probe_tcp(monitor, started).await,
            other => Observation::down(None, None, format!("unsupported check type {other:?}")),
        }
    };
    // One deadline around everything (DNS, connect, TLS, headers, body).
    let observation = tokio::time::timeout(timeout, probe)
        .await
        .unwrap_or_else(|_| Observation::down(None, None, format!("timeout after {timeout_secs}s")));
    CheckOutcome {
        status: if observation.error.is_none() { CheckStatus::Up } else { CheckStatus::Down },
        status_code: observation.status_code,
        response_time_ms: observation.response_time_ms,
        error: observation.error,
        checked_at,
    }
}

struct Observation {
    status_code: Option<u16>,
    response_time_ms: Option<u32>,
    error: Option<String>,
}

impl Observation {
    fn up(status_code: Option<u16>, started: Instant) -> Self {
        Self { status_code, response_time_ms: Some(elapsed_ms(started)), error: None }
    }

    fn down(status_code: Option<u16>, response_time_ms: Option<u32>, error: String) -> Self {
        Self { status_code, response_time_ms, error: Some(error) }
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

async fn probe_http(
    http: &reqwest::Client,
    monitor: &Monitor,
    timeout: Duration,
    started: Instant,
) -> Observation {
    let Some(url) = monitor.url.as_deref() else {
        return Observation::down(None, None, "monitor has no url".to_string());
    };
    let method = if monitor.http_method == "HEAD" { reqwest::Method::HEAD } else { reqwest::Method::GET };
    let mut response = match http.request(method, url).timeout(timeout).send().await {
        Ok(response) => response,
        Err(e) => return Observation::down(None, None, describe_request_error(&e, timeout)),
    };
    let code = response.status().as_u16();
    let in_range = (monitor.expected_status_min..=monitor.expected_status_max).contains(&i32::from(code));
    if !in_range {
        return Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!(
                "expected status {}-{}, got {code}",
                monitor.expected_status_min, monitor.expected_status_max
            ),
        );
    }
    if monitor.check_type != "keyword" {
        return Observation::up(Some(code), started);
    }

    let body = match read_capped(&mut response, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(e) => return Observation::down(Some(code), None, describe_request_error(&e, timeout)),
    };
    let keyword = monitor.keyword.as_deref().unwrap_or_default();
    let found = String::from_utf8_lossy(&body).contains(keyword);
    match (monitor.keyword_mode.as_str(), found) {
        ("not_contains", true) => {
            Observation::down(Some(code), Some(elapsed_ms(started)), format!("keyword \"{keyword}\" found"))
        }
        ("not_contains", false) | (_, true) => Observation::up(Some(code), started),
        (_, false) => Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" not found"),
        ),
    }
}

async fn read_capped(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn probe_tcp(monitor: &Monitor, started: Instant) -> Observation {
    let (Some(host), Some(port)) = (monitor.host.as_deref(), monitor.port) else {
        return Observation::down(None, None, "monitor has no host/port".to_string());
    };
    let Ok(port) = u16::try_from(port) else {
        return Observation::down(None, None, format!("invalid port {port}"));
    };
    // A (host, port) tuple, not a "host:port" string, so IPv6 literals work.
    match tokio::net::TcpStream::connect((host, port)).await {
        Ok(_stream) => Observation::up(None, started),
        Err(e) => Observation::down(None, None, describe_io_error(&e)),
    }
}

fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
    if error.is_timeout() {
        return format!("timeout after {}s", timeout.as_secs());
    }
    if error.is_redirect() {
        return "too many redirects".to_string();
    }
    match io_error_kind(error) {
        Some(std::io::ErrorKind::ConnectionRefused) => "connection refused".to_string(),
        Some(std::io::ErrorKind::ConnectionReset) => "connection reset".to_string(),
        _ if error.is_connect() => format!("connection failed: {}", root_cause(error)),
        _ => format!("request failed: {}", root_cause(error)),
    }
}

fn describe_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
        std::io::ErrorKind::ConnectionReset => "connection reset".to_string(),
        _ => format!("connection failed: {error}"),
    }
}

/// reqwest wraps the OS error several layers deep (reqwest -> hyper-util ->
/// io::Error); walk the `source()` chain to find it.
fn io_error_kind(error: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        current = err.source();
    }
    None
}

/// The innermost error's message, e.g. "failed to lookup address
/// information: ..." instead of reqwest's generic "error sending request".
fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}
```

In `nami-engine/src/lib.rs`, add `pub mod uptime;` after `pub mod router;`.

The `connection refused` mapping walks `std::error::Error::source()` down to the `std::io::Error` that hyper-util wraps; this was verified with reqwest 0.13.5 (`io_kind=ConnectionRefused` for a closed local port).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd nami-engine && cargo test --test uptime_checker`
Expected: 14 passed (about 1-2 s; the timeout test waits 1 s by design).

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src/lib.rs nami-engine/src/uptime nami-engine/tests/uptime_checker.rs
git commit -F - <<'EOF'
feat(engine): add http, keyword and tcp uptime checker (NAMI-UP-3)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 4 (NAMI-UP-4): Worker + incidents + alerts

Branch `feat/NAMI-UP-4-worker-incidents-alerts`, worktree `.worktrees/NAMI-UP-4/`.

**Files:**
- Create: `nami-engine/migrations/0010_create_incidents.sql`
- Create: `nami-engine/src/models/incident.rs`, `nami-engine/src/uptime/store.rs`, `nami-engine/src/uptime/alerts.rs`, `nami-engine/src/uptime/worker.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/uptime/mod.rs`, `nami-engine/src/main.rs`, `nami-engine/tests/migrations.rs`, `nami-engine/tests/common/mod.rs`
- Test: `nami-engine/tests/uptime_worker.rs` (plus unit tests in `alerts.rs`)

**Interfaces:**
- Consumes: `ClickHouse::insert_rows`, `ClickHouse::create_test_database`, `AppState`, `AppState::for_test` (Task 1); `Monitor` (Task 2); `CheckOutcome`, `CheckStatus`, `perform_check`, `build_http_client` (Task 3); `Mailer::send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError>` (existing).
- Produces:
  - `models::incident::open_for_monitor(conn: &mut PgConnection, project_id: Uuid, monitor_id: Uuid, title: &str, cause: Option<&str>, started_at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>` (`None` when an incident is already open) and `resolve_for_monitor(conn: &mut PgConnection, monitor_id: Uuid, resolved_at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>`.
  - `uptime::store::UptimeCheckRow { monitor_id, project_id, checked_at, status: CheckStatus, status_code, response_time_ms, error }` (`Serialize + Deserialize`), `UptimeCheckRow::from_outcome(&Monitor, &CheckOutcome) -> Self`, `insert_checks(&ClickHouse, &[UptimeCheckRow]) -> Result<(), ClickHouseError>`.
  - `uptime::alerts::TransitionKind { Down, Recovered }`, `Subject { Monitor(Uuid) }` (Task 6 adds `Heartbeat(Uuid)`), `Transition { kind, subject, project_id: Uuid, name: String, cause: Option<String>, at: DateTime<Utc> }`, `async notify_transition(state: &AppState, transition: &Transition)` (never fails), `render(ui_base_url: &str, &Transition) -> (String, String)`.
  - `uptime::worker::claim_due_monitors(&PgPool, limit: i64) -> Result<Vec<Monitor>, sqlx::Error>`, `apply_outcome(&PgPool, &Monitor, &CheckOutcome) -> Result<Option<Transition>, sqlx::Error>`, `run_due_checks(&AppState) -> Result<usize, AppError>`, `run_forever(AppState)`; constants `TICK` (5 s), `CLAIM_BATCH` (50), `MAX_IN_FLIGHT` (20).
  - Test helpers: `state_with_clickhouse(&PgPool) -> (AppState, ClickHouse)`, `mail_for(recipient) -> Vec<(String, String)>`, `settled_mail(recipient, count) -> Vec<(String, String)>`, `make_due(&PgPool, Uuid)`.

How the pieces fit: `claim_due_monitors` picks and reschedules due monitors in ONE `UPDATE ... WHERE id IN (SELECT ... FOR UPDATE SKIP LOCKED) RETURNING *` (no select-then-update race; overlapping ticks or engine instances never double-claim). `apply_outcome` locks the monitor row (`FOR UPDATE`) and reads state from the locked row, not from the claim-time snapshot. The partial unique index `incidents_one_open_per_monitor` plus `ON CONFLICT ... DO NOTHING` makes a duplicate open incident — and therefore a duplicate DOWN email — impossible even if two writers race. A ClickHouse insert failure is logged and does not stop state changes or alerts.

- [ ] **Step 1: Add Mailpit and ClickHouse helpers to `tests/common/mod.rs`**

Append to `nami-engine/tests/common/mod.rs` (never delete Mailpit messages — other suites share the inbox; filter by recipient instead):

```rust
/// AppState whose ClickHouse client is bound to a fresh per-test database.
/// Call `clickhouse.drop_database()` at the end of the test.
pub async fn state_with_clickhouse(pool: &sqlx::PgPool) -> (AppState, nami_engine::clickhouse::ClickHouse) {
    let clickhouse = nami_engine::clickhouse::ClickHouse::create_test_database()
        .await
        .expect("ClickHouse must be running: docker compose up -d clickhouse");
    (AppState { clickhouse: clickhouse.clone(), ..AppState::for_test(pool.clone()) }, clickhouse)
}

pub const MAILPIT: &str = "http://localhost:8025";

/// Every message Mailpit holds for `recipient`, oldest first, as
/// (subject, plain-text body).
pub async fn mail_for(recipient: &str) -> Vec<(String, String)> {
    let search = url::Url::parse_with_params(
        &format!("{MAILPIT}/api/v1/search"),
        &[("query", format!("to:\"{recipient}\"")), ("limit", "50".to_string())],
    )
    .unwrap();
    let found: Value = reqwest::get(search).await.unwrap().json().await.unwrap();
    let mut messages = Vec::new();
    // Mailpit lists newest first.
    for summary in found["messages"].as_array().cloned().unwrap_or_default().iter().rev() {
        let to_recipient = summary["To"]
            .as_array()
            .is_some_and(|to| to.iter().any(|address| address["Address"] == recipient));
        if !to_recipient {
            continue;
        }
        let id = summary["ID"].as_str().unwrap();
        let full: Value = reqwest::get(format!("{MAILPIT}/api/v1/message/{id}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        messages.push((
            full["Subject"].as_str().unwrap_or_default().to_string(),
            full["Text"].as_str().unwrap_or_default().to_string(),
        ));
    }
    messages
}

/// Waits (up to 5 s) until `recipient` has at least `count` messages, then
/// waits 500 ms more and returns everything — so a duplicate that arrives
/// late still makes an exact-count assertion fail.
pub async fn settled_mail(recipient: &str, count: usize) -> Vec<(String, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while mail_for(recipient).await.len() < count && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    mail_for(recipient).await
}

/// Makes the monitor due on the next `run_due_checks`.
pub async fn make_due(pool: &sqlx::PgPool, monitor_id: Uuid) {
    sqlx::query("update monitors set next_check_at = now() - interval '1 second' where id = $1")
        .bind(monitor_id)
        .execute(pool)
        .await
        .unwrap();
}
```

- [ ] **Step 2: Write the failing test `tests/uptime_worker.rs`**

```rust
// Requires `docker compose up -d postgres mailpit clickhouse`.
mod common;

use chrono::Utc;
use common::{
    add_member, make_due, seed_http_monitor, seed_project, seed_user, settled_mail, state_with_clickhouse, unique_email,
};
use nami_engine::{
    email::Mailer,
    state::AppState,
    uptime::{
        checker::{CheckOutcome, CheckStatus},
        worker::{apply_outcome, claim_due_monitors, run_due_checks},
    },
};
use serde::Deserialize;
use uuid::Uuid;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

async fn server_with_status(status: u16) -> MockServer {
    let server = MockServer::start().await;
    respond_with_status(&server, status).await;
    server
}

async fn respond_with_status(server: &MockServer, status: u16) {
    server.reset().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(status)).mount(server).await;
}

/// A project with one member whose address is unique to this test.
async fn project_with_recipient(pool: &sqlx::PgPool) -> (Uuid, String) {
    let project = seed_project(pool).await;
    let email = unique_email("oncall");
    let user = seed_user(pool, &email, "member").await;
    add_member(pool, project, user, "viewer").await;
    (project, email)
}

async fn monitor_state(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, bool) {
    sqlx::query_as("select current_status, consecutive_failures, last_checked_at is not null from monitors where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn incidents_of(pool: &sqlx::PgPool, monitor: Uuid) -> Vec<(String, String, Option<String>, bool)> {
    sqlx::query_as(
        "select status, title, cause, resolved_at is not null from incidents where monitor_id = $1 order by started_at",
    )
    .bind(monitor)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn claim_returns_only_due_unpaused_monitors_and_reschedules_them(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let due = seed_http_monitor(&pool, project, "due", "http://127.0.0.1:9/", 2).await;
    let paused = seed_http_monitor(&pool, project, "paused", "http://127.0.0.1:9/", 2).await;
    let later = seed_http_monitor(&pool, project, "later", "http://127.0.0.1:9/", 2).await;
    sqlx::query("update monitors set paused = true where id = $1").bind(paused).execute(&pool).await.unwrap();
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(later)
        .execute(&pool)
        .await
        .unwrap();

    let claimed = claim_due_monitors(&pool, 50).await.unwrap();
    assert_eq!(claimed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![due]);
    let seconds_ahead: f64 = sqlx::query_scalar(
        "select extract(epoch from next_check_at - now())::float8 from monitors where id = $1",
    )
    .bind(due)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((55.0..=60.0).contains(&seconds_ahead), "rescheduled {seconds_ahead}s ahead, expected ~60");

    assert!(claim_due_monitors(&pool, 50).await.unwrap().is_empty(), "a claimed monitor is not handed out twice");
}

#[sqlx::test]
async fn claim_respects_the_limit_and_concurrent_claims_never_overlap(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    for i in 0..10 {
        seed_http_monitor(&pool, project, &format!("m{i}"), "http://127.0.0.1:9/", 2).await;
    }
    let (a, b) = tokio::join!(claim_due_monitors(&pool, 4), claim_due_monitors(&pool, 4));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!((a.len(), b.len()), (4, 4));
    assert!(a.iter().all(|m| b.iter().all(|n| n.id != m.id)), "the same monitor was claimed twice");
    assert_eq!(claim_due_monitors(&pool, 50).await.unwrap().len(), 2, "exactly the remaining two are left");
}

#[sqlx::test]
async fn a_pending_monitor_that_succeeds_goes_up_silently(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(200).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 2).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(monitor_state(&pool, monitor).await, ("up".to_string(), 0, true));
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty(), "pending -> up sends no email");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn failures_below_the_threshold_open_no_incident(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 3).await;

    run_due_checks(&state).await.unwrap();
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await, ("pending".to_string(), 2, true));
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// Review Focus: a monitor that stays down keeps exactly one open incident
/// and sends no further DOWN emails until it recovers.
#[sqlx::test]
async fn reaching_the_threshold_opens_one_incident_and_alerts_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 2).await;

    run_due_checks(&state).await.unwrap();
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "Checkout API is down".to_string(),
            Some("expected status 200-399, got 503".to_string()),
            false
        )]
    );
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, "[Nami] Checkout API is DOWN");
    assert!(mail[0].1.contains("expected status 200-399, got 503"), "{}", mail[0].1);
    let link = format!("{}/projects/{project}/monitors/{monitor}", state.config.ui_base_url);
    assert!(mail[0].1.contains(&link), "body should link to {link}: {}", mail[0].1);

    // Still failing: no second incident, no second email.
    for _ in 0..2 {
        make_due(&pool, monitor).await;
        run_due_checks(&state).await.unwrap();
    }
    assert_eq!(monitor_state(&pool, monitor).await.1, 4, "failures keep counting while down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_pending_monitor_down_at_threshold_one_alerts_immediately(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(500).await;
    let monitor = seed_http_monitor(&pool, project, "Brand new", &server.uri(), 1).await;

    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn recovery_resolves_the_incident_and_sends_one_email(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    run_due_checks(&state).await.unwrap();
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");

    respond_with_status(&server, 200).await;
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await, ("up".to_string(), 0, true));
    let incidents = incidents_of(&pool, monitor).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!((incidents[0].0.as_str(), incidents[0].3), ("resolved", true));
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(
        mail.iter().map(|(subject, _)| subject.as_str()).collect::<Vec<_>>(),
        vec!["[Nami] Checkout API is DOWN", "[Nami] Checkout API is back UP"]
    );
    ch.drop_database().await.unwrap();
}

#[derive(Debug, Deserialize)]
struct StoredCheck {
    monitor_id: Uuid,
    status: String,
    status_code: Option<u16>,
    error: Option<String>,
}

#[sqlx::test]
async fn run_due_checks_writes_one_clickhouse_row_per_check(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let healthy = server_with_status(200).await;
    let broken = server_with_status(502).await;
    let up = seed_http_monitor(&pool, project, "up", &healthy.uri(), 2).await;
    let down = seed_http_monitor(&pool, project, "down", &broken.uri(), 2).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 2);

    let mut rows: Vec<StoredCheck> = ch
        .query_rows(
            "SELECT monitor_id, status, status_code, error FROM uptime_checks
             WHERE project_id = {project_id:UUID}",
            &[("project_id", project.to_string())],
        )
        .await
        .unwrap();
    rows.sort_by_key(|r| r.status.clone());
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].monitor_id, rows[0].status.as_str(), rows[0].status_code), (down, "down", Some(502)));
    assert_eq!(rows[0].error.as_deref(), Some("expected status 200-399, got 502"));
    assert_eq!((rows[1].monitor_id, rows[1].status.as_str(), rows[1].status_code), (up, "up", Some(200)));
    ch.drop_database().await.unwrap();
}

/// Review Focus: an SMTP outage during a transition does not abort the
/// pass — the incident is still recorded and the worker returns normally.
#[sqlx::test]
async fn an_smtp_outage_does_not_abort_the_pass(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    // Port 1 is never listening: every send fails fast.
    let state = AppState { mailer: Mailer::new("localhost", 1), ..state };
    let (project, _) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_clickhouse_outage_does_not_block_state_changes_or_alerts(pool: sqlx::PgPool) {
    // AppState::for_test's ClickHouse database does not exist: every insert fails.
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
}

fn failed_check() -> CheckOutcome {
    CheckOutcome {
        status: CheckStatus::Down,
        status_code: None,
        response_time_ms: None,
        error: Some("connection refused".to_string()),
        checked_at: Utc::now(),
    }
}

#[sqlx::test]
async fn results_for_monitors_paused_or_deleted_mid_check_are_discarded(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let paused = seed_http_monitor(&pool, project, "paused", "http://127.0.0.1:9/", 1).await;
    let deleted = seed_http_monitor(&pool, project, "deleted", "http://127.0.0.1:9/", 1).await;
    let claimed = claim_due_monitors(&pool, 50).await.unwrap();
    sqlx::query("update monitors set paused = true where id = $1").bind(paused).execute(&pool).await.unwrap();
    sqlx::query("delete from monitors where id = $1").bind(deleted).execute(&pool).await.unwrap();

    for monitor in &claimed {
        assert_eq!(apply_outcome(&pool, monitor, &failed_check()).await.unwrap(), None);
    }
    assert_eq!(monitor_state(&pool, paused).await, ("pending".to_string(), 0, false));
    let incidents: i64 = sqlx::query_scalar("select count(*) from incidents").fetch_one(&pool).await.unwrap();
    assert_eq!(incidents, 0);
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test uptime_worker`
Expected: compile error ``unresolved import `nami_engine::uptime::worker` ``.

- [ ] **Step 4: Write the incidents migration and extend the migrations test**

`nami-engine/migrations/0010_create_incidents.sql`:

```sql
-- The spec's incidents table minus heartbeat_id, which
-- 0011_create_heartbeats.sql adds once the heartbeats table exists.
create table incidents (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    monitor_id uuid references monitors(id) on delete cascade,
    title text not null,
    cause text,
    status text not null default 'open' check (status in ('open', 'resolved')),
    started_at timestamptz not null default now(),
    resolved_at timestamptz
);

-- Incident lists filter by project and status, newest first.
create index incidents_project_status_idx on incidents (project_id, status, started_at desc);

-- At most one open incident per monitor. The worker relies on this to stay
-- idempotent even if two engine instances race on the same monitor.
create unique index incidents_one_open_per_monitor on incidents (monitor_id) where status = 'open';
```

In `nami-engine/tests/migrations.rs`, add `"incidents",` after `"monitors",`.

- [ ] **Step 5: Write the incident helpers**

`nami-engine/src/models/incident.rs`:

```rust
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

/// Opens an incident for the monitor unless one is already open (the
/// `incidents_one_open_per_monitor` index turns a duplicate into a no-op).
/// Returns the new incident's id, or `None` if one was already open.
pub async fn open_for_monitor(
    conn: &mut PgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (monitor_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(monitor_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Resolves the monitor's open incident, if there is one. Returns its id.
pub async fn resolve_for_monitor(
    conn: &mut PgConnection,
    monitor_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where monitor_id = $1 and status = 'open'
         returning id",
    )
    .bind(monitor_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}
```

In `nami-engine/src/models/mod.rs`, add `pub mod incident;` after `pub mod monitor;`.

- [ ] **Step 6: Write the ClickHouse row store**

`nami-engine/src/uptime/store.rs`:

```rust
//! Check history in ClickHouse (`uptime_checks`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::checker::{CheckOutcome, CheckStatus};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    models::monitor::Monitor,
};

/// One row of `uptime_checks`; also the JSON shape of
/// `GET /projects/{id}/monitors/{monitor_id}/checks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UptimeCheckRow {
    pub monitor_id: Uuid,
    pub project_id: Uuid,
    pub checked_at: DateTime<Utc>,
    pub status: CheckStatus,
    pub status_code: Option<u16>,
    pub response_time_ms: Option<u32>,
    pub error: Option<String>,
}

impl UptimeCheckRow {
    pub fn from_outcome(monitor: &Monitor, outcome: &CheckOutcome) -> Self {
        UptimeCheckRow {
            monitor_id: monitor.id,
            project_id: monitor.project_id,
            checked_at: outcome.checked_at,
            status: outcome.status,
            status_code: outcome.status_code,
            response_time_ms: outcome.response_time_ms,
            error: outcome.error.clone(),
        }
    }
}

pub async fn insert_checks(ch: &ClickHouse, rows: &[UptimeCheckRow]) -> Result<(), ClickHouseError> {
    ch.insert_rows("uptime_checks", rows).await
}
```

- [ ] **Step 7: Write the alert entry point (with its unit tests)**

`nami-engine/src/uptime/alerts.rs`:

```rust
//! Incident notifications. `notify_transition` is the single entry point:
//! the Incident Management sub-project replaces its body (escalation
//! policies, webhook/Slack/Telegram/Discord channels) without touching the
//! worker.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    Down,
    Recovered,
}

/// What changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Monitor(Uuid),
}

/// A state change worth telling people about: something went down (an
/// incident opened) or came back up (its incident resolved).
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub subject: Subject,
    pub project_id: Uuid,
    pub name: String,
    pub cause: Option<String>,
    pub at: DateTime<Utc>,
}

/// Emails every member of the transition's project. Failures are logged and
/// swallowed: alert delivery must never abort the worker or undo the state
/// change that triggered it.
pub async fn notify_transition(state: &AppState, transition: &Transition) {
    let recipients: Vec<String> = match sqlx::query_scalar(
        "select u.email from project_members pm
         join users u on u.id = pm.user_id
         where pm.project_id = $1
         order by u.email",
    )
    .bind(transition.project_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(recipients) => recipients,
        Err(e) => {
            tracing::error!(error = %e, project_id = %transition.project_id, "failed to load alert recipients");
            return;
        }
    };
    let (subject, body) = render(&state.config.ui_base_url, transition);
    for recipient in recipients {
        if let Err(e) = state.mailer.send(&recipient, &subject, body.clone()).await {
            tracing::error!(error = ?e, %recipient, "failed to send alert email");
        }
    }
}

/// Subject and plain-text body for a transition's email.
pub fn render(ui_base_url: &str, transition: &Transition) -> (String, String) {
    // Names are validated to contain no control characters; stripping them
    // again here keeps a header-injection attempt out of the subject even
    // for rows written some other way.
    let name: String = transition.name.chars().filter(|c| !c.is_control()).collect();
    let base = ui_base_url.trim_end_matches('/');
    let link = match transition.subject {
        Subject::Monitor(id) => format!("{base}/projects/{}/monitors/{id}", transition.project_id),
    };
    let at = transition.at.format("%Y-%m-%d %H:%M:%S UTC");
    match transition.kind {
        TransitionKind::Down => (
            format!("[Nami] {name} is DOWN"),
            format!(
                "{name} is DOWN.\n\nCause: {}\nSince: {at}\n\nDetails: {link}\n",
                transition.cause.as_deref().unwrap_or("unknown")
            ),
        ),
        TransitionKind::Recovered => (
            format!("[Nami] {name} is back UP"),
            format!("{name} is back UP.\n\nRecovered at: {at}\n\nDetails: {link}\n"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transition(kind: TransitionKind, name: &str) -> Transition {
        Transition {
            kind,
            subject: Subject::Monitor(Uuid::nil()),
            project_id: Uuid::nil(),
            name: name.to_string(),
            cause: Some("expected status 200-399, got 503".to_string()),
            at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn down_email_names_the_cause_time_and_link() {
        let (subject, body) = render("http://ui.test/", &transition(TransitionKind::Down, "Checkout API"));
        assert_eq!(subject, "[Nami] Checkout API is DOWN");
        assert!(body.contains("Cause: expected status 200-399, got 503"), "{body}");
        assert!(body.contains("Since: 2026-10-14 17:46:40 UTC"), "{body}");
        let nil = Uuid::nil();
        assert!(body.contains(&format!("http://ui.test/projects/{nil}/monitors/{nil}")), "{body}");
    }

    #[test]
    fn recovery_email_has_its_own_subject() {
        let (subject, body) = render("http://ui.test", &transition(TransitionKind::Recovered, "Checkout API"));
        assert_eq!(subject, "[Nami] Checkout API is back UP");
        assert!(body.contains("Recovered at: 2026-10-14 17:46:40 UTC"), "{body}");
    }

    #[test]
    fn control_characters_never_reach_the_subject() {
        let (subject, _) = render("http://ui.test", &transition(TransitionKind::Down, "API\r\nBcc: x@evil.test"));
        assert_eq!(subject, "[Nami] APIBcc: x@evil.test is DOWN");
    }
}
```

- [ ] **Step 8: Write the worker and register the modules**

`nami-engine/src/uptime/worker.rs`:

```rust
//! The background uptime worker: every tick it claims due monitors, checks
//! them, stores the results, applies the state machine, and sends alerts.

use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use super::{
    alerts::{self, Subject, Transition, TransitionKind},
    checker::{self, CheckOutcome, CheckStatus},
    store::{self, UptimeCheckRow},
};
use crate::{
    error::AppError,
    models::{incident, monitor::Monitor},
    state::AppState,
};

pub const TICK: Duration = Duration::from_secs(5);
/// Monitors claimed per tick.
pub const CLAIM_BATCH: i64 = 50;
/// Checks in flight at once.
pub const MAX_IN_FLIGHT: usize = 20;

/// Atomically claims up to `limit` due, unpaused monitors and reschedules
/// them `interval_seconds` ahead, in ONE statement: `for update skip locked`
/// means two overlapping ticks (or two engine instances) can never claim
/// the same monitor, and nothing can change between choosing and
/// rescheduling.
pub async fn claim_due_monitors(pool: &PgPool, limit: i64) -> Result<Vec<Monitor>, sqlx::Error> {
    sqlx::query_as(
        "update monitors m
         set next_check_at = now() + make_interval(secs => m.interval_seconds)
         where m.id in (
             select id from monitors
             where not paused and next_check_at <= now()
             order by next_check_at
             limit $1
             for update skip locked
         )
         returning m.*",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Applies one check result to its monitor's state in a single transaction.
/// The row is locked (`for update`) and the state read from that locked row
/// rather than from `monitor` (a snapshot taken at claim time), so
/// concurrent results for one monitor are applied one after the other.
///
/// - success: failures reset; `down` -> `up` resolves the open incident and
///   returns a `Recovered` transition; `pending` -> `up` is silent.
/// - failure: failures + 1; reaching the confirmation threshold while not
///   already `down` (including from `pending`) sets `down`, opens an
///   incident and returns a `Down` transition.
///
/// A monitor deleted or paused while its check was in flight is left alone.
pub async fn apply_outcome(
    pool: &PgPool,
    monitor: &Monitor,
    outcome: &CheckOutcome,
) -> Result<Option<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, String, String, i32, i32, bool)> = sqlx::query_as(
        "select project_id, name, current_status, consecutive_failures, confirmation_threshold, paused
         from monitors where id = $1 for update",
    )
    .bind(monitor.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((project_id, name, status, failures, threshold, paused)) = row else {
        return Ok(None);
    };
    if paused {
        return Ok(None);
    }

    let transition = match outcome.status {
        CheckStatus::Up => {
            sqlx::query(
                "update monitors set current_status = 'up', consecutive_failures = 0, last_checked_at = $2
                 where id = $1",
            )
            .bind(monitor.id)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
            if status == "down" {
                incident::resolve_for_monitor(&mut tx, monitor.id, outcome.checked_at).await?;
                Some(Transition {
                    kind: TransitionKind::Recovered,
                    subject: Subject::Monitor(monitor.id),
                    project_id,
                    name,
                    cause: None,
                    at: outcome.checked_at,
                })
            } else {
                None
            }
        }
        CheckStatus::Down => {
            let failures = failures.saturating_add(1);
            let goes_down = failures >= threshold && status != "down";
            sqlx::query(
                "update monitors
                 set consecutive_failures = $2,
                     current_status = case when $3 then 'down' else current_status end,
                     last_checked_at = $4
                 where id = $1",
            )
            .bind(monitor.id)
            .bind(failures)
            .bind(goes_down)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
            if goes_down {
                let opened = incident::open_for_monitor(
                    &mut tx,
                    project_id,
                    monitor.id,
                    &format!("{name} is down"),
                    outcome.error.as_deref(),
                    outcome.checked_at,
                )
                .await?;
                // `None`: an incident was somehow already open — it already
                // alerted, so this must not alert again.
                opened.map(|_| Transition {
                    kind: TransitionKind::Down,
                    subject: Subject::Monitor(monitor.id),
                    project_id,
                    name,
                    cause: outcome.error.clone(),
                    at: outcome.checked_at,
                })
            } else {
                None
            }
        }
    };
    tx.commit().await?;
    Ok(transition)
}

/// One worker pass over due monitors. Returns how many checks ran.
pub async fn run_due_checks(state: &AppState) -> Result<usize, AppError> {
    let monitors = claim_due_monitors(&state.db, CLAIM_BATCH).await?;
    if monitors.is_empty() {
        return Ok(0);
    }

    // Built per pass: cheap next to the checks themselves, and it never
    // outlives the runtime that drives its connections.
    let http = checker::build_http_client();
    let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut checks = JoinSet::new();
    for monitor in monitors {
        let http = http.clone();
        let semaphore = Arc::clone(&semaphore);
        checks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("the check semaphore is never closed");
            let outcome = checker::perform_check(&http, &monitor).await;
            (monitor, outcome)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = checks.join_next().await {
        match joined {
            Ok(result) => results.push(result),
            Err(e) => tracing::error!(error = %e, "uptime check task failed"),
        }
    }

    // ClickHouse holds history only: if it is unavailable, still apply state
    // changes and send alerts.
    let rows: Vec<UptimeCheckRow> = results.iter().map(|(m, o)| UptimeCheckRow::from_outcome(m, o)).collect();
    if let Err(e) = store::insert_checks(&state.clickhouse, &rows).await {
        tracing::error!(error = %e, count = rows.len(), "failed to store uptime checks");
    }

    let mut transitions = Vec::new();
    for (monitor, outcome) in &results {
        match apply_outcome(&state.db, monitor, outcome).await {
            Ok(Some(transition)) => transitions.push(transition),
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, monitor_id = %monitor.id, "failed to apply check outcome"),
        }
    }
    for transition in &transitions {
        alerts::notify_transition(state, transition).await;
    }
    Ok(results.len())
}

/// Ticks every [`TICK`] forever. Each tick runs in its own task, so even a
/// panic inside it is logged here instead of stopping the worker.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let tick_state = state.clone();
        if let Err(e) = tokio::spawn(async move { tick(&tick_state).await }).await {
            tracing::error!(error = %e, "uptime worker tick panicked");
        }
    }
}

async fn tick(state: &AppState) {
    if let Err(e) = run_due_checks(state).await {
        tracing::error!(error = ?e, "uptime check pass failed");
    }
}
```

Replace `nami-engine/src/uptime/mod.rs` with:

```rust
//! Uptime monitoring: performing checks (`checker`), storing results in
//! ClickHouse (`store`), the background worker and its state machine
//! (`worker`), and alert delivery (`alerts`).
pub mod alerts;
pub mod checker;
pub mod store;
pub mod worker;
```

- [ ] **Step 9: Spawn the worker from `main.rs`**

In `nami-engine/src/main.rs`, replace `let app = router::build_router(state).layer(session_layer);` with:

```rust
    if state.config.uptime_worker_enabled {
        tokio::spawn(nami_engine::uptime::worker::run_forever(state.clone()));
        tracing::info!("uptime worker started");
    } else {
        tracing::info!("uptime worker disabled (UPTIME_WORKER_ENABLED=false)");
    }

    let app = router::build_router(state).layer(session_layer);
```

- [ ] **Step 10: Run the tests to verify they pass**

Run: `cd nami-engine && docker compose up -d postgres mailpit clickhouse && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test uptime_worker --test migrations --lib && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: `uptime_worker` 11 passed, lib unit tests include the 3 `uptime::alerts::tests`, everything else unchanged.

- [ ] **Step 11: Smoke-check the worker against a real target**

With at least one project in the dev database (create one in the UI if needed):

```bash
cd nami-engine
DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo run
# in another shell, from nami-engine/:
docker compose exec -T postgres psql -U nami -d nami -c "insert into monitors (project_id, name, check_type, url, interval_seconds) select id, 'example', 'http', 'https://example.com', 30 from projects limit 1"
sleep 10
docker compose exec -T postgres psql -U nami -d nami -c "select name, current_status, last_checked_at from monitors where name = 'example'"
docker compose exec -T postgres psql -U nami -d nami -c "delete from monitors where name = 'example'"
```

Expected: the engine logs `uptime worker started`; within one tick the row shows `up` with a `last_checked_at`. (If the row never changes, check the Global Constraints note: the engine and `docker compose exec` may be talking to different Postgres servers.)

- [ ] **Step 12: Commit**

```bash
git add nami-engine/migrations/0010_create_incidents.sql nami-engine/src nami-engine/tests
git commit -F - <<'EOF'
feat(engine): add uptime worker, incidents and email alerts (NAMI-UP-4)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 5 (NAMI-UP-5): Read APIs

Branch `feat/NAMI-UP-5-read-apis`, worktree `.worktrees/NAMI-UP-5/`.

**Files:**
- Modify: `nami-engine/src/uptime/store.rs` (append read queries), `nami-engine/src/routes/monitors.rs` (list with uptime, `/checks`, `/stats`), `nami-engine/src/models/incident.rs` (list query)
- Create: `nami-engine/src/routes/incidents.rs`
- Modify: `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`
- Test: `nami-engine/tests/uptime_read_api.rs`

**Interfaces:**
- Consumes: `ClickHouse::query_rows` (Task 1); `monitor::{find_in_project, list_for_project}` (Task 2); `UptimeCheckRow`, `insert_checks`, `CheckStatus` (Tasks 3-4); test helpers from `tests/common/mod.rs`.
- Produces:
  - `store::uptime_24h_by_monitor(&ClickHouse, project_id: Uuid, now: DateTime<Utc>) -> Result<HashMap<Uuid, f64>, ClickHouseError>` (monitors without checks are absent).
  - `store::recent_checks(&ClickHouse, project_id, monitor_id, limit: u32) -> Result<Vec<UptimeCheckRow>, ClickHouseError>` (newest first).
  - `store::monitor_stats(&ClickHouse, project_id, monitor_id, now) -> Result<MonitorStats, ClickHouseError>`; `MonitorStats { uptime_24h, uptime_7d, uptime_30d, avg_response_ms_24h: Option<f64>, series: Vec<SeriesBucket> }`; `SeriesBucket { bucket_start: DateTime<Utc>, avg_response_ms: Option<f64>, up_ratio: Option<f64> }`; `SERIES_BUCKETS = 48`, `BUCKET_SECONDS = 1800`.
  - `models::incident::IncidentListItem { id, project_id, monitor_id: Option<Uuid>, heartbeat_id: Option<Uuid>, source_name: Option<String>, title, cause: Option<String>, status, started_at, resolved_at: Option<DateTime<Utc>> }` and `list_for_project(&PgPool, project_id, status: Option<&str>, monitor_id: Option<Uuid>) -> Result<Vec<IncidentListItem>, sqlx::Error>`.
  - HTTP: `GET /projects/{id}/monitors` now returns each monitor's fields flattened plus `uptime_24h: number | null`; `GET /projects/{id}/monitors/{monitor_id}/checks?limit=` (default 50, clamped to 1..=500); `GET /projects/{id}/monitors/{monitor_id}/stats`; `GET /projects/{id}/incidents?status=open|resolved&monitor_id=` (newest first, max 200, `400` for any other status).

Every window is computed from the engine's clock and passed to ClickHouse as epoch milliseconds (`fromUnixTimestamp64Milli({x:Int64}, 'UTC')`), with `checked_at <= now` as the upper bound, so tests can pin `now` and bucket boundaries are deterministic. Ratios are computed in Rust from `countIf`/`count()` (UInt64) — no float division in SQL, no NaN.

- [ ] **Step 1: Write the failing test `tests/uptime_read_api.rs`**

`checked_at` is `DateTime64(3)`: any timestamp compared after a round trip must be whole milliseconds (see the `start` in `checks_are_newest_first_with_a_default_and_clamped_limit`).

```rust
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::{app, call, seed_http_monitor, seed_project, state_with_clickhouse, test_app, user_with_role};
use nami_engine::{
    clickhouse::ClickHouse,
    uptime::{
        checker::CheckStatus,
        store::{self, SeriesBucket, UptimeCheckRow},
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

fn check(project: Uuid, monitor: Uuid, at: DateTime<Utc>, up: bool, response_ms: Option<u32>) -> UptimeCheckRow {
    UptimeCheckRow {
        monitor_id: monitor,
        project_id: project,
        checked_at: at,
        status: if up { CheckStatus::Up } else { CheckStatus::Down },
        status_code: up.then_some(200),
        response_time_ms: response_ms,
        error: (!up).then(|| "connection refused".to_string()),
    }
}

/// Start of the half-hour bucket containing the current time.
fn current_bucket_start() -> DateTime<Utc> {
    let secs = Utc::now().timestamp();
    DateTime::from_timestamp(secs - secs.rem_euclid(1800), 0).unwrap()
}

/// History for `monitor`, relative to `now` = bucket start + 10 minutes:
/// - current bucket: up 100 ms, up 200 ms, down
/// - 10 buckets back (5 h ago): up 300 ms
/// - 2 days ago: down; 20 days ago: up 50 ms; 40 days ago: down
/// - 5 minutes after `now`: down (must be ignored)
/// plus one down check for `other` (another monitor, same project).
async fn seed_history(ch: &ClickHouse, project: Uuid, monitor: Uuid, other: Uuid, now: DateTime<Utc>) {
    let minutes = |m: i64| now - Duration::minutes(m);
    let rows = vec![
        check(project, monitor, minutes(1), true, Some(100)),
        check(project, monitor, minutes(2), true, Some(200)),
        check(project, monitor, minutes(3), false, None),
        check(project, monitor, minutes(300), true, Some(300)),
        check(project, monitor, now - Duration::days(2), false, None),
        check(project, monitor, now - Duration::days(20), true, Some(50)),
        check(project, monitor, now - Duration::days(40), false, None),
        check(project, monitor, now + Duration::minutes(5), false, None),
        check(project, other, minutes(1), false, None),
        check(Uuid::new_v4(), monitor, minutes(1), false, None), // another project's row
    ];
    store::insert_checks(ch, &rows).await.unwrap();
}

#[tokio::test]
async fn stats_compute_each_window_and_the_24h_average() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start() + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let stats = store::monitor_stats(&ch, project, monitor, now).await.unwrap();

    assert_eq!(stats.uptime_24h, Some(0.75), "3 up + 1 down");
    assert_eq!(stats.uptime_7d, Some(0.6), "+ the down check 2 days ago");
    assert!((stats.uptime_30d.unwrap() - 4.0 / 6.0).abs() < 1e-9, "+ the up check 20 days ago");
    assert_eq!(stats.avg_response_ms_24h, Some(200.0), "mean of 100, 200, 300; nulls ignored");
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn the_series_has_48_half_hour_buckets_with_nulls_for_gaps() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let bucket = current_bucket_start();
    let now = bucket + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let series = store::monitor_stats(&ch, project, monitor, now).await.unwrap().series;

    assert_eq!(series.len(), 48);
    assert_eq!(series[0].bucket_start, bucket - Duration::minutes(30 * 47));
    assert_eq!(series[47].bucket_start, bucket);
    assert!(series.windows(2).all(|w| w[1].bucket_start - w[0].bucket_start == Duration::minutes(30)));
    assert_eq!(series[47].avg_response_ms, Some(150.0));
    assert!((series[47].up_ratio.unwrap() - 2.0 / 3.0).abs() < 1e-9, "the row after `now` is excluded");
    assert_eq!(
        series[37],
        SeriesBucket { bucket_start: bucket - Duration::minutes(300), avg_response_ms: Some(300.0), up_ratio: Some(1.0) }
    );
    assert_eq!(series.iter().filter(|b| b.up_ratio.is_some()).count(), 2, "every other bucket is a null gap");
    assert!(series.iter().filter(|b| b.up_ratio.is_none()).all(|b| b.avg_response_ms.is_none()));
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn a_monitor_without_checks_has_null_stats_and_an_empty_series() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let stats = store::monitor_stats(&ch, Uuid::new_v4(), Uuid::new_v4(), Utc::now()).await.unwrap();
    assert_eq!(
        (stats.uptime_24h, stats.uptime_7d, stats.uptime_30d, stats.avg_response_ms_24h),
        (None, None, None, None)
    );
    assert_eq!(stats.series.len(), 48);
    assert!(stats.series.iter().all(|b| b.up_ratio.is_none() && b.avg_response_ms.is_none()));
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn uptime_by_monitor_is_grouped_per_monitor_of_the_project() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start() + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let uptime = store::uptime_24h_by_monitor(&ch, project, now).await.unwrap();

    assert_eq!(uptime.len(), 2, "{uptime:?}");
    assert_eq!(uptime[&monitor], 0.75);
    assert_eq!(uptime[&other], 0.0);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_monitor_list_includes_24h_uptime_or_null(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let checked = seed_http_monitor(&pool, project, "a-checked", "https://example.com", 2).await;
    let fresh = seed_http_monitor(&pool, project, "b-fresh", "https://example.com", 2).await;
    let hour_ago = Utc::now() - Duration::hours(1);
    let rows: Vec<UptimeCheckRow> = [true, true, true, false]
        .into_iter()
        .map(|up| check(project, checked, hour_ago, up, Some(120)))
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(&app, "GET", &format!("/projects/{project}/monitors"), Some(&viewer), None).await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["id"], checked.to_string());
    assert_eq!(list[0]["uptime_24h"], json!(0.75));
    assert_eq!(list[0]["name"], "a-checked", "monitor fields are flattened alongside uptime_24h");
    assert_eq!(list[1]["id"], fresh.to_string());
    assert_eq!(list[1]["uptime_24h"], Value::Null);
    ch.drop_database().await.unwrap();
}

/// Review Focus: with ClickHouse unavailable the list still loads, with
/// Postgres status and `uptime_24h: null`, instead of a 500.
#[sqlx::test]
async fn the_monitor_list_still_loads_when_clickhouse_is_unavailable(pool: sqlx::PgPool) {
    // AppState::for_test points ClickHouse at a database that does not exist.
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    sqlx::query("update monitors set current_status = 'down' where id = $1").bind(monitor).execute(&pool).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(&app, "GET", &format!("/projects/{project}/monitors"), Some(&viewer), None).await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["current_status"], "down");
    assert_eq!(list[0]["uptime_24h"], Value::Null);
}

#[sqlx::test]
async fn checks_are_newest_first_with_a_default_and_clamped_limit(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    // Whole milliseconds: checked_at is DateTime64(3), so finer precision
    // would not round-trip.
    let start = DateTime::from_timestamp_millis((Utc::now() - Duration::hours(2)).timestamp_millis()).unwrap();
    let rows: Vec<UptimeCheckRow> = (0..501)
        .map(|i| check(project, monitor, start + Duration::seconds(i), i % 2 == 0, Some(10)))
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}/checks");
    let get = |query: &str| {
        let (app, viewer, uri) = (app.clone(), viewer.clone(), format!("{uri}{query}"));
        async move { call(&app, "GET", &uri, Some(&viewer), None).await }
    };

    let (status, newest_two) = get("?limit=2").await;
    assert_eq!(status, StatusCode::OK, "{newest_two}");
    let newest: Vec<DateTime<Utc>> = newest_two
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["checked_at"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(newest, vec![start + Duration::seconds(500), start + Duration::seconds(499)]);
    assert_eq!(newest_two[0]["status"], "up");
    assert_eq!(get("").await.1.as_array().unwrap().len(), 50, "default limit");
    assert_eq!(get("?limit=1000").await.1.as_array().unwrap().len(), 500, "limit is capped at 500");
    assert_eq!(get("?limit=0").await.1.as_array().unwrap().len(), 1, "limit is at least 1");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_stats_endpoint_returns_uptime_and_48_buckets(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let hour_ago = Utc::now() - Duration::hours(1);
    let rows: Vec<UptimeCheckRow> = [true, true, true, false]
        .into_iter()
        .map(|up| check(project, monitor, hour_ago, up, Some(100)))
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, stats) =
        call(&app, "GET", &format!("/projects/{project}/monitors/{monitor}/stats"), Some(&viewer), None).await;

    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["uptime_24h"], json!(0.75));
    assert_eq!(stats["avg_response_ms_24h"], json!(100.0));
    assert_eq!(stats["series"].as_array().unwrap().len(), 48);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn read_endpoints_enforce_rbac_and_project_isolation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let own = seed_http_monitor(&pool, project, "own", "https://example.com", 2).await;
    let foreign = seed_http_monitor(&pool, other_project, "foreign", "https://example.com", 2).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    for uri in [
        format!("/projects/{project}/monitors"),
        format!("/projects/{project}/monitors/{own}/checks"),
        format!("/projects/{project}/monitors/{own}/stats"),
        format!("/projects/{project}/incidents"),
    ] {
        assert_eq!(call(&app, "GET", &uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN, "{uri}");
    }
    for uri in [
        format!("/projects/{project}/monitors/{foreign}/checks"),
        format!("/projects/{project}/monitors/{foreign}/stats"),
    ] {
        assert_eq!(call(&app, "GET", &uri, Some(&viewer), None).await.0, StatusCode::NOT_FOUND, "{uri}");
    }
}

async fn seed_incident(
    pool: &sqlx::PgPool,
    project: Uuid,
    monitor: Uuid,
    status: &str,
    started_minutes_ago: i32,
) -> Uuid {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, status, started_at, resolved_at)
         values ($1, $2, 'down', 'connection refused', $3,
                 now() - make_interval(mins => $4),
                 case when $3 = 'resolved' then now() else null end)
         returning id",
    )
    .bind(project)
    .bind(monitor)
    .bind(status)
    .bind(started_minutes_ago)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn incidents_are_newest_first_and_filterable(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let api = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let web = seed_http_monitor(&pool, project, "Web", "https://example.com", 2).await;
    let elsewhere = seed_http_monitor(&pool, other_project, "Elsewhere", "https://example.com", 2).await;
    let old_resolved = seed_incident(&pool, project, api, "resolved", 120).await;
    let api_open = seed_incident(&pool, project, api, "open", 60).await;
    let web_open = seed_incident(&pool, project, web, "open", 30).await;
    seed_incident(&pool, other_project, elsewhere, "open", 10).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let ids = |list: &Value| -> Vec<String> {
        list.as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect()
    };
    let base = format!("/projects/{project}/incidents");

    let (status, all) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(ids(&all), vec![web_open.to_string(), api_open.to_string(), old_resolved.to_string()]);
    assert_eq!(all[0]["source_name"], "Web");
    assert_eq!(all[0]["monitor_id"], web.to_string());

    let (_, open) = call(&app, "GET", &format!("{base}?status=open"), Some(&viewer), None).await;
    assert_eq!(ids(&open), vec![web_open.to_string(), api_open.to_string()]);
    let (_, resolved) = call(&app, "GET", &format!("{base}?status=resolved"), Some(&viewer), None).await;
    assert_eq!(ids(&resolved), vec![old_resolved.to_string()]);
    assert_ne!(resolved[0]["resolved_at"], Value::Null);
    let (_, for_api) = call(&app, "GET", &format!("{base}?monitor_id={api}"), Some(&viewer), None).await;
    assert_eq!(ids(&for_api), vec![api_open.to_string(), old_resolved.to_string()]);
    let (status, _) = call(&app, "GET", &format!("{base}?status=bogus"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test uptime_read_api`
Expected: compile error — `monitor_stats`, `SeriesBucket` and `uptime_24h_by_monitor` not found in `uptime::store`.

- [ ] **Step 3: Append the read queries to `src/uptime/store.rs`**

Append to `nami-engine/src/uptime/store.rs`:

```rust
/// Buckets in the stats series: 48 x 30 minutes = the last 24 hours.
pub const SERIES_BUCKETS: i64 = 48;
/// Must match `INTERVAL 30 MINUTE` in the series query.
pub const BUCKET_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorStats {
    pub uptime_24h: Option<f64>,
    pub uptime_7d: Option<f64>,
    pub uptime_30d: Option<f64>,
    pub avg_response_ms_24h: Option<f64>,
    pub series: Vec<SeriesBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesBucket {
    pub bucket_start: DateTime<Utc>,
    pub avg_response_ms: Option<f64>,
    pub up_ratio: Option<f64>,
}

fn ratio(up: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| up as f64 / total as f64)
}

/// 24-hour uptime per monitor of one project, from one grouped query.
/// Monitors with no checks in the window are absent from the map.
pub async fn uptime_24h_by_monitor(
    ch: &ClickHouse,
    project_id: Uuid,
    now: DateTime<Utc>,
) -> Result<std::collections::HashMap<Uuid, f64>, ClickHouseError> {
    #[derive(Deserialize)]
    struct Row {
        monitor_id: Uuid,
        up: u64,
        total: u64,
    }
    let rows: Vec<Row> = ch
        .query_rows(
            "SELECT monitor_id, countIf(status = 'up') AS up, count() AS total
             FROM uptime_checks
             WHERE project_id = {project_id:UUID}
               AND checked_at >= fromUnixTimestamp64Milli({since_ms:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')
             GROUP BY monitor_id",
            &[
                ("project_id", project_id.to_string()),
                ("since_ms", (now - chrono::Duration::hours(24)).timestamp_millis().to_string()),
                ("now_ms", now.timestamp_millis().to_string()),
            ],
        )
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| ratio(row.up, row.total).map(|uptime| (row.monitor_id, uptime)))
        .collect())
}

/// The monitor's most recent checks, newest first.
pub async fn recent_checks(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_id: Uuid,
    limit: u32,
) -> Result<Vec<UptimeCheckRow>, ClickHouseError> {
    ch.query_rows(
        "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms, error
         FROM uptime_checks
         WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
         ORDER BY checked_at DESC
         LIMIT {limit:UInt32}",
        &[
            ("project_id", project_id.to_string()),
            ("monitor_id", monitor_id.to_string()),
            ("limit", limit.to_string()),
        ],
    )
    .await
}

/// Uptime over 24 h / 7 d / 30 d, 24 h average response time, and a 24 h
/// series of [`SERIES_BUCKETS`] half-hour buckets ending with the bucket
/// that contains `now`. Buckets without checks are present with nulls, so a
/// chart always gets a stable x-axis. `now` is a parameter so tests can pin
/// the bucket boundaries.
pub async fn monitor_stats(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_id: Uuid,
    now: DateTime<Utc>,
) -> Result<MonitorStats, ClickHouseError> {
    #[derive(Deserialize, Default)]
    struct Totals {
        up_24h: u64,
        total_24h: u64,
        up_7d: u64,
        total_7d: u64,
        up_30d: u64,
        total_30d: u64,
        avg_response_ms_24h: Option<f64>,
    }
    #[derive(Deserialize)]
    struct BucketRow {
        bucket: i64,
        avg_response_ms: Option<f64>,
        up: u64,
        total: u64,
    }

    let millis = |t: DateTime<Utc>| t.timestamp_millis().to_string();
    let totals: Vec<Totals> = ch
        .query_rows(
            "SELECT
                 countIf(status = 'up' AND checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS up_24h,
                 countIf(checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS total_24h,
                 countIf(status = 'up' AND checked_at >= fromUnixTimestamp64Milli({since_7d:Int64}, 'UTC')) AS up_7d,
                 countIf(checked_at >= fromUnixTimestamp64Milli({since_7d:Int64}, 'UTC')) AS total_7d,
                 countIf(status = 'up') AS up_30d,
                 count() AS total_30d,
                 avgIf(response_time_ms, checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS avg_response_ms_24h
             FROM uptime_checks
             WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
               AND checked_at >= fromUnixTimestamp64Milli({since_30d:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')",
            &[
                ("project_id", project_id.to_string()),
                ("monitor_id", monitor_id.to_string()),
                ("since_24h", millis(now - chrono::Duration::hours(24))),
                ("since_7d", millis(now - chrono::Duration::days(7))),
                ("since_30d", millis(now - chrono::Duration::days(30))),
                ("now_ms", millis(now)),
            ],
        )
        .await?;
    let totals = totals.into_iter().next().unwrap_or_default();

    let current_bucket = now.timestamp() - now.timestamp().rem_euclid(BUCKET_SECONDS);
    let first_bucket = current_bucket - (SERIES_BUCKETS - 1) * BUCKET_SECONDS;
    let rows: Vec<BucketRow> = ch
        .query_rows(
            "SELECT toUnixTimestamp(toStartOfInterval(checked_at, INTERVAL 30 MINUTE)) AS bucket,
                    avg(response_time_ms) AS avg_response_ms,
                    countIf(status = 'up') AS up,
                    count() AS total
             FROM uptime_checks
             WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
               AND checked_at >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')
             GROUP BY bucket",
            &[
                ("project_id", project_id.to_string()),
                ("monitor_id", monitor_id.to_string()),
                ("from_ms", (first_bucket * 1000).to_string()),
                ("now_ms", millis(now)),
            ],
        )
        .await?;
    let by_bucket: std::collections::HashMap<i64, BucketRow> = rows.into_iter().map(|r| (r.bucket, r)).collect();
    let series = (0..SERIES_BUCKETS)
        .map(|i| {
            let start = first_bucket + i * BUCKET_SECONDS;
            let row = by_bucket.get(&start);
            SeriesBucket {
                bucket_start: DateTime::from_timestamp(start, 0).expect("bucket start is a valid timestamp"),
                avg_response_ms: row.and_then(|r| r.avg_response_ms),
                up_ratio: row.and_then(|r| ratio(r.up, r.total)),
            }
        })
        .collect();

    Ok(MonitorStats {
        uptime_24h: ratio(totals.up_24h, totals.total_24h),
        uptime_7d: ratio(totals.up_7d, totals.total_7d),
        uptime_30d: ratio(totals.up_30d, totals.total_30d),
        avg_response_ms_24h: totals.avg_response_ms_24h,
        series,
    })
}
```

- [ ] **Step 4: Serve uptime, checks and stats from the monitor routes**

Replace `nami-engine/src/routes/monitors.rs` with (the list handler degrades to `uptime_24h: null` when ClickHouse fails; `/checks` and `/stats` look the monitor up by both ids before touching ClickHouse, so foreign ids are 404):

```rust
use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::monitor::{self, CreateMonitorRequest, Monitor, PatchMonitorRequest},
    state::AppState,
    uptime::store::{self, MonitorStats, UptimeCheckRow},
};

// RBAC mirrors routes/projects.rs: viewer reads, member creates/edits,
// admin deletes. Authorization runs before any lookup, so a caller without
// access gets 403 whether or not the monitor exists.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/monitors", get(list_monitors).post(create_monitor))
        .route(
            "/projects/{id}/monitors/{monitor_id}",
            get(get_monitor).patch(patch_monitor).delete(delete_monitor),
        )
        .route("/projects/{id}/monitors/{monitor_id}/checks", get(list_checks))
        .route("/projects/{id}/monitors/{monitor_id}/stats", get(get_stats))
}

/// A monitor plus its 24-hour uptime (`null` when it has no checks in the
/// window, or when ClickHouse is unavailable).
#[derive(Serialize)]
struct MonitorSummary {
    #[serde(flatten)]
    monitor: Monitor,
    uptime_24h: Option<f64>,
}

async fn list_monitors(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<MonitorSummary>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let monitors = monitor::list_for_project(&state.db, project_id).await?;
    // Uptime decorates this list; current status comes from Postgres. If
    // ClickHouse is down, still list every monitor, with uptime unknown.
    let uptime = match store::uptime_24h_by_monitor(&state.clickhouse, project_id, Utc::now()).await {
        Ok(uptime) => uptime,
        Err(e) => {
            tracing::warn!(error = %e, %project_id, "24h uptime unavailable; listing monitors without it");
            HashMap::new()
        }
    };
    Ok(Json(
        monitors
            .into_iter()
            .map(|monitor| {
                let uptime_24h = uptime.get(&monitor.id).copied();
                MonitorSummary { monitor, uptime_24h }
            })
            .collect(),
    ))
}

async fn create_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateMonitorRequest>,
) -> Result<(StatusCode, Json<Monitor>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = monitor::insert_monitor(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // A global admin bypasses membership, so the project itself may
            // not exist: SQLSTATE 23503 (foreign_key_violation) -> 404.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchMonitorRequest>,
) -> Result<Json<Monitor>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = monitor::update_in_project(&state.db, project_id, monitor_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_monitor(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !monitor::delete_in_project(&state.db, project_id, monitor_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Deserialize)]
struct ChecksQuery {
    limit: Option<u32>,
}

async fn list_checks(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ChecksQuery>,
) -> Result<Json<Vec<UptimeCheckRow>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(store::recent_checks(&state.clickhouse, project_id, monitor_id, limit).await?))
}

async fn get_stats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorStats>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(store::monitor_stats(&state.clickhouse, project_id, monitor_id, Utc::now()).await?))
}
```

- [ ] **Step 5: Add the incident list query and route**

Replace `nami-engine/src/models/incident.rs` with (until Task 6 adds the column, `heartbeat_id` is selected as `null::uuid`):

```rust
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// One row of `GET /projects/{id}/incidents`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct IncidentListItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>,
    /// Name of the monitor (or heartbeat) the incident belongs to.
    pub source_name: Option<String>,
    pub title: String,
    pub cause: Option<String>,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Newest first, capped at 200. `status` and `monitor_id` are optional
/// filters (`None` = no filter).
pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<&str>,
    monitor_id: Option<Uuid>,
) -> Result<Vec<IncidentListItem>, sqlx::Error> {
    sqlx::query_as(
        "select i.id, i.project_id, i.monitor_id, null::uuid as heartbeat_id,
                m.name as source_name, i.title, i.cause, i.status, i.started_at, i.resolved_at
         from incidents i
         left join monitors m on m.id = i.monitor_id
         where i.project_id = $1
           and ($2::text is null or i.status = $2)
           and ($3::uuid is null or i.monitor_id = $3)
         order by i.started_at desc
         limit 200",
    )
    .bind(project_id)
    .bind(status)
    .bind(monitor_id)
    .fetch_all(pool)
    .await
}

/// Opens an incident for the monitor unless one is already open (the
/// `incidents_one_open_per_monitor` index turns a duplicate into a no-op).
/// Returns the new incident's id, or `None` if one was already open.
pub async fn open_for_monitor(
    conn: &mut PgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (monitor_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(monitor_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Resolves the monitor's open incident, if there is one. Returns its id.
pub async fn resolve_for_monitor(
    conn: &mut PgConnection,
    monitor_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where monitor_id = $1 and status = 'open'
         returning id",
    )
    .bind(monitor_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}
```

`nami-engine/src/routes/incidents.rs`:

```rust
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::incident::{self, IncidentListItem},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/incidents", get(list_incidents))
}

#[derive(Deserialize)]
struct IncidentsQuery {
    status: Option<String>,
    monitor_id: Option<Uuid>,
}

async fn list_incidents(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Query(query): Query<IncidentsQuery>,
) -> Result<Json<Vec<IncidentListItem>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    if let Some(status) = query.status.as_deref() {
        if !matches!(status, "open" | "resolved") {
            return Err(AppError::Validation("status must be open or resolved".into()));
        }
    }
    let incidents =
        incident::list_for_project(&state.db, project_id, query.status.as_deref(), query.monitor_id).await?;
    Ok(Json(incidents))
}
```

In `nami-engine/src/routes/mod.rs` add `pub mod incidents;` after `pub mod monitors;`. In `nami-engine/src/router.rs::build_router` add `.merge(routes::incidents::router())` after `.merge(routes::monitors::router())`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test uptime_read_api --test monitors_api && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: `uptime_read_api` 10 passed, `monitors_api` still 9 passed (its list assertions only read monitor fields), everything else unchanged.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/src nami-engine/tests/uptime_read_api.rs
git commit -F - <<'EOF'
feat(engine): add uptime stats, check history and incident list APIs (NAMI-UP-5)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 6 (NAMI-UP-6): Heartbeats

Branch `feat/NAMI-UP-6-heartbeats`, worktree `.worktrees/NAMI-UP-6/`.

**Files:**
- Create: `nami-engine/migrations/0011_create_heartbeats.sql`
- Create: `nami-engine/src/models/heartbeat.rs`, `nami-engine/src/uptime/heartbeat.rs`, `nami-engine/src/routes/heartbeats.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/models/incident.rs`, `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/alerts.rs`, `nami-engine/src/uptime/worker.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/heartbeats.rs` (plus one unit test in `alerts.rs`)

**Interfaces:**
- Consumes: `validate_name` (Task 2); `incident::*` (Tasks 4-5); `alerts::{Transition, TransitionKind, Subject, notify_transition}` and `worker::run_forever` (Task 4); `auth::token::new_opaque_token() -> (String, String)` (existing; the raw half is 32 random bytes hex-encoded).
- Produces:
  - `models::heartbeat::Heartbeat { id, project_id, name, token, period_seconds: i32, grace_seconds: i32, paused: bool, current_status: String, last_ping_at: Option<DateTime<Utc>>, created_at }` (`Serialize`, includes `token`), `HeartbeatConfig`, `CreateHeartbeatRequest` (grace defaults to 60), `PatchHeartbeatRequest`, `validate(HeartbeatConfig) -> Result<HeartbeatConfig, String>`, `insert_heartbeat`, `list_for_project`, `find_in_project`, `update_in_project`, `delete_in_project` (same shapes as the monitor queries).
  - `models::incident::open_for_heartbeat(...)`, `resolve_for_heartbeat(...)` (same signatures as the monitor variants, keyed by `heartbeat_id`); `list_for_project` now returns real `heartbeat_id` and `source_name = coalesce(monitor name, heartbeat name)`.
  - `uptime::alerts::Subject::Heartbeat(Uuid)` (email links to `/projects/{p}/heartbeats/{id}`).
  - `uptime::heartbeat::PingOutcome { UnknownToken, Recorded { transition: Option<Transition> } }`, `record_ping(&PgPool, token: &str) -> Result<PingOutcome, sqlx::Error>`, `sweep_overdue(&PgPool) -> Result<Vec<Transition>, sqlx::Error>`.
  - `uptime::worker::sweep_heartbeats(&AppState) -> Result<usize, AppError>`, called from every tick after `run_due_checks`.
  - HTTP: `POST|GET /projects/{id}/heartbeats`, `GET|PATCH|DELETE /projects/{id}/heartbeats/{heartbeat_id}` (RBAC like monitors), public `GET|POST /api/v1/heartbeat/{token}` → `200 {"ok": true}` or `404 {"error":"not found"}`.

State machine: a ping on an unpaused heartbeat sets `up` and, if it was `down`, resolves the open incident and returns a `Recovered` transition (the route sends the email in a background task so the cron job is not kept waiting). The sweep flips every unpaused, not-yet-down heartbeat whose `coalesce(last_ping_at, created_at) + period + grace` is in the past to `down` and opens its incident — one `UPDATE ... RETURNING` whose `current_status <> 'down'` guard is re-checked under each row lock, so concurrent sweeps cannot double-alert. Paused heartbeats keep recording pings but never change state.

- [ ] **Step 1: Write the failing test `tests/heartbeats.rs`**

```rust
// Requires `docker compose up -d postgres mailpit`.
mod common;

use axum::http::StatusCode;
use common::{add_member, call, seed_project, seed_user, settled_mail, test_app, unique_email, user_with_role};
use nami_engine::{state::AppState, uptime::worker::sweep_heartbeats};
use serde_json::{json, Value};
use uuid::Uuid;

/// A project with one member whose address is unique to this test.
async fn project_with_recipient(pool: &sqlx::PgPool) -> (Uuid, String) {
    let project = seed_project(pool).await;
    let email = unique_email("oncall");
    let user = seed_user(pool, &email, "member").await;
    add_member(pool, project, user, "viewer").await;
    (project, email)
}

/// Inserts a heartbeat created `created_seconds_ago` seconds ago that has
/// never pinged. Returns (id, token).
async fn seed_heartbeat(
    pool: &sqlx::PgPool,
    project: Uuid,
    name: &str,
    period: i32,
    grace: i32,
    created_seconds_ago: i32,
) -> (Uuid, String) {
    let token = Uuid::new_v4().simple().to_string();
    let id = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         values ($1, $2, $3, $4, $5, now() - make_interval(secs => $6))
         returning id",
    )
    .bind(project)
    .bind(name)
    .bind(&token)
    .bind(period)
    .bind(grace)
    .bind(f64::from(created_seconds_ago))
    .fetch_one(pool)
    .await
    .unwrap();
    (id, token)
}

async fn heartbeat_state(pool: &sqlx::PgPool, id: Uuid) -> (String, bool) {
    sqlx::query_as("select current_status, last_ping_at is not null from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn incidents_of(pool: &sqlx::PgPool, heartbeat: Uuid) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as("select status, title, cause from incidents where heartbeat_id = $1 order by started_at")
        .bind(heartbeat)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn member_creates_reads_and_updates_admin_deletes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/heartbeats");

    let (status, created) =
        call(&app, "POST", &collection, Some(&member), Some(json!({ "name": "Nightly backup", "period_seconds": 86400 }))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let token = created["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(created["grace_seconds"], 60, "default grace");
    assert_eq!(created["current_status"], "pending");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    assert_eq!(detail["token"], token, "the detail view keeps showing the token");
    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Backup", "grace_seconds": 300, "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!((patched["name"].clone(), patched["grace_seconds"].clone()), (json!("Backup"), json!(300)));
    assert_eq!(patched["period_seconds"], 86400);
    assert_eq!(patched["token"], token, "editing never rotates the token");

    assert_eq!(call(&app, "DELETE", &item, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &item, Some(&admin), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn heartbeat_rbac_and_project_isolation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (foreign, _) = seed_heartbeat(&pool, other_project, "foreign", 60, 0, 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/heartbeats");
    let body = json!({ "name": "x", "period_seconds": 60 });

    assert_eq!(call(&app, "POST", &collection, Some(&viewer), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &collection, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    let foreign_uri = format!("{collection}/{foreign}");
    assert_eq!(call(&app, "GET", &foreign_uri, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&app, "PATCH", &foreign_uri, Some(&admin), Some(json!({ "name": "hijacked" }))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "DELETE", &foreign_uri, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    let name: String = sqlx::query_scalar("select name from heartbeats where id = $1")
        .bind(foreign)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "foreign");
}

#[sqlx::test]
async fn invalid_heartbeats_are_rejected_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (json!({ "name": "x", "period_seconds": 29 }), "period_seconds"),
        (json!({ "name": "x", "period_seconds": 60, "grace_seconds": -1 }), "grace_seconds"),
        (json!({ "name": " ", "period_seconds": 60 }), "name"),
    ] {
        let (status, json) =
            call(&app, "POST", &format!("/projects/{project}/heartbeats"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        assert!(json["error"].as_str().unwrap().contains(fragment), "{body}: {json}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from heartbeats").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn a_ping_needs_no_session_and_marks_the_heartbeat_up(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, body) = call(&app, "POST", &format!("/api/v1/heartbeat/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "ok": true }));
    assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));

    let (status, _) = call(&app, "GET", &format!("/api/v1/heartbeat/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "GET pings work too");
    assert!(incidents_of(&pool, id).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty(), "pending -> up sends no email");
}

#[sqlx::test]
async fn an_unknown_token_is_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (status, body) = call(&app, "POST", "/api/v1/heartbeat/not-a-real-token", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "not found" }));
}

#[sqlx::test]
async fn the_sweep_marks_an_overdue_heartbeat_down_and_alerts_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    // Never pinged, created 120 s ago, period 30 s + grace 0 s: overdue.
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);

    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    assert_eq!(
        incidents_of(&pool, id).await,
        vec![(
            "open".to_string(),
            "Nightly backup missed its heartbeat".to_string(),
            Some("no ping received within 30s (+0s grace)".to_string())
        )]
    );
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, "[Nami] Nightly backup is DOWN");
    let link = format!("{}/projects/{project}/heartbeats/{id}", state.config.ui_base_url);
    assert!(mail[0].1.contains(&link), "{}", mail[0].1);

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0, "already down: no second incident");
    assert_eq!(incidents_of(&pool, id).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
}

#[sqlx::test]
async fn a_ping_on_a_down_heartbeat_resolves_it_and_sends_a_recovery(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();

    let (status, _) = call(&app, "POST", &format!("/api/v1/heartbeat/{token}"), None, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert_eq!(incidents_of(&pool, id).await[0].0, "resolved");
    // The recovery email is sent in the background: settled_mail waits for it.
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(
        mail.iter().map(|(subject, _)| subject.as_str()).collect::<Vec<_>>(),
        vec!["[Nami] Nightly backup is DOWN", "[Nami] Nightly backup is back UP"]
    );
}

#[sqlx::test]
async fn fresh_recently_pinged_and_paused_heartbeats_are_not_swept(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (fresh, _) = seed_heartbeat(&pool, project, "fresh", 60, 0, 0).await;
    let (pinged, _) = seed_heartbeat(&pool, project, "pinged", 60, 30, 86_400).await;
    sqlx::query("update heartbeats set last_ping_at = now() - interval '80 seconds' where id = $1")
        .bind(pinged)
        .execute(&pool)
        .await
        .unwrap();
    let (paused, _) = seed_heartbeat(&pool, project, "paused", 30, 0, 120).await;
    sqlx::query("update heartbeats set paused = true where id = $1").bind(paused).execute(&pool).await.unwrap();

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    for id in [fresh, pinged, paused] {
        assert_eq!(heartbeat_state(&pool, id).await.0, "pending");
    }
}

#[sqlx::test]
async fn a_paused_heartbeat_accepts_pings_without_changing_state(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&AppState::for_test(pool.clone())).await.unwrap();
    sqlx::query("update heartbeats set paused = true where id = $1").bind(id).execute(&pool).await.unwrap();

    let (status, _) = call(&app, "POST", &format!("/api/v1/heartbeat/{token}"), None, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await, ("down".to_string(), true), "ping recorded, status frozen");
    assert_eq!(incidents_of(&pool, id).await[0].0, "open");
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1, "only the original DOWN email");
}

#[sqlx::test]
async fn heartbeat_incidents_appear_in_the_incident_list(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(&app, "GET", &format!("/projects/{project}/incidents"), Some(&viewer), None).await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["heartbeat_id"], id.to_string());
    assert_eq!(list[0]["monitor_id"], Value::Null);
    assert_eq!(list[0]["source_name"], "Nightly backup");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeats`
Expected: compile error — `sweep_heartbeats` not found in `nami_engine::uptime::worker`.

- [ ] **Step 3: Write the migration and extend the migrations test**

`nami-engine/migrations/0011_create_heartbeats.sql` (this also completes the spec's `incidents` table):

```sql
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

create index heartbeats_project_idx on heartbeats (project_id);

-- Completes the spec's incidents table: 0010 could not reference
-- heartbeats before this table existed.
alter table incidents add column heartbeat_id uuid references heartbeats(id) on delete cascade;

-- At most one open incident per heartbeat (mirrors the monitor index).
create unique index incidents_one_open_per_heartbeat on incidents (heartbeat_id) where status = 'open';
```

In `nami-engine/tests/migrations.rs`, add `"heartbeats",` after `"incidents",`.

- [ ] **Step 4: Write the heartbeat model**

`nami-engine/src/models/heartbeat.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::validate_name;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct Heartbeat {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// The ping secret (`GET|POST /api/v1/heartbeat/{token}`). Stored in the
    /// clear, unlike auth tokens, because the UI keeps showing the ping URL.
    pub token: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
    pub current_status: String,
    pub last_ping_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// The user-editable fields of a heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub struct HeartbeatConfig {
    pub name: String,
    pub period_seconds: i32,
    pub grace_seconds: i32,
    pub paused: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateHeartbeatRequest {
    pub name: String,
    pub period_seconds: i32,
    #[serde(default = "default_grace_seconds")]
    pub grace_seconds: i32,
    #[serde(default)]
    pub paused: bool,
}

fn default_grace_seconds() -> i32 {
    60
}

impl CreateHeartbeatRequest {
    pub fn into_config(self) -> HeartbeatConfig {
        HeartbeatConfig {
            name: self.name,
            period_seconds: self.period_seconds,
            grace_seconds: self.grace_seconds,
            paused: self.paused,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchHeartbeatRequest {
    pub name: Option<String>,
    pub period_seconds: Option<i32>,
    pub grace_seconds: Option<i32>,
    pub paused: Option<bool>,
}

impl PatchHeartbeatRequest {
    pub fn apply_to(self, base: HeartbeatConfig) -> HeartbeatConfig {
        HeartbeatConfig {
            name: self.name.unwrap_or(base.name),
            period_seconds: self.period_seconds.unwrap_or(base.period_seconds),
            grace_seconds: self.grace_seconds.unwrap_or(base.grace_seconds),
            paused: self.paused.unwrap_or(base.paused),
        }
    }
}

pub fn validate(mut c: HeartbeatConfig) -> Result<HeartbeatConfig, String> {
    c.name = validate_name(&c.name)?;
    if c.period_seconds < 30 {
        return Err("period_seconds must be at least 30".into());
    }
    if c.grace_seconds < 0 {
        return Err("grace_seconds must not be negative".into());
    }
    Ok(c)
}

pub async fn insert_heartbeat(
    pool: &PgPool,
    project_id: Uuid,
    c: &HeartbeatConfig,
) -> Result<Heartbeat, sqlx::Error> {
    // 32 random bytes, hex-encoded (the same generator as auth tokens).
    let (token, _hash) = crate::auth::token::new_opaque_token();
    sqlx::query_as(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, paused)
         values ($1, $2, $3, $4, $5, $6)
         returning *",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&token)
    .bind(c.period_seconds)
    .bind(c.grace_seconds)
    .bind(c.paused)
    .fetch_one(pool)
    .await
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Heartbeat>, sqlx::Error> {
    sqlx::query_as("select * from heartbeats where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by BOTH ids, like `monitor::find_in_project`.
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
) -> Result<Option<Heartbeat>, sqlx::Error> {
    sqlx::query_as("select * from heartbeats where id = $1 and project_id = $2")
        .bind(heartbeat_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write under a row lock, like `monitor::update_in_project`.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
    patch: PatchHeartbeatRequest,
) -> Result<Option<Heartbeat>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Heartbeat> =
        sqlx::query_as("select * from heartbeats where id = $1 and project_id = $2 for update")
            .bind(heartbeat_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let base = HeartbeatConfig {
        name: current.name,
        period_seconds: current.period_seconds,
        grace_seconds: current.grace_seconds,
        paused: current.paused,
    };
    let config = validate(patch.apply_to(base)).map_err(AppError::Validation)?;
    let updated: Heartbeat = sqlx::query_as(
        "update heartbeats set name = $3, period_seconds = $4, grace_seconds = $5, paused = $6
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(heartbeat_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(config.period_seconds)
    .bind(config.grace_seconds)
    .bind(config.paused)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from heartbeats where id = $1 and project_id = $2")
        .bind(heartbeat_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
```

In `nami-engine/src/models/mod.rs`, add `pub mod heartbeat;` after `pub mod incident;`.

- [ ] **Step 5: Teach incidents about heartbeats**

Replace `nami-engine/src/models/incident.rs` with (the list query now joins heartbeats; two heartbeat variants of open/resolve are added):

```rust
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// One row of `GET /projects/{id}/incidents`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct IncidentListItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>,
    /// Name of the monitor (or heartbeat) the incident belongs to.
    pub source_name: Option<String>,
    pub title: String,
    pub cause: Option<String>,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Newest first, capped at 200. `status` and `monitor_id` are optional
/// filters (`None` = no filter).
pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
    status: Option<&str>,
    monitor_id: Option<Uuid>,
) -> Result<Vec<IncidentListItem>, sqlx::Error> {
    sqlx::query_as(
        "select i.id, i.project_id, i.monitor_id, i.heartbeat_id,
                coalesce(m.name, h.name) as source_name, i.title, i.cause, i.status,
                i.started_at, i.resolved_at
         from incidents i
         left join monitors m on m.id = i.monitor_id
         left join heartbeats h on h.id = i.heartbeat_id
         where i.project_id = $1
           and ($2::text is null or i.status = $2)
           and ($3::uuid is null or i.monitor_id = $3)
         order by i.started_at desc
         limit 200",
    )
    .bind(project_id)
    .bind(status)
    .bind(monitor_id)
    .fetch_all(pool)
    .await
}

/// Opens an incident for the monitor unless one is already open (the
/// `incidents_one_open_per_monitor` index turns a duplicate into a no-op).
/// Returns the new incident's id, or `None` if one was already open.
pub async fn open_for_monitor(
    conn: &mut PgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (monitor_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(monitor_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Resolves the monitor's open incident, if there is one. Returns its id.
pub async fn resolve_for_monitor(
    conn: &mut PgConnection,
    monitor_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where monitor_id = $1 and status = 'open'
         returning id",
    )
    .bind(monitor_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Heartbeat counterpart of [`open_for_monitor`], guarded by
/// `incidents_one_open_per_heartbeat`.
pub async fn open_for_heartbeat(
    conn: &mut PgConnection,
    project_id: Uuid,
    heartbeat_id: Uuid,
    title: &str,
    cause: Option<&str>,
    started_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into incidents (project_id, heartbeat_id, title, cause, started_at)
         values ($1, $2, $3, $4, $5)
         on conflict (heartbeat_id) where status = 'open' do nothing
         returning id",
    )
    .bind(project_id)
    .bind(heartbeat_id)
    .bind(title)
    .bind(cause)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await
}

/// Heartbeat counterpart of [`resolve_for_monitor`].
pub async fn resolve_for_heartbeat(
    conn: &mut PgConnection,
    heartbeat_id: Uuid,
    resolved_at: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "update incidents set status = 'resolved', resolved_at = $2
         where heartbeat_id = $1 and status = 'open'
         returning id",
    )
    .bind(heartbeat_id)
    .bind(resolved_at)
    .fetch_optional(&mut *conn)
    .await
}
```

- [ ] **Step 6: Add the heartbeat state machine, the alert subject and the sweep**

`nami-engine/src/uptime/heartbeat.rs`:

```rust
//! Heartbeat state machine: pings (`record_ping`) and the overdue sweep
//! (`sweep_overdue`). Same transitions as monitors: going down opens an
//! incident, a ping on a down heartbeat resolves it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::alerts::{Subject, Transition, TransitionKind};
use crate::models::incident;

#[derive(Debug, PartialEq)]
pub enum PingOutcome {
    UnknownToken,
    Recorded { transition: Option<Transition> },
}

/// Records a ping in one transaction, under the heartbeat's row lock so a
/// ping and a concurrent sweep apply one after the other.
///
/// Paused heartbeats keep accepting pings — `last_ping_at` stays fresh, so
/// resuming does not immediately trip the sweep — but their state is frozen:
/// no status change, no incident change, no alert.
pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "select id, project_id, name, current_status, paused from heartbeats where token = $1 for update",
    )
    .bind(token)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, project_id, name, status, paused)) = row else {
        return Ok(PingOutcome::UnknownToken);
    };

    if paused {
        sqlx::query("update heartbeats set last_ping_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(PingOutcome::Recorded { transition: None });
    }

    let pinged_at: DateTime<Utc> = sqlx::query_scalar(
        "update heartbeats set last_ping_at = now(), current_status = 'up' where id = $1 returning last_ping_at",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let transition = if status == "down" {
        incident::resolve_for_heartbeat(&mut tx, id, pinged_at).await?;
        Some(Transition {
            kind: TransitionKind::Recovered,
            subject: Subject::Heartbeat(id),
            project_id,
            name,
            cause: None,
            at: pinged_at,
        })
    } else {
        None
    };
    tx.commit().await?;
    Ok(PingOutcome::Recorded { transition })
}

/// Marks every unpaused heartbeat whose last ping (or creation, if it never
/// pinged) is older than `period + grace` as down and opens its incident,
/// in one transaction. The `current_status <> 'down'` guard is re-checked
/// under each row's lock, so two concurrent sweeps cannot both flip (and
/// alert on) the same heartbeat. The interval is computed in float8 so a
/// huge period plus grace cannot overflow int4 and break the sweep for
/// every heartbeat.
pub async fn sweep_overdue(pool: &PgPool) -> Result<Vec<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let overdue: Vec<(Uuid, Uuid, String, i32, i32)> = sqlx::query_as(
        "update heartbeats set current_status = 'down'
         where not paused
           and current_status <> 'down'
           and coalesce(last_ping_at, created_at)
               + make_interval(secs => period_seconds::float8 + grace_seconds::float8) < now()
         returning id, project_id, name, period_seconds, grace_seconds",
    )
    .fetch_all(&mut *tx)
    .await?;
    // now() is the transaction's start time: the same instant the UPDATE used.
    let now: DateTime<Utc> = sqlx::query_scalar("select now()").fetch_one(&mut *tx).await?;

    let mut transitions = Vec::new();
    for (id, project_id, name, period, grace) in overdue {
        let cause = format!("no ping received within {period}s (+{grace}s grace)");
        let opened = incident::open_for_heartbeat(
            &mut tx,
            project_id,
            id,
            &format!("{name} missed its heartbeat"),
            Some(&cause),
            now,
        )
        .await?;
        if opened.is_some() {
            transitions.push(Transition {
                kind: TransitionKind::Down,
                subject: Subject::Heartbeat(id),
                project_id,
                name,
                cause: Some(cause),
                at: now,
            });
        }
    }
    tx.commit().await?;
    Ok(transitions)
}
```

Replace `nami-engine/src/uptime/mod.rs` with:

```rust
//! Uptime monitoring: performing checks (`checker`), storing results in
//! ClickHouse (`store`), the background worker and its state machine
//! (`worker`), heartbeat pings and the overdue sweep (`heartbeat`), and
//! alert delivery (`alerts`).
pub mod alerts;
pub mod checker;
pub mod heartbeat;
pub mod store;
pub mod worker;
```

In `nami-engine/src/uptime/alerts.rs`:
- add `Heartbeat(Uuid),` to `enum Subject` after `Monitor(Uuid),`;
- add this arm to the `let link = match transition.subject { ... }` match, after the `Subject::Monitor(id)` arm:

```rust
        Subject::Heartbeat(id) => format!("{base}/projects/{}/heartbeats/{id}", transition.project_id),
```

- add this unit test inside `mod tests`, before `control_characters_never_reach_the_subject`:

```rust
    #[test]
    fn heartbeat_emails_link_to_the_heartbeat_page() {
        let heartbeat = Transition {
            subject: Subject::Heartbeat(Uuid::nil()),
            ..transition(TransitionKind::Down, "Nightly backup")
        };
        let (subject, body) = render("http://ui.test", &heartbeat);
        assert_eq!(subject, "[Nami] Nightly backup is DOWN");
        let nil = Uuid::nil();
        assert!(body.contains(&format!("http://ui.test/projects/{nil}/heartbeats/{nil}")), "{body}");
    }
```

In `nami-engine/src/uptime/worker.rs`:
- add `heartbeat,` to the `use super::{ ... }` list, between `checker::{...},` and `store::{...},`;
- insert before the `/// Ticks every [`TICK`] forever.` doc comment:

```rust
/// Marks overdue heartbeats down (opening incidents) and alerts for each.
/// Returns how many went down.
pub async fn sweep_heartbeats(state: &AppState) -> Result<usize, AppError> {
    let transitions = heartbeat::sweep_overdue(&state.db).await?;
    for transition in &transitions {
        alerts::notify_transition(state, transition).await;
    }
    Ok(transitions.len())
}

```

- replace the body of `async fn tick` with:

```rust
    if let Err(e) = run_due_checks(state).await {
        tracing::error!(error = ?e, "uptime check pass failed");
    }
    if let Err(e) = sweep_heartbeats(state).await {
        tracing::error!(error = ?e, "heartbeat sweep failed");
    }
```

- [ ] **Step 7: Write the routes and mount them**

`nami-engine/src/routes/heartbeats.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::heartbeat::{self, CreateHeartbeatRequest, Heartbeat, PatchHeartbeatRequest},
    state::AppState,
    uptime::{alerts, heartbeat::{record_ping, PingOutcome}},
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/heartbeats", get(list_heartbeats).post(create_heartbeat))
        .route(
            "/projects/{id}/heartbeats/{heartbeat_id}",
            get(get_heartbeat).patch(patch_heartbeat).delete(delete_heartbeat),
        )
}

/// The public ping endpoint. No session: the token in the path is the
/// secret, and cron jobs call the engine directly.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/v1/heartbeat/{token}", get(ping).post(ping))
}

async fn list_heartbeats(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Heartbeat>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(heartbeat::list_for_project(&state.db, project_id).await?))
}

async fn create_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateHeartbeatRequest>,
) -> Result<(StatusCode, Json<Heartbeat>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let config = heartbeat::validate(payload.into_config()).map_err(AppError::Validation)?;
    let created = heartbeat::insert_heartbeat(&state.db, project_id, &config)
        .await
        .map_err(|e| match &e {
            // Global admins bypass membership: the project may not exist.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Heartbeat>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let found = heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(found))
}

async fn patch_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchHeartbeatRequest>,
) -> Result<Json<Heartbeat>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let updated = heartbeat::update_in_project(&state.db, project_id, heartbeat_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

async fn delete_heartbeat(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !heartbeat::delete_in_project(&state.db, project_id, heartbeat_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn ping(State(state): State<AppState>, Path(token): Path<String>) -> Result<Json<Value>, AppError> {
    match record_ping(&state.db, &token).await? {
        PingOutcome::UnknownToken => Err(AppError::NotFound),
        PingOutcome::Recorded { transition } => {
            if let Some(transition) = transition {
                // A cron job is waiting on this response: deliver the
                // recovery alert in the background.
                let state = state.clone();
                tokio::spawn(async move { alerts::notify_transition(&state, &transition).await });
            }
            Ok(Json(json!({ "ok": true })))
        }
    }
}
```

In `nami-engine/src/routes/mod.rs` add `pub mod heartbeats;` after `pub mod incidents;`. In `nami-engine/src/router.rs::build_router` add, after `.merge(routes::incidents::router())`:

```rust
        .merge(routes::heartbeats::router())
        .merge(routes::heartbeats::public_router())
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test heartbeats --test migrations --test uptime_read_api --lib && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: `heartbeats` 10 passed, the incident-list tests in `uptime_read_api` still pass with the new join, lib unit tests include `heartbeat_emails_link_to_the_heartbeat_page`, the whole suite is green.

- [ ] **Step 9: Commit**

```bash
git add nami-engine/migrations/0011_create_heartbeats.sql nami-engine/src nami-engine/tests
git commit -F - <<'EOF'
feat(engine): add heartbeats with public ping endpoint and overdue sweep (NAMI-UP-6)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 7 (NAMI-UP-7): UI — project section + monitors

Branch `feat/NAMI-UP-7-ui-monitors`, worktree `.worktrees/NAMI-UP-7/`.

**Files:**
- Create: `nami-ui/src/lib/uptime.ts`, `nami-ui/src/lib/uptime.test.ts`
- Create: `nami-ui/src/lib/server/monitor-form.ts`, `nami-ui/src/lib/server/monitor-form.test.ts`
- Modify: `nami-ui/src/lib/server/engine.ts`, `nami-ui/src/lib/server/engine.test.ts`
- Create: `nami-ui/src/lib/components/StatusBadge.svelte`, `MonitorForm.svelte`, `ResponseTimeChart.svelte`
- Create: `nami-ui/src/routes/(app)/projects/[id]/+layout.server.ts`, `+layout.svelte`
- Modify: `nami-ui/src/routes/(app)/projects/[id]/+page.server.ts` (now a redirect)
- Move: `nami-ui/src/routes/(app)/projects/[id]/+page.svelte` → `settings/+page.svelte`; Create: `settings/+page.server.ts`
- Create: `monitors/+page.server.ts`, `monitors/+page.svelte`, `monitors/new/+page.server.ts`, `monitors/new/+page.svelte`, `monitors/[monitorId]/+page.server.ts`, `monitors/[monitorId]/+page.svelte`, `monitors/[monitorId]/edit/+page.server.ts`, `monitors/[monitorId]/edit/+page.svelte` (all under `nami-ui/src/routes/(app)/projects/[id]/`)

**Interfaces:**
- Consumes (engine, Tasks 2-5): `GET /projects/{id}`, `GET|POST /projects/{id}/monitors` (list items carry `uptime_24h`), `GET|PATCH|DELETE /projects/{id}/monitors/{monitor_id}`, `GET .../checks?limit=`, `GET .../stats`, `GET /projects/{id}/incidents?monitor_id=`; `callEngine(event, path, init?)` (existing).
- Produces:
  - `#lib/uptime`: types `Status`, `CheckType`, `Monitor`, `MonitorSummary`, `UptimeCheck`, `SeriesBucket`, `MonitorStats`, `Incident`; helpers `formatUptime(ratio)`, `formatMs(ms)`, `timeAgo(iso, now)`, `formatDuration(fromIso, toIso, now)`, `formatUtc(iso)`, `formatClockUtc(iso)`, `monitorTarget(m)`, `niceTicks(max, count?)`, `statusSince(monitor, incidents)`.
  - `#lib/server/engine`: `engineErrorMessage(response: Response, fallback: string): Promise<string>`.
  - `#lib/server/monitor-form`: `type FormResult = { ok: true; payload: Record<string, unknown> } | { ok: false; message: string }`, `formText(form, field)`, `formWholeNumber(form, field)`, `monitorPayloadFromForm(form: FormData): FormResult`.
  - Components: `StatusBadge { status: Status; paused?: boolean }`, `MonitorForm { monitor?: Monitor | null; submitLabel: string; message?: string | null; cancelHref: string }`, `ResponseTimeChart { series: SeriesBucket[] }`.
  - Pages: `/projects/[id]` (redirect), `/projects/[id]/settings`, `/projects/[id]/monitors`, `/new`, `/[monitorId]`, `/[monitorId]/edit`; the project layout's data `project: { id, name, slug }` is available to every child page.

Worktree note: run `npm install` inside this worktree's `nami-ui/`. Never symlink `node_modules` from another checkout — `svelte-kit sync` writes the route types into `node_modules/$app`, so a shared `node_modules` makes two checkouts overwrite each other's types (svelte-check then reports `Cannot find module './$types'` everywhere).

- [ ] **Step 1: Write the failing unit tests**

`nami-ui/src/lib/uptime.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
	formatClockUtc,
	formatDuration,
	formatMs,
	formatUptime,
	formatUtc,
	monitorTarget,
	niceTicks,
	statusSince,
	timeAgo,
	type Incident
} from './uptime';

describe('formatUptime', () => {
	it('shows two decimals and a dash for unknown', () => {
		expect(formatUptime(null)).toBe('—');
		expect(formatUptime(undefined)).toBe('—');
		expect(formatUptime(1)).toBe('100.00%');
		expect(formatUptime(0.75)).toBe('75.00%');
		expect(formatUptime(0)).toBe('0.00%');
	});

	// Review Focus: uptime that is not exactly 100% never displays as 100.00%.
	it('truncates instead of rounding, so any downtime stays visible', () => {
		expect(formatUptime(0.99999)).toBe('99.99%');
		expect(formatUptime(86_399 / 86_400)).toBe('99.99%');
		expect(formatUptime(0.12349)).toBe('12.34%');
	});

	it('is not thrown off by floating-point error', () => {
		expect(formatUptime(0.57)).toBe('57.00%'); // 0.57 * 10000 = 5699.999…
		expect(formatUptime(0.29)).toBe('29.00%');
	});
});

describe('formatMs', () => {
	it('rounds to whole milliseconds', () => {
		expect(formatMs(199.6)).toBe('200 ms');
		expect(formatMs(null)).toBe('—');
	});
});

describe('timeAgo', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('picks the largest whole unit', () => {
		expect(timeAgo(null, now)).toBe('never');
		expect(timeAgo('2026-10-13T11:59:48Z', now)).toBe('12s ago');
		expect(timeAgo('2026-10-13T11:55:00Z', now)).toBe('5m ago');
		expect(timeAgo('2026-10-13T09:00:00Z', now)).toBe('3h ago');
		expect(timeAgo('2026-10-11T12:00:00Z', now)).toBe('2d ago');
	});
	it('never goes negative for timestamps slightly in the future', () => {
		expect(timeAgo('2026-10-13T12:00:02Z', now)).toBe('0s ago');
	});
});

describe('formatDuration', () => {
	const now = Date.parse('2026-10-13T12:00:00Z');
	it('uses two units at most', () => {
		expect(formatDuration('2026-10-13T11:59:15Z', '2026-10-13T12:00:00Z', now)).toBe('45s');
		expect(formatDuration('2026-10-13T11:57:30Z', '2026-10-13T12:00:00Z', now)).toBe('2m 30s');
		expect(formatDuration('2026-10-13T09:55:00Z', '2026-10-13T12:00:00Z', now)).toBe('2h 5m');
		expect(formatDuration('2026-10-10T08:00:00Z', '2026-10-13T12:00:00Z', now)).toBe('3d 4h');
	});
	it('measures an ongoing incident up to now', () => {
		expect(formatDuration('2026-10-13T11:00:00Z', null, now)).toBe('1h 0m');
	});
});

describe('UTC formatting', () => {
	it('normalizes engine timestamps of any precision', () => {
		expect(formatUtc('2026-10-13T10:00:30.123456Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatUtc('2026-10-13T10:00:30.5Z')).toBe('2026-10-13 10:00:30 UTC');
		expect(formatClockUtc('2026-10-13T09:30:00Z')).toBe('09:30');
	});
});

describe('monitorTarget', () => {
	it('shows the url or host:port', () => {
		expect(monitorTarget({ check_type: 'http', url: 'https://x.test', host: null, port: null })).toBe('https://x.test');
		expect(monitorTarget({ check_type: 'tcp', url: null, host: 'db', port: 5432 })).toBe('db:5432');
	});
});

describe('niceTicks', () => {
	it('ends on a round value at or above the max', () => {
		expect(niceTicks(183)).toEqual([0, 50, 100, 150, 200]);
		expect(niceTicks(1234)).toEqual([0, 500, 1000, 1500]);
		expect(niceTicks(40)).toEqual([0, 10, 20, 30, 40]);
		expect(niceTicks(7)).toEqual([0, 2, 4, 6, 8]);
	});
	it('has a default scale when there is no data', () => {
		expect(niceTicks(0)).toEqual([0, 50, 100, 150, 200]);
	});
});

describe('statusSince', () => {
	const created = '2026-10-01T00:00:00Z';
	const incident = (status: 'open' | 'resolved', started: string, resolved: string | null): Incident => ({
		id: started,
		project_id: 'p',
		monitor_id: 'm',
		heartbeat_id: null,
		source_name: 'API',
		title: 'API is down',
		cause: null,
		status,
		started_at: started,
		resolved_at: resolved
	});
	const history = [
		incident('resolved', '2026-10-05T00:00:00Z', '2026-10-05T01:00:00Z'),
		incident('resolved', '2026-10-09T00:00:00Z', '2026-10-09T00:30:00Z')
	];

	it('is the open incident start while down', () => {
		const open = incident('open', '2026-10-12T00:00:00Z', null);
		expect(statusSince({ current_status: 'down', created_at: created }, [open, ...history])).toBe(open.started_at);
	});
	it('is the latest recovery while up', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, history)).toBe('2026-10-09T00:30:00Z');
	});
	it('falls back to creation', () => {
		expect(statusSince({ current_status: 'up', created_at: created }, [])).toBe(created);
		expect(statusSince({ current_status: 'pending', created_at: created }, history)).toBe(created);
	});
});
```

`nami-ui/src/lib/server/monitor-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { monitorPayloadFromForm } from './monitor-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('monitorPayloadFromForm', () => {
	it('sends http fields and parsed numbers for an http monitor', () => {
		const result = monitorPayloadFromForm(
			form({
				name: ' API ',
				check_type: 'http',
				url: ' https://example.com ',
				http_method: 'HEAD',
				expected_status_min: '200',
				expected_status_max: '299',
				interval_seconds: '30',
				timeout_seconds: '5',
				confirmation_threshold: '3',
				host: 'ignored',
				keyword: 'ignored'
			})
		);
		expect(result).toEqual({
			ok: true,
			payload: {
				name: 'API',
				check_type: 'http',
				url: 'https://example.com',
				http_method: 'HEAD',
				expected_status_min: 200,
				expected_status_max: 299,
				interval_seconds: 30,
				timeout_seconds: 5,
				confirmation_threshold: 3
			}
		});
	});

	it('sends host and port, never a url, for a tcp monitor', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'DB', check_type: 'tcp', host: 'db.internal', port: '5432', url: 'https://stale.test' })
		);
		expect(result).toEqual({ ok: true, payload: { name: 'DB', check_type: 'tcp', host: 'db.internal', port: 5432 } });
	});

	it('keeps the keyword untrimmed and defaults the mode', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'Home', check_type: 'keyword', url: 'https://example.com', keyword: ' Welcome ' })
		);
		expect(result.ok && result.payload.keyword).toBe(' Welcome ');
		expect(result.ok && result.payload.keyword_mode).toBe('contains');
	});

	it('omits blank numbers so the engine defaults apply', () => {
		const result = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', interval_seconds: '' }));
		expect(result.ok && 'interval_seconds' in result.payload).toBe(false);
	});

	it('rejects non-whole numbers', () => {
		expect(monitorPayloadFromForm(form({ name: 'API', check_type: 'http', interval_seconds: '1.5' }))).toEqual({
			ok: false,
			message: 'interval_seconds must be a whole number'
		});
		expect(monitorPayloadFromForm(form({ name: 'DB', check_type: 'tcp', port: 'abc' }))).toEqual({
			ok: false,
			message: 'port must be a whole number'
		});
	});
});
```

In `nami-ui/src/lib/server/engine.test.ts`, change the import to `import { callEngine, engineErrorMessage } from './engine';` and append:

```ts
describe('engineErrorMessage', () => {
	it("returns the engine's error message", async () => {
		const response = new Response(JSON.stringify({ error: 'interval_seconds must be at least 30' }), { status: 400 });
		expect(await engineErrorMessage(response, 'fallback')).toBe('interval_seconds must be at least 30');
	});

	it('falls back for a non-JSON body', async () => {
		const response = new Response('Failed to deserialize the JSON body', { status: 422 });
		expect(await engineErrorMessage(response, 'Could not save monitor.')).toBe('Could not save monitor.');
	});
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd nami-ui && npm test`
Expected: FAIL — `Failed to resolve import "./uptime"` / `"./monitor-form"`, and `engineErrorMessage` is not exported.

- [ ] **Step 3: Write the helpers**

`nami-ui/src/lib/uptime.ts`:

```ts
// Shapes of nami-engine's uptime JSON, plus pure display helpers shared by
// the monitor and incident pages. All times render in UTC so server- and
// client-rendered markup agree (and match the alert emails).

export type Status = 'pending' | 'up' | 'down';
export type CheckType = 'http' | 'keyword' | 'tcp';

export type Monitor = {
	id: string;
	project_id: string;
	name: string;
	check_type: CheckType;
	url: string | null;
	host: string | null;
	port: number | null;
	http_method: 'GET' | 'HEAD';
	keyword: string | null;
	keyword_mode: 'contains' | 'not_contains';
	expected_status_min: number;
	expected_status_max: number;
	interval_seconds: number;
	timeout_seconds: number;
	confirmation_threshold: number;
	paused: boolean;
	current_status: Status;
	consecutive_failures: number;
	last_checked_at: string | null;
	next_check_at: string;
	created_at: string;
};

export type MonitorSummary = Monitor & { uptime_24h: number | null };

export type UptimeCheck = {
	monitor_id: string;
	project_id: string;
	checked_at: string;
	status: 'up' | 'down';
	status_code: number | null;
	response_time_ms: number | null;
	error: string | null;
};

export type SeriesBucket = {
	bucket_start: string;
	avg_response_ms: number | null;
	up_ratio: number | null;
};

export type MonitorStats = {
	uptime_24h: number | null;
	uptime_7d: number | null;
	uptime_30d: number | null;
	avg_response_ms_24h: number | null;
	series: SeriesBucket[];
};

export type Incident = {
	id: string;
	project_id: string;
	monitor_id: string | null;
	heartbeat_id: string | null;
	source_name: string | null;
	title: string;
	cause: string | null;
	status: 'open' | 'resolved';
	started_at: string;
	resolved_at: string | null;
};

/**
 * Uptime ratio (0..1) as a percentage with two decimals, truncated rather
 * than rounded so downtime never disappears: 0.99999 shows "99.99%", never
 * "100.00%". The epsilon only absorbs floating-point error
 * (0.57 * 10000 = 5699.999…); real ratios are k/n with n < 100,000 checks
 * per window, so it cannot lift a lower value into the next hundredth.
 */
export function formatUptime(ratio: number | null | undefined): string {
	if (ratio === null || ratio === undefined || !Number.isFinite(ratio)) return '—';
	const hundredths = Math.floor(ratio * 10_000 + 1e-7);
	return `${(hundredths / 100).toFixed(2)}%`;
}

export function formatMs(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
	return `${Math.round(ms)} ms`;
}

/** "12s ago", "5m ago", "3h ago", "2d ago"; "never" for null. */
export function timeAgo(iso: string | null, now: number): string {
	if (!iso) return 'never';
	const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
	if (seconds < 60) return `${seconds}s ago`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	return `${Math.floor(hours / 24)}d ago`;
}

/** Elapsed time from `fromIso` to `toIso` (or to `now` while ongoing). */
export function formatDuration(fromIso: string, toIso: string | null, now: number): string {
	const end = toIso ? Date.parse(toIso) : now;
	let seconds = Math.max(0, Math.round((end - Date.parse(fromIso)) / 1000));
	const days = Math.floor(seconds / 86_400);
	seconds -= days * 86_400;
	const hours = Math.floor(seconds / 3_600);
	seconds -= hours * 3_600;
	const minutes = Math.floor(seconds / 60);
	seconds -= minutes * 60;
	if (days > 0) return `${days}d ${hours}h`;
	if (hours > 0) return `${hours}h ${minutes}m`;
	if (minutes > 0) return `${minutes}m ${seconds}s`;
	return `${seconds}s`;
}

/** "2026-10-13 14:30:05 UTC". */
export function formatUtc(iso: string): string {
	return `${new Date(iso).toISOString().slice(0, 19).replace('T', ' ')} UTC`;
}

/** "14:30" in UTC, for chart axes. */
export function formatClockUtc(iso: string): string {
	return new Date(iso).toISOString().slice(11, 16);
}

/** What the monitor checks: the URL, or host:port for TCP. */
export function monitorTarget(m: Pick<Monitor, 'check_type' | 'url' | 'host' | 'port'>): string {
	return m.check_type === 'tcp' ? `${m.host ?? ''}:${m.port ?? ''}` : (m.url ?? '');
}

/** Clean y-axis ticks from 0 to a round value >= max (steps of 1/2/5 x 10^n). */
export function niceTicks(max: number, count = 4): number[] {
	if (!(max > 0)) return [0, 50, 100, 150, 200];
	const rough = max / count;
	const magnitude = 10 ** Math.floor(Math.log10(rough));
	const step = [1, 2, 5, 10].map((m) => m * magnitude).find((s) => s >= rough) ?? 10 * magnitude;
	const top = Math.ceil(max / step) * step;
	return Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step);
}

/**
 * When the monitor entered its current status. The engine stores no
 * state-change timestamp, so derive it from incidents: down since the open
 * incident started; up since the latest recovery (or creation, if it never
 * went down); pending since creation.
 */
export function statusSince(
	monitor: Pick<Monitor, 'current_status' | 'created_at'>,
	incidents: Incident[]
): string {
	if (monitor.current_status === 'down') {
		return incidents.find((i) => i.status === 'open')?.started_at ?? monitor.created_at;
	}
	if (monitor.current_status === 'up') {
		let latest: string | null = null;
		for (const incident of incidents) {
			if (incident.resolved_at && (!latest || Date.parse(incident.resolved_at) > Date.parse(latest))) {
				latest = incident.resolved_at;
			}
		}
		return latest ?? monitor.created_at;
	}
	return monitor.created_at;
}
```

`nami-ui/src/lib/server/monitor-form.ts`:

```ts
// Turns the monitor form's FormData into nami-engine's JSON body. Only the
// fields of the selected check type are sent; numbers must be whole. The
// engine still validates everything — this only shapes the request and
// catches non-numeric input before the round trip.

export type FormResult =
	| { ok: true; payload: Record<string, unknown> }
	| { ok: false; message: string };

const COMMON_NUMBERS = ['interval_seconds', 'timeout_seconds', 'confirmation_threshold'] as const;
const HTTP_NUMBERS = ['expected_status_min', 'expected_status_max'] as const;

export function formText(form: FormData, field: string): string {
	return String(form.get(field) ?? '').trim();
}

/** `undefined` when blank (the engine applies its default). */
export function formWholeNumber(form: FormData, field: string): number | undefined | 'invalid' {
	const raw = formText(form, field);
	if (raw === '') return undefined;
	return /^-?\d+$/.test(raw) ? Number(raw) : 'invalid';
}

export function monitorPayloadFromForm(form: FormData): FormResult {
	const checkType = formText(form, 'check_type');
	const payload: Record<string, unknown> = { name: formText(form, 'name'), check_type: checkType };

	const numbers: string[] = [...COMMON_NUMBERS];
	if (checkType === 'tcp') {
		payload.host = formText(form, 'host');
		numbers.push('port');
	} else {
		payload.url = formText(form, 'url');
		payload.http_method = formText(form, 'http_method') || 'GET';
		numbers.push(...HTTP_NUMBERS);
		if (checkType === 'keyword') {
			// Not trimmed: leading/trailing spaces can be part of the keyword.
			payload.keyword = String(form.get('keyword') ?? '');
			payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
		}
	}

	for (const field of numbers) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	return { ok: true, payload };
}
```

Append to `nami-ui/src/lib/server/engine.ts`:

```ts

/**
 * The engine's `{ "error": "..." }` message, or `fallback` when the body is
 * not that shape (axum's own extractor rejections are plain text).
 */
export async function engineErrorMessage(response: Response, fallback: string): Promise<string> {
	try {
		const body = await response.json();
		if (body && typeof body.error === 'string') return body.error;
	} catch {
		// not JSON
	}
	return fallback;
}
```

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cd nami-ui && npm test`
Expected: PASS — 3 files (`engine.test.ts`, `uptime.test.ts`, `monitor-form.test.ts`), 26 tests.

- [ ] **Step 5: Write the shared components (invoke the `dataviz` skill first)**

Before writing any chart code, invoke the `dataviz` skill (Skill tool) and follow it. The chart below already applies it — one series so no legend, 2 px round-capped line, ≥ 8 px end marker with a 2 px surface ring, hairline solid gridlines one step off the surface, the latest value as the only direct label, crosshair + tooltip on hover, the same readout on keyboard focus (a visually hidden range input), a data-table twin, gaps drawn as gaps — and its line colour `teal-600` passed `validate_palette.js --mode dark --surface "#141414"` (teal-500/400 fail the dark lightness band). Re-check anything you change against the skill.

`nami-ui/src/lib/components/StatusBadge.svelte` (status is never colour-alone — the dot always sits next to its label; "green" is the theme's teal):

```svelte
<script lang="ts">
	import type { Status } from '#lib/uptime';

	let { status, paused = false }: { status: Status; paused?: boolean } = $props();

	// Status is never colour-alone: the dot always sits next to its label.
	const label = $derived(
		paused ? 'Paused' : status === 'up' ? 'Up' : status === 'down' ? 'Down' : 'Pending'
	);
	const dot = $derived(
		paused
			? 'bg-neutral-600'
			: status === 'up'
				? 'bg-teal-400'
				: status === 'down'
					? 'bg-red-400'
					: 'bg-neutral-500'
	);
	const text = $derived(
		paused || status === 'pending'
			? 'text-neutral-400'
			: status === 'up'
				? 'text-teal-300'
				: 'text-red-400'
	);
</script>

<span class="inline-flex items-center gap-2 text-sm {text}">
	<span class="h-2 w-2 shrink-0 rounded-full {dot}" aria-hidden="true"></span>
	{label}
</span>
```

`nami-ui/src/lib/components/ResponseTimeChart.svelte`:

```svelte
<script lang="ts">
	// Average response time per 30-minute bucket over the last 24 hours.
	// One series, so no legend (the card title names it). The line is
	// teal-600, validated in-band against the #141414 card surface; gaps
	// (buckets without checks) break the line instead of being interpolated.
	import {
		formatClockUtc,
		formatMs,
		formatUptime,
		formatUtc,
		niceTicks,
		type SeriesBucket
	} from '#lib/uptime';

	let { series }: { series: SeriesBucket[] } = $props();

	const HEIGHT = 220;
	const PAD = { top: 16, right: 72, bottom: 28, left: 56 };
	const PLOT_HEIGHT = HEIGHT - PAD.top - PAD.bottom;
	const BUCKET_MS = 30 * 60 * 1000;

	let width = $state(720);
	let hoverIndex = $state<number | null>(null);
	let focusIndex = $state<number | null>(null);

	const plotWidth = $derived(Math.max(width - PAD.left - PAD.right, 1));
	// Time labels every 6 hours, or every 12 on narrow screens so they never
	// collide with each other or with the "now" label.
	const labelEvery = $derived(plotWidth < 420 ? 24 : 12);
	const points = $derived(
		series.flatMap((bucket, i) =>
			bucket.avg_response_ms === null ? [] : [{ i, v: bucket.avg_response_ms }]
		)
	);
	const ticks = $derived(niceTicks(Math.max(0, ...points.map((p) => p.v))));
	const yMax = $derived(ticks[ticks.length - 1]);
	const last = $derived(points.length > 0 ? points[points.length - 1] : null);
	const active = $derived(hoverIndex ?? focusIndex);

	function xAt(i: number): number {
		return PAD.left + (series.length > 1 ? (i / (series.length - 1)) * plotWidth : plotWidth / 2);
	}

	function yAt(v: number): number {
		return PAD.top + PLOT_HEIGHT - (v / yMax) * PLOT_HEIGHT;
	}

	// Runs of consecutive buckets with data: each run is one path, a run of
	// one bucket is drawn as a dot.
	const runs = $derived.by(() => {
		const result: { i: number; v: number }[][] = [];
		let run: { i: number; v: number }[] = [];
		series.forEach((bucket, i) => {
			if (bucket.avg_response_ms === null) {
				if (run.length > 0) result.push(run);
				run = [];
			} else {
				run.push({ i, v: bucket.avg_response_ms });
			}
		});
		if (run.length > 0) result.push(run);
		return result;
	});

	function pathFor(run: { i: number; v: number }[]): string {
		return run
			.map((p, k) => `${k === 0 ? 'M' : 'L'}${xAt(p.i).toFixed(1)},${yAt(p.v).toFixed(1)}`)
			.join(' ');
	}

	function bucketRange(bucket: SeriesBucket): string {
		const end = new Date(Date.parse(bucket.bucket_start) + BUCKET_MS).toISOString();
		return `${formatClockUtc(bucket.bucket_start)}–${formatClockUtc(end)} UTC`;
	}

	function describe(i: number): string {
		const bucket = series[i];
		if (!bucket) return '';
		return `${bucketRange(bucket)}: ${formatMs(bucket.avg_response_ms)} average, ${formatUptime(bucket.up_ratio)} up`;
	}

	// The crosshair snaps to the nearest bucket.
	function onPointerMove(event: PointerEvent) {
		const box = (event.currentTarget as SVGSVGElement).getBoundingClientRect();
		const ratio = (event.clientX - box.left - PAD.left) / plotWidth;
		hoverIndex = Math.min(series.length - 1, Math.max(0, Math.round(ratio * (series.length - 1))));
	}
</script>

<div class="relative" bind:clientWidth={width}>
	<!-- Keyboard access: arrow keys move through the buckets and show the
	     same readout as hovering; screen readers get it as the value text. -->
	<label class="sr-only" for="response-time-inspector">Inspect response time by half hour</label>
	<input
		id="response-time-inspector"
		class="peer sr-only"
		type="range"
		min="0"
		max={Math.max(series.length - 1, 0)}
		step="1"
		value={focusIndex ?? Math.max(series.length - 1, 0)}
		aria-valuetext={describe(focusIndex ?? series.length - 1)}
		oninput={(event) => (focusIndex = Number(event.currentTarget.value))}
		onfocus={(event) => (focusIndex = Number(event.currentTarget.value))}
		onblur={() => (focusIndex = null)}
	/>
	<svg
		{width}
		height={HEIGHT}
		role="img"
		aria-label="Average response time per 30 minutes over the last 24 hours"
		class="block rounded-md peer-focus-visible:ring-2 peer-focus-visible:ring-teal-500/50"
		onpointermove={onPointerMove}
		onpointerleave={() => (hoverIndex = null)}
	>
		{#each ticks as tick (tick)}
			<line
				x1={PAD.left}
				x2={PAD.left + plotWidth}
				y1={yAt(tick)}
				y2={yAt(tick)}
				class="stroke-[#262626]"
				stroke-width="1"
			/>
			<text
				x={PAD.left - 8}
				y={yAt(tick)}
				dy="0.32em"
				text-anchor="end"
				class="fill-neutral-500 text-[11px] tabular-nums">{tick} ms</text
			>
		{/each}

		{#each series as bucket, i (bucket.bucket_start)}
			{#if i % labelEvery === 0}
				<text
					x={xAt(i)}
					y={HEIGHT - 8}
					text-anchor="middle"
					class="fill-neutral-500 text-[11px] tabular-nums">{formatClockUtc(bucket.bucket_start)}</text
				>
			{/if}
		{/each}
		<text x={PAD.left + plotWidth} y={HEIGHT - 8} text-anchor="end" class="fill-neutral-500 text-[11px]"
			>{labelEvery === 12 ? 'now (UTC)' : 'now'}</text
		>

		{#each runs as run (run[0].i)}
			{#if run.length === 1}
				<circle cx={xAt(run[0].i)} cy={yAt(run[0].v)} r="4" class="fill-teal-600" />
			{:else}
				<path
					d={pathFor(run)}
					fill="none"
					class="stroke-teal-600"
					stroke-width="2"
					stroke-linejoin="round"
					stroke-linecap="round"
				/>
			{/if}
		{/each}

		{#if active !== null && series[active]}
			<line
				x1={xAt(active)}
				x2={xAt(active)}
				y1={PAD.top}
				y2={PAD.top + PLOT_HEIGHT}
				class="stroke-neutral-600"
				stroke-width="1"
			/>
		{/if}

		{#if last}
			<!-- End marker with a 2px surface ring, and the latest value as the
			     chart's only direct label. -->
			<circle
				cx={xAt(last.i)}
				cy={yAt(last.v)}
				r="4"
				class="fill-teal-600 stroke-[#141414]"
				stroke-width="2"
			/>
			<text
				x={xAt(last.i) + 10}
				y={yAt(last.v)}
				dy="0.32em"
				class="fill-neutral-200 text-xs font-medium tabular-nums">{formatMs(last.v)}</text
			>
		{/if}
	</svg>

	{#if points.length === 0}
		<p class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-neutral-500">
			{series.some((bucket) => bucket.up_ratio !== null)
				? 'No response times in the last 24 hours: every check failed before a response arrived.'
				: 'No checks in the last 24 hours yet.'}
		</p>
	{/if}

	{#if active !== null && series[active]}
		{@const bucket = series[active]}
		<div
			class="pointer-events-none absolute top-2 w-44 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-xs shadow-lg"
			style:left="{Math.max(0, Math.min(xAt(active) + 12, width - 184))}px"
		>
			<div class="text-sm font-semibold text-neutral-100 tabular-nums">
				{formatMs(bucket.avg_response_ms)}
			</div>
			<div class="text-neutral-400">avg response · {formatUptime(bucket.up_ratio)} up</div>
			<div class="text-neutral-500">{bucketRange(bucket)}</div>
		</div>
	{/if}
</div>

<details class="mt-3 text-sm">
	<summary class="cursor-pointer text-neutral-500 hover:text-neutral-300">Show data table</summary>
	<div class="mt-2 max-h-64 overflow-y-auto">
		<table class="w-full text-left text-xs">
			<thead class="text-neutral-500">
				<tr>
					<th class="py-1 font-medium">Half hour (UTC)</th>
					<th class="py-1 font-medium">Avg response</th>
					<th class="py-1 font-medium">Uptime</th>
				</tr>
			</thead>
			<tbody class="text-neutral-300 tabular-nums">
				{#each series as bucket (bucket.bucket_start)}
					<tr class="border-t border-[#262626]">
						<td class="py-1">{formatUtc(bucket.bucket_start)}</td>
						<td class="py-1">{formatMs(bucket.avg_response_ms)}</td>
						<td class="py-1">{formatUptime(bucket.up_ratio)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</details>
```

`nami-ui/src/lib/components/MonitorForm.svelte` (the fields switch with the check type, a client-side `$state`, so the form uses `use:enhance`; actions return `fail()` so a validation error keeps what the user typed):

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import type { CheckType, Monitor } from '#lib/uptime';

	let {
		monitor = null,
		submitLabel,
		message = null,
		cancelHref
	}: {
		monitor?: Monitor | null;
		submitLabel: string;
		message?: string | null;
		cancelHref: string;
	} = $props();

	// Read once, in a function, on purpose: after mount the select drives
	// which fields render, independent of later prop updates.
	function initialCheckType(): CheckType {
		return monitor?.check_type ?? 'http';
	}
	let checkType = $state<CheckType>(initialCheckType());

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
</script>

<!-- use:enhance keeps the typed values and the selected check type on a
     validation error (the action returns fail(), so the form is not reset). -->
<form method="POST" use:enhance class="space-y-5">
	{#if message}
		<div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
			{message}
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for="name" class={label}>Name</label>
			<input id="name" name="name" type="text" required maxlength="200" value={monitor?.name ?? ''} class={input} />
		</div>
		<div>
			<label for="check_type" class={label}>Check type</label>
			<select id="check_type" name="check_type" bind:value={checkType} class={input}>
				<option value="http">HTTP status</option>
				<option value="keyword">Keyword on page</option>
				<option value="tcp">TCP port</option>
			</select>
		</div>
	</div>

	{#if checkType === 'tcp'}
		<div class="grid gap-4 sm:grid-cols-[1fr_8rem]">
			<div>
				<label for="host" class={label}>Host</label>
				<input id="host" name="host" type="text" required placeholder="db.internal" value={monitor?.host ?? ''} class={input} />
			</div>
			<div>
				<label for="port" class={label}>Port</label>
				<input id="port" name="port" type="number" required min="1" max="65535" value={monitor?.port ?? ''} class={input} />
			</div>
		</div>
	{:else}
		<div>
			<label for="url" class={label}>URL</label>
			<input id="url" name="url" type="url" required placeholder="https://example.com/health" value={monitor?.url ?? ''} class={input} />
		</div>
		{#if checkType === 'keyword'}
			<!-- Keyword checks need a body to search, so they always use GET. -->
			<input type="hidden" name="http_method" value="GET" />
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="keyword" class={label}>Keyword (case-sensitive)</label>
					<input id="keyword" name="keyword" type="text" required value={monitor?.keyword ?? ''} class={input} />
				</div>
				<div>
					<label for="keyword_mode" class={label}>Up when the page</label>
					<select id="keyword_mode" name="keyword_mode" value={monitor?.keyword_mode ?? 'contains'} class={input}>
						<option value="contains">contains the keyword</option>
						<option value="not_contains">does not contain the keyword</option>
					</select>
				</div>
			</div>
		{:else}
			<div>
				<label for="http_method" class={label}>Method</label>
				<select id="http_method" name="http_method" value={monitor?.http_method ?? 'GET'} class={input}>
					<option value="GET">GET</option>
					<option value="HEAD">HEAD</option>
				</select>
			</div>
		{/if}
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="expected_status_min" class={label}>Lowest OK status</label>
				<input id="expected_status_min" name="expected_status_min" type="number" min="100" max="599" value={monitor?.expected_status_min ?? 200} class={input} />
			</div>
			<div>
				<label for="expected_status_max" class={label}>Highest OK status</label>
				<input id="expected_status_max" name="expected_status_max" type="number" min="100" max="599" value={monitor?.expected_status_max ?? 399} class={input} />
			</div>
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-3">
		<div>
			<label for="interval_seconds" class={label}>Check every (seconds)</label>
			<input id="interval_seconds" name="interval_seconds" type="number" min="30" value={monitor?.interval_seconds ?? 60} class={input} />
		</div>
		<div>
			<label for="timeout_seconds" class={label}>Timeout (seconds)</label>
			<input id="timeout_seconds" name="timeout_seconds" type="number" min="1" max="60" value={monitor?.timeout_seconds ?? 10} class={input} />
		</div>
		<div>
			<label for="confirmation_threshold" class={label}>Failures before alerting</label>
			<input id="confirmation_threshold" name="confirmation_threshold" type="number" min="1" max="10" value={monitor?.confirmation_threshold ?? 2} class={input} />
		</div>
	</div>

	<div class="flex items-center gap-3">
		<button
			type="submit"
			class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
		>
			{submitLabel}
		</button>
		<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">Cancel</a>
	</div>
</form>
```

- [ ] **Step 6: Turn the project page into a section with a sub-nav**

`nami-ui/src/routes/(app)/projects/[id]/+layout.server.ts`:

```ts
import { error } from '@sveltejs/kit';
import { callEngine } from '#lib/server/engine';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}`);
	if (!response.ok) {
		// Engine always returns 403 here, even for a nonexistent project
		// (anti-enumeration design) — never try to distinguish the two cases.
		throw error(403, 'You do not have access to this project.');
	}
	const project: { id: string; name: string; slug: string } = await response.json();
	return { project };
};
```

`nami-ui/src/routes/(app)/projects/[id]/+layout.svelte` (Heartbeats and Incidents tabs arrive with their pages in Task 8):

```svelte
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const tabs = $derived([
		{ href: `/projects/${data.project.id}/monitors`, label: 'Monitors' },
		{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
	]);
</script>

<div class="space-y-6">
	<div>
		<a href="/projects" class="text-sm text-neutral-500 hover:text-neutral-300">← Projects</a>
		<h1 class="mt-2 text-lg font-semibold text-neutral-100">{data.project.name}</h1>
		<p class="text-sm text-neutral-500">{data.project.slug}</p>
	</div>

	<nav class="flex gap-1 border-b border-[#262626]" aria-label="Project sections">
		{#each tabs as tab (tab.href)}
			{@const active = page.url.pathname.startsWith(tab.href)}
			<a
				href={tab.href}
				aria-current={active ? 'page' : undefined}
				class="-mb-px border-b-2 px-3 py-2 text-sm transition-colors {active
					? 'border-teal-500 text-neutral-100'
					: 'border-transparent text-neutral-500 hover:text-neutral-300'}"
			>
				{tab.label}
			</a>
		{/each}
	</nav>

	{@render children()}
</div>
```

Move the member forms to Settings and make the old page a redirect:

```bash
cd "nami-ui/src/routes/(app)/projects/[id]"
mkdir -p settings
git mv +page.svelte settings/+page.svelte
```

Replace `nami-ui/src/routes/(app)/projects/[id]/+page.server.ts` with (a page with only a redirecting server load needs no `+page.svelte`; SvelteKit 3 renders a missing page component as empty):

```ts
import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

// The project page is now a section (see +layout.svelte): land on its
// first tab. The member forms moved to ./settings.
export const load: PageServerLoad = async (event) => {
	throw redirect(307, `/projects/${event.params.id}/monitors`);
};
```

`nami-ui/src/routes/(app)/projects/[id]/settings/+page.server.ts` (the `addMember`/`removeMember` actions, unchanged):

```ts
import type { Actions } from './$types';
import { callEngine } from '#lib/server/engine';

// Moved unchanged from the old /projects/[id] page.
export const actions: Actions = {
	addMember: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, `/projects/${event.params.id}/members`, {
			method: 'POST',
			body: JSON.stringify({
				user_id: form.get('user_id'),
				project_role: form.get('project_role')
			})
		});
		const body = await response.json();
		return {
			action: 'add' as const,
			success: response.ok,
			message: response.ok ? 'Member added.' : (body.error ?? 'could not add member')
		};
	},
	removeMember: async (event) => {
		const form = await event.request.formData();
		const userId = form.get('user_id');
		const response = await callEngine(
			event,
			`/projects/${event.params.id}/members/${userId}`,
			{ method: 'DELETE' }
		);
		const body = await response.json();
		return {
			action: 'remove' as const,
			success: response.ok,
			message: response.ok ? 'Member removed.' : (body.error ?? 'could not remove member')
		};
	}
};
```

Replace `nami-ui/src/routes/(app)/projects/[id]/settings/+page.svelte` with (the old header card and back link are gone — the layout shows both; the two forms are unchanged):

```svelte
<script lang="ts">
	import type { PageProps } from './$types';

	let { form }: PageProps = $props();
</script>

<!-- The member forms, moved unchanged from the old project page. -->
<div class="space-y-6">
	<p class="text-sm text-neutral-500">
		This project has no visible member list yet — add or remove members by their user ID.
	</p>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Add member</h2>

		{#if form?.action === 'add'}
			<div
				class="mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
			>
				{form.message}
			</div>
		{/if}

		<form method="POST" action="?/addMember" class="mt-4 space-y-4">
			<div>
				<label for="add-user-id" class="mb-1.5 block text-sm font-medium text-neutral-300">User ID</label>
				<input
					id="add-user-id"
					name="user_id"
					type="text"
					placeholder="User ID (UUID)"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
				<p class="mt-1 text-xs text-neutral-600">
					Ask the user for their ID — it's shown on their Dashboard page.
				</p>
			</div>
			<div>
				<label for="project_role" class="mb-1.5 block text-sm font-medium text-neutral-300">Role</label>
				<select
					id="project_role"
					name="project_role"
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				>
					<option value="admin">admin</option>
					<option value="member" selected>member</option>
					<option value="viewer">viewer</option>
				</select>
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Add member
			</button>
		</form>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Remove member</h2>

		{#if form?.action === 'remove'}
			<div
				class="mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
			>
				{form.message}
			</div>
		{/if}

		<form method="POST" action="?/removeMember" class="mt-4 space-y-4">
			<div>
				<label for="remove-user-id" class="mb-1.5 block text-sm font-medium text-neutral-300">User ID</label>
				<input
					id="remove-user-id"
					name="user_id"
					type="text"
					placeholder="User ID (UUID)"
					required
					class="w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none"
				/>
				<p class="mt-1 text-xs text-neutral-600">
					Ask the user for their ID — it's shown on their Dashboard page.
				</p>
			</div>
			<button
				type="submit"
				class="rounded-md border border-[#262626] px-4 py-2 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
			>
				Remove member
			</button>
		</form>
	</div>
</div>
```

- [ ] **Step 7: Write the monitors list (auto-refresh every 30 s)**

`nami-ui/src/routes/(app)/projects/[id]/monitors/+page.server.ts`:

```ts
import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { MonitorSummary } from '#lib/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors`);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load monitors.')
		);
	}
	const monitors: MonitorSummary[] = await response.json();
	return { monitors };
};
```

`nami-ui/src/routes/(app)/projects/[id]/monitors/+page.svelte`:

```svelte
<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { formatUptime, monitorTarget, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const typeLabel = { http: 'HTTP', keyword: 'Keyword', tcp: 'TCP' } as const;

	// Re-run the load every 30 s like BetterStack's dashboard, and tick a
	// clock so "last checked" stays current in between. `refreshAll` is Kit
	// 3's replacement for the deprecated `invalidateAll`.
	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414]">
	<div class="flex items-center justify-between border-b border-[#262626] px-6 py-4">
		<div>
			<h2 class="text-base font-semibold text-neutral-100">Monitors</h2>
			<p class="text-xs text-neutral-500">Refreshes every 30 seconds.</p>
		</div>
		<a
			href="/projects/{data.project.id}/monitors/new"
			class="rounded-md bg-teal-500 px-3 py-1.5 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
		>
			New monitor
		</a>
	</div>

	{#if data.monitors.length === 0}
		<p class="px-6 py-10 text-center text-sm text-neutral-500">No monitors yet — create one to start checking.</p>
	{:else}
		<div class="overflow-x-auto">
			<table class="w-full text-sm">
				<thead class="text-left text-xs text-neutral-500">
					<tr class="border-b border-[#262626]">
						<th class="px-6 py-2 font-medium">Status</th>
						<th class="px-6 py-2 font-medium">Name</th>
						<th class="px-6 py-2 font-medium">Type</th>
						<th class="px-6 py-2 font-medium">Target</th>
						<th class="px-6 py-2 font-medium">Uptime (24 h)</th>
						<th class="px-6 py-2 font-medium">Last checked</th>
					</tr>
				</thead>
				<tbody>
					{#each data.monitors as monitor (monitor.id)}
						<tr class="border-b border-[#262626] last:border-0 hover:bg-[#0a0a0a]">
							<td class="px-6 py-3"><StatusBadge status={monitor.current_status} paused={monitor.paused} /></td>
							<td class="px-6 py-3">
								<a
									href="/projects/{data.project.id}/monitors/{monitor.id}"
									class="font-medium text-neutral-100 hover:text-teal-300">{monitor.name}</a
								>
							</td>
							<td class="px-6 py-3 text-neutral-400">{typeLabel[monitor.check_type]}</td>
							<td class="max-w-xs truncate px-6 py-3 font-mono text-xs text-neutral-400" title={monitorTarget(monitor)}>
								{monitorTarget(monitor)}
							</td>
							<td class="px-6 py-3 text-neutral-200 tabular-nums">{formatUptime(monitor.uptime_24h)}</td>
							<td class="px-6 py-3 text-neutral-500">{timeAgo(monitor.last_checked_at, now)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
```

- [ ] **Step 8: Write the create and edit pages**

`nami-ui/src/routes/(app)/projects/[id]/monitors/new/+page.server.ts`:

```ts
import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { Actions } from './$types';

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/monitors`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not create monitor.') });
		}
		const created: { id: string } = await response.json();
		throw redirect(303, `/projects/${event.params.id}/monitors/${created.id}`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/monitors/new/+page.svelte`:

```svelte
<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">New monitor</h2>
	<div class="mt-4">
		<MonitorForm
			submitLabel="Create monitor"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors"
		/>
	</div>
</div>
```

`nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/edit/+page.server.ts`:

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { Monitor } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	if (response.status === 404) throw error(404, 'Monitor not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load monitor.'));
	}
	const monitor: Monitor = await response.json();
	return { monitor };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not save monitor.') });
		}
		throw redirect(303, `/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/edit/+page.svelte`:

```svelte
<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">Edit {data.monitor.name}</h2>
	<div class="mt-4">
		<MonitorForm
			monitor={data.monitor}
			submitLabel="Save changes"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors/{data.monitor.id}"
		/>
	</div>
</div>
```

- [ ] **Step 9: Write the monitor detail page**

`nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.server.ts` (named actions only — `pause`, `resume`, `delete`; history comes from ClickHouse, so a failed `/stats` or `/checks` call renders empty sections instead of an error page):

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { Incident, Monitor, MonitorStats, UptimeCheck } from '#lib/uptime';
import type { Actions, PageServerLoad, RequestEvent } from './$types';

export const load: PageServerLoad = async (event) => {
	const base = `/projects/${event.params.id}/monitors/${event.params.monitorId}`;
	const [monitorRes, statsRes, checksRes, incidentsRes] = await Promise.all([
		callEngine(event, base),
		callEngine(event, `${base}/stats`),
		callEngine(event, `${base}/checks?limit=20`),
		callEngine(event, `/projects/${event.params.id}/incidents?monitor_id=${event.params.monitorId}`)
	]);
	if (monitorRes.status === 404) throw error(404, 'Monitor not found.');
	if (!monitorRes.ok) {
		throw error(
			monitorRes.status === 403 ? 403 : 500,
			await engineErrorMessage(monitorRes, 'Could not load monitor.')
		);
	}
	const monitor: Monitor = await monitorRes.json();
	// History lives in ClickHouse: if it is unavailable, still render the
	// monitor (its status comes from Postgres) with the history sections empty.
	const stats: MonitorStats | null = statsRes.ok ? await statsRes.json() : null;
	const checks: UptimeCheck[] = checksRes.ok ? await checksRes.json() : [];
	const incidents: Incident[] = incidentsRes.ok ? await incidentsRes.json() : [];
	return { monitor, stats, checks, incidents };
};

async function setPaused(event: RequestEvent, paused: boolean) {
	const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
		method: 'PATCH',
		body: JSON.stringify({ paused })
	});
	if (!response.ok) {
		return fail(response.status, { message: await engineErrorMessage(response, 'Could not update monitor.') });
	}
	return { success: true };
}

export const actions: Actions = {
	pause: (event) => setPaused(event, true),
	resume: (event) => setPaused(event, false),
	delete: async (event) => {
		const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
			method: 'DELETE'
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not delete monitor.') });
		}
		throw redirect(303, `/projects/${event.params.id}/monitors`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import ResponseTimeChart from '#lib/components/ResponseTimeChart.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import {
		formatDuration,
		formatMs,
		formatUptime,
		formatUtc,
		monitorTarget,
		statusSince,
		timeAgo
	} from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const base = $derived(`/projects/${data.project.id}/monitors/${data.monitor.id}`);
	const since = $derived(statusSince(data.monitor, data.incidents));
	const headline = $derived(
		data.monitor.paused
			? { text: 'PAUSED', class: 'text-neutral-400' }
			: data.monitor.current_status === 'up'
				? { text: 'UP', class: 'text-teal-300' }
				: data.monitor.current_status === 'down'
					? { text: 'DOWN', class: 'text-red-400' }
					: { text: 'PENDING', class: 'text-neutral-400' }
	);
	const tiles = $derived([
		{ label: 'Uptime, 24 hours', value: formatUptime(data.stats?.uptime_24h) },
		{ label: 'Uptime, 7 days', value: formatUptime(data.stats?.uptime_7d) },
		{ label: 'Uptime, 30 days', value: formatUptime(data.stats?.uptime_30d) },
		{ label: 'Avg response, 24 hours', value: formatMs(data.stats?.avg_response_ms_24h) }
	]);

	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div>
				<p class="text-3xl font-semibold tracking-tight {headline.class}">{headline.text}</p>
				<p class="mt-1 text-sm text-neutral-500">
					since {formatUtc(since)} ({timeAgo(since, now)})
				</p>
				<h2 class="mt-4 text-base font-semibold text-neutral-100">{data.monitor.name}</h2>
				<p class="font-mono text-xs break-all text-neutral-400">{monitorTarget(data.monitor)}</p>
				<p class="mt-1 text-xs text-neutral-500">
					Checked every {data.monitor.interval_seconds}s · last check {timeAgo(data.monitor.last_checked_at, now)}
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<a href="{base}/edit" class={button}>Edit</a>
				{#if data.monitor.paused}
					<form method="POST" action="?/resume" use:enhance>
						<button type="submit" class={button}>Resume</button>
					</form>
				{:else}
					<form method="POST" action="?/pause" use:enhance>
						<button type="submit" class={button}>Pause</button>
					</form>
				{/if}
				<form
					method="POST"
					action="?/delete"
					use:enhance={({ cancel }) => {
						if (!confirm(`Delete monitor "${data.monitor.name}"? Its incidents are deleted too.`)) cancel();
					}}
				>
					<button
						type="submit"
						class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
					>
						Delete
					</button>
				</form>
			</div>
		</div>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}
	</div>

	<div class="grid grid-cols-2 gap-4 lg:grid-cols-4">
		{#each tiles as tile (tile.label)}
			<div class="rounded-xl border border-[#262626] bg-[#141414] p-4">
				<p class="text-xs text-neutral-500">{tile.label}</p>
				<p class="mt-1 text-xl font-semibold text-neutral-100">{tile.value}</p>
			</div>
		{/each}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Response time, last 24 hours</h3>
		<div class="mt-4">
			{#if data.stats}
				<ResponseTimeChart series={data.stats.series} />
			{:else}
				<p class="text-sm text-neutral-500">Check history is unavailable right now.</p>
			{/if}
		</div>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<h3 class="border-b border-[#262626] px-6 py-4 text-sm font-semibold text-neutral-200">Recent checks</h3>
		{#if data.checks.length === 0}
			<p class="px-6 py-6 text-sm text-neutral-500">No checks yet.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full text-sm">
					<thead class="text-left text-xs text-neutral-500">
						<tr class="border-b border-[#262626]">
							<th class="px-6 py-2 font-medium">Time</th>
							<th class="px-6 py-2 font-medium">Result</th>
							<th class="px-6 py-2 font-medium">Status code</th>
							<th class="px-6 py-2 font-medium">Response</th>
							<th class="px-6 py-2 font-medium">Error</th>
						</tr>
					</thead>
					<tbody class="tabular-nums">
						{#each data.checks as check (check.checked_at)}
							<tr class="border-b border-[#262626] last:border-0">
								<td class="px-6 py-2 whitespace-nowrap text-neutral-400">{formatUtc(check.checked_at)}</td>
								<td class="px-6 py-2"><StatusBadge status={check.status} /></td>
								<td class="px-6 py-2 text-neutral-300">{check.status_code ?? '—'}</td>
								<td class="px-6 py-2 text-neutral-300">{formatMs(check.response_time_ms)}</td>
								<td class="px-6 py-2 text-neutral-400">{check.error ?? ''}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<h3 class="border-b border-[#262626] px-6 py-4 text-sm font-semibold text-neutral-200">Incidents</h3>
		{#if data.incidents.length === 0}
			<p class="px-6 py-6 text-sm text-neutral-500">No incidents.</p>
		{:else}
			<ul>
				{#each data.incidents as incident (incident.id)}
					<li class="flex flex-wrap items-baseline justify-between gap-2 border-b border-[#262626] px-6 py-3 last:border-0">
						<div>
							<p class="text-sm text-neutral-200">{incident.cause ?? incident.title}</p>
							<p class="text-xs text-neutral-500">Started {formatUtc(incident.started_at)}</p>
						</div>
						<p class="text-xs {incident.status === 'open' ? 'text-red-400' : 'text-neutral-500'}">
							{incident.status === 'open' ? 'Ongoing' : 'Resolved'} · {formatDuration(
								incident.started_at,
								incident.resolved_at,
								now
							)}
						</p>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>
```

- [ ] **Step 10: Type-check and unit-test**

Run: `cd nami-ui && npm run check && npm test`
Expected: `svelte-check` reports `0 ERRORS 0 WARNINGS`; vitest passes (26 tests).

- [ ] **Step 11: Live walkthrough against the real stack**

```bash
cd nami-engine && docker compose up -d postgres mailpit clickhouse
DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo run      # worker enabled by default
cd ../nami-ui && npm run dev
```

In a browser at http://localhost:5173, logged in as a global admin (the first registered user):
1. Open a project (create one under Projects if needed): `/projects/<id>` lands on the Monitors tab; the Settings tab still adds/removes members exactly as before; an unknown project id shows the 403 page.
2. New monitor → HTTP status, URL `https://example.com`, interval 30. You land on its detail page: PENDING, then (refresh, or wait for the 30 s auto-refresh) UP with a response time, uptime tiles at 100.00% and the chart's end dot.
3. New monitor → Keyword on page, URL `https://example.com`, keyword `Example Domain` → UP. First submit it with URL `ftp://example.com/`: the engine's `url must be a valid http(s) URL` appears and the typed values (and the Keyword fields) are kept.
4. Edit the keyword monitor: keyword `NotOnThePage`, failures before alerting `1` → Save. Within one tick it shows DOWN, the Incidents card lists `keyword "NotOnThePage" not found`, and Mailpit (http://localhost:8025) holds `[Nami] <name> is DOWN` with a link to the detail page. Set the keyword back → UP, a `is back UP` email, the incident shows Resolved with a duration.
5. Hover the chart: the crosshair snaps to half hours and the tooltip shows average response, uptime and the UTC range; Tab to the chart and use the arrow keys for the same readout; "Show data table" lists 48 rows.
6. Pause → the headline shows PAUSED and the button becomes Resume (no reload); Resume; Delete → confirm dialog → back on the list without it.
7. Narrow the window to phone width: the chart axis shows two time labels plus "now" without overlap; tables scroll inside their cards.

Delete the walkthrough monitors afterwards.

- [ ] **Step 12: Commit**

```bash
git add nami-ui/src
git commit -F - <<'EOF'
feat(ui): add project section with monitor list, detail, chart and forms (NAMI-UP-7)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 8 (NAMI-UP-8): UI — heartbeats + incidents

Branch `feat/NAMI-UP-8-ui-heartbeats-incidents`, worktree `.worktrees/NAMI-UP-8/` (run `npm install` in its `nami-ui/`).

**Files:**
- Modify: `nami-ui/.env.example`, `nami-ui/src/lib/uptime.ts`, `nami-ui/src/lib/uptime.test.ts`, `nami-ui/src/lib/server/engine.ts`, `nami-ui/src/lib/server/engine.test.ts`, `nami-ui/src/routes/(app)/projects/[id]/+layout.svelte`
- Create: `nami-ui/src/lib/server/heartbeat-form.ts`, `nami-ui/src/lib/server/heartbeat-form.test.ts`, `nami-ui/src/lib/components/CopyButton.svelte`
- Create (under `nami-ui/src/routes/(app)/projects/[id]/`): `heartbeats/+page.server.ts`, `heartbeats/+page.svelte`, `heartbeats/[heartbeatId]/+page.server.ts`, `heartbeats/[heartbeatId]/+page.svelte`, `incidents/+page.server.ts`, `incidents/+page.svelte`

**Interfaces:**
- Consumes (engine, Tasks 5-6): `GET|POST /projects/{id}/heartbeats`, `GET|PATCH|DELETE /projects/{id}/heartbeats/{heartbeat_id}` (heartbeat JSON includes `token`), `GET /projects/{id}/incidents?status=open|resolved`; from Task 7: `callEngine`, `engineErrorMessage`, `formText`, `formWholeNumber`, `FormResult`, `StatusBadge`, `formatUtc`, `formatDuration`, `timeAgo`, `Incident`, `Status`, the project layout data `project`.
- Produces: `#lib/uptime` additions `type Heartbeat`, `pingUrl(engineUrl, token)`, `formatSeconds(total)`; `#lib/server/engine` `publicEngineUrl(): string` (env `PUBLIC_ENGINE_URL`, default `http://localhost:8080`, trailing slash trimmed); `#lib/server/heartbeat-form` `heartbeatPayloadFromForm(form): FormResult`; component `CopyButton { value: string; label?: string }`; pages `/projects/[id]/heartbeats`, `/projects/[id]/heartbeats/[heartbeatId]`, `/projects/[id]/incidents`; Heartbeats and Incidents tabs in the project sub-nav.

The ping URL shown in the UI points at the engine itself, because cron jobs ping it directly (they are not browser sessions and never pass through the BFF); `PUBLIC_ENGINE_URL` is that address as your servers reach it and may differ from the internal `ENGINE_URL`.

- [ ] **Step 1: Write the failing unit tests**

In `nami-ui/src/lib/uptime.test.ts`, add `formatSeconds,` after `formatMs,` and `pingUrl,` after `niceTicks,` in the import list, and append:

```ts

describe('pingUrl', () => {
	it('joins the engine URL and token without a double slash', () => {
		expect(pingUrl('https://engine.example.com/', 'abc')).toBe('https://engine.example.com/api/v1/heartbeat/abc');
		expect(pingUrl('http://localhost:8080', 'abc')).toBe('http://localhost:8080/api/v1/heartbeat/abc');
	});
});

describe('formatSeconds', () => {
	it('shows at most two units', () => {
		expect(formatSeconds(86_400)).toBe('1d');
		expect(formatSeconds(5_400)).toBe('1h 30m');
		expect(formatSeconds(90)).toBe('1m 30s');
		expect(formatSeconds(90_061)).toBe('1d 1h');
		expect(formatSeconds(0)).toBe('0s');
	});
});
```

In `nami-ui/src/lib/server/engine.test.ts`, change the two import lines to

```ts
import { afterEach, describe, it, expect, vi } from 'vitest';
import { callEngine, engineErrorMessage, publicEngineUrl } from './engine';
```

and append:

```ts

describe('publicEngineUrl', () => {
	const original = process.env.PUBLIC_ENGINE_URL;
	afterEach(() => {
		if (original === undefined) delete process.env.PUBLIC_ENGINE_URL;
		else process.env.PUBLIC_ENGINE_URL = original;
	});

	it('defaults to the local engine', () => {
		delete process.env.PUBLIC_ENGINE_URL;
		expect(publicEngineUrl()).toBe('http://localhost:8080');
	});

	it('uses PUBLIC_ENGINE_URL without a trailing slash', () => {
		process.env.PUBLIC_ENGINE_URL = 'https://status-engine.example.com/';
		expect(publicEngineUrl()).toBe('https://status-engine.example.com');
	});
});
```

`nami-ui/src/lib/server/heartbeat-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { heartbeatPayloadFromForm } from './heartbeat-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

describe('heartbeatPayloadFromForm', () => {
	it('parses name, period and grace', () => {
		expect(heartbeatPayloadFromForm(form({ name: ' Backup ', period_seconds: '86400', grace_seconds: '600' }))).toEqual({
			ok: true,
			payload: { name: 'Backup', period_seconds: 86400, grace_seconds: 600 }
		});
	});

	it('omits a blank grace so the engine default applies', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '60', grace_seconds: '' }))).toEqual({
			ok: true,
			payload: { name: 'Backup', period_seconds: 60 }
		});
	});

	it('rejects non-whole numbers', () => {
		expect(heartbeatPayloadFromForm(form({ name: 'Backup', period_seconds: '1h' }))).toEqual({
			ok: false,
			message: 'period_seconds must be a whole number'
		});
	});
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd nami-ui && npm test`
Expected: FAIL — `pingUrl`/`formatSeconds`/`publicEngineUrl` are not exported and `./heartbeat-form` does not resolve.

- [ ] **Step 3: Implement the helpers and the env var**

In `nami-ui/src/lib/uptime.ts`, insert after the `Incident` type (before the `formatUptime` doc comment):

```ts
export type Heartbeat = {
	id: string;
	project_id: string;
	name: string;
	token: string;
	period_seconds: number;
	grace_seconds: number;
	paused: boolean;
	current_status: Status;
	last_ping_at: string | null;
	created_at: string;
};

/** The URL a cron job pings: straight to nami-engine, not through this BFF. */
export function pingUrl(engineUrl: string, token: string): string {
	return `${engineUrl.replace(/\/+$/, '')}/api/v1/heartbeat/${token}`;
}

/** Compact period: 86400 -> "1d", 5400 -> "1h 30m", 90 -> "1m 30s". */
export function formatSeconds(total: number): string {
	const parts: string[] = [];
	let rest = Math.max(0, Math.floor(total));
	for (const [unit, size] of [
		['d', 86_400],
		['h', 3_600],
		['m', 60],
		['s', 1]
	] as const) {
		const amount = Math.floor(rest / size);
		rest -= amount * size;
		if (amount > 0) parts.push(`${amount}${unit}`);
	}
	return parts.length > 0 ? parts.slice(0, 2).join(' ') : '0s';
}

```

Append to `nami-ui/src/lib/server/engine.ts`:

```ts

/**
 * nami-engine's URL as your cron jobs reach it, for heartbeat ping URLs.
 * Pings go straight to the engine (cron jobs are not browser sessions), so
 * this can differ from the internal ENGINE_URL this BFF calls.
 */
export function publicEngineUrl(): string {
	return (process.env.PUBLIC_ENGINE_URL ?? 'http://localhost:8080').replace(/\/+$/, '');
}
```

`nami-ui/src/lib/server/heartbeat-form.ts`:

```ts
import { formText, formWholeNumber, type FormResult } from './monitor-form';

/** Heartbeat form -> nami-engine JSON body (name, period, grace). */
export function heartbeatPayloadFromForm(form: FormData): FormResult {
	const payload: Record<string, unknown> = { name: formText(form, 'name') };
	for (const field of ['period_seconds', 'grace_seconds']) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	return { ok: true, payload };
}
```

Append to `nami-ui/.env.example`:

```dotenv
# nami-engine's URL as your cron jobs reach it — shown in heartbeat ping
# URLs. Pings go straight to the engine, not through nami-ui.
PUBLIC_ENGINE_URL=http://localhost:8080
```

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cd nami-ui && npm test`
Expected: PASS — 4 files, 33 tests.

- [ ] **Step 5: Write the copy button**

`nami-ui/src/lib/components/CopyButton.svelte` (the code blocks it sits beside are also `select-all`, so copying works where the Clipboard API is unavailable):

```svelte
<script lang="ts">
	let { value, label = 'Copy' }: { value: string; label?: string } = $props();

	let copied = $state(false);

	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			// Clipboard API unavailable (e.g. plain-HTTP origin): the code
			// block next to this button stays click-to-select.
			copied = false;
		}
	}
</script>

<button
	type="button"
	onclick={copy}
	class="shrink-0 rounded-md border border-[#262626] px-2 py-1 text-xs text-neutral-400 transition-colors hover:border-teal-500/30 hover:text-neutral-100"
>
	{copied ? 'Copied' : label}
</button>
```

- [ ] **Step 6: Write the heartbeats list + create page**

`nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.server.ts`:

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, publicEngineUrl } from '#lib/server/engine';
import { heartbeatPayloadFromForm } from '#lib/server/heartbeat-form';
import type { Heartbeat } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/heartbeats`);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load heartbeats.')
		);
	}
	const heartbeats: Heartbeat[] = await response.json();
	return { heartbeats, engineUrl: publicEngineUrl() };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = heartbeatPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/heartbeats`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not create heartbeat.') });
		}
		const created: { id: string } = await response.json();
		throw redirect(303, `/projects/${event.params.id}/heartbeats/${created.id}`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { formatSeconds, pingUrl, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="border-b border-[#262626] px-6 py-4">
			<h2 class="text-base font-semibold text-neutral-100">Heartbeats</h2>
			<p class="text-xs text-neutral-500">
				Cron jobs ping their URL after each run; a heartbeat goes down when no ping arrives within its period plus
				grace.
			</p>
		</div>
		{#if data.heartbeats.length === 0}
			<p class="px-6 py-10 text-center text-sm text-neutral-500">No heartbeats yet — create one below.</p>
		{:else}
			<ul>
				{#each data.heartbeats as heartbeat (heartbeat.id)}
					{@const url = pingUrl(data.engineUrl, heartbeat.token)}
					<li class="space-y-2 border-b border-[#262626] px-6 py-4 last:border-0">
						<div class="flex flex-wrap items-center justify-between gap-2">
							<div class="flex items-center gap-4">
								<StatusBadge status={heartbeat.current_status} paused={heartbeat.paused} />
								<a
									href="/projects/{data.project.id}/heartbeats/{heartbeat.id}"
									class="font-medium text-neutral-100 hover:text-teal-300">{heartbeat.name}</a
								>
							</div>
							<p class="text-xs text-neutral-500">
								every {formatSeconds(heartbeat.period_seconds)} · grace {formatSeconds(heartbeat.grace_seconds)} ·
								last ping {timeAgo(heartbeat.last_ping_at, now)}
							</p>
						</div>
						<div class="flex items-center gap-2">
							<code
								class="block flex-1 truncate rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-1.5 font-mono text-xs text-neutral-400 select-all"
								>{url}</code
							>
							<CopyButton value={url} />
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-base font-semibold text-neutral-100">New heartbeat</h2>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}
		<form method="POST" use:enhance class="mt-4 grid gap-4 sm:grid-cols-[1fr_10rem_10rem_auto] sm:items-end">
			<div>
				<label for="hb-name" class={label}>Name</label>
				<input id="hb-name" name="name" type="text" required maxlength="200" placeholder="Nightly backup" class={input} />
			</div>
			<div>
				<label for="hb-period" class={label}>Period (seconds)</label>
				<input id="hb-period" name="period_seconds" type="number" required min="30" value="86400" class={input} />
			</div>
			<div>
				<label for="hb-grace" class={label}>Grace (seconds)</label>
				<input id="hb-grace" name="grace_seconds" type="number" min="0" value="60" class={input} />
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
			>
				Create
			</button>
		</form>
	</div>
</div>
```

- [ ] **Step 7: Write the heartbeat detail page**

`nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.server.ts` (named actions only: `update`, `pause`, `resume`, `delete`):

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, publicEngineUrl } from '#lib/server/engine';
import { heartbeatPayloadFromForm } from '#lib/server/heartbeat-form';
import type { Heartbeat } from '#lib/uptime';
import type { Actions, PageServerLoad, RequestEvent } from './$types';

function heartbeatPath(params: { id: string; heartbeatId: string }): string {
	return `/projects/${params.id}/heartbeats/${params.heartbeatId}`;
}

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, heartbeatPath(event.params));
	if (response.status === 404) throw error(404, 'Heartbeat not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load heartbeat.'));
	}
	const heartbeat: Heartbeat = await response.json();
	return { heartbeat, engineUrl: publicEngineUrl() };
};

async function patch(event: RequestEvent, body: Record<string, unknown>) {
	const response = await callEngine(event, heartbeatPath(event.params), { method: 'PATCH', body: JSON.stringify(body) });
	if (!response.ok) {
		return fail(response.status, { message: await engineErrorMessage(response, 'Could not update heartbeat.') });
	}
	return { saved: true };
}

export const actions: Actions = {
	update: async (event) => {
		const parsed = heartbeatPayloadFromForm(await event.request.formData());
		if (!parsed.ok) return fail(400, { message: parsed.message });
		return patch(event, parsed.payload);
	},
	pause: (event) => patch(event, { paused: true }),
	resume: (event) => patch(event, { paused: false }),
	delete: async (event) => {
		const response = await callEngine(event, heartbeatPath(event.params), { method: 'DELETE' });
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not delete heartbeat.') });
		}
		throw redirect(303, `/projects/${event.params.id}/heartbeats`);
	}
};
```

`nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.svelte` (the settings form passes `update({ reset: false })` so the inputs keep showing what was just saved):

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import { refreshAll } from '$app/navigation';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { formatSeconds, formatUtc, pingUrl, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const url = $derived(pingUrl(data.engineUrl, data.heartbeat.token));
	const curl = $derived(`curl -fsS -m 10 --retry 3 ${url}`);
	const cron = $derived(`0 3 * * * /usr/local/bin/backup.sh && ${curl}`);

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const button =
		'rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100';
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<div class="flex flex-wrap items-start justify-between gap-4">
			<div class="space-y-2">
				<StatusBadge status={data.heartbeat.current_status} paused={data.heartbeat.paused} />
				<h2 class="text-base font-semibold text-neutral-100">{data.heartbeat.name}</h2>
				<p class="text-sm text-neutral-500">
					Last ping:
					{#if data.heartbeat.last_ping_at}
						{formatUtc(data.heartbeat.last_ping_at)} ({timeAgo(data.heartbeat.last_ping_at, now)})
					{:else}
						never
					{/if}
				</p>
				<p class="text-sm text-neutral-500">
					Expected every {formatSeconds(data.heartbeat.period_seconds)}, with {formatSeconds(data.heartbeat.grace_seconds)} grace.
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				{#if data.heartbeat.paused}
					<form method="POST" action="?/resume" use:enhance><button type="submit" class={button}>Resume</button></form>
				{:else}
					<form method="POST" action="?/pause" use:enhance><button type="submit" class={button}>Pause</button></form>
				{/if}
				<form
					method="POST"
					action="?/delete"
					use:enhance={({ cancel }) => {
						if (!confirm(`Delete heartbeat "${data.heartbeat.name}"? Its incidents are deleted too.`)) cancel();
					}}
				>
					<button
						type="submit"
						class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
					>
						Delete
					</button>
				</form>
			</div>
		</div>
		{#if form?.message}
			<div class="mt-4 rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
				{form.message}
			</div>
		{/if}
	</div>

	<div class="space-y-3 rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Ping URL</h3>
		<p class="text-sm text-neutral-500">
			Request it (GET or POST, no login needed) at the end of every successful run.
		</p>
		<div class="flex items-center gap-2">
			<code class="block flex-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs break-all text-neutral-300 select-all">{url}</code>
			<CopyButton value={url} />
		</div>
		<p class="pt-2 text-sm text-neutral-500">Example crontab entry:</p>
		<div class="flex items-center gap-2">
			<code class="block flex-1 rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 font-mono text-xs break-all text-neutral-300 select-all">{cron}</code>
			<CopyButton value={cron} />
		</div>
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h3 class="text-sm font-semibold text-neutral-200">Settings</h3>
		{#if form?.saved}
			<div class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">Saved.</div>
		{/if}
		<!-- reset: false keeps the inputs showing what was just saved. -->
		<form
			method="POST"
			action="?/update"
			use:enhance={() =>
				async ({ update }) =>
					update({ reset: false })}
			class="mt-4 grid gap-4 sm:grid-cols-[1fr_10rem_10rem_auto] sm:items-end"
		>
			<div>
				<label for="hb-name" class={label}>Name</label>
				<input id="hb-name" name="name" type="text" required maxlength="200" value={data.heartbeat.name} class={input} />
			</div>
			<div>
				<label for="hb-period" class={label}>Period (seconds)</label>
				<input id="hb-period" name="period_seconds" type="number" required min="30" value={data.heartbeat.period_seconds} class={input} />
			</div>
			<div>
				<label for="hb-grace" class={label}>Grace (seconds)</label>
				<input id="hb-grace" name="grace_seconds" type="number" required min="0" value={data.heartbeat.grace_seconds} class={input} />
			</div>
			<button
				type="submit"
				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
			>
				Save
			</button>
		</form>
	</div>
</div>
```

- [ ] **Step 8: Write the incidents page**

`nami-ui/src/routes/(app)/projects/[id]/incidents/+page.server.ts` (only `open` or `resolved` is ever forwarded to the engine):

```ts
import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { Incident } from '#lib/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// Anything but "resolved" shows open incidents: the engine only accepts
	// open|resolved, so never forward the raw query value.
	const status = event.url.searchParams.get('status') === 'resolved' ? 'resolved' : 'open';
	const response = await callEngine(event, `/projects/${event.params.id}/incidents?status=${status}`);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load incidents.')
		);
	}
	const incidents: Incident[] = await response.json();
	return { incidents, status };
};
```

`nami-ui/src/routes/(app)/projects/[id]/incidents/+page.svelte`:

```svelte
<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import { formatDuration, formatUtc } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});

	const tabs = [
		{ status: 'open', label: 'Open' },
		{ status: 'resolved', label: 'Resolved' }
	] as const;

	function sourceHref(incident: (typeof data.incidents)[number]): string | null {
		if (incident.monitor_id) return `/projects/${data.project.id}/monitors/${incident.monitor_id}`;
		if (incident.heartbeat_id) return `/projects/${data.project.id}/heartbeats/${incident.heartbeat_id}`;
		return null;
	}
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414]">
	<div class="flex items-center justify-between border-b border-[#262626] px-6 py-4">
		<h2 class="text-base font-semibold text-neutral-100">Incidents</h2>
		<div class="flex gap-1 rounded-md border border-[#262626] bg-[#0a0a0a] p-0.5">
			{#each tabs as tab (tab.status)}
				<a
					href="?status={tab.status}"
					aria-current={data.status === tab.status ? 'page' : undefined}
					class="rounded px-3 py-1 text-sm transition-colors {data.status === tab.status
						? 'bg-[#262626] text-neutral-100'
						: 'text-neutral-500 hover:text-neutral-300'}">{tab.label}</a
				>
			{/each}
		</div>
	</div>

	{#if data.incidents.length === 0}
		<p class="px-6 py-10 text-center text-sm text-neutral-500">
			{data.status === 'open' ? 'No open incidents — everything is up.' : 'No resolved incidents yet.'}
		</p>
	{:else}
		<ul>
			{#each data.incidents as incident (incident.id)}
				{@const href = sourceHref(incident)}
				<li class="flex flex-wrap items-start justify-between gap-3 border-b border-[#262626] px-6 py-4 last:border-0">
					<div class="space-y-1">
						<p class="text-sm font-medium text-neutral-100">
							{#if href}
								<a {href} class="hover:text-teal-300">{incident.title}</a>
							{:else}
								{incident.title}
							{/if}
						</p>
						{#if incident.cause}
							<p class="text-sm text-neutral-400">{incident.cause}</p>
						{/if}
						<p class="text-xs text-neutral-500">
							Started {formatUtc(incident.started_at)}
							{#if incident.resolved_at}· resolved {formatUtc(incident.resolved_at)}{/if}
						</p>
					</div>
					<span
						class="rounded-full border px-2 py-0.5 text-xs {incident.status === 'open'
							? 'border-red-500/30 bg-red-500/10 text-red-400'
							: 'border-[#262626] text-neutral-400'}"
					>
						{incident.status === 'open' ? 'Ongoing' : 'Resolved'} · {formatDuration(
							incident.started_at,
							incident.resolved_at,
							now
						)}
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
```

- [ ] **Step 9: Add the Heartbeats and Incidents tabs**

In `nami-ui/src/routes/(app)/projects/[id]/+layout.svelte`, make the `tabs` array:

```ts
	const tabs = $derived([
		{ href: `/projects/${data.project.id}/monitors`, label: 'Monitors' },
		{ href: `/projects/${data.project.id}/heartbeats`, label: 'Heartbeats' },
		{ href: `/projects/${data.project.id}/incidents`, label: 'Incidents' },
		{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
	]);
```

- [ ] **Step 10: Type-check and unit-test**

Run: `cd nami-ui && npm run check && npm test`
Expected: `svelte-check` reports `0 ERRORS 0 WARNINGS`; vitest passes (33 tests).

- [ ] **Step 11: Live walkthrough against the real stack**

Start the engine and `npm run dev` as in Task 7, then in the browser:
1. Heartbeats tab → create "Nightly backup", period 30, grace 0. You land on its detail page: Pending, "Last ping: never", the ping URL `http://localhost:8080/api/v1/heartbeat/<64 hex chars>` and a crontab example, each with a working Copy button.
2. Run the shown command in a terminal: `curl -fsS -m 10 --retry 3 <ping URL>` → `{"ok":true}`; the page (after refresh or the 30 s auto-refresh) shows Up and the last ping time. `curl http://localhost:8080/api/v1/heartbeat/nope` → `{"error":"not found"}`.
3. Stop pinging for ~40 s: the heartbeat goes Down, Mailpit has `[Nami] Nightly backup is DOWN` linking to the heartbeat page, and the Incidents tab (Open) lists "Nightly backup missed its heartbeat" with an ongoing duration.
4. Ping again → Up, a `is back UP` email, and the incident moves to the Resolved tab with its duration.
5. Settings card: change grace to 120 → Save → "Saved." and the inputs keep the new values; Pause → the status shows Paused, a ping still updates "Last ping" but never changes the status; Resume; Delete → confirm → back on the list.
6. The Monitors/Heartbeats/Incidents/Settings tabs highlight the active section.

Delete the walkthrough heartbeat afterwards.

- [ ] **Step 12: Commit**

```bash
git add nami-ui/.env.example nami-ui/src
git commit -F - <<'EOF'
feat(ui): add heartbeat and incident pages (NAMI-UP-8)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```
