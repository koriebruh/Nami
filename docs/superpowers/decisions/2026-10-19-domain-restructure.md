# Domain restructure (2026-10-19) — path map and conventions

On 2026-10-19 the owner raised three problems: PRs were too small and too many, migrations were one file per change, and the folder layout was messy. Decisions: squash the migrations into domain baselines and reset the dev databases; restructure now, under a feature freeze; ship one PR per sub-project phase. This was done in one PR (branch `refactor/domain-structure`) as a pure move: no behaviour, SQL or JSON change. Test counts are identical before and after (engine 1161, UI vitest 526) and `pg_dump --schema-only` of the old and new migrations is identical.

Use this file to translate paths in plans written before the restructure.

## Engine modules (`nami-engine/src/`)

`models/` and `routes/` are gone. Each domain owns its models and routes; `core/router.rs` mounts every domain's routes. A file whose name became a directory became that directory's `mod.rs` (`uptime/checker.rs`, `uptime/heartbeat.rs`, `logs/ingest.rs`, `logs/query.rs`), so `crate::uptime::checker::perform_check`, `crate::uptime::heartbeat::record_ping`, `crate::logs::ingest::prepare` and `crate::logs::query::Sql` keep their paths.

| Old | New |
|---|---|
| `config.rs`, `crypto.rs`, `db.rs`, `clickhouse.rs`, `email.rs`, `error.rs`, `serve.rs`, `state.rs`, `download.rs`, `router.rs` | `core/<same name>.rs` |
| `middleware/{mod,rbac}.rs` | `core/middleware/{mod,rbac}.rs` |
| `routes/health.rs` | `core/health.rs` |
| `models/mod.rs` (`nullable`, `validate_name`, `validate_label`, `with_unchanged_values`, `has_invisible_or_bidi`, `is_invisible_or_bidi_char`) | `core/validate.rs` |
| `models/mod.rs` (`ProjectQuota`) | `uptime/quota.rs` |
| `auth/{oauth,password,token,totp}.rs` | unchanged |
| `routes/auth.rs` | `auth/routes.rs` |
| `models/user.rs` | `auth/user.rs` |
| `models/token.rs` (stored verification/reset tokens) | `auth/token_store.rs` |
| `models/project.rs` | `projects/model.rs` |
| `routes/projects.rs` | `projects/routes.rs` |
| `models/monitor.rs` | `uptime/monitor/model.rs` |
| `models/monitor_request.rs` | `uptime/monitor/request.rs` |
| `models/target.rs` | `uptime/monitor/target.rs` |
| `routes/monitors.rs` | `uptime/monitor/routes.rs` |
| `uptime/heartbeat.rs` | `uptime/heartbeat/mod.rs` |
| `models/heartbeat.rs` | `uptime/heartbeat/model.rs` |
| `routes/heartbeats.rs` | `uptime/heartbeat/routes.rs` |
| `uptime/pings.rs` | `uptime/heartbeat/pings.rs` |
| `models/monitor_group.rs` | `uptime/group/model.rs` |
| `routes/monitor_groups.rs` | `uptime/group/routes.rs` |
| `models/maintenance.rs` | `uptime/maintenance/model.rs` |
| `routes/maintenance.rs` | `uptime/maintenance/routes.rs` |
| `uptime/checker.rs` | `uptime/checker/mod.rs` |
| `uptime/{dns,ping,tls,expiry,mail,udp}.rs` | `uptime/checker/<same name>.rs` |
| `uptime/{worker,store,availability,export,alerts,alert_throttle}.rs`, `uptime/import/` | unchanged |
| `models/incident.rs` | `incidents/model.rs` |
| `routes/incidents.rs` | `incidents/routes.rs` |
| `models/source.rs` | `logs/source/model.rs` |
| `routes/sources.rs` | `logs/source/routes.rs` |
| `models/log_view.rs` | `logs/view/model.rs` |
| `routes/log_views.rs` | `logs/view/routes.rs` |
| `logs/saved_query.rs` | `logs/view/saved_query.rs` |
| `logs/ingest.rs` | `logs/ingest/mod.rs` |
| `logs/{normalize,msgpack,store,pipeline,pipeline_cache,rate_limit,budget,usage}.rs` | `logs/ingest/<same name>.rs` |
| `routes/logs_ingest.rs` | `logs/ingest/routes.rs` |
| `logs/query.rs` | `logs/query/mod.rs` |
| `logs/{search,tail,export,pattern,patterns,slots}.rs` | `logs/query/<same name>.rs` |
| `routes/logs.rs` | `logs/query/routes.rs` |
| `logs/syslog/` | unchanged |

Module paths follow the files: `crate::models::monitor::Monitor` is `crate::uptime::monitor::model::Monitor`, `crate::routes::heartbeats::MAX_PINGS_IN_FLIGHT` is `crate::uptime::heartbeat::routes::MAX_PINGS_IN_FLIGHT`, `crate::logs::store` is `crate::logs::ingest::store`, `crate::state::AppState` is `crate::core::state::AppState`. Modules stay `pub` (the integration tests import them); `uptime::quota` is `pub(crate)`.

## Engine tests (`nami-engine/tests/`)

55 test binaries became one target per domain. Each `tests/<domain>/main.rs` mounts `tests/common/` with `#[path = "../common/mod.rs"] mod common;` and declares one `mod <file>;` per test file; inside a test file, `mod common;` became `use crate::common;`. Fixtures stay in `tests/fixtures/` (read relative to the crate root). Run one domain with `cargo test --test uptime`, one file with `cargo test --test uptime monitors_api::`.

| Target | Test files (old `tests/<file>.rs`) |
|---|---|
| `tests/core/main.rs` | clickhouse, crypto, db_pool, email, health, json_bodies, mailer, migrations, startup |
| `tests/auth/main.rs` | login, oauth, password_reset, rate_limit, register, totp |
| `tests/projects/main.rs` | rbac |
| `tests/uptime/main.rs` | availability, checker_advanced_options, checker_expiry, checker_http_config, checker_mail_udp, checker_ping_dns, checks_export, heartbeat_pings, heartbeats, maintenance_windows, monitor_advanced_options, monitor_groups, monitor_import, monitor_request_config, monitor_secret_retarget, monitor_types_api, monitors_api, recovery_threshold, uptime_checker, uptime_read_api, uptime_worker, uptime_worker_timing |
| `tests/incidents/main.rs` | incidents |
| `tests/logs/main.rs` | log_views, logs_export, logs_ingest, logs_patterns, logs_pipeline_api, logs_pipeline_ingest, logs_quota, logs_search, logs_store, logs_syslog, logs_tail, sources_api |
| standalone, unchanged | `clickhouse_proxy.rs`, `uptime_checker_proxy.rs` (set process-wide proxy variables), `logs_decode_memory.rs` (installs a counting global allocator) |

A test that changes process-wide state (environment variables, the global allocator) must stay a standalone `tests/<name>.rs` binary; everything else goes into its domain target.

## UI (`nami-ui/src/lib/`)

`components/` (with `components/ui/`, the design system), `assets/` and `index.ts` are unchanged.

| Old | New |
|---|---|
| `uuid.ts` (+ test) | `core/uuid.ts` |
| `uptime.ts`, `availability.ts`, `maintenance.ts` (+ tests) | `uptime/<same name>.ts` |
| `logs.ts` (+ test) | `logs/logs.ts` |
| `server/engine.ts` (+ test), `server/relay-stream.ts` (+ test), `server/fake-event.ts` | `server/core/<same name>.ts` |
| `server/monitor-form.ts`, `server/heartbeat-form.ts`, `server/maintenance-form.ts`, `server/import-form.ts` (+ tests), `server/availability.ts` | `server/uptime/<same name>.ts` |
| `server/log-proxy.ts`, `server/source-form.ts` (+ tests) | `server/logs/<same name>.ts` |

Imports follow: `#lib/uuid` is `#lib/core/uuid`, `#lib/uptime` is `#lib/uptime/uptime`, `#lib/server/engine` is `#lib/server/core/engine`, `#lib/server/monitor-form` is `#lib/server/uptime/monitor-form`, `#lib/logs` is `#lib/logs/logs`.

## Migrations

The 26 files in `nami-engine/migrations/` were replaced by five domain baselines, in foreign-key order. Column order inside each table is the order the old migrations created and added the columns in.

| New baseline | Old files folded in |
|---|---|
| `0001_auth.sql` | 0001_create_users, 0002_create_oauth_identities, 0005_create_tokens, 0006_add_password_changed_at, 0008_add_token_hash_indexes |
| `0002_projects.sql` | 0003_create_projects, 0004_create_project_members, 0007_add_projects_created_by |
| `0003_uptime.sql` | 0009_create_monitors, 0011_create_heartbeats (heartbeats part), 0012_monitor_request_config, 0013_monitor_recovery_threshold, 0014_monitor_ping_dns, 0015_monitor_expiry_checks, 0016_monitor_mail_udp, 0017_create_maintenance_windows, 0018_create_monitor_groups, 0019_phase1_hardening (worker_state, heartbeats.outage_anchor_at), 0023_heartbeat_groups, 0024_monitor_advanced_request |
| `0004_incidents.sql` | 0010_create_incidents, 0011_create_heartbeats (incidents.heartbeat_id and its unique index), 0019_phase1_hardening (incident source indexes), 0022_incident_details |
| `0005_logs.sql` | 0040_create_sources, 0041_create_source_usage_daily, 0042_add_sources_pipeline, 0043_create_log_views |

Schema-equivalence proof: the old files applied to a scratch database and the baselines applied to another give the same `pg_dump --schema-only --no-owner --no-privileges` output, byte for byte apart from pg_dump's random `\restrict` token (15 tables; every column, type, default, constraint name and definition, index and foreign key).

The ClickHouse DDL list (`core/clickhouse.rs`, `MIGRATIONS`) is grouped by domain (uptime: `uptime_checks`, `heartbeat_pings`; logs: `logs`) with the statements unchanged. They are idempotent `CREATE TABLE IF NOT EXISTS` and independent of each other, so the order does not matter.

Existing databases cannot be migrated in place: their `_sqlx_migrations` rows record the old versions and checksums, so the engine refuses to start on them. Recreate each Postgres database (dev and test) after this change; ClickHouse needs nothing.

### Retired reserved ranges

The per-sub-project number ranges are retired and must not be used: 0012-0029 (Uptime Phase 2), 0020-0021 (Phase 2 fix wave), 0022-0039 (Uptime Phase 3), 0040-0059 (Logs), 0060-0099 (Incident management), 0100-0119 (Status pages), 0120-0139 (Metrics).

## Conventions from now on

- **Migrations:** number new files sequentially after the newest file in `nami-engine/migrations/` (the first is `0006_`), name them `NNNN_<domain>_<change>.sql` (e.g. `0006_logs_alerts.sql`), and add at most one file per domain per phase PR. A phase that changes the schema of several domains adds one file for each. Never edit a migration that has merged.
- **Engine layout:** new code goes into its domain (`core`, `auth`, `projects`, `uptime`, `incidents`, `logs`, and a new top-level domain module for each new sub-project: `statuspages`, `metrics`, ...). A domain's routes live in the domain and are mounted in `core/router.rs`. `core` holds infrastructure only, never domain logic.
- **Tests:** a new integration test file becomes a `mod` of its domain target (`tests/<domain>/main.rs`); a new domain gets a new target. Only a test that changes process-wide state gets a standalone binary.
- **UI:** shared, non-component code goes under `src/lib/<domain>/` and `src/lib/server/<domain>/`; components stay in `src/lib/components/`.
- **Workflow:** one PR per sub-project phase (see `CLAUDE.md`, "Git branching workflow").
