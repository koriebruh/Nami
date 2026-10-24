# Log Management — Part 2 Implementation Plan

> **Status:** done. NAMI-LOG-9..13 merged as PRs #56, #63, #67, #75, #76; NAMI-LOG-14..19 shipped as one phase PR. Decisions: `docs/superpowers/decisions/2026-10-25-log-management-part2.md`.
> **Branch:** `feat/logs-part2` in `.worktrees/logs-part2/`.
> **Depends on:** Logs Part 1 and Uptime Phases 1-3 (merged).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish BetterStack-style log management in Nami. This part adds per-source ingest pipeline rules (drop, remove fields, redact, rename, sample) with an editor and live preview, message patterns (`/logs/patterns` and an explorer Patterns tab), streamed CSV/NDJSON export capped at 100,000 rows, saved views, and log alerts (a threshold over a window opens an incident and sends notifications). It also adds a 30-day usage chart per source, live-tail resume across reconnects, and the explorer fixes that Part 1 deferred.

**Architecture:** The pipeline is a pure module (`logs::ingest::pipeline`). It compiles a source's rules once and applies them to a normalized `LogEvent`. It plugs into the one shared event→row step that the Part 1 fix wave extracted (`store::RowBatch::push`), so HTTP and syslog run the same order: parse → field mapping → caps → **pipeline** → pattern → row → encoded caps → quota accounting → insert. The compiled rules are cached per source and keyed on `sources.updated_at`. The token lookup already returns that value, so a cache hit costs no query. Patterns, export and alert counts reuse Part 1's parameter parser and `logs::query::Sql` builder (fixed fragments and numbered placeholders only) and the fix wave's read budgets. Export streams ClickHouse `JSONEachRow` line by line through a bounded channel, with caps on rows, bytes and line length. It never buffers a result. Saved views and log alerts live in Postgres and share one validated query shape (`logs::view::saved_query`). A worker loop claims due alerts with the monitors' `for update skip locked` pattern and reuses `alerts::notify_transition` and `uptime::worker::Notifications`. nami-ui proxies export and live tail as streams, and preview calls as small JSON POSTs, through allow-listing `+server.ts` endpoints.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, `regex` 1.13, `tokio-stream` 0.1). One crate becomes a direct dependency: `regex-syntax` (already in `Cargo.lock` as a dependency of `regex`; `cargo add regex-syntax@<locked version>` in Task 10). No other new crates. Postgres 17, ClickHouse 26.9, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-log-management-design.md` (including "Parity additions (2026-10-13)"). Part 1 plan: `docs/superpowers/plans/2026-10-13-log-management-part1.md`. Roadmap: `docs/superpowers/roadmap.md` §3. Part 1 rulings, limits and deferred items: `docs/superpowers/decisions/2026-10-13-log-management-part1.md` (the committed record; the scratch ledger under `.superpowers/sdd/` is local only and is not a source of truth).

**When to start:** now, with NAMI-LOG-14. Tasks 9-13 merged to `main` and are kept below as the record of what landed. The Logs Part 1 final-review fix wave merged as `94f9362` (PR #50) while this plan was being written, and the plan is aligned to that merged code. The table below lists what each task takes from it. Task 11 is the only task that depends on the shared event→row step.

**Interfaces assumed from the fix wave** (verify each against the branch before starting the task that uses it; adapt names minimally, keep the behaviour and the tests, and record the mapping as a "Drift" note in the task):

| Interface | Where (`main` `94f9362`) | Used by |
|---|---|---|
| `core::clickhouse::MAX_RESPONSE_BYTES` (32 MiB), `ClickHouse::with_max_response_bytes`, `ClickHouseError::ResponseTooLarge { limit }`, capped `read_capped` body reader | `src/core/clickhouse.rs` | T9, T12, T14 |
| `logs::query::search::READ_SETTINGS` (`max_result_bytes=33554432`, `result_overflow_mode=throw`); `search::user_error` mapping `TOO_MANY_ROWS_OR_BYTES` / `ResponseTooLarge` to 400 | `src/logs/query/search.rs` | T9, T12, T14 |
| `logs::query::search::{QueryPermit, QuerySlots, MAX_QUERIES_PER_USER = 4}`; `QueryPermit::acquire(&Arc<LogsState>, user_id) -> Result<QueryPermit, AppError>` (429 `RateLimited`). It currently also guards entry reads (`logs/query/routes.rs` `log_entry`), which Task 9 moves to a cap of their own (R-P2-23) | `src/logs/query/search.rs`, `LogsState.query_slots` | T9, T14 |
| `logs::query::search::{ENTRY_COLUMNS (cut list columns + `truncated`), FULL_COLUMNS, LogEntry.truncated, entry()}`, `GET /projects/{id}/logs/{log_id}?dt=<rfc3339>`; UI: `LogEntry.truncated?`, `entryHref`, the `logs/entry/[logId=uuid]/+server.ts` proxy, and expanding a cut row loads the whole row (`f3b358f`) | `src/logs/query/search.rs`, `src/logs/query/routes.rs`, `nami-ui/src/lib/logs/logs.ts`, explorer page | T9 (entry cap), T15, T17 (reused as is) |
| `Sql::list` made `pub` | `src/logs/query/mod.rs` | T9, T12 |
| `rate_limit::{ProjectLimits, MAX_IN_FLIGHT_PER_PROJECT = 8, MAX_DECODES_PER_PROJECT = 2}`, `LogsState.project_limits`; `ingest::prepare(.., shares, project_shares)` | `src/logs/ingest/rate_limit.rs`, `src/logs/ingest/mod.rs` | T11 (unchanged, only threaded through) |
| `config.max_sources_per_project` (`MAX_SOURCES_PER_PROJECT`, default 50), `source::create_source` → 409 | `src/core/config.rs`, `src/logs/source/model.rs` | T12 (source-name map size bound) |
| The shared event→row step (item 6): `store::RowBatch { with_capacity(capacity, max_stored_bytes), push(&mut self, event: LogEvent, source: &IngestSource) -> Result<(), StoredTooLarge>, rows(), into_rows(), len(), stored_bytes(), encode(max_bytes) -> Result<EncodedRows, ClickHouseError> }`, `store::EncodedRows { body, events, bytes, per_source: SourceCounts }`, `store::SourceCounts = HashMap<Uuid, (u64, u64)>`, `store::encode_run(rows, max_bytes)`, `store::chunk_rows(rows, max_bytes) -> Result<(Vec<Range<usize>>, usize), ClickHouseError>`. HTTP: `ingest::row_batch` (private) → `build_batch`/`build_rows_within`. Syslog: the batcher builds a `RowBatch` in `spawn_blocking`, then runs `chunk_rows` on the async task (T11 moves it into the blocking build, R-P2-24) and encodes each run in its own `spawn_blocking`. `RowBatch::push` is the documented pipeline hook point | `src/logs/ingest/store.rs`, `src/logs/ingest/mod.rs`, `src/logs/syslog/mod.rs` | T11 |
| `serve::{MAX_PUBLIC_CONNECTIONS = 512, MAX_API_CONNECTIONS = 512}` plus header-read deadlines on both listeners | `src/core/serve.rs` | final review (risk: the caps are global per listener; per-IP limits stay with Platform) |

Implementers still check each row against the branch before starting the task that uses it. A later fix may rename something; adapt minimally, keep the behaviour and the tests, and record the mapping as a "Drift" note in the task.

**Verification status:** this plan was **not** replayed on a scratch copy. That differs from Part 1. Planning ran docs-only, with Docker left untouched. The code blocks are written against `main` `94f9362` as read on 2026-10-19 and have not been compiled. Implementers run every step, treat a failing compile in a given block as drift to fix minimally, and keep the tests' assertions.

**Drift check (2026-10-24):** Tasks 14-18 were re-read against `feat/logs-part2` after the domain restructure (PR #78) and the merges of Tasks 9-13. Paths, module names and test helpers are in the new layout. Where the current code behaves differently from what a task assumed, the task carries a "Drift (2026-10-24)" note with the minimal adaptation; the tests' assertions are unchanged.

**Plan review (2026-10-19, file names as they were before the restructure).** Every signature this plan uses was spot-checked against `94f9362` (`store`, `query`, `search`, `clickhouse`, `tail`, `routes/logs*.rs`, `models/source.rs`, `models/incident.rs`, `models/maintenance.rs`, `uptime/alerts.rs`, `uptime/worker.rs`, the test helpers in `tests/common`, `tests/logs_*.rs`, `tests/core/migrations.rs`, and nami-ui's `engine.ts`, `log-proxy.ts`, `fake-event.ts`, `lib/logs.ts`, the tail proxy and the explorer). Fixed in this revision (each fix is in the task it belongs to):
- Drift that would not compile or would fail: `tests/core/migrations.rs` has no `mod common;` (migration tests now seed with SQL); `store.rs`'s test helper is `source(retention_days)`, not `test_source()`; the tail proxy test's `fakeEvent` returns `{ event, controller }`; `Prepared` (not only `Batch`) must carry the pipeline counts; `new Response('', { status: 204 })` throws (null-body status); `cargo test` takes one test-name filter (several go after `--`); a 64 KiB + 1 message is truncated by `from_json` before the pipeline sees it; `updated_at` strings do not compare lexicographically (chrono prints 0, 3 or 6 fraction digits); `"garbage\r\nx"` panics in the request builder; `99999999999999999999` is not a `u64` and the engine answers 400; T13 needs T10's `query::parse_path`, so it is not independent of T10.
- Security: regex memory budget per pipeline and in the cache (R-P2-26); compile and preview run off the async runtime; regex redaction also covers numbers (R-P2-5); every write to `sources` bumps `updated_at` monotonically (R-P2-6); export ends at a deadline even when the client stops reading, has a per-instance cap, and never writes a ClickHouse exception line into the file (R-P2-12, R-P2-13, R-P2-15); the alert worker shares one evaluation budget across passes, skips evaluations of an edited alert, records `last_error`, and opens and resolves incidents through one seam (R-P2-18, R-P2-19, R-P2-27); project-wide maintenance windows hold log alerts (R-P2-18, a ruling the controller may reverse); `Last-Event-ID` is length-capped before decoding (R-P2-20).
- Coverage: the LOG-8 deferred items the first draft missed (error display, `<pre>` wrapping, a11y, a live-tail lifecycle test) are in T15 (R-P2-21). Part 1 deferred items without a target are listed in R-P2-22.

**Execution:** one phase, per CLAUDE.md and `docs/superpowers/plan-conventions.md`. Tasks 9-13 merged to `main` under the old one-PR-per-task flow. Everything left runs on the one branch `feat/logs-part2` in `.worktrees/logs-part2/`, in order: T14, T15, T16, T17, T18, then T19 (NAMI-LOG-19). Each task is one or more commits with its task ID in the message (Conventional Commits), followed by a task review (spec compliance and quality) with fix commits on the same branch until it is clean. T19 is the phase security review, its fix wave, the decisions doc, the roadmap ticks and the one phase PR. Dependencies among the remaining tasks: T14 needs T13 (merged). T15's engine half depends on nothing unmerged; its UI half edits `log-proxy.ts` and `lib/logs/logs.ts`, which T17 edits too, so T15 lands before T17. T16 needs T10 and T11 (merged). T17 needs T9, T12, T13 (merged) and T15. T18 needs T14 and T16 (`relayJsonPost`). Work run in parallel (for example T15's engine half beside T14) goes in an extra worktree on a short-lived local branch that is merged back into `feat/logs-part2` before its review (CLAUDE.md, rule 5). How changes are written down: new files appear in full where the code decides something (security-relevant logic, SQL, migrations, proxies). Elsewhere the plan gives exact signatures, names and values, and the implementer writes the body. Tests always appear in full. Engine tests need `docker compose up -d postgres clickhouse mailpit` (never restart running containers).

Engine test command, used by every task below (substitute the domain target and the test file):

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <domain> <file>::
```

Unit tests use the same environment with `--lib` instead of `--test <domain>`. `cargo test` accepts **one** test-name filter before `--`. Several filters go after it: `cargo test --lib -- logs::query::export core::clickhouse::`.

UI commands: `cd nami-ui && npm run check && npx vitest run <paths>` (expect `svelte-check found 0 errors and 0 warnings`).

## Roadmap coverage

Roadmap §3 Part 2. Part 1's items (R3.01 to R3.12) are ticked. The items below stay unticked until the phase PR merges; NAMI-LOG-19 ticks them.

| Roadmap | Item | Tasks |
|---|---|---|
| R3.13 | Per-source ingest pipeline: drop, remove fields, redact, rename, sample | NAMI-LOG-10 (#63), NAMI-LOG-11 (#67), NAMI-LOG-16 (editor UI) |
| R3.14 | Message pattern grouping + explorer Patterns tab | NAMI-LOG-9 (#56), NAMI-LOG-17 (tab) |
| R3.15 | Streamed CSV/NDJSON export, capped at 100k rows | NAMI-LOG-12 (#75), NAMI-LOG-17 (export menu and warning) |
| R3.16 | Saved views | NAMI-LOG-13 (#76), NAMI-LOG-17 (UI) |
| R3.17 | Log alerts (threshold over a window opens an incident) | NAMI-LOG-14 (engine), NAMI-LOG-18 (UI) |
| R3.18 | Live-tail resume across reconnects, explorer fixes Part 1 deferred | NAMI-LOG-15 |
| R3.19 | 30-day usage chart per source | NAMI-LOG-16 |
| R3.20 | UI: pipeline editor, Patterns tab, saved views, alerts | NAMI-LOG-16, NAMI-LOG-17, NAMI-LOG-18 |
| R3.21 | Part 2 security review + fix wave | NAMI-LOG-19 |

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**). Never install natively and never restart a running container.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`). No compile-time macros, no ORM. Postgres SQL strings are literals.
- ClickHouse: the spec requires "the SQL text is assembled only from fixed fragments with numbered placeholders". In code that means only `logs::query::Sql`, whose `push` takes `&'static str`. Every value, every JSON path segment included, is a bound parameter. The project id is always bound from the path.
- Extra ClickHouse settings are fixed code constants and are never names in `RESERVED_SETTINGS`. A setting the client owns for a new request kind is added to `RESERVED_SETTINGS`, and the drift test is updated with it.
- Regexes (spec): the `regex` crate on the Rust side (linear time). At most **256 characters**. Compiled with a size limit, once per rule-set version. Invalid patterns are rejected at write time with 400. Search regexes run in ClickHouse `match` (RE2, linear) with the pattern as a bound `String` parameter. The search side already limits them to 10 filters of 256 characters each.
- RBAC: two levels, as in CLAUDE.md. An authorization failure is **403**, never 404. An id from another project inside an accessible project path is **404**. A `source_id` from another project matches nothing in reads, and is rejected with 400 when stored in a view or an alert.
- Roles per feature: viewers read pipelines, patterns, export, views, alerts and usage. Members edit pipelines (PUT, test), create/edit/delete views and create/edit alerts. **Admins delete alerts** (as monitors). Ingestion is token only.
- Every state-changing engine request carries a JSON body (`core::router::require_json_body`, already global). Every request body is parsed by a `deny_unknown_fields` struct.
- Pipeline (spec): at most **50** rules per source, replaced as a whole. Match is `{"field": path, "equals": value}` or `{"message_regex": pattern}`. Kinds are `drop`, `remove_fields`, `redact` (`keys` case-insensitive at any depth → `"[REDACTED]"`, `regex` over `message` and every string value of `fields`), `rename` (overwrites the target; a missing source is a no-op), and `sample` (`n` 2-10000, keep the 1st, (n+1)th, … per rule per engine instance). The first matching `drop` ends processing. Redaction runs before pattern extraction and storage. The test endpoint takes at most **100** sample events and stores nothing. Regex budget (R-P2-26): each regex compiles with `size_limit` and `dfa_size_limit` of **256 KiB**, and a pipeline holds at most **20** regexes. Compiling rules and running the preview are CPU work and go to `spawn_blocking`, never an async worker.
- Patterns (spec): `GET /projects/{id}/logs/patterns` (search filters and window) returns the top **100** patterns by count as `{pattern_hash, pattern, count, first_seen, last_seen}`. `pattern_hash` is a decimal string.
- Export (spec): `GET /projects/{id}/logs/export?format=csv|ndjson` (viewer, same filters and window as search) streams newest-first up to **100,000** rows, then stops. `X-Nami-Export-Limit: 100000`. NDJSON rows carry every stored column. CSV has `dt, level, source, message, trace_id` plus at most **10** `columns` field paths, extracted server-side. CSV is RFC 4180, and a cell starting with `=`, `+`, `-`, `@`, tab or CR gets a leading `'`. ClickHouse `max_execution_time` is **120 s**. At most **2** concurrent exports per user (third → 429) and **8** per engine instance (ninth → 429). An export task ends at a **150 s** deadline even when its client stops reading (R-P2-13), so a stalled download cannot hold a slot.
- Saved views (spec): Postgres `log_views` (project, name, query JSON with the search parameters and picked columns, created_by). Members create/edit/delete, viewers read.
- Log alerts (spec): Postgres `log_alerts` with `threshold` (count), `comparison` (`gt` | `lt`), `window_seconds` (**60-86400**), `interval_seconds` (**>= 60**), `enabled`, `state` (`ok` | `alerting`), `last_evaluated_at`, `next_evaluation_at`. The worker claims due alerts atomically. A met condition while `ok` → `alerting`, opens an incident (`incidents.log_alert_id`) and notifies through `alerts::notify_transition`. A cleared condition while `alerting` → `ok` and resolves the incident. While a **project-wide** maintenance window is in effect, the alert's state machine stands still, as for heartbeats (R-P2-18). Incidents are opened and resolved only through `logs::alert::{open_alert_incident, resolve_alert_incident}` (R-P2-27).
- Outbound requests: none of this plan's features makes one. Alert notifications go only through `alerts::notify_transition` (email today). When the Incident Management sub-project adds webhook/Slack channels to that function, it applies their link-local and private-address refusal (`uptime::checker::is_forbidden_destination`). Log alerts must never call a sender directly.
- Crypto: no new secrets. Source tokens keep their per-row AAD (`sources.token:{id}`). The pipeline never touches the token.
- nami-ui: every engine call goes through `callEngine` with `enginePath`. Route params use the `[id=uuid]`/`[...Id=uuid]` matchers and are checked with `isUuid` before reaching an engine path. Failed calls render through `engineFailure`/`engineErrorMessage`. A 401 redirects to `/login`. Deletes are two-step (a JS `confirm()` plus a `confirm=yes` field, and a bare POST asks first). Streams (export, tail) are relayed as bodies and never read or buffered in the BFF. Only allow-listed query keys and headers pass. Modules under `#lib/server` are never imported by components. Times are shown in UTC. Tokens never appear in a URL.
- Tests: test binaries run one after another (`cargo test`). ClickHouse tests use their own `nami_test_*` database and drop it.
- Commits: Conventional Commits whose message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **A redaction regex that matches the empty string** (`a*`, `\b`, `(?:)`) would put `[REDACTED]` between every character, multiplying a 64 KiB message into megabytes. Such a regex must be rejected at write time. Any redaction that grows a value (`\d` over a message of digits) must be capped again before storage → Task 10 (`a_regex_that_matches_the_empty_string_is_rejected`), Task 11 (`redaction_that_grows_a_message_is_capped_again`).
- **A pipeline edit must reach ingestion without a restart, and a source whose stored rules no longer compile must not leak.** A redact rule that silently stops applying is a data leak, so ingestion for that source fails closed: HTTP 500 and syslog frames dropped, with nothing stored → Task 11 (`a_pipeline_change_applies_to_the_next_request`, `a_stored_pipeline_that_does_not_compile_fails_closed`).
- **An export that ends early must say so.** Byte cap, mid-stream ClickHouse error or line cap: the file must end with an explicit error trailer, never just stop as if complete. A window holding more than 100,000 rows exports the newest 100,000, and the UI warns first → Task 12 (`hitting_the_byte_cap_ends_with_an_error_trailer`, `the_row_cap_holds_and_is_announced`), Task 17 (`warns before a truncated export`).
- **A ClickHouse outage during alert evaluation must not resolve (or open) incidents.** An evaluation that fails leaves the state, the incident and `last_evaluated_at` unchanged, and the alert is retried at its next interval → Task 14 (`a_failed_count_changes_nothing`).
- **A redact regex must catch a secret whatever JSON type carries it.** A card number sent as a JSON number is as secret as one sent as a string → Task 10 (`redact_by_regex_rewrites_the_message_and_every_string_value`, numbers case).
- **A pipeline must not cost unbounded memory or block the runtime.** At most 20 regexes of at most 256 KiB compiled each, a bounded cache, compile and preview in `spawn_blocking` → Task 10 (`a_pipeline_holds_at_most_twenty_regexes`), Task 11 (cache bound).
- **A download that stops being read must not hold an export slot.** The export task ends at its deadline whatever the client does → Task 12 (`a_stalled_export_gives_its_slot_back_at_the_deadline`).
- **An alert edited during an evaluation must not fire on its old settings** → Task 14 (`an_evaluation_of_an_edited_alert_is_dropped`).
- **A saved view with a relative preset must reopen relative** ("last 1 h" means the hour before opening, not the hour when it was saved). A view naming a source that was later deleted must still open → Task 13 (`a_relative_view_stays_relative_and_a_deleted_source_is_kept`), Task 17 (`view state round-trips and stays relative`).

## Decisions the spec leaves open (rulings)

- **R-P2-1 Names.** Migrations: what LOG-10 (`sources.pipeline`) and LOG-13 (`log_views`) added is in the baseline `0005_logs.sql` (folded on 2026-10-19). T14 adds two phase files, `NNNN_logs_alerts.sql` (`log_alerts`) and then `NNNN_incidents_log_alerts.sql` (`incidents.log_alert_id`, its FK and indexes), where NNNN is the next free number in `nami-engine/migrations/` and the logs file takes the lower one, because the incidents FK references `log_alerts`. Engine modules: `logs::ingest::pipeline`, `logs::ingest::pipeline_cache`, `logs::query::patterns`, `logs::query::export`, `logs::query::slots`, `logs::view::saved_query`, `logs::alert`. Routes: `logs::view::routes`, `logs::alert::routes`, with the pipeline and patterns endpoints added to the existing `logs::source::routes` / `logs::query::routes`. Models: `logs::view::model`, `logs::alert::model`.
- **R-P2-2 Rule wire format.** The spec names rules by kind. On the wire a rule is one flat object with a `type` key: `{"type":"drop","match":{...}}`, `{"type":"remove_fields","paths":[...]}`, `{"type":"redact","keys":[...],"regex":"..."}`, `{"type":"rename","from":"a","to":"b"}`, `{"type":"sample","n":10,"match":{...}}`. A key the kind does not use → 400. Errors name the rule by 0-based index: `rules[3]: regex must be at most 256 characters`.
- **R-P2-3 Paths in rules.** Paths follow the search filter syntax (1-5 segments of `^[A-Za-z0-9_-]{1,64}$`). In `match.field` and `rename`, the single-segment paths `level` and `message` address the normalized values (spec). `rename` from `message` moves the message into a field and leaves the message empty. Renaming to `message` sets it (a non-string becomes its JSON text). Renaming to `level` goes through `normalize::level_from_name`, so an unknown name gives `info`. `remove_fields` addresses `fields` only, and `level`/`message` there → 400 (use `rename` or `redact`). `redact.keys` are key *names* (1-64 characters, no control characters, at most 50) and never paths. Redaction never changes `level`. At most 50 paths per `remove_fields`.
- **R-P2-4 `equals` comparison** follows search's `eq`: a string field compares unquoted, any other JSON value by its JSON text (`200` matches `200` and `"200"`). A missing field never matches. `equals` is a string, number or boolean (≤ 1,024 characters of text), or 400.
- **R-P2-5 Redaction regex** must not match the empty string (400). Without that guard, a regex like `a*` would put a marker between every character. Replacement is applied to `message` and to string values at any depth of `fields` (array elements included). A **number** at any depth is matched on its JSON text, and a number with a match becomes the replaced text as a string (`{"card": 4111111111111111}` with `\d{16}` stores `{"card": "[REDACTED]"}`). Booleans and nulls are left alone. Keys are never rewritten. Rules run top to bottom, so a value moved by a later `rename` is only covered by a redact rule that comes after it. The editor's help text says so (Task 16).
- **R-P2-6 Cache and freshness (stricter than the spec's "checked at most every 10 s").** `IngestSource` gains `updated_at`, which both token lookups already read in the same query. The cache entry is used only when its `updated_at` equals the source's, so an edit applies to the very next request on every instance (syslog looks its sources up per batch and has no source cache, so this holds there too). A miss loads `sources.pipeline` (one indexed query, under the ingest query semaphore for HTTP; the permit is released before compiling) and compiles it in `spawn_blocking`. At most **1,024** entries; the cache is cleared when full. Sample counters live in the compiled rules, so they restart after an edit. Every write to `sources` (`set_pipeline`, `update_in_project`, `rotate_token`) sets `updated_at = greatest(clock_timestamp(), updated_at + interval '1 microsecond')`, so the value only ever grows and a cache key can never come back to an older rule set. Today `update_in_project` and `rotate_token` use `now()` (transaction start), which can go backwards under row-lock waits.
- **R-P2-7 Fail closed.** Stored rules were validated by the same compiler. If they no longer compile (a future change) or fail to load, the source's events are not stored: HTTP returns 500 `internal error` (logged with the source id, never the token), and syslog frames are dropped and counted as `store_failed`. Silently skipping a `redact` rule would leak data.
- **R-P2-8 What `accepted` counts.** `202 {"accepted": N}` counts stored rows, after the pipeline. Dropped and sampled-out events are counted in `source_usage_daily.events_dropped` / `events_sampled_out` (columns from Part 1) and are never in `accepted`. The response shape is unchanged. Usage `bytes` and the quota count stored bytes after the pipeline (spec order: pipeline before quota accounting).
- **R-P2-9 Test endpoint.** `POST /projects/{id}/sources/{source_id}/pipeline/test` is member-only (it is the editor's preview). It runs `normalize::from_json`, then the submitted rules with fresh sample counters, then `store::build_row_at` with the source's real retention. It returns exactly what would be stored, without ids. The body is limited to axum's default 2 MiB.
- **R-P2-10 Patterns query cost.** Settings are `READ_SETTINGS` plus `max_rows_to_group_by=1000000` and `group_by_overflow_mode=any`. Beyond one million distinct patterns in a window, counts become approximate instead of failing. The UI notes "counts are approximate for very large windows". Each request holds one per-user query slot.
- **R-P2-11 Pattern IPv6 over-match (deferred from Part 1).** An IPv6 candidate becomes `<ip>` only when it parses, has an address group (as today), and either has at least two non-empty groups, starts with `::`, or is a single four-character group followed by `::` (`fe80::`). So `5::` and `12::` stay text, while `::1`, `fe80::` and `2001:db8::` stay `<ip>`. Only new rows change. Existing rows keep their stored pattern, so a pattern may appear in both forms during the retention period (documented in the PR).
- **R-P2-12 Export transport.** A new `ClickHouse::stream_rows_with` posts without `wait_end_of_query` (rows flow as ClickHouse produces them). It sends `max_execution_time=120`, `readonly=2`, `cancel_http_readonly_queries_on_client_close=1` and `http_write_exception_in_output_format=1`, and uses a per-request timeout of 150 s (reqwest's per-request timeout replaces the client's 30 s). It sends no `max_result_bytes` (the export's own caps bound it, R-P2-13). `http_write_exception_in_output_format` joins `RESERVED_SETTINGS`. A non-2xx answer is read through the capped reader and mapped by `search::user_error` *before* any response header is sent, so an invalid RE2 is a 400, not a broken file. After that, an error can only be reported in the body. It ends the stream with a trailer: NDJSON `{"_nami_export_error":"<reason>"}`, CSV a last record `# export stopped early: <reason>`. How ClickHouse marks a mid-stream error differs by version (a `{"exception":…}` line, or an `__exception__` block on newer servers, or a cut connection). The export therefore does not look for one shape. Every line must start with the first selected column's key (`{"id":` for NDJSON, `{"dt":` for CSV). A line that does not is never written to the file and ends it with the "query failed" trailer. A transport error ends it with the "interrupted" trailer.
- **R-P2-13 Export caps.** `MAX_EXPORT_ROWS = 100_000` (also the SQL `LIMIT`), `MAX_EXPORT_BYTES = 512 MiB` written, `MAX_EXPORT_LINE_BYTES = 4 MiB` per ClickHouse line. A stored row is at most 64 KiB of message plus 256 KiB of fields; JSON escaping makes that at most about 2 MiB. Reaching the row cap ends the file normally. The UI warned first, and the header tells API clients. Reaching the byte or line cap writes the trailer. The output channel holds 16 chunks. **Deadline:** the streaming task has an overall deadline, `ExportLimits.deadline` (150 s, the same as the request timeout). Every `next_line` and every `tx.send` runs under `timeout_at(deadline, ..)`, so a client that stays connected but stops reading cannot hold the slot. reqwest's own timeout is only checked while the body is polled, so it would not end such a task. At the deadline the task tries the trailer for at most 1 s, then returns. A client that disconnects ends the task at once (`tx.closed()` / a failed send), which drops the ClickHouse response, and ClickHouse cancels the query (`cancel_http_readonly_queries_on_client_close`).
- **R-P2-14 Export columns.** The repeatable key is `columns` (spec wording), up to 10 search-syntax paths, extracted in ClickHouse with the same text expression as `eq` filters (strings unquoted, other values as JSON text, missing → empty cell). The CSV `source` column is the source's name from Postgres, mapped once before streaming (at most `MAX_SOURCES_PER_PROJECT` names). A row of a since-deleted source shows its id. `Content-Disposition: attachment; filename="nami-logs-<to as %Y%m%dT%H%M%SZ>.<csv|ndjson>"` contains no user text.
- **R-P2-15 Per-user slots for entry reads and exports.** A new counter, `logs::query::slots::UserSlots` (Task 9), separate from the fix wave's 4 search/histogram slots. Exports hold one of 2 per user and one of `MAX_EXPORTS_PER_INSTANCE = 8` in total (`UserSlots::acquire_capped(user, per_user, total)`, which counts a running total beside the per-user map), with the permit moved into the streaming task, so a slot is held for exactly as long as the stream runs (at most the deadline, R-P2-13). Each export runs a ClickHouse query for up to 120 s with no result-size setting, so without the total a crowd of users could fill ClickHouse with exports. Entry reads hold one of 8 per user (R-P2-23). The duplication with `search::QuerySlots` is left for the security wave to fold if it wants.
- **R-P2-16 UI export warning.** The browser downloads through a plain link, so it cannot read `X-Nami-Export-Limit` before the download. The UI therefore holds `EXPORT_ROW_LIMIT = 100_000` and compares it with the histogram total before starting. An engine test pins the header to 100000 and a UI test pins the constant. A `HEAD` probe was rejected: axum answers HEAD by running the GET handler, which would start an export.
- **R-P2-17 Query shape shared by views and alerts.** `LogQuery { q, in_fields, levels, source_ids, filters, pattern_hashes }`. `filters` items are the explorer's `{path, op, value?}` objects. It is validated by converting to query pairs and running Part 1's `query::parse_filters`, so it is identical to search validation. `ViewQuery` adds `range` (`15m|1h|6h|24h|7d|custom`, default `1h`), `from`/`to` (custom only, from < to, ≤ 31 days) and `columns` (≤ 10 paths). Alerts store a `LogQuery`. Stored `source_ids` must belong to the project when written (400). A source deleted later just matches nothing. Names go through `core::validate::validate_label("name", ..)`. View names are unique per project (409). At most **200** views and **100** alerts per project (409), to bound the evaluation load.
- **R-P2-18 Alert semantics.** The window is `[now − window_seconds, now]` at evaluation. `gt` fires when `count > threshold`, `lt` when `count < threshold`. `lt` needs `threshold >= 1` (400), because `lt 0` can never fire. Thresholds go up to 10^12. A PATCH sets `next_evaluation_at = now()` so an edit takes effect at once. Disabling an `alerting` alert sets `ok` and resolves its open incident without a notification. Deleting an alert deletes its incidents (FK cascade, same as monitors). **Maintenance (ruling, reversible by the controller):** a *project-wide* maintenance window (`monitor_id` null) in effect at `started_at` holds the alert's state machine, as it holds heartbeats: the count, `last_count` and `last_evaluated_at` are recorded, but `state` does not change, no incident opens or resolves, and nothing is sent. The first evaluation after the window applies the transition normally. A monitor-scoped window does not apply. Reason: a project-wide window means "the whole project is under planned work", and log volume and error counts are what planned work changes most. Paging on them would make project-wide windows useless. The check is `uptime::maintenance::model::active_windows(&mut *tx, project_id, started_at).await?.covers_heartbeats()` (project-wide windows only). Add a `covers_project()` alias with that body so the call reads right. **Stale evaluations** are skipped: one that started no later than `last_evaluated_at`, or one whose claimed `updated_at` differs from the row's (the alert was edited while it ran, so its count used old settings; the PATCH already made the alert due again). A failed count changes nothing except `last_error` / `last_error_at` (a fixed, user-safe text: the `AppError::Validation` message from `search::user_error`, else `"the log store is unavailable"`), so an alert whose query keeps failing is visible in the UI instead of silently never evaluating. A successful evaluation clears `last_error`. Incident title: `Log alert: <name>`. Cause: `<count> matching events in the last <window> (threshold: > <n>)`, or `< <n>` for `lt`.
- **R-P2-19 Alert worker.** `logs::alert::run_forever` runs where the uptime worker runs (`UPTIME_WORKER_ENABLED`). It ticks every 5 s and claims due alerts with `for update skip locked`, rescheduled `interval_seconds` ahead in the claiming statement. Several engine instances can therefore run it at once: each alert is claimed by one of them per interval. Evaluations share one `Arc<Semaphore>` of `MAX_ALERT_EVALUATIONS = 8` across every pass of the instance (passes overlap when ClickHouse is slow). A tick claims at most `min(ALERT_CLAIM_BATCH = 50, available permits)` alerts, and none when no permit is free, so an alert is never claimed (and pushed one interval ahead) without capacity to evaluate it. Notifications go through a `Notifications` of its own. Per-evaluation budget: one `count()` under `READ_SETTINGS` and the client's 25 s `max_execution_time`, holding one permit. Alert storms are bounded by the state machine (an email only on a transition), the unique open incident per alert, `interval_seconds >= 60` (a flapping alert sends at most one email per interval) and 100 alerts per project. Confirmation periods arrive with Metrics' shared evaluator.
- **R-P2-20 Live-tail resume (deferred from Part 1).** Every `event: log` carries `id: <Cursor::encode()>` (opaque, `(dt, id)`). On connect, a `Last-Event-ID` header holding a valid cursor dated within the last **5 minutes** makes the first poll send the rows after that cursor (oldest first, at most `TAIL_BATCH`), instead of only remembering them. A cursor that is malformed, longer than 128 bytes (checked before decoding), older or in the future is ignored (normal start). The cursor is untrusted input and changes only two things: the lower `dt` bound of the first poll (never more than 5 minutes back) and a bound flag column. The project comes from the path, the filters from the query string, and the replay is capped at `TAIL_BATCH` rows, so a forged cursor cannot reach another project's rows or read more than a normal poll. When the resume poll fails, the next poll tries the resume again (resume ends only with the first poll that succeeds). With more than `TAIL_BATCH` rows after the cursor, the newest `TAIL_BATCH` are replayed and the older part of the gap is lost. Late rows dated at or before the cursor that arrived during the gap are not replayed either (both documented). nami-ui forwards `Last-Event-ID` only when it matches `^[A-Za-z0-9_-]{1,128}$`. The explorer leaves reconnects to `EventSource` (it closes the stream only on `end: session`), so the browser's own reconnect after the 30-minute `end: timeout` carries the header.
- **R-P2-21 Explorer fixes (deferred from Part 1).** The tail effect depends only on the derived tail query string and the `live` flag, so changing columns or the tab does not restart it. "Load more" carries a generation number and an `AbortController`. Navigation aborts it, and a response from an older generation is discarded. Live rows are validated (`isLogEntry`), deduplicated by id and capped at `LIVE_ROW_CAP`. Turning live off clears them. The rest of the LOG-8 deferred line is also Part 2: a failed histogram or sources call shows a message instead of looking empty (`histogramError`, `sourcesError` from the load), `<pre>` fields wrap (`whitespace-pre-wrap wrap-anywhere`, as the message `<pre>` already does), the live status is announced (`role="status" aria-live="polite"`), the results/patterns tabs are a labelled `nav`, and the live-tail lifecycle gets a test. For that test the EventSource handling moves out of `+page.svelte` into `nami-ui/src/lib/logs/live-tail.ts` (`startLiveTail`), which takes the `EventSource` constructor as a parameter, so vitest drives it with a fake.
- **R-P2-22 Not in Part 2.** Graceful shutdown and per-IP HTTP limits go to Platform (Part 1 ruling). Alert no-data handling, confirmation periods and per-series incidents go to Metrics (spec). Alert routing through escalation policies goes to Incident Management (roadmap §4). The "View trace" link waits for Traces (spec). Part 1 deferred items that the decisions log gives no target stay open, and the Part 2 final review decides each one: LOG-3 (2-permit large-body semaphore, `shrink_to_fit` on the prefix result, memory-test baseline, aggregate encoded bodies awaiting ClickHouse), LOG-4 (unreachable UDP oversize branch on IPv4), LOG-5 (numeric `eq` compares text, Rust pre-check stricter than RE2, `user_error` substring matching, paging window not in the cursor, about 32 histogram buckets at 31 days, empty `contains` matches all), LOG-6 (no test for `MAX_UNKNOWN_CHECKS`), LOG-7 (viewers see Reveal/Rotate, token panel and snippet a11y, rsyslog template token unescaped, no component tests). When Traces adds OTLP logs, its events must reach `RowBatch::push` with `trace_id`/`span_id` already checked (`normalize::checked_id`), so an invalid id is in `fields` before redaction runs (today `build_row` moves it after the pipeline; HTTP and syslog are unaffected because `from_json` checks ids and syslog sets none).
- **R-P2-23 Entry reads get their own cap (controller follow-up from the fix wave).** One explorer page load takes 2 of the 4 query slots (search + histogram). A load-more and two expanded rows would then hit 429. `GET /projects/{id}/logs/{log_id}` therefore stops taking a `QueryPermit` and takes one of `MAX_ENTRY_READS_PER_USER = 8` entry slots instead. An entry read reads one granule by `dt` and `id` under `READ_SETTINGS`, so 8 at once cost little. The spec line "at most 4 searches, histograms and entry reads" becomes "at most 4 searches and histograms, and 8 entry reads", changed in the same PR (Task 9).
- **R-P2-24 Syslog row chunking off the runtime (controller follow-up).** The batcher calls `store::chunk_rows` (which JSON-encodes every row to measure it) on the async task. Task 11 moves it into the blocking task that builds the `RowBatch`. That task returns the rows, the runs and the oversized count together, and only the per-run encode and the insert stay as they are.
- **R-P2-25 A tail poll reads only new rows, pinned by a counter (controller follow-up).** `LogsState.tail_stats: tail::TailStats { id_polls: AtomicU64, rows_fetched: AtomicU64 }` counts id polls and list rows read by `fetch_rows`. Task 15's test asserts that 10 polls over 100 old rows read exactly the 2 new rows. The counters are cheap and are also what a later metrics endpoint would export.
- **R-P2-26 Regex budget per pipeline.** Each pipeline regex compiles with `RegexBuilder::size_limit(REGEX_SIZE_LIMIT)` and `dfa_size_limit(REGEX_DFA_SIZE_LIMIT)`, both **256 KiB**. A pipeline holds at most `MAX_REGEXES = 20` regexes (`message_regex` and `redact.regex` together). The error names the rule holding the 21st: `rules[i]: a pipeline may hold at most 20 regular expressions`. A pattern over the size limit gets `"regex is too complex (compiled size limit)"` (`regex::Error::CompiledTooBig`), any other error `"regex is not a valid regular expression"`. Neither echoes the pattern. Bound: a cached pipeline holds at most 20 × 256 KiB of programs, plus one lazy-DFA cache of at most 256 KiB per regex per thread using it at once. Ingest decoding is capped at 4 threads, the preview at one blocking task per request. With the cache at 1,024 entries that is a bounded worst case. The spec's line "compiled once per rule-set version with a size limit" gains "at most 20 per pipeline" (Task 10). CPU stays bounded: a request decodes at most 50 MiB of values, each regex scans it once in linear time, and the decode places (4, at most 2 per project) bound how many such scans run at once.
- **R-P2-27 One seam for log-alert incidents.** The Incident Management sub-project (next) replaces how incidents open and notify (escalation policies, severities, `source = 'log_alert'`). Log alerts touch incidents only through two functions in `logs::alert`: `open_alert_incident(conn, alert, count, at) -> Result<Option<Transition>, sqlx::Error>` (calls `incident::open_for_log_alert` and builds the `Down` transition) and `resolve_alert_incident(conn, alert_id, project_id, name, at, notify: bool) -> Result<Option<Transition>, sqlx::Error>` (calls `incident::resolve_for_log_alert`, returns the `Recovered` transition only when `notify`). `apply_evaluation` and `logs::alert::model::update` (disable) call these and nothing else. Transitions still leave only through `uptime::alerts::notify_transition`, so swapping in the unified incident API is a change to those two functions.

## File Structure

New layout (`docs/superpowers/decisions/2026-10-19-domain-restructure.md`). Entries for T9-T13 are merged and listed as the record; T14-T18 create or change the rest.

```
nami-engine/
  migrations/
    0005_logs.sql                      baseline: holds what T10 (sources.pipeline) and T13 (log_views) added (merged)
    NNNN_logs_alerts.sql               T14 log_alerts (lower number: the incidents file's FK needs it)
    NNNN_incidents_log_alerts.sql      T14 incidents.log_alert_id, its partial unique index and lookup index
  src/
    main.rs                            T14 spawn logs::alert::run_forever (uptime_worker_enabled branch)
    core/clickhouse.rs                 T12 stream_rows_with, RowStream, LineTooLong, reserved setting (merged)
    core/router.rs                     T13 merged logs::view routes; T14 merges logs::alert routes
    logs/mod.rs                        module list; LogsState gained entry_slots (T9), pipelines (T11), export_slots + export_limits (T12), all merged; T14 adds `pub mod alert;`; T15 adds tail_stats
    logs/ingest/pipeline.rs            T10 rules, compile, apply (merged)
    logs/ingest/pipeline_cache.rs      T11 per-source compiled-rules cache (merged)
    logs/ingest/store.rs               T11 RowBatch::push runs the pipeline (merged)
    logs/ingest/usage.rs               T11 record_pipeline (merged)
    logs/ingest/routes.rs              T11 pipeline lookup before Work (merged)
    logs/syslog/mod.rs                 T11 pipelines per batch; chunk_rows in the blocking build (merged)
    logs/query/mod.rs                  T10 pub parse_path; T12 pub push_field_text (merged)
    logs/query/pattern.rs              T9 IPv6 over-match fix (merged)
    logs/query/patterns.rs             T9 top patterns query (merged)
    logs/query/slots.rs                T9 UserSlots; T12 export slots (merged)
    logs/query/export.rs               T12 params, SQL, CSV, streaming task (merged)
    logs/query/tail.rs                 T15 event ids, resume, TailStats
    logs/query/routes.rs               T9 patterns, T12 export (merged); T15 Last-Event-ID
    logs/source/model.rs               T10/T11 pipeline read/write, monotonic updated_at, IngestSource.updated_at (merged)
    logs/source/routes.rs              T10 pipeline GET/PUT/test (merged)
    logs/view/{mod,model,routes,saved_query}.rs   T13 saved views, LogQuery, ViewQuery (merged)
    logs/alert/mod.rs                  T14 claim, count, apply, seam, worker loop
    logs/alert/model.rs                T14 LogAlert, validate, CRUD
    logs/alert/routes.rs               T14 CRUD + preview routes
    incidents/model.rs                 T14 open/resolve_for_log_alert, list joins log_alerts
    uptime/alerts.rs                   T14 Subject::LogAlert in render and still_down
    uptime/worker.rs                   T14 spawn_alerts made pub(crate)
    uptime/maintenance/model.rs        T14 ActiveWindows::covers_project alias
  tests/
    logs/main.rs                       T14 adds `mod log_alerts;`
    logs/logs_patterns.rs              T9 (merged)
    logs/logs_pipeline_api.rs          T10 (merged)
    logs/logs_pipeline_ingest.rs       T11 (merged)
    logs/logs_export.rs                T12 (merged)
    logs/log_views.rs                  T13 (merged)
    logs/log_alerts.rs                 T14
    logs/logs_tail.rs                  T15 (+ resume tests)
    core/migrations.rs                 T10/T13 (merged); T14 log_alerts
    core/clickhouse.rs                 T12 reserved settings (merged)

nami-ui/src/
  lib/logs/logs.ts (+ test)            T15 isLogEntry, mergeLiveRows, tailKey; T16 usageBars; T17 tab/pattern state, view <-> state, export href; T18 exports parseFilter
  lib/logs/live-tail.ts (+ test)       T15 startLiveTail (EventSource lifecycle, testable with a fake)
  lib/logs/pipeline.ts (+ test)        T16 rule types, drafts, sample parsing
  lib/logs/log-alerts.ts (+ test)      T18 types, describeCondition
  lib/server/core/json-relay.ts (+ test)        T16 relayJsonPost (T18 reuses it)
  lib/server/logs/log-proxy.ts (+ test)         T12 EXPORT_PARAMS (merged); T15 lastEventIdHeader; T17 PATTERN_PARAMS
  lib/server/logs/log-alert-form.ts (+ test)    T18
  lib/uptime/uptime.ts                 T18 Incident.log_alert_id
  lib/components/UsageChart.svelte     T16
  lib/components/logs/PipelineEditor.svelte  T16
  lib/components/logs/PatternsTable.svelte   T17
  lib/components/logs/SavedViews.svelte      T17
  lib/components/logs/ExportMenu.svelte      T17
  lib/components/logs/LogAlertForm.svelte    T18
  routes/(app)/projects/[id=uuid]/
    incidents/+page.svelte             T18 link for log-alert incidents
    logs/+layout.svelte                T18 Alerts sub-tab
    logs/+page.server.ts, +page.svelte T15 fixes; T17 tabs, views, export
    logs/tail/+server.ts               T15 Last-Event-ID
    logs/export/+server.ts (+ test)    T12 streamed export proxy (merged)
    logs/sources/[sourceId=uuid]/+page.*          T16 usage chart (days=30)
    logs/sources/[sourceId=uuid]/pipeline/+page.* T16 editor
    logs/sources/[sourceId=uuid]/pipeline/test/+server.ts (+ test) T16 preview proxy
    logs/alerts/+page.*, new/+page.*, [alertId=uuid]/+page.*   T18
    logs/alerts/preview/+server.ts (+ test)                      T18
```

---
### Task 9 (NAMI-LOG-9): Message patterns endpoint, the IPv6 over-match fix, and a separate cap for entry reads (done, PR #56)

Merged. Kept as the record of what landed; do not re-run its steps.

**Files:**
- Modify: `nami-engine/src/logs/query/pattern.rs` (`looks_like_ipv6`, tests)
- Create: `nami-engine/src/logs/query/patterns.rs`, `nami-engine/src/logs/query/slots.rs`
- Modify: `nami-engine/src/logs/mod.rs` (module list, `LogsState.entry_slots`), `nami-engine/src/logs/query/routes.rs` (patterns route; `log_entry` takes an entry slot instead of a `QueryPermit`), `docs/superpowers/specs/2026-10-13-log-management-design.md` (the per-user cap sentence, R-P2-23)
- Test: `nami-engine/tests/logs/logs_patterns.rs`, append to `nami-engine/tests/logs/logs_search.rs` (+ unit tests in `pattern.rs`, `patterns.rs`, `slots.rs`)

**Interfaces:**
- Consumes: `query::{parse_window, parse_filters, push_where, push_window, query_pairs, Sql}`, `search::{READ_SETTINGS, user_error, QueryPermit}` (fix wave), `require_project_role`.
- Produces: `logs::query::patterns::{TOP_PATTERNS: u32 = 100, PATTERN_SETTINGS, PatternCount, top_patterns}`; route `GET /projects/{id}/logs/patterns` → `{"patterns": [PatternCount]}`. Also (used by T12):

```rust
// logs::query::slots
#[derive(Debug, Default)] pub struct UserSlots { /* Mutex<HashMap<Uuid, usize>> */ }
#[derive(Debug)] pub struct UserPermit { /* Arc<UserSlots>, user id */ }
impl UserSlots {
    pub fn acquire(self: &Arc<Self>, user_id: Uuid, max: usize) -> Result<UserPermit, AppError>; // 429 RateLimited
    pub fn open(&self, user_id: Uuid) -> usize;
}
// logs::query::search
pub const MAX_ENTRY_READS_PER_USER: usize = 8;
// logs::LogsState gains `pub entry_slots: Arc<slots::UserSlots>`
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternCount {
    /// Decimal string: a UInt64 does not fit a JavaScript number.
    pub pattern_hash: String,
    pub pattern: String,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}
pub async fn top_patterns(ch: &ClickHouse, project_id: Uuid, window: &Window, filters: &LogFilters)
    -> Result<Vec<PatternCount>, AppError>;
```

- [ ] **Step 1: Write the failing pattern unit tests** (append to `mod tests` in `src/logs/query/pattern.rs`)

```rust
#[test]
fn a_number_before_a_double_colon_is_not_an_address() {
    assert_eq!(normalize_message("retry 5:: next"), "retry <num>:: next");
    assert_eq!(normalize_message("code 12::warn"), "code <num>::warn");
}

#[test]
fn real_ipv6_addresses_are_still_replaced() {
    for (input, expected) in [
        ("from ::1 ok", "from <ip> ok"),
        ("net fe80:: up", "net <ip> up"),
        ("net 2001:db8:: up", "net <ip> up"),
        ("peer 2001:db8::7 refused", "peer <ip> refused"),
        ("peer fe80::1%eth0", "peer <ip>%eth0"),
        ("mapped ::ffff:192.0.2.1 ok", "mapped <ip> ok"),
    ] {
        assert_eq!(normalize_message(input), expected, "{input}");
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: the engine command with `--lib logs::query::pattern` (i.e. `cargo test --lib logs::query::pattern`).
Expected: FAIL. `a_number_before_a_double_colon_is_not_an_address` gets `retry <ip> next`. If `peer fe80::1%eth0` already differs today, fix the expectation to today's output for that case only and note it. The `%` handling is not part of this change.

- [ ] **Step 3: Narrow `looks_like_ipv6` (R-P2-11)**

```rust
/// An IPv6 address, as opposed to an identifier path or a number followed by
/// `::` that merely parses as one: besides parsing and having an address
/// group (see [`is_address_group`]), the text needs at least two non-empty
/// groups (`2001:db8::`), or to start with `::` (`::1`, `::ffff:…`), or to be
/// one full four-character group before `::` (`fe80::`). So `5::` and
/// `12::` (a number, then punctuation) stay text.
fn looks_like_ipv6(text: &str) -> bool {
    if !text.contains(':') || !text.split(':').any(is_address_group) {
        return false;
    }
    let groups: Vec<&str> = text.split(':').filter(|g| !g.is_empty()).collect();
    let shaped = groups.len() >= 2
        || text.starts_with("::")
        || (groups.len() == 1 && groups[0].len() == 4 && text.ends_with("::"));
    shaped && text.parse::<Ipv6Addr>().is_ok()
}
```

- [ ] **Step 4: Run the pattern tests to verify they pass**

Run: `cargo test --lib logs::query::pattern` (engine command).
Expected: PASS, including every pre-existing pattern test.

- [ ] **Step 5: Write the failing integration tests** (`nami-engine/tests/logs/logs_patterns.rs`)

```rust
// Top message patterns (`GET /projects/{id}/logs/patterns`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{Duration, SecondsFormat, Utc};
use common::{app, call, logs::{post_json, seed_source}, public_app, seed_project, state_with_clickhouse, user_with_role};
use serde_json::{Value, json};

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn window() -> String {
    let to = Utc::now() + Duration::minutes(1);
    let from = to - Duration::hours(1);
    format!(
        "from={}&to={}",
        encode(&from.to_rfc3339_opts(SecondsFormat::Millis, true)),
        encode(&to.to_rfc3339_opts(SecondsFormat::Millis, true))
    )
}

#[sqlx::test]
async fn patterns_are_counted_ranked_and_confined_to_the_project(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (foreign_source, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let events: Vec<Value> = (0..5)
        .map(|n| json!({ "message": format!("user {n} logged in from 10.0.0.{n}") }))
        .chain((0..2).map(|n| json!({ "message": format!("job {n} failed"), "level": "error" })))
        .collect();
    assert_eq!(post_json(&public, &token, json!(events)).await.0, StatusCode::ACCEPTED);
    for _ in 0..9 {
        post_json(&public, &foreign, json!({ "message": "theirs 1 only" })).await;
    }

    let uri = format!("/projects/{project}/logs/patterns?{}", window());
    let (status, body) = call(&app, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let patterns = body["patterns"].as_array().unwrap();
    let summary: Vec<(String, u64)> = patterns
        .iter()
        .map(|p| (p["pattern"].as_str().unwrap().to_string(), p["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("user <num> logged in from <ip>".to_string(), 5),
            ("job <num> failed".to_string(), 2),
        ],
        "most frequent first, other projects never counted"
    );
    let first = &patterns[0];
    assert!(first["pattern_hash"].as_str().unwrap().parse::<u64>().is_ok());
    assert!(first["first_seen"].as_str().unwrap() <= first["last_seen"].as_str().unwrap());

    // The hash filters search to exactly those rows.
    let hash = first["pattern_hash"].as_str().unwrap();
    let (_, page) = call(&app, "GET", &format!("/projects/{project}/logs?{}&pattern_hash={hash}", window()), Some(&viewer), None).await;
    assert_eq!(page["logs"].as_array().unwrap().len(), 5);

    // Filters narrow the patterns; another project's source matches nothing.
    let (_, errors) = call(&app, "GET", &format!("{uri}&level=error"), Some(&viewer), None).await;
    assert_eq!(errors["patterns"].as_array().unwrap().len(), 1);
    let (_, foreign_rows) = call(&app, "GET", &format!("{uri}&source_id={foreign_source}"), Some(&viewer), None).await;
    assert_eq!(foreign_rows["patterns"], json!([]));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn at_most_one_hundred_patterns_are_returned(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    // 120 distinct patterns: the word changes, so no placeholder merges them.
    let words = (0..120).map(|n| format!("event{}", char::from(b'a' + (n % 26) as u8)).repeat(1 + n / 26));
    let events: Vec<Value> = words.map(|w| json!({ "message": format!("{w} happened") })).collect();
    post_json(&public, &token, json!(events)).await;
    let (_, body) = call(&app, "GET", &format!("/projects/{project}/logs/patterns?{}", window()), Some(&viewer), None).await;
    assert_eq!(body["patterns"].as_array().unwrap().len(), 100);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn patterns_need_access_and_validate_like_search(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let base = format!("/projects/{project}/logs/patterns");
    assert_eq!(call(&app, "GET", &base, None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "GET", &base, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    for bad in [
        "from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z",
        "level=loud",
        "filter=%7B%22path%22%3A%22a%22%2C%22op%22%3A%22gt%22%2C%22value%22%3A%22x%22%7D",
    ] {
        let (status, body) = call(&app, "GET", &format!("{base}?{bad}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    // Injection text is a literal: no error, no rows.
    let evil = encode("'; DROP TABLE logs; -- {p0:String}");
    let (status, body) = call(&app, "GET", &format!("{base}?q={evil}"), Some(&viewer), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "patterns": [] })));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn patterns_share_the_per_user_query_slots(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (user_id, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let held: Vec<_> = (0..nami_engine::logs::search::MAX_QUERIES_PER_USER)
        .map(|_| nami_engine::logs::search::QueryPermit::acquire(&logs, user_id).unwrap())
        .collect();
    let uri = format!("/projects/{project}/logs/patterns");
    assert_eq!(call(&app, "GET", &uri, Some(&viewer), None).await.0, StatusCode::TOO_MANY_REQUESTS);
    drop(held);
    assert_eq!(call(&app, "GET", &uri, Some(&viewer), None).await.0, StatusCode::OK);
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 6: Run them to verify they fail**

Run: engine command with `--test logs logs_patterns::`.
Expected: FAIL. The route does not exist; the router answers 404 for the first assertion that expects 200.

- [ ] **Step 7: Implement `logs/patterns.rs` and the route**

The SQL is fixed fragments only:

```rust
pub const TOP_PATTERNS: u32 = 100;

/// `READ_SETTINGS` plus a bound on grouping: past a million distinct
/// patterns in the window, ClickHouse keeps counting the patterns it has and
/// ignores new ones (approximate, not an error); see R-P2-10.
pub const PATTERN_SETTINGS: [(&str, &str); 4] = [
    ("max_result_bytes", "33554432"),
    ("result_overflow_mode", "throw"),
    ("max_rows_to_group_by", "1000000"),
    ("group_by_overflow_mode", "any"),
];

pub async fn top_patterns(ch: &ClickHouse, project_id: Uuid, window: &Window, filters: &LogFilters)
    -> Result<Vec<PatternCount>, AppError>
{
    let mut sql = Sql::new();
    sql.push("SELECT toString(pattern_hash) AS pattern_hash, any(pattern) AS pattern, count() AS count, \
              min(dt) AS first_seen, max(dt) AS last_seen FROM logs");
    query::push_where(&mut sql, project_id, filters);
    query::push_window(&mut sql, window);
    sql.push(" GROUP BY pattern_hash ORDER BY count DESC, pattern_hash LIMIT ")
        .value("UInt32", TOP_PATTERNS.to_string());
    ch.query_rows_with(sql.text(), &sql.params(), &PATTERN_SETTINGS)
        .await
        .map_err(search::user_error)
}
```

Add a unit test `pattern_settings_extend_the_read_settings` asserting that `PATTERN_SETTINGS[..2] == search::READ_SETTINGS`. If `READ_SETTINGS` grew in the merged fix wave, build `PATTERN_SETTINGS` to contain all of it.

The `toString(pattern_hash) AS pattern_hash` alias shadows the column in `WHERE` and `GROUP BY`, as it already does in `search::ENTRY_COLUMNS`. ClickHouse compares the `pattern_hash IN ({p:UInt64})` filter with the alias by converting the bound numbers, which Part 1's `pattern_hash` search test pins. Grouping by the text form groups exactly as by the number. Keep the alias (the API field is `pattern_hash`), and keep `patterns_are_counted_ranked_and_confined_to_the_project`'s hash-filter assertion, which would catch a change in that behaviour.

Route in `routes/logs.rs`, registered **before** `/projects/{id}/logs/{log_id}` (axum matches literal segments before captures, but keep it adjacent for readers):

```rust
/// `GET /projects/{id}/logs/patterns?from&to&<search filters>`: the top
/// patterns of the window (viewer). Holds one of the user's query slots.
async fn log_patterns(State(state): State<AppState>, user: CurrentUser, Path(project_id): Path<Uuid>,
    RawQuery(raw): RawQuery) -> Result<Json<serde_json::Value>, AppError>
```

Order inside the handler: `require_project_role(.., "viewer")`, then parse window and filters (400), then `QueryPermit::acquire` (429), then `top_patterns`, which returns `json!({ "patterns": rows })`.

- [ ] **Step 8: Run the tests to verify they pass**

Run: engine command with `--test logs logs_patterns::`, then `--lib logs::`.
Expected: PASS.

- [ ] **Step 9: Write the failing entry-cap tests (R-P2-23)**

`src/logs/query/slots.rs` `mod tests` (create the file with `todo!()` bodies first):

```rust
use super::*;

#[test]
fn slots_cap_each_user_and_come_back_on_drop() {
    let slots = Arc::new(UserSlots::default());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let first = slots.acquire(a, 2).unwrap();
    let _second = slots.acquire(a, 2).unwrap();
    assert!(matches!(slots.acquire(a, 2), Err(AppError::RateLimited)));
    let _other = slots.acquire(b, 2).unwrap();
    drop(first);
    assert_eq!(slots.open(a), 1);
    assert!(slots.acquire(a, 2).is_ok());
}
```

Append to `tests/logs/logs_search.rs` (it needs no `fixture`; every import it uses, and the file's `encode` helper, are already there):

```rust
#[sqlx::test]
async fn entry_reads_do_not_compete_with_searches(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!({ "message": "one" })).await;
    let (_, page) = call(&app, "GET", &format!("/projects/{project}/logs"), Some(&viewer), None).await;
    let row = &page["logs"][0];
    let entry = format!("/projects/{project}/logs/{}?dt={}", row["id"].as_str().unwrap(), encode(row["dt"].as_str().unwrap()));

    // Every search slot taken (a page, a histogram, a load-more, ...): entry reads still work.
    let searches: Vec<_> = (0..nami_engine::logs::search::MAX_QUERIES_PER_USER)
        .map(|_| nami_engine::logs::search::QueryPermit::acquire(&logs, user).unwrap())
        .collect();
    assert_eq!(call(&app, "GET", &entry, Some(&viewer), None).await.0, StatusCode::OK);
    drop(searches);

    // Entry reads have their own cap.
    let entries: Vec<_> = (0..nami_engine::logs::search::MAX_ENTRY_READS_PER_USER)
        .map(|_| logs.entry_slots.acquire(user, nami_engine::logs::search::MAX_ENTRY_READS_PER_USER).unwrap())
        .collect();
    assert_eq!(call(&app, "GET", &entry, Some(&viewer), None).await.0, StatusCode::TOO_MANY_REQUESTS);
    drop(entries);
    assert_eq!(call(&app, "GET", &entry, Some(&viewer), None).await.0, StatusCode::OK);
    ch.drop_database().await.unwrap();
}
```

- [ ] **Step 10: Run them to verify they fail**

Run: `cargo test --lib logs::query::slots` and the engine command with `--test logs logs_search::`.
Expected: FAIL. The first panics at `todo!()`. The second gets 429 while the search slots are held, because entry reads still take a `QueryPermit`.

- [ ] **Step 11: Implement `slots.rs` and switch the entry route**

`UserSlots`: the same counting as `search::QuerySlots`, behind `Mutex<HashMap<Uuid, usize>>`, with `PoisonError::into_inner` and the entry removed at zero. The permit holds an `Arc<UserSlots>`. `LogsState` gains `pub entry_slots: Arc<slots::UserSlots>`. In `log_entry`, replace `QueryPermit::acquire(..)` with `state.logs.entry_slots.acquire(user.id, search::MAX_ENTRY_READS_PER_USER)?`. In the spec's Search section, change "one user may have at most 4 searches, histograms and entry reads running (→ 429)" to "one user may have at most 4 searches and histograms, and 8 entry reads, running (→ 429)". Run Step 10's commands again: PASS.

- [ ] **Step 12: Run the full engine suite and clippy**

Run: engine command without `--test` (all binaries), then `cargo clippy --all-targets -- -D warnings`.
Expected: all green, no warnings.

- [ ] **Step 13: Commit**

```bash
git add nami-engine/src/logs/query/pattern.rs nami-engine/src/logs/query/patterns.rs nami-engine/src/logs/query/slots.rs nami-engine/src/logs/query/search.rs \
  nami-engine/src/logs/mod.rs nami-engine/src/logs/query/routes.rs nami-engine/tests/logs/logs_patterns.rs nami-engine/tests/logs/logs_search.rs \
  docs/superpowers/specs/2026-10-13-log-management-design.md
git commit -m "feat(engine): top message patterns, IPv6 pattern fix, own cap for entry reads (NAMI-LOG-9)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10 (NAMI-LOG-10): Ingest pipeline rules, compiler and editor API (done, PR #63)

Merged. Kept as the record of what landed; do not re-run its steps.

**Files:**
- Create: the `sources.pipeline` migration (now folded into the baseline `nami-engine/migrations/0005_logs.sql`), `nami-engine/src/logs/ingest/pipeline.rs`
- Modify: `nami-engine/src/logs/query/mod.rs` (extract `pub fn parse_path`), `nami-engine/src/logs/mod.rs`, `nami-engine/src/logs/source/model.rs` (pipeline read/write, `ingest_source`, monotonic `updated_at` in `update_in_project` and `rotate_token`), `nami-engine/src/logs/source/routes.rs` (3 routes), `nami-engine/tests/core/migrations.rs`, `docs/superpowers/specs/2026-10-13-log-management-design.md` (the regex line gains "at most 20 per pipeline", R-P2-26)
- Test: `nami-engine/tests/logs/logs_pipeline_api.rs` (+ unit tests in `pipeline.rs`)

**Interfaces:**
- Consumes: `normalize::{LogEvent, from_json, level_from_name}`, `store::build_row_at`, `logs::source::model::IngestSource`.
- Produces (used by T11, T16):

```rust
// logs::query
pub fn parse_path(raw: &str) -> Result<Vec<String>, String>; // the filter path rule, now shared

// logs::ingest::pipeline
pub const MAX_RULES: usize = 50;
pub const MAX_REGEX_CHARS: usize = 256;
pub const REGEX_SIZE_LIMIT: usize = 256 * 1024;      // R-P2-26
pub const REGEX_DFA_SIZE_LIMIT: usize = 256 * 1024;  // R-P2-26
pub const MAX_REGEXES: usize = 20;                   // per pipeline, R-P2-26
pub const MAX_RULE_ITEMS: usize = 50;      // paths of remove_fields, keys of redact
pub const MAX_KEY_CHARS: usize = 64;
pub const MAX_EQUALS_CHARS: usize = 1024;
pub const SAMPLE_RANGE: std::ops::RangeInclusive<u32> = 2..=10_000;
pub const MAX_TEST_EVENTS: usize = 100;
pub const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind { Drop, RemoveFields, Redact, Rename, Sample }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub equals: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub message_regex: Option<String>,
}

/// One rule as stored and sent (R-P2-2): a flat object keyed by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(rename = "type")] pub kind: RuleKind,
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")] pub when: Option<Match>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub n: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rules[{index}]: {message}")]
pub struct PipelineError { pub index: usize, pub message: String }

pub enum Outcome { Keep(LogEvent), Dropped, SampledOut }

pub struct CompiledPipeline { /* steps + one AtomicU64 per sample rule */ }
impl CompiledPipeline {
    pub fn empty() -> CompiledPipeline;
    pub fn is_empty(&self) -> bool;
    pub fn apply(&self, event: LogEvent) -> Outcome;
}
impl std::fmt::Debug for CompiledPipeline { /* rule count only */ }

/// `value` must be a JSON array of at most MAX_RULES rule objects.
pub fn parse_rules(value: &serde_json::Value) -> Result<Vec<Rule>, PipelineError>;
pub fn compile(rules: &[Rule]) -> Result<CompiledPipeline, PipelineError>;

// logs::source::model
pub async fn get_pipeline(pool: &PgPool, project_id: Uuid, source_id: Uuid)
    -> Result<Option<(serde_json::Value, DateTime<Utc>)>, sqlx::Error>;
/// Replaces the rules and bumps `updated_at`; `None` when the project has no such source.
pub async fn set_pipeline(pool: &PgPool, project_id: Uuid, source_id: Uuid, rules: &serde_json::Value)
    -> Result<Option<DateTime<Utc>>, sqlx::Error>;
/// The ingest view of one source of the project (the preview builds rows with it).
pub async fn ingest_source(pool: &PgPool, project_id: Uuid, source_id: Uuid)
    -> Result<Option<IngestSource>, sqlx::Error>;
```

- Routes (added to `router()` in `nami-engine/src/logs/source/routes.rs`): `GET /projects/{id}/sources/{source_id}/pipeline` (viewer) → `{"rules": [...], "updated_at": "..."}`. `PUT` the same path (member), body `{"rules": [...]}` → 200 with the same shape. `POST /projects/{id}/sources/{source_id}/pipeline/test` (member), body `{"rules": [...], "events": [{...}]}` → `{"results": [{"outcome": "kept", "event": {dt, level, message, fields, trace_id, span_id, pattern}} | {"outcome": "dropped"} | {"outcome": "sampled_out"}]}`.

- [ ] **Step 1: Write the migration and its failing migration test**

The `sources.pipeline` migration as written then (its statement is now part of the baseline `nami-engine/migrations/0005_logs.sql`):

```sql
-- Per-source ingest pipeline (Logs part 2): an ordered list of at most 50
-- rules, replaced as a whole by PUT .../pipeline and validated by the engine
-- (logs::ingest::pipeline::compile) before it is stored. The engine caches compiled
-- rules per source keyed on sources.updated_at, so every write here must bump
-- updated_at (logs::source::model::set_pipeline does).
alter table sources
    add column pipeline jsonb not null default '[]'::jsonb
        check (jsonb_typeof(pipeline) = 'array' and jsonb_array_length(pipeline) <= 50);
```

Add to `tests/core/migrations.rs`. That file has no `mod common;` (it tests the schema with plain SQL), so the test seeds its rows with SQL too. The `seed_source_row` helper is reused by the T13 and T14 migration tests:

```rust
/// A project and one source in it, inserted with plain SQL (no engine code).
async fn seed_source_row(pool: &sqlx::PgPool) -> (uuid::Uuid, uuid::Uuid) {
    let project: uuid::Uuid = sqlx::query_scalar(
        "insert into projects (name, slug) values ('p', gen_random_uuid()::text) returning id")
        .fetch_one(pool).await.unwrap();
    let source: uuid::Uuid = sqlx::query_scalar(
        "insert into sources (project_id, name, token_hash, token_enc)
         values ($1, 'api', md5(gen_random_uuid()::text), '\\x00'::bytea) returning id")
        .bind(project).fetch_one(pool).await.unwrap();
    (project, source)
}

#[sqlx::test]
async fn sources_have_an_empty_pipeline_by_default_and_reject_non_arrays(pool: sqlx::PgPool) {
    let (_, source) = seed_source_row(&pool).await;
    let pipeline: serde_json::Value = sqlx::query_scalar("select pipeline from sources where id = $1")
        .bind(source).fetch_one(&pool).await.unwrap();
    assert_eq!(pipeline, serde_json::json!([]));
    for bad in [r#"{}"#, r#""x""#] {
        let result = sqlx::query("update sources set pipeline = $1::jsonb where id = $2")
            .bind(bad).bind(source).execute(&pool).await;
        assert!(result.is_err(), "{bad} must be refused");
    }
    let too_many = serde_json::Value::Array(vec![serde_json::json!({"type":"drop"}); 51]);
    assert!(sqlx::query("update sources set pipeline = $1 where id = $2")
        .bind(too_many).bind(source).execute(&pool).await.is_err());
}
```

(Check `uuid` and `serde_json` are usable from that test binary: both are regular dependencies of the crate, so they are.)

- [ ] **Step 2: Run it to verify it fails, then passes after the migration file exists**

Run: engine command with `--test core migrations::`.
Expected: FAIL before the file exists (`column "pipeline" does not exist`). PASS after.

- [ ] **Step 3: Write the failing pipeline unit tests** (`mod tests` in `src/logs/ingest/pipeline.rs`; create the file with the public items above as `todo!()` bodies so it compiles)

```rust
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{Value, json};

use super::*;
use crate::logs::ingest::normalize::from_json;

fn event(value: Value) -> LogEvent {
    from_json(value.as_object().unwrap().clone(), Utc::now())
}

fn rules(value: Value) -> CompiledPipeline {
    compile(&parse_rules(&value).unwrap()).unwrap()
}

fn kept(outcome: Outcome) -> LogEvent {
    match outcome {
        Outcome::Keep(event) => event,
        Outcome::Dropped => panic!("dropped"),
        Outcome::SampledOut => panic!("sampled out"),
    }
}

fn rejected(value: Value) -> PipelineError {
    match parse_rules(&value) {
        Err(error) => error,
        Ok(parsed) => compile(&parsed).err().expect("must be rejected"),
    }
}

#[test]
fn drop_matches_on_level_message_and_fields_and_ends_processing() {
    let p = rules(json!([
        {"type":"drop","match":{"field":"level","equals":"debug"}},
        {"type":"drop","match":{"field":"http.status","equals":200}},
        {"type":"drop","match":{"message_regex":"^healthz"}},
        {"type":"rename","from":"a","to":"b"}
    ]));
    assert!(matches!(p.apply(event(json!({"level":"DEBUG","message":"x"}))), Outcome::Dropped));
    assert!(matches!(p.apply(event(json!({"message":"x","http":{"status":200}}))), Outcome::Dropped));
    assert!(matches!(p.apply(event(json!({"message":"x","http":{"status":"200"}}))), Outcome::Dropped),
        "a number compares by its JSON text, like search's eq");
    assert!(matches!(p.apply(event(json!({"message":"healthz ok"}))), Outcome::Dropped));
    let e = kept(p.apply(event(json!({"message":"real","a":1}))));
    assert_eq!(e.fields.get("b"), Some(&json!(1)));
    assert!(e.fields.get("a").is_none());
}

#[test]
fn remove_fields_deletes_nested_keys_and_ignores_missing_ones() {
    let p = rules(json!([{"type":"remove_fields","paths":["user.email","password","nope.deeper"]}]));
    let e = kept(p.apply(event(json!({"message":"m","password":"x","user":{"email":"a@b","id":7}}))));
    assert_eq!(Value::Object(e.fields), json!({"user":{"id":7}}));
}

#[test]
fn redact_by_key_name_is_case_insensitive_at_any_depth() {
    let p = rules(json!([{"type":"redact","keys":["password","Authorization"]}]));
    let e = kept(p.apply(event(json!({
        "message":"login",
        "PassWord":"hunter2",
        "req":{"headers":{"authorization":"Bearer abc"},"list":[{"password":"x"}]}
    }))));
    assert_eq!(Value::Object(e.fields), json!({
        "PassWord":"[REDACTED]",
        "req":{"headers":{"authorization":"[REDACTED]"},"list":[{"password":"[REDACTED]"}]}
    }));
}

#[test]
fn redact_by_regex_rewrites_the_message_and_every_string_value() {
    let p = rules(json!([{"type":"redact","regex":"\\b\\d{4}-\\d{4}-\\d{4}-\\d{4}\\b"}]));
    let e = kept(p.apply(event(json!({
        "message":"card 4111-1111-1111-1111 declined",
        "card":{"number":"4111-1111-1111-1111","n":[ "4111-1111-1111-1111" ]},
        "amount": 41111111,
        "ok": true
    }))));
    assert_eq!(e.message, "card [REDACTED] declined");
    assert_eq!(e.fields["card"], json!({"number":"[REDACTED]","n":["[REDACTED]"]}));
    assert_eq!(e.fields["amount"], json!(41111111), "a number without a match is left alone");
    assert_eq!(e.fields["ok"], json!(true));

    // R-P2-5: a number is matched on its JSON text; one with a match becomes a string.
    let p = rules(json!([{"type":"redact","regex":"\\d{16}"}]));
    let e = kept(p.apply(event(json!({"message":"m","card":4111111111111111_u64,"deep":[{"pan":5500000000000004_u64}]}))));
    assert_eq!(e.fields["card"], json!("[REDACTED]"));
    assert_eq!(e.fields["deep"], json!([{"pan":"[REDACTED]"}]));
}

#[test]
fn rename_moves_values_overwrites_targets_and_handles_level_and_message() {
    let p = rules(json!([
        {"type":"rename","from":"msg2","to":"message"},
        {"type":"rename","from":"sev","to":"level"},
        {"type":"rename","from":"a.b","to":"c.d"},
        {"type":"rename","from":"missing","to":"x"}
    ]));
    let e = kept(p.apply(event(json!({"message":"old","msg2":"new","sev":"WARNING","a":{"b":1},"c":{"d":0,"e":2}}))));
    assert_eq!(e.message, "new");
    assert_eq!(e.level, "warn");
    assert_eq!(Value::Object(e.fields), json!({"a":{},"c":{"d":1,"e":2}}));

    let p = rules(json!([{"type":"rename","from":"message","to":"original"}]));
    let e = kept(p.apply(event(json!({"message":"keep me"}))));
    assert_eq!(e.message, "");
    assert_eq!(e.fields["original"], json!("keep me"));
}

#[test]
fn sample_keeps_the_first_of_every_n_matching_events() {
    let p = rules(json!([{"type":"sample","n":3,"match":{"field":"level","equals":"info"}}]));
    let outcomes: Vec<bool> = (0..7)
        .map(|_| matches!(p.apply(event(json!({"message":"m"}))), Outcome::Keep(_)))
        .collect();
    assert_eq!(outcomes, [true, false, false, true, false, false, true]);
    assert!(matches!(p.apply(event(json!({"message":"m","level":"error"}))), Outcome::Keep(_)),
        "events the match leaves out are not sampled");
    let all = rules(json!([{"type":"sample","n":2}]));
    assert!(matches!(all.apply(event(json!({"message":"a"}))), Outcome::Keep(_)));
    assert!(matches!(all.apply(event(json!({"message":"b"}))), Outcome::SampledOut));
}

#[test]
fn invalid_rules_are_rejected_with_their_index() {
    let long = "a".repeat(257);
    for (value, index, needle) in [
        (json!([{"type":"drop"}]), 0, "match"),
        (json!([{"type":"drop","match":{"field":"a"}}]), 0, "equals"),
        (json!([{"type":"drop","match":{"field":"a","equals":1,"message_regex":"x"}}]), 0, "either"),
        (json!([{"type":"rename","from":"a","to":"b"},{"type":"redact"}]), 1, "keys or regex"),
        (json!([{"type":"redact","regex":"("}]), 0, "regex"),
        (json!([{"type":"redact","regex":long}]), 0, "256"),
        (json!([{"type":"drop","match":{"message_regex":long}}]), 0, "256"),
        (json!([{"type":"sample","n":1}]), 0, "2"),
        (json!([{"type":"sample","n":10001}]), 0, "10000"),
        (json!([{"type":"remove_fields","paths":["level"]}]), 0, "level"),
        (json!([{"type":"remove_fields","paths":[]}]), 0, "paths"),
        (json!([{"type":"remove_fields","paths":["a..b"]}]), 0, "path"),
        (json!([{"type":"rename","from":"a","to":"b","n":2}]), 0, "n"),
        (json!([{"type":"drop","match":{"field":"a","equals":1},"extra":1}]), 0, "extra"),
        (json!([{"type":"explode"}]), 0, "variant"),
        (json!([{"type":"redact","keys":[""]}]), 0, "key"),
    ] {
        let error = rejected(value.clone());
        assert_eq!(error.index, index, "{value}: {error}");
        assert!(error.to_string().to_lowercase().contains(needle), "{value}: {error}");
    }
    let too_many = Value::Array(vec![json!({"type":"sample","n":2}); 51]);
    assert_eq!(rejected(too_many).index, 50);
    assert!(parse_rules(&json!({"type":"drop"})).is_err(), "not an array");
}

#[test]
fn a_regex_that_matches_the_empty_string_is_rejected() {
    for pattern in ["a*", "\\b", "(?:)", "x?"] {
        let error = rejected(json!([{"type":"redact","regex":pattern}]));
        assert!(error.message.contains("empty"), "{pattern}: {error}");
    }
}

#[test]
fn a_pipeline_holds_at_most_twenty_regexes() {
    let at_limit: Vec<Value> = (0..MAX_REGEXES).map(|n| json!({"type":"redact","regex":format!("secret{n}")})).collect();
    assert!(compile(&parse_rules(&Value::Array(at_limit.clone())).unwrap()).is_ok());
    let mut over = at_limit;
    over.push(json!({"type":"drop","match":{"message_regex":"x"}}));
    let error = rejected(Value::Array(over));
    assert_eq!(error.index, MAX_REGEXES, "{error}");
    assert!(error.message.contains("20"), "{error}");
    // A pattern whose compiled program passes the size limit is refused, without echoing it.
    let huge = rejected(json!([{"type":"redact","regex":"\\w{200}\\w{50}"}]));
    assert!(huge.message.contains("too complex") && !huge.message.contains("\\w"), "{huge}");
}

#[test]
fn catastrophic_patterns_run_in_linear_time() {
    // 64 KiB exactly: `from_json` truncates anything longer before the pipeline sees it.
    let message = format!("{}!", "a".repeat(64 * 1024 - 1));
    let p = rules(json!([
        {"type":"drop","match":{"message_regex":"(a+)+$"}},
        {"type":"redact","regex":"(a|aa)+b"}
    ]));
    let started = Instant::now();
    let e = kept(p.apply(event(json!({ "message": message, "f": "a".repeat(64 * 1024) }))));
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(e.message.ends_with('!'));
}

#[test]
fn rules_round_trip_through_json() {
    let value = json!([
        {"type":"drop","match":{"field":"level","equals":"debug"}},
        {"type":"remove_fields","paths":["a.b"]},
        {"type":"redact","keys":["token"],"regex":"secret-\\d+"},
        {"type":"rename","from":"a","to":"b"},
        {"type":"sample","n":10}
    ]);
    let parsed = parse_rules(&value).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    assert!(compile(&[]).unwrap().is_empty());
}
```

- [ ] **Step 4: Run them to verify they fail**

Run: `cargo test --lib logs::ingest::pipeline` (engine command).
Expected: FAIL (panics at `todo!()`).

- [ ] **Step 5: Implement `logs/pipeline.rs`**

First run `cargo add regex-syntax@$(cargo tree -i regex-syntax --depth 0 | awk '{print $2}' | tr -d v)` so the direct dependency pins the version already locked.

Compiled form (private), and the parts the tests do not determine:

```rust
enum Target { Level, Message, Field(Vec<String>) }

enum Condition {
    Equals { target: Target, text: String },
    MessageRegex(Regex),
}

enum Step {
    Drop(Condition),
    Remove(Vec<Vec<String>>),
    Redact { keys: Vec<String> /* lowercased */, regex: Option<Regex> },
    Rename { from: Target, to: Target },
    Sample { n: u64, when: Option<Condition>, counter: usize },
}

pub struct CompiledPipeline { steps: Vec<Step>, counters: Vec<AtomicU64> }

fn compile_regex(pattern: &str) -> Result<Regex, String> {
    if pattern.chars().count() > MAX_REGEX_CHARS {
        return Err(format!("regex must be at most {MAX_REGEX_CHARS} characters"));
    }
    // Neither message echoes the pattern: a regex can carry a secret's shape.
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => "regex is too complex (compiled size limit)".to_string(),
            _ => "regex is not a valid regular expression".to_string(),
        })
}
```

`compile` counts the regexes it compiles (`message_regex` and `redact.regex`) and fails on the rule holding number `MAX_REGEXES + 1`: `"a pipeline may hold at most 20 regular expressions"`.

Rules for `compile` (each failure is `PipelineError { index, message }`):
- `parse_rules`: not an array → index 0, `"rules must be a JSON array"`. More than `MAX_RULES` → index 50, `"at most 50 rules"`. Each element is deserialized on its own with `serde_json::from_value::<Rule>`, so an unknown `type` or key reports that element's index, with serde's message (it names the field).
- Per kind, the allowed keys: `drop`: `match` (required, else `"drop needs a match"`). `remove_fields`: `paths` (1..=50, else `"remove_fields needs 1 to 50 paths"`). `redact`: `keys` (≤ 50) and/or `regex`, at least one present, else `"redact needs keys or regex"`. `rename`: `from`, `to` (both required). `sample`: `n` (required, in `SAMPLE_RANGE`, message `"n must be between 2 and 10000"`), `match` optional. A key that is present but unused by the kind → `"<key> is not used by <kind> rules"`.
- `match`: exactly one of (`field` + `equals`) or `message_regex`, else `"match needs either field and equals, or message_regex"`. `equals` is a string, number or bool, ≤ `MAX_EQUALS_CHARS` of text. Its comparison text is the string itself or the JSON text otherwise.
- Paths go through `query::parse_path`. In `match.field`/`rename`, the exact strings `level` / `message` become `Target::Level`/`Target::Message`. In `remove_fields`, they are rejected: `"level and message cannot be removed; use rename or redact"`.
- `redact.keys`: 1..=64 chars, no `char::is_control`, else `"redact key must be 1-64 characters without control characters"`. Stored lowercased (`to_lowercase`).
- `redact.regex`: `compile_regex`, then reject when the pattern can match an empty string anywhere: `regex_syntax::Parser::new().parse(pattern)` and `hir.properties().minimum_len() == Some(0)` → `"redact regex must not match the empty string"`. A check like `is_match("")` is not enough: `\b` does not match `""` but matches zero-width between words. `message_regex` uses `compile_regex` only. A drop condition may match empty. Defense in depth: the redact replacement skips zero-width matches (it iterates `find_iter` and replaces only `m.start() < m.end()`), so a stored rule from before this check can never insert markers between characters.

`apply` walks the steps top to bottom: `Drop` → `Outcome::Dropped` on match. `Sample` (when its condition matches, or always without one) → `if counter.fetch_add(1, Ordering::Relaxed) % n != 0 { return SampledOut }`. `Remove` deletes each path from `event.fields` (walk objects; a non-object or missing step is a no-op; delete the last key). `Redact` with keys → recursive walk over `fields` (objects and arrays), replacing the value of every object key whose `to_lowercase()` is in `keys` by `Value::String(REDACTED.into())`. With a regex → the zero-width-skipping replacement over `event.message` and over every `Value::String` at any depth, assigning only when something was replaced, and over the JSON text of every `Value::Number` at any depth: a number with a match is replaced by `Value::String(<replaced text>)` (R-P2-5). `Rename` → `take(from)` then `put(to, value)`. `take`: Level → `std::mem::replace(&mut event.level, "info".into())` as `Value::String`; Message → `mem::take(&mut event.message)`; Field → remove from fields (missing → stop, no-op). `put`: Level → `normalize::level_from_name(text)`; Message → string or JSON text; Field → walk/create objects, replacing a non-object intermediate with `{}`, then insert. Recursion depth is bounded by the decoders (≤ 128 levels).

`Debug for CompiledPipeline` prints `CompiledPipeline { rules: <n> }`. Never print patterns. A regex can hold a secret's shape.

Extract `pub fn parse_path(raw: &str) -> Result<Vec<String>, String>` in `query.rs` from `parse_filter` (same checks and messages). `parse_filter` calls it. The existing query tests must still pass unchanged.

- [ ] **Step 6: Run the unit tests to verify they pass**

Run: `cargo test --lib logs::ingest::pipeline` and `cargo test --lib logs::query` (engine command).
Expected: PASS.

- [ ] **Step 7: Write the failing API tests** (`nami-engine/tests/logs/logs_pipeline_api.rs`)

```rust
// Pipeline editor API (`/projects/{id}/sources/{source_id}/pipeline`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use common::{app, call, logs::{seed_source, stored_rows}, seed_project, state_with_clickhouse, test_app, user_with_role};
use serde_json::json;

#[sqlx::test]
async fn members_replace_the_rules_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline");

    let (status, empty) = call(&app, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["rules"], json!([]));
    // Parsed, not compared as text: chrono prints 0, 3 or 6 fraction digits.
    let at = |v: &serde_json::Value| chrono::DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap()).unwrap();
    let before = at(&empty);

    let rules = json!([{"type":"redact","keys":["password"]},{"type":"sample","n":10}]);
    let (status, saved) = call(&app, "PUT", &uri, Some(&member), Some(json!({ "rules": rules }))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["rules"], rules);
    assert!(at(&saved) > before, "updated_at is bumped");
    // Saving the same rules again still moves updated_at forward (R-P2-6).
    let (_, again) = call(&app, "PUT", &uri, Some(&member), Some(json!({ "rules": rules }))).await;
    assert!(at(&again) > at(&saved));
    assert_eq!(call(&app, "GET", &uri, Some(&viewer), None).await.1["rules"], rules);

    let body = Some(json!({ "rules": [] }));
    assert_eq!(call(&app, "PUT", &uri, Some(&viewer), body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PUT", &uri, Some(&outsider), body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PUT", &uri, None, body).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn a_source_of_another_project_is_not_found(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/sources/{foreign}/pipeline");
    assert_eq!(call(&app, "GET", &uri, Some(&member), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "PUT", &uri, Some(&member), Some(json!({"rules": []}))).await.0, StatusCode::NOT_FOUND);
    let stored: serde_json::Value = sqlx::query_scalar("select pipeline from sources where id = $1")
        .bind(foreign).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, json!([]), "never written through another project's path");
}

#[sqlx::test]
async fn invalid_rules_are_400_naming_the_rule_and_nothing_is_stored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline");
    for (rules, needle) in [
        (json!([{"type":"sample","n":2},{"type":"redact","regex":"("}]), "rules[1]"),
        (json!([{"type":"redact","regex":"a".repeat(257)}]), "rules[0]"),
        (json!([{"type":"redact","regex":"a*"}]), "empty"),
        (json!(vec![json!({"type":"sample","n":2}); 51]), "rules[50]"),
        (json!({"type":"drop"}), "array"),
    ] {
        let (status, body) = call(&app, "PUT", &uri, Some(&member), Some(json!({ "rules": rules }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains(needle), "{body}");
    }
    let (status, _) = call(&app, "PUT", &uri, Some(&member), Some(json!({ "rules": [], "extra": 1 }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown body keys are refused by the extractor");
    assert_eq!(call(&app, "GET", &uri, Some(&member), None).await.1["rules"], json!([]));
}

#[sqlx::test]
async fn the_test_endpoint_previews_without_storing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline/test");
    let body = json!({
        "rules": [
            {"type":"drop","match":{"field":"level","equals":"debug"}},
            {"type":"redact","keys":["password"],"regex":"tok_[a-z0-9]+"},
            {"type":"sample","n":2,"match":{"message_regex":"^tick"}}
        ],
        "events": [
            {"level":"debug","message":"noise"},
            {"message":"login tok_abc123 for user 7","password":"hunter2"},
            {"message":"tick 1"},
            {"message":"tick 2"}
        ]
    });
    let (status, out) = call(&app, "POST", &uri, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let results = out["results"].as_array().unwrap();
    assert_eq!(results[0], json!({"outcome":"dropped"}));
    assert_eq!(results[1]["outcome"], "kept");
    assert_eq!(results[1]["event"]["message"], "login [REDACTED] for user 7");
    assert_eq!(results[1]["event"]["fields"], json!({"password":"[REDACTED]"}));
    assert_eq!(results[1]["event"]["pattern"], "login [REDACTED] for user <num>");
    assert_eq!(results[2]["outcome"], "kept");
    assert_eq!(results[3], json!({"outcome":"sampled_out"}));
    assert!(stored_rows(&ch, project).await.is_empty(), "the preview stores nothing");

    assert_eq!(call(&app, "POST", &uri, Some(&viewer), Some(body)).await.0, StatusCode::FORBIDDEN);
    let many = json!({ "rules": [], "events": vec![json!({"message":"m"}); 101] });
    assert_eq!(call(&app, "POST", &uri, Some(&member), Some(many)).await.0, StatusCode::BAD_REQUEST);
    let not_object = json!({ "rules": [], "events": ["text"] });
    let (status, err) = call(&app, "POST", &uri, Some(&member), Some(not_object)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["error"].as_str().unwrap().contains("events[0]"), "{err}");
    ch.drop_database().await.unwrap();
}
```

If the existing JSON extractor rejection in this codebase maps to 400 (check `logs::source::routes` create tests), change `UNPROCESSABLE_ENTITY` above to match the established status. Do not invent a new mapping.

- [ ] **Step 8: Run them to verify they fail**

Run: engine command with `--test logs logs_pipeline_api::`.
Expected: FAIL (routes return 404/405).

- [ ] **Step 9: Implement the model functions and routes**

`logs::source::model`:

```rust
pub async fn get_pipeline(pool: &PgPool, project_id: Uuid, source_id: Uuid)
    -> Result<Option<(serde_json::Value, DateTime<Utc>)>, sqlx::Error>
{
    sqlx::query_as("select pipeline, updated_at from sources where id = $1 and project_id = $2")
        .bind(source_id).bind(project_id).fetch_optional(pool).await
}

pub async fn set_pipeline(pool: &PgPool, project_id: Uuid, source_id: Uuid, rules: &serde_json::Value)
    -> Result<Option<DateTime<Utc>>, sqlx::Error>
{
    // clock_timestamp(), not now(): two writes in one transaction would
    // otherwise share an updated_at and the cache could keep the older rules.
    sqlx::query_scalar(
        "update sources set pipeline = $3, updated_at = greatest(clock_timestamp(), updated_at + interval '1 microsecond')
         where id = $1 and project_id = $2 returning updated_at")
        .bind(source_id).bind(project_id).bind(rules).fetch_optional(pool).await
}
```

In the same file, `update_in_project` and `rotate_token` change `updated_at = now()` to the same `greatest(clock_timestamp(), updated_at + interval '1 microsecond')` (R-P2-6: `now()` is the transaction start, so a patch that waited on the row lock could write an older `updated_at` than a pipeline save that committed meanwhile). `sources_api` keeps passing unchanged. `ingest_source` is `select id, project_id, retention_days, daily_byte_quota from sources where id = $1 and project_id = $2` (Task 11 adds `updated_at` to it with the other two lookups).

Routes (`routes/sources.rs`), with request bodies `#[serde(deny_unknown_fields)] struct PipelineBody { rules: serde_json::Value }` and `struct PipelineTestBody { rules: serde_json::Value, events: Vec<serde_json::Value> }`:
- GET: `require_project_role(viewer)`, then `get_pipeline`. `None` → `AppError::NotFound`.
- PUT: `require_project_role(member)`, then `pipeline::parse_rules` + `compile` in `spawn_blocking` (error → `AppError::Validation(e.to_string())`), then `set_pipeline` with `serde_json::to_value(&parsed)` (the canonical form). `None` → 404.
- POST test: `require_project_role(member)`, then `logs::source::model::ingest_source(pool, project_id, source_id)` (404 when absent). `events.len() > MAX_TEST_EVENTS` → 400 `"at most 100 sample events"`. A non-object event → 400 `"events[i] must be a JSON object"` (checked before any CPU work). Then one `spawn_blocking` task compiles the rules (400) and, for each event, runs `from_json(object, now)`, `apply`, and on `Keep` `store::build_row_at(event, &source, now)`, serializing `{dt, level, message, fields: <parsed object>, trace_id, span_id, pattern}`. Up to 100 events of 2 MiB against 20 regexes is real CPU, which must not run on an async worker.

- [ ] **Step 10: Run the tests to verify they pass**

Run: engine command with `--test logs logs_pipeline_api::`, `--test core migrations::`, `--test logs sources_api::`.
Expected: PASS.

- [ ] **Step 11: Full engine suite + clippy, then commit**

Run: engine command (all), `cargo clippy --all-targets -- -D warnings`.

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/migrations nami-engine/src/logs/ingest/pipeline.rs \
  nami-engine/src/logs/query/mod.rs nami-engine/src/logs/mod.rs nami-engine/src/logs/source/model.rs \
  nami-engine/src/logs/source/routes.rs nami-engine/tests/logs/logs_pipeline_api.rs nami-engine/tests/core/migrations.rs \
  docs/superpowers/specs/2026-10-13-log-management-design.md
git commit -m "feat(engine): ingest pipeline rules with validation and preview API (NAMI-LOG-10)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 11 (NAMI-LOG-11): Run the pipeline on every ingest path (done, PR #67)

Merged. Kept as the record of what landed; do not re-run its steps.

**Files:**
- Create: `nami-engine/src/logs/ingest/pipeline_cache.rs`
- Modify: `nami-engine/src/logs/source/model.rs` (`IngestSource.updated_at`, `load_pipeline`), `nami-engine/src/logs/ingest/store.rs` (`RowBatch::push` runs the pipeline, `RowBatch::pipeline_counts`), `nami-engine/src/logs/ingest/mod.rs` (`prepare`/`build_batch`/`build_rows_within` take the pipeline), `nami-engine/src/logs/ingest/routes.rs` (look the pipeline up before `Work`), `nami-engine/src/logs/syslog/mod.rs` (pipelines per batch; `chunk_rows` moved into the blocking build, R-P2-24), `nami-engine/src/logs/ingest/usage.rs` (`record_pipeline`), `nami-engine/src/logs/mod.rs` (`LogsState.pipelines`)
- Test: `nami-engine/tests/logs/logs_pipeline_ingest.rs`; append to `nami-engine/tests/logs/logs_syslog.rs`; unit tests in `pipeline_cache.rs`, `store.rs`, `usage.rs`

**Interfaces:**
- Consumes: `pipeline::{parse_rules, compile, CompiledPipeline, Outcome}` (T10); the fix wave's shared step `store::RowBatch` (see the header table); `IngestLimits::queries` (Part 1).
- Produces:

```rust
// logs::source::model
pub struct IngestSource { pub id: Uuid, pub project_id: Uuid, pub retention_days: i32,
    pub daily_byte_quota: Option<i64>, pub updated_at: DateTime<Utc> }   // + updated_at
/// The stored rules of one source and the `updated_at` they belong to.
pub async fn load_pipeline(pool: &PgPool, source_id: Uuid)
    -> Result<Option<(serde_json::Value, DateTime<Utc>)>, sqlx::Error>;

// logs::ingest::pipeline_cache
pub const MAX_CACHED_PIPELINES: usize = 1024;   // R-P2-6, R-P2-26
#[derive(Debug, Default)] pub struct PipelineCache { /* Mutex<HashMap<Uuid, (DateTime<Utc>, Arc<CompiledPipeline>)>> */ }
#[derive(Debug, thiserror::Error)]
pub enum PipelineLoadError { #[error("database error: {0}")] Database(#[from] sqlx::Error),
    #[error("the source's stored pipeline does not compile: {0}")] Invalid(String) }
impl PipelineCache {
    /// The cached rules when they belong to `updated_at` exactly.
    pub fn get(&self, source_id: Uuid, updated_at: DateTime<Utc>) -> Option<Arc<CompiledPipeline>>;
    pub fn put(&self, source_id: Uuid, updated_at: DateTime<Utc>, pipeline: Arc<CompiledPipeline>);
    /// get, or load + compile + put (stored at the `updated_at` the load returned).
    /// `db_permits` (HTTP: `IngestLimits::queries`; syslog: `None`) guards only
    /// the Postgres read; the compile runs in `spawn_blocking` after the permit
    /// is released.
    pub async fn for_source(&self, pool: &PgPool, source: &IngestSource, db_permits: Option<&Semaphore>)
        -> Result<Arc<CompiledPipeline>, PipelineLoadError>;
}

// logs::LogsState gains `pub pipelines: pipeline_cache::PipelineCache`

// logs::ingest::usage
impl UsageTracker { pub fn record_pipeline(&self, source_id: Uuid, day: NaiveDate, dropped: u64, sampled_out: u64); }

// logs::ingest::store (the fix wave's shared step, extended)
/// Per source: (events dropped, events sampled out) by the pipeline.
pub type PipelineCounts = HashMap<Uuid, (u64, u64)>;
impl RowBatch {
    /// Runs `pipeline` on the event, then (when kept) adds its row as before.
    /// A dropped or sampled-out event is only counted.
    pub fn push(&mut self, event: LogEvent, source: &IngestSource, pipeline: &CompiledPipeline)
        -> Result<(), StoredTooLarge>;
    pub fn pipeline_counts(&self) -> &PipelineCounts;
}
// logs::ingest: build_batch / build_rows_within / prepare gain `pipeline: &CompiledPipeline`
// (prepare: `Arc<CompiledPipeline>`, moved into its blocking task); `Batch` and
// `Prepared` both gain `pipeline: PipelineCounts` (Work::store reads it from `Prepared`).
```

- [ ] **Step 1: Re-read the shared step on the current `main`**

Check `store::RowBatch`, `ingest::row_batch` and the syslog batcher (`logs/syslog/mod.rs`, `store`) against the header table. A rename since `94f9362` is mapped minimally and noted in the PR. The rule stays: the pipeline runs inside `RowBatch::push`, between the event and `build_row`, the one place both paths turn an event into a row.

- [ ] **Step 2: Write the failing unit tests**

In `pipeline_cache.rs`:

```rust
#[test]
fn an_entry_is_used_only_for_its_own_updated_at() {
    let cache = PipelineCache::default();
    let id = Uuid::new_v4();
    let t0 = Utc::now();
    let t1 = t0 + chrono::Duration::microseconds(1);
    cache.put(id, t0, Arc::new(CompiledPipeline::empty()));
    assert!(cache.get(id, t0).is_some());
    assert!(cache.get(id, t1).is_none(), "an edited source misses");
    assert!(cache.get(Uuid::new_v4(), t0).is_none());
}

#[test]
fn the_cache_is_cleared_when_full() {
    let cache = PipelineCache::default();
    let t = Utc::now();
    for _ in 0..MAX_CACHED_PIPELINES {
        cache.put(Uuid::new_v4(), t, Arc::new(CompiledPipeline::empty()));
    }
    let last = Uuid::new_v4();
    cache.put(last, t, Arc::new(CompiledPipeline::empty()));
    assert_eq!(cache.len(), 1, "cleared, then the new entry stored");
    assert!(cache.get(last, t).is_some());
}
```

(`pub fn len(&self) -> usize` is added for this test.)

In `store.rs` `mod tests` (the module's helper is `source(retention_days)`; add `use crate::logs::{normalize::{self, from_json}, pipeline::{compile, parse_rules, CompiledPipeline}}; use serde_json::json;` as far as they are not imported yet):

```rust
#[test]
fn the_pipeline_runs_between_the_event_and_the_row() {
    let source = source(30);
    let pipeline = compile(&parse_rules(&json!([
        {"type":"drop","match":{"field":"level","equals":"debug"}},
        {"type":"redact","keys":["secret"],"regex":"tok_[a-z0-9]+"},
        {"type":"sample","n":2,"match":{"message_regex":"^tick"}}
    ])).unwrap()).unwrap();
    let events = [
        json!({"level":"debug","message":"noise"}),
        json!({"message":"use tok_abc1 now","secret":"s3cr3t","nested":{"SECRET":"x"}}),
        json!({"message":"tick 1"}),
        json!({"message":"tick 2"}),
    ]
    .map(|v| from_json(v.as_object().unwrap().clone(), Utc::now()));
    let mut batch = RowBatch::new(usize::MAX);
    for event in events {
        batch.push(event, &source, &pipeline).unwrap();
    }
    assert_eq!(batch.pipeline_counts()[&source.id], (1, 1));
    let encoded = batch.encode(usize::MAX).unwrap();
    assert_eq!(encoded.events, 2);
    assert_eq!(encoded.per_source[&source.id].0, 2);
    assert!(!encoded.body.contains("tok_abc1") && !encoded.body.contains("s3cr3t"), "{}", encoded.body);
    assert!(encoded.body.contains(r#""pattern":"use [REDACTED] now""#), "{}", encoded.body);
}

#[test]
fn redaction_that_grows_a_message_is_capped_again() {
    let source = source(30);
    let pipeline = compile(&parse_rules(&json!([{"type":"redact","regex":"\\d"}])).unwrap()).unwrap();
    let digits = "7".repeat(60 * 1024);
    let event = from_json(json!({ "message": digits, "f": "1".repeat(200 * 1024) }).as_object().unwrap().clone(), Utc::now());
    let mut batch = RowBatch::new(usize::MAX);
    batch.push(event, &source, &pipeline).unwrap();
    let encoded = batch.encode(usize::MAX).unwrap();
    let row: serde_json::Value = serde_json::from_str(encoded.body.lines().next().unwrap()).unwrap();
    let message = row["message"].as_str().unwrap();
    assert!(message.len() <= normalize::MAX_MESSAGE_BYTES, "{}", message.len());
    assert!(message.ends_with(normalize::TRUNCATION_MARKER));
    assert!(row["fields"].as_str().unwrap().len() <= normalize::MAX_FIELDS_BYTES + 1024);
}
```

(`source(30)` is `store.rs`'s existing helper. It gains `updated_at: Utc::now()` with the struct.)

In `usage.rs`:

```rust
#[test]
fn pipeline_outcomes_are_counted() {
    let tracker = UsageTracker::default();
    let (id, day) = (Uuid::new_v4(), Utc::now().date_naive());
    tracker.record_pipeline(id, day, 3, 5);
    tracker.record_pipeline(id, day, 1, 0);
    let pending = tracker.pending(id, day);
    assert_eq!((pending.events_dropped, pending.events_sampled_out, pending.events), (4, 5, 0));
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test --lib logs::` (engine command).
Expected: compile errors (`PipelineCache`, `record_pipeline` and the new `push` argument do not exist yet).

- [ ] **Step 4: Implement the cache, the model change, the step change and the usage counter**

- `IngestSource` gains `updated_at`. `find_by_token`, `find_by_token_hashes` and T10's `ingest_source` add `updated_at` to their select lists (`IngestSource` is a `FromRow`; the hashes query flattens it). Update every `IngestSource { .. }` literal in tests (`updated_at: Utc::now()`).
- `load_pipeline`: `select pipeline, updated_at from sources where id = $1`.
- `PipelineCache::for_source`: on `get` hit return it. Otherwise `load_pipeline` (holding a permit of `db_permits` when given, released right after the query) → `None` (source deleted meanwhile) → `Arc::new(CompiledPipeline::empty())` and nothing cached. `Some((rules, at))` → `parse_rules` + `compile` inside `tokio::task::spawn_blocking` (an error → `PipelineLoadError::Invalid(e.to_string())`; a join error → `Invalid` too, so the source fails closed), then `put(source.id, at, ..)` and return. `put` clears the map first when it already holds `MAX_CACHED_PIPELINES` entries.
- `RowBatch::push`: `match pipeline.apply(event)`: `Keep(event)` → the existing body (`build_row`, stored cap). `Dropped` / `SampledOut` → bump `pipeline_counts[source.id]`, no row, `Ok(())`. An empty pipeline costs nothing (`apply` on no steps returns `Keep` at once). Every caller of `push` passes a pipeline. Tests and other callers with no rules pass `&CompiledPipeline::empty()`.
- `UsageTracker::record_pipeline` → `record` with `events_dropped`/`events_sampled_out`.

- [ ] **Step 5: Run the unit tests to verify they pass**

Run: `cargo test --lib logs::` (engine command).
Expected: PASS.

- [ ] **Step 6: Write the failing ingest tests** (`nami-engine/tests/logs/logs_pipeline_ingest.rs`)

```rust
// The source pipeline on HTTP ingestion. Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use common::{app, call, logs::{post_json, seed_source, stored_rows}, public_app, seed_project, state_with_clickhouse, user_with_role};
use serde_json::{Value, json};

async fn set_rules(app: &axum::Router, cookie: &axum::http::HeaderValue, project: uuid::Uuid, source: uuid::Uuid, rules: Value) {
    let uri = format!("/projects/{project}/sources/{source}/pipeline");
    let (status, body) = call(app, "PUT", &uri, Some(cookie), Some(json!({ "rules": rules }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[sqlx::test]
async fn a_redacted_value_never_reaches_clickhouse(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(&app, &member, project, source, json!([
        {"type":"redact","keys":["password","api_key"],"regex":"sk_live_[A-Za-z0-9]+"}
    ])).await;

    let (status, body) = post_json(&public, &token, json!({
        "message": "charging with sk_live_ABC123 for 'sk_live_QUOTED9'",
        "PASSWORD": "hunter2",
        "deep": { "Api_Key": "k-1", "list": [ { "password": "p" }, "sk_live_INARRAY" ] }
    })).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 1 })));
    let rows = stored_rows(&ch, project).await;
    let row = &rows[0];
    let text = row.to_string();
    for secret in ["sk_live_ABC123", "sk_live_QUOTED9", "hunter2", "k-1", "sk_live_INARRAY", "\"p\""] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert_eq!(row["message"], "charging with [REDACTED] for '[REDACTED]'");
    assert!(!row["pattern"].as_str().unwrap().contains("sk_live"), "{}", row["pattern"]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn drops_and_samples_are_counted_and_not_accepted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(&app, &member, project, source, json!([
        {"type":"drop","match":{"field":"level","equals":"debug"}},
        {"type":"sample","n":2}
    ])).await;
    let events: Vec<Value> = (0..4).map(|n| json!({ "message": format!("m{n}") }))
        .chain([json!({ "level": "debug", "message": "noise" })]).collect();
    let (status, body) = post_json(&public, &token, json!(events)).await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "accepted": 2 })));
    assert_eq!(stored_rows(&ch, project).await.len(), 2);
    let pending = logs.usage.pending(source, chrono::Utc::now().date_naive());
    assert_eq!((pending.events, pending.events_dropped, pending.events_sampled_out), (2, 1, 2));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_pipeline_change_applies_to_the_next_request(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    post_json(&public, &token, json!({ "message": "secret-1" })).await; // cached: empty rules
    set_rules(&app, &member, project, source, json!([{"type":"redact","regex":"secret-\\d"}])).await;
    post_json(&public, &token, json!({ "message": "secret-2" })).await;
    set_rules(&app, &member, project, source, json!([])).await;
    post_json(&public, &token, json!({ "message": "secret-3" })).await;
    let mut messages: Vec<String> = stored_rows(&ch, project).await.iter()
        .map(|r| r["message"].as_str().unwrap().to_string()).collect();
    messages.sort();
    assert_eq!(messages, ["[REDACTED]", "secret-1", "secret-3"]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_stored_pipeline_that_does_not_compile_fails_closed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, other_token) = seed_source(&pool, project, "other").await;
    // Written around the API (as a future incompatible change could leave it).
    sqlx::query("update sources set pipeline = '[{\"type\":\"redact\",\"regex\":\"(\"}]'::jsonb, updated_at = clock_timestamp() where id = $1")
        .bind(source).execute(&pool).await.unwrap();
    let (status, body) = post_json(&public, &token, json!({ "message": "password=hunter2" })).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body, json!({ "error": "internal error" }));
    assert_eq!(post_json(&public, &other_token, json!({ "message": "fine" })).await.0, StatusCode::ACCEPTED,
        "other sources keep ingesting");
    let rows = stored_rows(&ch, project).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "fine");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_quota_counts_bytes_after_the_pipeline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(&app, &member, project, source, json!([{"type":"remove_fields","paths":["blob"]}])).await;
    post_json(&public, &token, json!({ "message": "m", "blob": "x".repeat(10_000) })).await;
    let pending = logs.usage.pending(source, chrono::Utc::now().date_naive());
    assert_eq!(pending.bytes, 1 + 2, "message \"m\" plus fields \"{{}}\"");
    ch.drop_database().await.unwrap();
}
```

Append to `nami-engine/tests/logs/logs_syslog.rs` (uses that file's helpers):

```rust
#[sqlx::test]
async fn syslog_frames_go_through_the_pipeline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "syslog").await;
    nami_engine::logs::source::model::set_pipeline(&pool, project, source, &json!([
        {"type":"drop","match":{"message_regex":"^noise"}},
        {"type":"redact","regex":"pw=\\S+"}
    ])).await.unwrap();
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    for message in ["noise 1", "login pw=hunter2 ok"] {
        tcp.write_all(format!("{}\n", frame(&token, message)).as_bytes()).await.unwrap();
    }
    rows_eventually(&ch, project, 1).await;
    // Both frames were in one batch; give a straggler time to show up anyway.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = stored_rows(&ch, project).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["message"], "login [REDACTED] ok");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn syslog_frames_of_a_source_whose_pipeline_fails_are_dropped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "syslog").await;
    sqlx::query("update sources set pipeline = '[{\"type\":\"redact\",\"regex\":\"(\"}]'::jsonb, updated_at = clock_timestamp() where id = $1")
        .bind(source).execute(&pool).await.unwrap();
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "pw=hunter2")).as_bytes()).await.unwrap();
    let stats = stats_eventually(&handle, |s| s.store_failed >= 1).await;
    assert_eq!(stats.store_failed, 1);
    assert!(stored_rows(&ch, project).await.is_empty());
    ch.drop_database().await.unwrap();
}
```

(Add any missing imports at the top of `logs_syslog.rs`: `serde_json::json`, `common::logs::stored_rows` are already used there. Check before adding.)

- [ ] **Step 7: Run them to verify they fail**

Run: engine command with `--test logs logs_pipeline_ingest::`, then `--test logs logs_syslog::`.
Expected: FAIL. Rules are stored but not applied (secrets present, `accepted` 5).

- [ ] **Step 8: Wire the pipeline into both paths**

- HTTP (`routes/logs_ingest.rs`): after the body is read and before building `Work`, resolve the pipeline, with the Postgres read under the same semaphore the quota read uses: `let pipeline = state.logs.pipelines.for_source(&state.db, &source, Some(&limits.queries)).await`. `Err(e)` → `tracing::error!(error = %e, source_id = %source.id, "log pipeline unavailable")` and `IngestError::Internal`. The `Arc<CompiledPipeline>` moves into `Work` and from there into `prepare`/`build_batch` (a new parameter `pipeline: Arc<CompiledPipeline>`, moved into the blocking task). In `Work::store`, after a successful insert (or with zero rows, when every event was dropped or sampled out: no insert is sent then), record `record_pipeline(source.id, day, dropped, sampled_out)` from `Prepared::pipeline`, next to `record_accepted`. A failed insert records neither (the client retries). The `202` count is the stored events (R-P2-8).
- Syslog batcher (`Batcher::store`): after the source lookup and the rate/quota filters, collect the distinct sources of the batch and resolve each with `state.logs.pipelines.for_source(&state.db, &source, None)` (one `load_pipeline` per cache miss). Frames of a source whose pipeline fails are removed from `allowed` and counted as `store_failed`, with one rate-limited warning (`self.warnings.warn`, never frame content). The blocking build task receives a `HashMap<Uuid, Arc<CompiledPipeline>>` and calls `rows.push(event, &source, &pipelines[&source.id])`. R-P2-24: the same blocking task then runs `store::chunk_rows(rows.rows(), max_bytes)` and returns `(Arc<RowBatch>, runs, oversized, budget)`, so no row is serialized on the async task any more. The per-run `encode_run` + insert loop is unchanged. `record_pipeline` is recorded per source from `rows.pipeline_counts()` once the runs are done (the counts do not depend on the inserts).
- Add a syslog unit test `rows_are_chunked_inside_the_blocking_build` that asserts on the factored function. Extract the build closure into `fn build_and_chunk(allowed, pipelines, max_bytes) -> Result<Built, ClickHouseError>` and test it directly: 3 frames of which one is larger than `max_bytes` give 2 rows in runs and `oversized == 1`.
- Update the order-of-checks doc comment on `ingest_logs` and the module doc of `logs/mod.rs` (add `pipeline`, `pipeline_cache`).

- [ ] **Step 9: Run the tests to verify they pass**

Run: engine command with `--test logs logs_pipeline_ingest::`, `--test logs logs_syslog::`, `--test logs logs_ingest::`, `--test logs logs_quota::`.
Expected: PASS.

- [ ] **Step 10: Full engine suite + clippy, then commit**

```bash
git add nami-engine/src nami-engine/tests/logs/logs_pipeline_ingest.rs nami-engine/tests/logs/logs_syslog.rs
git commit -m "feat(engine): apply source pipelines to HTTP and syslog ingestion (NAMI-LOG-11)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12 (NAMI-LOG-12): Streamed CSV/NDJSON export (done, PR #75)

Merged. Kept as the record of what landed; do not re-run its steps.

**Files:**
- Modify: `nami-engine/src/core/clickhouse.rs` (`send` split out of `post`, `stream_rows_with`, `RowStream`, `ClickHouseError::LineTooLong`, `EXPORT_MAX_EXECUTION_SECS`, `STREAM_REQUEST_TIMEOUT`, reserved setting)
- Create: `nami-engine/src/logs/query/export.rs`
- Modify: `nami-engine/src/logs/query/mod.rs` (`pub fn push_field_text`, `pub(crate) fn last`/`values`), `nami-engine/src/logs/query/slots.rs` (`acquire_capped`, total count), `nami-engine/src/logs/mod.rs` (`export_slots`, `export_limits`), `nami-engine/src/logs/source/model.rs` (`names_for_project`), `nami-engine/src/logs/query/routes.rs` (route), `nami-engine/tests/core/clickhouse.rs` (reserved list), `docs/superpowers/specs/2026-10-13-log-management-design.md` (Export: "and 8 per engine instance"; the task ends at its 150 s deadline)
- Create (UI): `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/export/+server.ts`, `.../logs/export/server.test.ts`
- Modify (UI): `nami-ui/src/lib/server/logs/log-proxy.ts` (+ test): `EXPORT_PARAMS`
- Test: `nami-engine/tests/logs/logs_export.rs` (+ unit tests in `export.rs`, `clickhouse.rs`)

**Interfaces:**
- Consumes: `query::{parse_window, parse_filters, parse_path, push_where, push_window, Sql}`, `search::user_error`, fix-wave `read_capped`/`max_response_bytes`, `slots::UserSlots` (T9).
- Produces:

```rust
// clickhouse
pub const EXPORT_MAX_EXECUTION_SECS: u64 = 120;
pub const STREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(150);
pub struct RowStream { /* response, buffer: Vec<u8>, start, scanned, max_line, finished */ }
impl RowStream { pub async fn next_line(&mut self) -> Result<Option<Vec<u8>>, ClickHouseError>; }
impl ClickHouse {
    pub async fn stream_rows_with(&self, sql: &str, params: &[(&str, String)],
        settings: &[(&'static str, &'static str)], max_line_bytes: usize) -> Result<RowStream, ClickHouseError>;
}
// ClickHouseError::LineTooLong { limit: usize }

// logs::query::export
pub const MAX_EXPORT_ROWS: u64 = 100_000;
pub const MAX_EXPORT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_EXPORT_LINE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EXPORTS_PER_USER: usize = 2;
pub const MAX_EXPORTS_PER_INSTANCE: usize = 8;                       // R-P2-15
pub const EXPORT_DEADLINE: Duration = Duration::from_secs(150);      // R-P2-13
pub const MAX_EXPORT_COLUMNS: usize = 10;
pub const EXPORT_CHUNK_BYTES: usize = 64 * 1024;
pub const EXPORT_LIMIT_HEADER: &str = "x-nami-export-limit";
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ExportFormat { Csv, Ndjson }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportLimits { pub rows: u64, pub bytes: u64, pub line_bytes: usize, pub deadline: Duration } // Default = the consts
#[derive(Debug, Clone, PartialEq)]
pub struct ExportParams { pub window: Window, pub filters: LogFilters, pub format: ExportFormat, pub columns: Vec<Vec<String>> }
pub fn parse_export(pairs: &[(String, String)], now: DateTime<Utc>) -> Result<ExportParams, String>;
pub fn export_sql(project_id: Uuid, params: &ExportParams, limit: u64) -> Sql;
pub fn push_csv_field(out: &mut String, value: &str);
pub fn csv_record(fields: &[&str]) -> String;
pub fn file_name(format: ExportFormat, to: DateTime<Utc>) -> String;
pub async fn stream(rows: RowStream, params: ExportParams, sources: HashMap<Uuid, String>,
    limits: ExportLimits, tx: mpsc::Sender<Result<Bytes, std::convert::Infallible>>);

// logs::LogsState gains `pub export_slots: Arc<slots::UserSlots>`, `pub export_limits: export::ExportLimits`
// logs::query::slots
impl UserSlots {
    /// As `acquire`, and also refused (429) once `total` permits of every user are out.
    pub fn acquire_capped(self: &Arc<Self>, user_id: Uuid, per_user: usize, total: usize) -> Result<UserPermit, AppError>;
}
// logs::source::model
pub async fn names_for_project(pool: &PgPool, project_id: Uuid) -> Result<HashMap<Uuid, String>, sqlx::Error>;
// logs::query
pub fn push_field_text(sql: &mut Sql, path: &[String]); // binds each segment, pushes the eq-filter text expression
```

- Route: `GET /projects/{id}/logs/export?format=csv|ndjson&from&to&<search filters>&columns=a.b*` (viewer).

- [ ] **Step 1: Write the failing unit tests**

`src/logs/query/export.rs` `mod tests`:

```rust
use super::*;

#[test]
fn csv_fields_follow_rfc_4180() {
    assert_eq!(
        csv_record(&["a", "b,c", "say \"hi\"", "line\nbreak", ""]),
        "a,\"b,c\",\"say \"\"hi\"\"\",\"line\nbreak\",\r\n"
    );
}

#[test]
fn formula_cells_are_prefixed() {
    for (input, expected) in [
        ("=cmd|' /C calc'!A0", "'=cmd|' /C calc'!A0"),
        ("+1", "'+1"),
        ("-2", "'-2"),
        ("@SUM(1)", "'@SUM(1)"),
        ("\tx", "'\tx"),
        ("\rx", "\"'\rx\""),
        ("=1,2", "\"'=1,2\""),
        ("a=1", "a=1"),
        (" =1", " =1"),
    ] {
        let mut out = String::new();
        push_csv_field(&mut out, input);
        assert_eq!(out, expected, "{input:?}");
    }
}

#[test]
fn export_parameters_are_validated() {
    let now = Utc::now();
    let pairs = |q: &str| query::query_pairs(Some(q));
    assert!(parse_export(&pairs("format=csv"), now).is_ok());
    assert_eq!(parse_export(&pairs("format=ndjson"), now).unwrap().format, ExportFormat::Ndjson);
    for bad in ["", "format=xlsx", "format=csv&columns=a..b", "format=csv&level=loud",
                "format=csv&from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z"] {
        assert!(parse_export(&pairs(bad), now).is_err(), "{bad}");
    }
    let eleven: String = (0..11).map(|n| format!("&columns=c{n}")).collect();
    let error = parse_export(&pairs(&format!("format=csv{eleven}")), now).unwrap_err();
    assert!(error.contains("10"), "{error}");
}

#[test]
fn the_sql_holds_no_user_text() {
    let now = Utc::now();
    let params = parse_export(&query::query_pairs(Some(
        "format=csv&columns=user.name&columns=http-status&q=%27%3B%20DROP%20TABLE%20logs%3B%20--",
    )), now).unwrap();
    let project = Uuid::new_v4();
    let sql = export_sql(project, &params, MAX_EXPORT_ROWS);
    let text = sql.text();
    for user_text in ["user", "name", "http-status", "DROP", &project.to_string()] {
        assert!(!text.contains(user_text), "{user_text} in {text}");
    }
    assert!(text.contains(" AS c0") && text.contains(" AS c1") && !text.contains(" AS c2"));
    assert!(text.contains(" ORDER BY dt DESC, id DESC LIMIT {p"), "{text}");
    let values: Vec<String> = sql.params().into_iter().map(|(_, v)| v).collect();
    assert!(values.contains(&"user".to_string()) && values.contains(&"100000".to_string()));
}

#[test]
fn file_names_carry_no_user_text() {
    let to = DateTime::parse_from_rfc3339("2026-10-19T08:05:03.250Z").unwrap().with_timezone(&Utc);
    assert_eq!(file_name(ExportFormat::Csv, to), "nami-logs-20261019T080503Z.csv");
    assert_eq!(file_name(ExportFormat::Ndjson, to), "nami-logs-20261019T080503Z.ndjson");
}

#[test]
fn the_default_limits_are_the_spec_values() {
    assert_eq!(ExportLimits::default(), ExportLimits {
        rows: 100_000, bytes: 512 * 1024 * 1024, line_bytes: 4 * 1024 * 1024, deadline: Duration::from_secs(150),
    });
}
```

`src/logs/query/slots.rs` `mod tests` (append):

```rust
#[test]
fn a_total_caps_every_user_together() {
    let slots = Arc::new(UserSlots::default());
    let held: Vec<_> = (0..3).map(|_| slots.acquire_capped(Uuid::new_v4(), 2, 3).unwrap()).collect();
    assert!(matches!(slots.acquire_capped(Uuid::new_v4(), 2, 3), Err(AppError::RateLimited)));
    drop(held);
    assert!(slots.acquire_capped(Uuid::new_v4(), 2, 3).is_ok());
}
```

`src/core/clickhouse.rs` `mod tests` (wiremock is already a dev-dependency there, see the existing `query_rows_with` wiremock test):

```rust
#[tokio::test]
async fn streamed_rows_arrive_line_by_line_and_long_lines_are_refused() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{\"a\":1}\n\n{\"a\":2}\n{\"a\":\"xxxxxxxxxxxxxxxxxxxx\"}\n"))
        .mount(&server).await;
    let ch = ClickHouse::new(&server.uri(), "u", "p", "nami_test_stream");
    let mut rows = ch.stream_rows_with("SELECT 1", &[], &[], 16).await.unwrap();
    assert_eq!(rows.next_line().await.unwrap().unwrap(), b"{\"a\":1}");
    assert_eq!(rows.next_line().await.unwrap().unwrap(), b"{\"a\":2}");
    assert!(matches!(rows.next_line().await, Err(ClickHouseError::LineTooLong { limit: 16 })));
    let sent = &server.received_requests().await.unwrap()[0];
    let query: Vec<(String, String)> = sent.url.query_pairs().into_owned().collect();
    let has = |k: &str, v: &str| query.iter().any(|(n, x)| n == k && x == v);
    assert!(has("max_execution_time", "120") && has("readonly", "2")
        && has("http_write_exception_in_output_format", "1")
        && has("cancel_http_readonly_queries_on_client_close", "1"));
    assert!(!query.iter().any(|(n, _)| n == "wait_end_of_query"), "rows must stream");
}

#[tokio::test]
async fn a_streamed_query_error_is_returned_before_any_row() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("Code: 427. CANNOT_COMPILE_REGEXP"))
        .mount(&server).await;
    let ch = ClickHouse::new(&server.uri(), "u", "p", "nami_test_stream");
    let error = ch.stream_rows_with("SELECT 1", &[], &[], 1024).await.err().unwrap();
    assert!(matches!(error, ClickHouseError::Server { status: 400, .. }));
}
```

Also add `"http_write_exception_in_output_format"` to `RESERVED_SETTINGS` (length 11 → 12), and extend the existing reserved-settings drift test (`tests/core/clickhouse.rs`, the wiremock-derived names test) so the streamed request's owned names are in the list.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib -- logs::query::export logs::query::slots clickhouse::` (engine environment).
Expected: compile errors (items missing).

- [ ] **Step 3: Implement the ClickHouse stream**

Split `post` into `send(&self, pairs, body, timeout: Option<Duration>) -> Result<reqwest::Response, ClickHouseError>` (builds the URL, basic auth, `.timeout(t)` when given; reqwest's per-request timeout replaces the client's 30 s) and `post`, which is `send(.., None)` then the fix wave's capped read. `stream_rows_with` sends the base settings + database + extra settings + `default_format=JSONEachRow`, `readonly=2`, `max_execution_time=EXPORT_MAX_EXECUTION_SECS`, `cancel_http_readonly_queries_on_client_close=1`, `http_write_exception_in_output_format=1` and the escaped `param_*` pairs. It does **not** send `wait_end_of_query`, and uses `send(.., Some(STREAM_REQUEST_TIMEOUT))`. Non-2xx → read the body through `read_capped` → `Server { status, body: trimmed }`.

```rust
impl RowStream {
    /// The next non-empty line, without its newline; `None` at the end of the
    /// result. A line longer than `max_line` is refused without reading on.
    ///
    /// Lines are taken from `buffer[start..]` by moving `start`, never by
    /// draining the front of the Vec per line (a chunk holding thousands of
    /// short lines would be shifted thousands of times). The consumed front
    /// is dropped once per chunk, before the next one is appended.
    pub async fn next_line(&mut self) -> Result<Option<Vec<u8>>, ClickHouseError> {
        loop {
            let from = self.start + self.scanned;
            if let Some(offset) = self.buffer[from..].iter().position(|b| *b == b'\n') {
                let end = from + offset;
                if end - self.start > self.max_line {
                    return Err(ClickHouseError::LineTooLong { limit: self.max_line });
                }
                let line = self.buffer[self.start..end].to_vec();
                self.start = end + 1;
                self.scanned = 0;
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Ok(Some(line));
            }
            self.scanned = self.buffer.len() - self.start;
            if self.scanned > self.max_line {
                return Err(ClickHouseError::LineTooLong { limit: self.max_line });
            }
            if self.finished {
                let rest = self.buffer.split_off(self.start);
                self.start = 0;
                self.scanned = 0;
                return Ok((!rest.iter().all(u8::is_ascii_whitespace)).then_some(rest));
            }
            match self.response.chunk().await? {
                Some(chunk) => {
                    self.buffer.drain(..self.start);
                    self.start = 0;
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.finished = true,
            }
        }
    }
}
```

- [ ] **Step 4: Implement `export.rs` and the helpers**

- `query::push_field_text(sql, path)`: `let segments: Vec<usize> = path.iter().map(|s| sql.bind(s.clone())).collect(); push_text(sql, &segments);`. Make `last`/`values` `pub(crate)`.
- `parse_export`: `parse_window` and `parse_filters` (their messages), then `format` = last `format` (`csv`|`ndjson`, else `"format must be csv or ndjson"`), then `columns` = every `columns` value through `parse_path` (`"columns: <message>"`), more than `MAX_EXPORT_COLUMNS` → `"at most 10 columns"`.
- `export_sql`, with these fixed fragments:

```rust
const NDJSON_SELECT: &str = "SELECT id, project_id, source_id, dt, level, message, fields, trace_id, span_id, \
     pattern, toString(pattern_hash) AS pattern_hash, retention_days FROM logs";
const CSV_SELECT: &str = "SELECT dt, level, source_id, message, trace_id";
const COLUMN_ALIASES: [&str; MAX_EXPORT_COLUMNS] =
    [" AS c0", " AS c1", " AS c2", " AS c3", " AS c4", " AS c5", " AS c6", " AS c7", " AS c8", " AS c9"];
```

  The CSV variant is `CSV_SELECT`, then for each column `", "` + `push_field_text` + `COLUMN_ALIASES[i]`, then `" FROM logs"`. Both variants then take `push_where`, `push_window` and `" ORDER BY dt DESC, id DESC LIMIT "` + `value("UInt64", limit)`.
- `push_csv_field`: guard first (a leading `=`, `+`, `-`, `@`, `\t`, `\r` → prepend `'`), then quote when the guarded value contains `"`, `,`, `\r` or `\n` (inner `"` doubled). `csv_record` joins with `,` and ends with `\r\n`.
- `file_name`: `format!("nami-logs-{}.{}", to.format("%Y%m%dT%H%M%SZ"), ext)`.
- `stream(..)`: `let deadline = Instant::now() + limits.deadline`. CSV first writes the header record `dt, level, source, message, trace_id, <each column path joined by .>` (through `push_csv_field`, so the formula guard covers it: a column path may start with `-`). Then it loops on `tokio::select! { line = timeout_at(deadline, rows.next_line()) => line, _ = tx.closed() => return }`. A timeout → trailer `"the export reached its time limit: narrow the time window or add filters"`. Every `tx.send` is `timeout_at(deadline, tx.send(..))`, and a timeout or a closed channel returns at once (the client is gone or not reading, R-P2-13). The final trailer send gets at most 1 s past the deadline.
  - `Ok(None)` → end.
  - A line that does not start with the first selected column's key (`{"id":` NDJSON, `{"dt":` CSV) → never written. `tracing::warn!` with at most its first 200 bytes, then trailer `"the query failed while streaming: narrow the time window or add filters"`. This covers ClickHouse's `{"exception":…}` line, the `__exception__` block of newer servers, and anything else unexpected (R-P2-12).
  - `Err(LineTooLong)` → trailer `"a row is larger than an export may carry"`.
  - Another `Err` → `tracing::warn!`, trailer `"the export was interrupted: try again or narrow the time window"`.
  - A line → NDJSON: the line verbatim + `\n`. CSV: parse as `serde_json::Map`, take `dt`, `level`, `source_id` (mapped through `sources`, falling back to the id), `message`, `trace_id`, `c0..` as strings (missing → empty), and write `csv_record`. A line that does not parse → the "interrupted" trailer.
  - Before appending a row, `written + row.len() > limits.bytes` → trailer `"the export reached its 512 MiB size limit: narrow the time window or add filters"`. Stop after `limits.rows` rows; the SQL `LIMIT` already ends there.
  - Output accumulates in a buffer that is sent (`tx.send(Ok(Bytes))`) at `EXPORT_CHUNK_BYTES` and at the end. A failed send → return.
  - Trailer: NDJSON `{"_nami_export_error":"<reason>"}\n`. CSV `csv_record(&["# export stopped early: <reason>"])`. The trailer is always sent, even past the byte cap.
- `names_for_project`: `select id, name from sources where project_id = $1` into a map.
- Route handler `export_logs`, in this order: `require_project_role(viewer)` → `parse_export` (400) → `state.logs.export_slots.acquire_capped(user.id, MAX_EXPORTS_PER_USER, MAX_EXPORTS_PER_INSTANCE)` (429) → `names_for_project` → `export_sql(.., limits.rows)` → `stream_rows_with(.., &[], limits.line_bytes)` mapped by `search::user_error` (400/500 before any byte is sent) → `mpsc::channel(16)` → spawn `stream`, holding the permit (`let _permit = permit;`) → `Response` with `Body::from_stream(ReceiverStream::new(rx))` and the headers `content-type` (`text/csv; charset=utf-8` | `application/x-ndjson`), `content-disposition: attachment; filename="<file_name>"`, `x-nami-export-limit: <limits.rows>`, `cache-control: no-store`, `x-content-type-options: nosniff`, `x-accel-buffering: no`.

- [ ] **Step 5: Run the unit tests to verify they pass**

Run: `cargo test --lib -- logs::query::export logs::query::slots clickhouse::` (engine environment).
Expected: PASS.

- [ ] **Step 6: Write the failing integration tests** (`nami-engine/tests/logs/logs_export.rs`)

```rust
// Streamed export (`GET /projects/{id}/logs/export`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{sync::Arc, time::Duration};

use axum::{body::Body, http::{Request, StatusCode, header}};
use chrono::{SecondsFormat, Utc};
use common::{app, call, logs::{post_json, seed_source}, public_app, seed_project, state_with_clickhouse, user_with_role};
use nami_engine::{core::clickhouse::ClickHouse, logs::{LogsState, export::ExportLimits}, core::state::AppState};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn window() -> String {
    let to = Utc::now() + chrono::Duration::minutes(1);
    let from = to - chrono::Duration::hours(1);
    format!("from={}&to={}", encode(&from.to_rfc3339_opts(SecondsFormat::Millis, true)),
        encode(&to.to_rfc3339_opts(SecondsFormat::Millis, true)))
}

async fn plant(ch: &ClickHouse, project: Uuid, source: Uuid, count: u64) {
    // Test-only SQL with the ids formatted in: the ids are UUIDs this test made.
    ch.execute(&format!(
        "INSERT INTO logs (id, project_id, source_id, dt, level, message, fields, trace_id, span_id, pattern, retention_days) \
         SELECT generateUUIDv4(), toUUID('{project}'), toUUID('{source}'), now64(3) - toIntervalMillisecond(number), \
         'info', concat('row ', toString(number)), '{{}}', '', '', 'row <num>', 30 FROM numbers({count})"
    )).await.unwrap();
}

async fn get(app: &axum::Router, uri: &str, cookie: &axum::http::HeaderValue) -> axum::response::Response {
    app.clone().oneshot(Request::get(uri).header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await.unwrap()
}

async fn text(response: axum::response::Response) -> String {
    String::from_utf8(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
}

#[sqlx::test]
async fn ndjson_carries_every_column_and_csv_the_picked_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!([
        { "message": "=cmd|' /C calc'!A0", "user": { "name": "@SUM(1)" }, "n": 5 },
        { "message": "plain, \"quoted\"", "user": { "name": "+1" }, "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" },
        { "message": "-2" }
    ])).await;
    post_json(&public, &foreign, json!({ "message": "never exported" })).await;

    let response = get(&app, &format!("/projects/{project}/logs/export?format=ndjson&{}", window()), &viewer).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/x-ndjson");
    assert_eq!(headers["x-nami-export-limit"], "100000");
    assert_eq!(headers["cache-control"], "no-store");
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(regex::Regex::new(r#"^attachment; filename="nami-logs-\d{8}T\d{6}Z\.ndjson"$"#).unwrap().is_match(disposition), "{disposition}");
    let body = text(response).await;
    let lines: Vec<Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "never another project's rows: {body}");
    let mut keys: Vec<&str> = lines[0].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["dt", "fields", "id", "level", "message", "pattern", "pattern_hash", "project_id",
        "retention_days", "source_id", "span_id", "trace_id"]);

    let response = get(&app, &format!("/projects/{project}/logs/export?format=csv&columns=user.name&columns=n&{}", window()), &viewer).await;
    assert_eq!(response.headers()["content-type"], "text/csv; charset=utf-8");
    let csv = text(response).await;
    let mut records = csv.split("\r\n");
    assert_eq!(records.next().unwrap(), "dt,level,source,message,trace_id,user.name,n");
    assert!(csv.contains(",api,'=cmd|' /C calc'!A0,,'@SUM(1),5\r\n"), "{csv}");
    assert!(csv.contains(",api,\"plain, \"\"quoted\"\"\",4bf92f3577b34da6a3ce929d0e0e4736,'+1,\r\n"), "{csv}");
    assert!(csv.contains(",api,'-2,,,\r\n"), "{csv}");
    assert!(!csv.contains("never exported"));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_row_cap_holds_and_is_announced(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_005).await;
    let response = get(&app, &format!("/projects/{project}/logs/export?format=ndjson&{}", window()), &viewer).await;
    assert_eq!(response.headers()["x-nami-export-limit"], "100000");
    let body = text(response).await;
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 100_000);
    assert!(lines[0].contains("\"row 0\""), "newest first: {}", lines[0]);
    assert!(!body.contains("_nami_export_error"), "the row cap is a normal end");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn hitting_the_byte_cap_ends_with_an_error_trailer(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = LogsState { export_limits: ExportLimits { bytes: 4096, ..ExportLimits::default() }, ..LogsState::default() };
    let state = AppState { logs: Arc::new(logs), ..state };
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 500).await;
    for format in ["ndjson", "csv"] {
        let body = text(get(&app, &format!("/projects/{project}/logs/export?format={format}&{}", window()), &viewer).await).await;
        let last = body.trim_end().lines().last().unwrap();
        assert!(body.len() < 4096 + 256, "{format}: {}", body.len());
        match format {
            "ndjson" => assert!(last.starts_with("{\"_nami_export_error\":"), "{last}"),
            _ => assert!(last.starts_with("# export stopped early:"), "{last}"),
        }
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_third_concurrent_export_is_refused(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_000).await;
    let uri = format!("/projects/{project}/logs/export?format=ndjson&{}", window());
    // Two streams held open, their bodies unread (each is far larger than the channel).
    let first = get(&app, &uri, &viewer).await;
    let second = get(&app, &uri, &viewer).await;
    assert_eq!((first.status(), second.status()), (StatusCode::OK, StatusCode::OK));
    let third = get(&app, &uri, &viewer).await;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    common::logs::eventually("an export slot comes back", Duration::from_secs(5), || logs.export_slots.open(user) < 2).await;
    assert_eq!(get(&app, &uri, &viewer).await.status(), StatusCode::OK);
    drop(second);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_stalled_export_gives_its_slot_back_at_the_deadline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = ExportLimits { deadline: Duration::from_secs(1), ..ExportLimits::default() };
    let logs = Arc::new(LogsState { export_limits: limits, ..LogsState::default() });
    let state = AppState { logs: logs.clone(), ..state };
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_000).await;
    let uri = format!("/projects/{project}/logs/export?format=ndjson&{}", window());
    // Connected but never read: the channel fills and the task waits on its send.
    let stalled = get(&app, &uri, &viewer).await;
    assert_eq!(stalled.status(), StatusCode::OK);
    assert_eq!(logs.export_slots.open(user), 1);
    common::logs::eventually("the deadline frees the slot", Duration::from_secs(5), || logs.export_slots.open(user) == 0).await;
    drop(stalled);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn export_needs_access_and_valid_parameters(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign_source, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/logs/export");
    assert_eq!(call(&app, "GET", &format!("{base}?format=csv"), None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "GET", &format!("{base}?format=csv"), Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    for bad in ["format=xlsx", "", "format=csv&columns=a..b",
                "format=csv&filter=%7B%22path%22%3A%22a%22%2C%22op%22%3A%22gt%22%2C%22value%22%3A%22x%22%7D"] {
        assert_eq!(call(&app, "GET", &format!("{base}?{bad}"), Some(&viewer), None).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let body = text(get(&app, &format!("{base}?format=ndjson&source_id={foreign_source}&{}", window()), &viewer).await).await;
    assert_eq!(body, "", "another project's source exports nothing");
    ch.drop_database().await.unwrap();
}
```

(`regex` is a regular dependency, so it is usable in tests. `tower::ServiceExt` is already used by `tests/common`.)

- [ ] **Step 7: Run them to verify they fail, then pass**

Run: engine command with `--test logs logs_export::`.
Expected: FAIL before the route is registered (404). PASS after Steps 3-4 are wired. `the_row_cap_holds_and_is_announced` takes a few seconds (100k rows).

- [ ] **Step 8: Write the failing BFF tests** (`nami-ui/src/routes/(app)/projects/[id=uuid]/logs/export/server.test.ts`)

```ts
import { describe, expect, it, vi } from 'vitest';
import { GET } from './+server';

const PROJECT = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';

function fakeEvent(fetchMock: ReturnType<typeof vi.fn>, url: string, id = PROJECT, signal?: AbortSignal) {
	return {
		fetch: fetchMock,
		params: { id },
		url: new URL(url),
		request: new Request(url, { headers: { cookie: 'id=session' }, signal }),
		cookies: { set: vi.fn() },
		getClientAddress: () => '203.0.113.5'
	} as any;
}

function neverEndingStream() {
	return new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(new TextEncoder().encode('{"a":1}\n'));
		}
	});
}

describe('GET /projects/[id]/logs/export', () => {
	it('relays the stream without waiting for its end, with only safe headers', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(neverEndingStream(), {
				status: 200,
				headers: {
					'content-type': 'application/x-ndjson',
					'content-disposition': 'attachment; filename="nami-logs-20261019T080503Z.ndjson"',
					'x-nami-export-limit': '100000',
					'set-cookie': 'leak=1',
					'x-internal': 'no'
				}
			})
		);
		const response = await GET(
			fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=ndjson&from=a&to=b&columns=x.y&level=error&live=1&cursor=c&token=s`)
		);
		expect(response.status).toBe(200);
		expect(fetchMock.mock.calls[0][0]).toBe(
			`http://localhost:8080/projects/${PROJECT}/logs/export?format=ndjson&from=a&to=b&columns=x.y&level=error`
		);
		expect(response.headers.get('content-type')).toBe('application/x-ndjson');
		expect(response.headers.get('content-disposition')).toBe('attachment; filename="nami-logs-20261019T080503Z.ndjson"');
		expect(response.headers.get('x-nami-export-limit')).toBe('100000');
		expect(response.headers.get('cache-control')).toBe('no-store');
		expect(response.headers.get('set-cookie')).toBeNull();
		expect(response.headers.get('x-internal')).toBeNull();
		const first = await response.body!.getReader().read();
		expect(new TextDecoder().decode(first.value)).toBe('{"a":1}\n');
	});

	it('drops an unexpected disposition and refuses an unexpected content type', async () => {
		const odd = vi.fn().mockResolvedValue(
			new Response('x', { status: 200, headers: { 'content-type': 'text/csv; charset=utf-8', 'content-disposition': 'attachment; filename="../../evil.sh"' } })
		);
		const response = await GET(fakeEvent(odd, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`));
		expect(response.headers.get('content-disposition')).toBe('attachment; filename="nami-logs.csv"');
		const html = vi.fn().mockResolvedValue(new Response('<html>', { status: 200, headers: { 'content-type': 'text/html' } }));
		expect((await GET(fakeEvent(html, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`))).status).toBe(502);
	});

	it('passes engine errors through and rejects a bad project id', async () => {
		for (const status of [400, 401, 403, 429]) {
			const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'nope' }), { status }));
			const response = await GET(fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`));
			expect(response.status).toBe(status);
			expect(await response.json()).toEqual({ error: 'nope' });
		}
		await expect(GET(fakeEvent(vi.fn(), 'http://ui.test/x', 'not-a-uuid'))).rejects.toMatchObject({ status: 404 });
	});

	it('aborts the engine request when the browser goes away', async () => {
		const controller = new AbortController();
		const fetchMock = vi.fn().mockResolvedValue(new Response(neverEndingStream(), { status: 200, headers: { 'content-type': 'text/csv; charset=utf-8' } }));
		await GET(fakeEvent(fetchMock, `http://ui.test/projects/${PROJECT}/logs/export?format=csv`, PROJECT, controller.signal));
		const init = fetchMock.mock.calls[0][1] as RequestInit;
		controller.abort();
		expect(init.signal?.aborted).toBe(true);
	});
});
```

And in `log-proxy.test.ts`: `expect(EXPORT_PARAMS).toEqual([...FILTER_PARAMS, 'from', 'to', 'format', 'columns'])`.

- [ ] **Step 9: Run them to verify they fail**

Run: `cd nami-ui && npx vitest run src/routes/\(app\)/projects/\[id=uuid\]/logs/export src/lib/server/log-proxy.test.ts`.
Expected: FAIL (module missing).

- [ ] **Step 10: Implement the proxy**

`log-proxy.ts`: `export const EXPORT_PARAMS: readonly string[] = Object.freeze([...FILTER_PARAMS, 'from', 'to', 'format', 'columns']);`

`logs/export/+server.ts`:

```ts
import { error } from '@sveltejs/kit';
import { callEngine, enginePath } from '#lib/server/core/engine';
import { EXPORT_PARAMS, pickLogParams } from '#lib/server/logs/log-proxy';
import { isUuid } from '#lib/core/uuid';
import type { RequestHandler } from './$types';

const TYPES = new Set(['text/csv; charset=utf-8', 'application/x-ndjson']);
const DISPOSITION = /^attachment; filename="nami-logs-\d{8}T\d{6}Z\.(csv|ndjson)"$/;

/**
 * Export download: relays nami-engine's streamed CSV/NDJSON body as it
 * arrives (never read or buffered here), with only the headers a download
 * needs. The browser leaving aborts the engine request, which ends the
 * ClickHouse query.
 */
export const GET: RequestHandler = async (event) => {
	if (!isUuid(event.params.id)) throw error(404, 'Not found');
	const query = pickLogParams(event.url.searchParams, EXPORT_PARAMS);
	let response: Response;
	try {
		response = await callEngine(event, enginePath`/projects/${event.params.id}/logs/export` + query, {
			signal: event.request.signal
		});
	} catch (cause) {
		if (event.request.signal.aborted) throw cause;
		return json502('the log service is unreachable');
	}
	if (!response.ok || !response.body) {
		return new Response(await response.text(), {
			status: response.ok ? 502 : response.status,
			headers: { 'content-type': response.headers.get('content-type') ?? 'application/json', 'cache-control': 'no-store' }
		});
	}
	const type = response.headers.get('content-type') ?? '';
	if (!TYPES.has(type)) {
		await response.body.cancel();
		return json502('unexpected export response');
	}
	const disposition = response.headers.get('content-disposition') ?? '';
	const extension = type === 'application/x-ndjson' ? 'ndjson' : 'csv';
	const headers: Record<string, string> = {
		'content-type': type,
		'content-disposition': DISPOSITION.test(disposition) ? disposition : `attachment; filename="nami-logs.${extension}"`,
		'cache-control': 'no-store',
		'x-content-type-options': 'nosniff',
		'x-accel-buffering': 'no'
	};
	const limit = response.headers.get('x-nami-export-limit');
	if (limit && /^\d{1,9}$/.test(limit)) headers['x-nami-export-limit'] = limit;
	return new Response(response.body, { headers });
};

function json502(message: string): Response {
	return new Response(JSON.stringify({ error: message }), {
		status: 502,
		headers: { 'content-type': 'application/json', 'cache-control': 'no-store' }
	});
}
```

(If `callEngine` adds its own default `accept` header, leave it. Check that `callEngine` passes `signal` through, as the tail proxy relies on.)

- [ ] **Step 11: Run all checks, then commit**

Run: engine command (all) + clippy; `cd nami-ui && npm run check && npx vitest run`.
Expected: green; svelte-check 0/0.

```bash
git add nami-engine/src nami-engine/tests/logs/logs_export.rs nami-engine/tests/core/clickhouse.rs \
  docs/superpowers/specs/2026-10-13-log-management-design.md \
  nami-ui/src/lib/server/logs/log-proxy.ts nami-ui/src/lib/server/log-proxy.test.ts \
  "nami-ui/src/routes/(app)/projects/[id=uuid]/logs/export"
git commit -m "feat: streamed CSV/NDJSON log export with caps, proxied by nami-ui (NAMI-LOG-12)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 13 (NAMI-LOG-13): Saved views and the shared query shape (done, PR #76)

Merged. Kept as the record of what landed; do not re-run its steps.

**Files:**
- Create: the `log_views` migration (now folded into the baseline `nami-engine/migrations/0005_logs.sql`), `nami-engine/src/logs/view/saved_query.rs`, `nami-engine/src/logs/view/model.rs`, `nami-engine/src/logs/view/routes.rs`
- Modify: `nami-engine/src/logs/mod.rs`, `nami-engine/src/core/validate.rs`, `nami-engine/src/core/router.rs`, `nami-engine/src/core/router.rs` (merge router), `nami-engine/tests/core/migrations.rs`
- Test: `nami-engine/tests/logs/log_views.rs` (+ unit tests in `saved_query.rs`)

**Interfaces:**
- Consumes: `query::{parse_filters, parse_path, MAX_WINDOW}`, `core::validate::validate_label`.
- Produces (T14 and T17 rely on these shapes):

```rust
// logs::view::saved_query
pub const VIEW_RANGES: [&str; 6] = ["15m", "1h", "6h", "24h", "7d", "custom"];
pub const MAX_VIEW_COLUMNS: usize = 10;

/// What rows a view or an alert matches (R-P2-17). Validated exactly like
/// search: `to_filters` turns it into query pairs and runs `parse_filters`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogQuery {
    #[serde(default, skip_serializing_if = "String::is_empty")] pub q: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub in_fields: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub levels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub source_ids: Vec<Uuid>,
    /// The explorer's filter objects: `{"path","op","value"?}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub filters: Vec<serde_json::Value>,
    /// Decimal strings (a UInt64 does not fit a JavaScript number).
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub pattern_hashes: Vec<String>,
}
impl LogQuery {
    pub fn pairs(&self) -> Vec<(String, String)>;
    pub fn to_filters(&self) -> Result<LogFilters, String>;
}

/// A saved explorer state: a LogQuery plus the time range and columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewQuery {
    #[serde(default = "default_range")] pub range: String,                 // "1h"
    #[serde(default, skip_serializing_if = "Option::is_none")] pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub to: Option<DateTime<Utc>>,
    /* q, in_fields, levels, source_ids, filters, pattern_hashes: same fields and attributes as
       LogQuery, written out. Not `#[serde(flatten)]`: serde does not support
       `deny_unknown_fields` together with `flatten`. */
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub columns: Vec<String>,
}
impl ViewQuery {
    pub fn log_query(&self) -> LogQuery;
    pub fn validate(&self) -> Result<(), String>;
}
/// 400 unless every id is a source of the project.
pub async fn check_sources(pool: &PgPool, project_id: Uuid, ids: &[Uuid]) -> Result<(), AppError>;

// logs::view::model
pub const MAX_VIEWS_PER_PROJECT: i64 = 200;
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct LogView { pub id: Uuid, pub project_id: Uuid, pub name: String,
    pub query: sqlx::types::Json<ViewQuery>, pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
pub enum CreatedView { Created(LogView), ProjectFull, NameTaken, NoProject }
pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<LogView>, sqlx::Error>; // by name
pub async fn get(pool: &PgPool, project_id: Uuid, view_id: Uuid) -> Result<Option<LogView>, sqlx::Error>;
pub async fn create(pool: &PgPool, project_id: Uuid, name: &str, query: &ViewQuery, created_by: Uuid) -> Result<CreatedView, sqlx::Error>;
/// `Ok(None)` = no such view in the project; `Err(Conflict)` = name taken.
pub async fn update(pool: &PgPool, project_id: Uuid, view_id: Uuid, name: Option<&str>, query: Option<&ViewQuery>) -> Result<Option<LogView>, AppError>;
pub async fn delete(pool: &PgPool, project_id: Uuid, view_id: Uuid) -> Result<bool, sqlx::Error>;
```

- Routes: `GET/POST /projects/{id}/log-views` (viewer / member), `GET/PATCH/DELETE /projects/{id}/log-views/{view_id}` (viewer / member / member). Bodies: `CreateViewRequest { name: String, query: ViewQuery }` and `PatchViewRequest { name: Option<String>, query: Option<ViewQuery> }`, both `deny_unknown_fields`. Responses: `LogView` JSON (201 on create, 204 on delete).

- [ ] **Step 1: Migration + failing migration test**

The `log_views` migration as written then (its statement is now part of the baseline `nami-engine/migrations/0005_logs.sql`):

```sql
-- Saved explorer views (Logs part 2). `query` is a logs::view::saved_query::ViewQuery
-- validated by the engine; members create, edit and delete, viewers read.
create table log_views (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    query jsonb not null default '{}'::jsonb check (jsonb_typeof(query) = 'object'),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, name)
);
```

Test in `tests/core/migrations.rs`: `log_views_names_are_unique_per_project`. Seed two projects with T10's `seed_source_row` (plain SQL; the file has no `mod common`). Insert two views with one name in one project → the second fails with code `23505`. The same name in another project succeeds. Add `"log_views"` to the table list of `all_tables_exist`.

Run: engine command with `--test core migrations::`. Expected: FAIL, then PASS once the file exists.

- [ ] **Step 2: Write the failing unit tests** (`src/logs/view/saved_query.rs`)

```rust
use serde_json::json;
use super::*;

fn view(value: serde_json::Value) -> Result<ViewQuery, String> {
    let view: ViewQuery = serde_json::from_value(value).map_err(|e| e.to_string())?;
    view.validate().map(|()| view)
}

#[test]
fn a_view_validates_like_search() {
    let ok = view(json!({
        "range": "6h", "q": "disk", "in_fields": true, "levels": ["error"],
        "filters": [{"path":"http.status","op":"gte","value":"500"}],
        "pattern_hashes": ["123"], "columns": ["user.name"]
    })).unwrap();
    let filters = ok.log_query().to_filters().unwrap();
    assert_eq!(filters.levels, ["error"]);
    assert_eq!(filters.pattern_hashes, [123]);
    assert!(filters.in_fields);
    assert_eq!(view(json!({})).unwrap().range, "1h");
    for bad in [
        json!({"range": "2h"}),
        json!({"range": "custom"}),
        json!({"range": "custom", "from": "2026-10-02T00:00:00Z", "to": "2026-10-01T00:00:00Z"}),
        json!({"range": "custom", "from": "2026-01-01T00:00:00Z", "to": "2026-03-01T00:00:00Z"}),
        json!({"range": "1h", "from": "2026-10-01T00:00:00Z"}),
        json!({"levels": ["loud"]}),
        json!({"filters": [{"path":"a","op":"gt","value":"x"}]}),
        json!({"filters": ["not an object"]}),
        json!({"pattern_hashes": ["-1"]}),
        json!({"columns": ["a..b"]}),
        json!({"columns": (0..11).map(|n| format!("c{n}")).collect::<Vec<_>>()}),
        json!({"q": "x".repeat(1025)}),
        json!({"surprise": 1}),
    ] {
        assert!(view(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn queries_round_trip_without_defaults() {
    let value = json!({"range":"15m","q":"a","levels":["warn"],"columns":["x"]});
    let parsed: ViewQuery = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    assert_eq!(serde_json::to_value(LogQuery::default()).unwrap(), json!({}));
}
```

- [ ] **Step 3: Run them to verify they fail; implement `saved_query.rs`; run them to verify they pass**

Run: `cargo test --lib logs::view::saved_query` (engine command). FAIL → implement → PASS.

`pairs()`: `("q", q)` when non-empty, `("in_fields", "true")` when set, each level as `level`, each source as `source_id`, each filter as `("filter", value.to_string())`, each hash as `pattern_hash`. `ViewQuery::validate`: `range ∈ VIEW_RANGES` else `"range must be one of 15m, 1h, 6h, 24h, 7d, custom"`. `custom` needs `from` and `to`, `from < to`, `to - from <= query::MAX_WINDOW` (the messages of `parse_window`). Any other range with `from`/`to` set → `"from and to are only used with range custom"`. Columns go through `parse_path` and are at most `MAX_VIEW_COLUMNS`. Then `self.log_query().to_filters().map(|_| ())`. `check_sources`: `select count(*) from sources where project_id = $1 and id = any($2)` against the number of distinct ids → `AppError::Validation("source_ids: every source must belong to this project")`.

- [ ] **Step 4: Write the failing API tests** (`nami-engine/tests/logs/log_views.rs`)

```rust
// Saved log views (`/projects/{id}/log-views`). Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, logs::seed_source, seed_project, test_app, user_with_role};
use serde_json::json;

#[sqlx::test]
async fn members_manage_views_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/log-views");
    let query = json!({"range":"24h","levels":["error"],"source_ids":[source],"columns":["user.id"]});

    let (status, created) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"Errors","query":query}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["query"], query);
    let id = created["id"].as_str().unwrap().to_string();
    let one = format!("{base}/{id}");

    let (status, list) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(call(&app, "GET", &one, Some(&viewer), None).await.1["name"], "Errors");

    let (status, renamed) = call(&app, "PATCH", &one, Some(&member), Some(json!({"name":"5xx"}))).await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!((renamed["name"].clone(), renamed["query"].clone()), (json!("5xx"), query));

    for (method, body) in [("POST", Some(json!({"name":"x","query":{}}))), ("PATCH", Some(json!({"name":"y"}))), ("DELETE", None)] {
        let uri = if method == "POST" { &base } else { &one };
        assert_eq!(call(&app, method, uri, Some(&viewer), body).await.0, StatusCode::FORBIDDEN, "{method}");
    }
    assert_eq!(call(&app, "GET", &base, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &base, None, None).await.0, StatusCode::UNAUTHORIZED);

    assert_eq!(call(&app, "DELETE", &one, Some(&member), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, "GET", &one, Some(&member), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_view_of_another_project_is_not_found(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, theirs) = user_with_role(&app, &pool, other, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{other}/log-views"), Some(&theirs), Some(json!({"name":"Theirs","query":{}}))).await;
    let foreign = format!("/projects/{project}/log-views/{}", created["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &foreign, Some(&member), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "PATCH", &foreign, Some(&member), Some(json!({"name":"mine"}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "DELETE", &foreign, Some(&member), None).await.0, StatusCode::NOT_FOUND);
    let name: String = sqlx::query_scalar("select name from log_views where project_id = $1").bind(other).fetch_one(&pool).await.unwrap();
    assert_eq!(name, "Theirs");
}

#[sqlx::test]
async fn invalid_views_are_400_and_conflicts_409(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign_source, _) = seed_source(&pool, other, "theirs").await;
    let (user, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let base = format!("/projects/{project}/log-views");
    for body in [
        json!({"name":"", "query":{}}),
        json!({"name":"a\u{0007}b", "query":{}}),
        json!({"name":"x", "query":{"range":"2h"}}),
        json!({"name":"x", "query":{"source_ids":[foreign_source]}}),
        json!({"name":"x", "query":{"filters":[{"path":"a","op":"regex","value":"("}]}}),
    ] {
        let (status, error) = call(&app, "POST", &base, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
    }
    let (status, _) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"x","query":{},"extra":1}))).await;
    assert!(status.is_client_error());
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(json!({"name":"Same","query":{}}))).await.0, StatusCode::CREATED);
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(json!({"name":"Same","query":{}}))).await.0, StatusCode::CONFLICT);
    sqlx::query("insert into log_views (project_id, name, created_by) select $1, 'v' || n, $2 from generate_series(1, 199) n")
        .bind(project).bind(user).execute(&pool).await.unwrap();
    let (status, body) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"one too many","query":{}}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn a_relative_view_stays_relative_and_a_deleted_source_is_kept(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-views"), Some(&member),
        Some(json!({"name":"Last hour","query":{"range":"1h","source_ids":[source]}}))).await;
    sqlx::query("delete from sources where id = $1").bind(source).execute(&pool).await.unwrap();
    let (status, view) = call(&app, "GET", &format!("/projects/{project}/log-views/{}", created["id"].as_str().unwrap()), Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["query"], json!({"range":"1h","source_ids":[source]}), "no absolute window was frozen in");
}
```

- [ ] **Step 5: Run them to verify they fail; implement the model and routes; run them to verify they pass**

Run: engine command with `--test logs log_views::`. FAIL (404) → implement → PASS.

`create` runs in a transaction: `select id from projects where id = $1 for no key update` (absent → `NoProject` → 404), then `select count(*) from log_views where project_id = $1` ≥ `MAX_VIEWS_PER_PROJECT` → `ProjectFull` (409 `"a project may have at most 200 saved views"`), then insert. A unique violation (`23505`) → `NameTaken` (409 `"a view with this name already exists"`). `update` binds `coalesce($3, name)`, `coalesce($4, query)` and `updated_at = now()` in `where id = $1 and project_id = $2`, mapping `23505` to `AppError::Conflict`. Handlers check the role, then `validate_label("name", ..)`, then `query.validate()`, then `check_sources`, then the model call.

- [ ] **Step 6: Full engine suite + clippy, then commit**

```bash
git add nami-engine/migrations nami-engine/src nami-engine/tests/logs/log_views.rs nami-engine/tests/core/migrations.rs
git commit -m "feat(engine): saved log views (NAMI-LOG-13)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14 (NAMI-LOG-14): Log alerts — API, evaluation worker, incidents and notifications

On `feat/logs-part2`. Commits carry `(NAMI-LOG-14)`.

**Files:**
- Create: `nami-engine/migrations/NNNN_logs_alerts.sql` (`log_alerts`), `nami-engine/migrations/NNNN_incidents_log_alerts.sql` (`incidents.log_alert_id` and its indexes; numbered after the logs file, R-P2-1), `nami-engine/src/logs/alert/mod.rs`, `nami-engine/src/logs/alert/model.rs`, `nami-engine/src/logs/alert/routes.rs`
- Modify: `nami-engine/src/incidents/model.rs` (open/resolve for log alerts, list join + `log_alert_id`), `nami-engine/src/uptime/alerts.rs` (`Subject::LogAlert` in `render` and `still_down`), `nami-engine/src/uptime/worker.rs` (`spawn_alerts` → `pub(crate)`), `nami-engine/src/uptime/maintenance/model.rs` (`ActiveWindows::covers_project`), `nami-engine/src/main.rs` (spawn), `nami-engine/src/logs/mod.rs` (`pub mod alert;`), `nami-engine/src/core/router.rs` (merge the `logs::alert` routes), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/logs/main.rs` (`mod log_alerts;`), `docs/superpowers/specs/2026-10-13-log-management-design.md` (Log alerts: the project-wide maintenance sentence, `last_error`)
- Test: `nami-engine/tests/logs/log_alerts.rs` (+ unit tests in `nami-engine/src/uptime/alerts.rs` and `nami-engine/src/logs/alert/mod.rs`)

**Drift (2026-10-24):**
- `uptime::alerts::Transition` gained `details: Option<String>` (heartbeat output, `nami-engine/src/uptime/alerts.rs:30-40`). Every log-alert transition sets `details: None`; `render` ignores it for `Subject::LogAlert`.
- `notify_transition` now runs flap control first (`state.alert_throttle.decide`, `nami-engine/src/uptime/alerts.rs:53-64`, `nami-engine/src/uptime/alert_throttle.rs`): at most one DOWN and one recovery email per subject per `ALERT_FLAP_WINDOW` (5 min), and a DOWN held back is sent later by `send_deferred` if `still_down` says so. Log-alert transitions go through it unchanged (the tests below send a trigger and then a recovery, which flap control lets through). `still_down` (`nami-engine/src/uptime/alerts.rs:117-142`) holds the two exhaustive `match`es on `Subject` besides `render` (`:218`); `log_dropped` in the worker only prints subjects and has no match. `still_down` gains a `LogAlert(id)` arm: `select exists (select 1 from log_alerts where id = $1 and enabled and state = 'alerting')`, and for maintenance `maintenance::heartbeat_in_maintenance` (project-wide windows only, as R-P2-18).
- `incidents::model::open_for_heartbeat` now reopens an incident resolved less than `HEARTBEAT_REOPEN_WINDOW_SECS` ago (`nami-engine/src/incidents/model.rs:95-155`, Uptime Phase 3 flap control). Log alerts do not reopen (the spec has no such rule): `open_for_log_alert` / `resolve_for_log_alert` mirror `open_for_monitor` / `resolve_for_monitor` (`nami-engine/src/incidents/model.rs:55-92`), a plain insert with `on conflict (log_alert_id) where status = 'open' do nothing`.
- Views cap a stored query at `saved_query::MAX_QUERY_BYTES` (16 KiB, `nami-engine/src/logs/view/saved_query.rs:18`, checked at the end of `ViewQuery::validate`). `logs::alert::model::validate` applies the same ceiling to the alert's `LogQuery` (`serde_json::to_vec(&query)` at most `MAX_QUERY_BYTES`, else `"query must be at most 16384 bytes"`).
- The saved-view model is `logs::view::model` (`create` with `CreatedView`, `nami-engine/src/logs/view/model.rs:80-121`), its routes `nami-engine/src/logs/view/routes.rs` (mounted at `nami-engine/src/core/router.rs:85`). The log-alert model, routes and mount follow them.
- Integration tests are modules of the `logs` target: the file starts with `use crate::common;` (as `nami-engine/tests/logs/log_views.rs:2`), not `mod common;`, and `nami-engine/tests/logs/main.rs` declares `mod log_alerts;`. The module is `logs::alert` (R-P2-1), so the tests call `alert::...`.

**Interfaces:**
- Consumes: `saved_query::{LogQuery, check_sources}` (T13), `query::{push_where, push_window, Sql, Window}`, `search::{READ_SETTINGS, user_error, QueryPermit}`, `uptime::alerts::{notify_transition, Transition, TransitionKind, Subject, sanitize_cause}`, `uptime::worker::Notifications`.
- Produces:

```rust
// uptime::alerts
pub enum Subject { Monitor(Uuid), Heartbeat(Uuid), LogAlert(Uuid) }

// incidents::model
pub async fn open_for_log_alert(conn: &mut PgConnection, project_id: Uuid, log_alert_id: Uuid, title: &str,
    cause: Option<&str>, started_at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>;
pub async fn resolve_for_log_alert(conn: &mut PgConnection, log_alert_id: Uuid, resolved_at: DateTime<Utc>)
    -> Result<Option<Uuid>, sqlx::Error>;
// IncidentListItem gains `pub log_alert_id: Option<Uuid>`; source_name = coalesce(m.name, h.name, la.name)

// logs::alert::model
pub const MAX_LOG_ALERTS_PER_PROJECT: i64 = 100;
pub const MAX_THRESHOLD: i64 = 1_000_000_000_000;
pub const WINDOW_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;
pub const INTERVAL_RANGE: std::ops::RangeInclusive<i32> = 60..=86_400;
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct LogAlert { pub id: Uuid, pub project_id: Uuid, pub name: String,
    pub query: sqlx::types::Json<LogQuery>, pub threshold: i64, pub comparison: String,
    pub window_seconds: i32, pub interval_seconds: i32, pub enabled: bool, pub state: String,
    pub last_evaluated_at: Option<DateTime<Utc>>, pub last_count: Option<i64>,
    pub last_error: Option<String>, pub last_error_at: Option<DateTime<Utc>>,
    pub next_evaluation_at: DateTime<Utc>, pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
#[derive(Debug, Clone, PartialEq)]
pub struct LogAlertConfig { pub name: String, pub query: LogQuery, pub threshold: i64, pub comparison: String,
    pub window_seconds: i32, pub interval_seconds: i32, pub enabled: bool }
pub fn validate(config: LogAlertConfig) -> Result<LogAlertConfig, String>;
pub enum CreatedAlert { Created(LogAlert), ProjectFull, NoProject }
pub async fn list_for_project(..) / get(..) / create(..) / update(pool, project_id, alert_id, config) -> Result<Option<LogAlert>, sqlx::Error> / delete(..) -> Result<bool, sqlx::Error>;

// logs::alert
pub const ALERT_TICK: Duration = Duration::from_secs(5);
pub const ALERT_CLAIM_BATCH: i64 = 50;
pub const MAX_ALERT_EVALUATIONS: usize = 8;
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DueAlert { pub id: Uuid, pub project_id: Uuid, pub name: String, pub query: sqlx::types::Json<LogQuery>,
    pub threshold: i64, pub comparison: String, pub window_seconds: i32,
    /// As claimed: an evaluation is dropped when the row's differs (edited meanwhile, R-P2-18).
    pub updated_at: DateTime<Utc> }
pub fn condition_met(comparison: &str, count: u64, threshold: i64) -> bool;
pub fn describe(alert: &DueAlert, count: u64) -> String;   // the incident cause, R-P2-18
pub async fn claim_due_alerts(pool: &PgPool, limit: i64) -> Result<Vec<DueAlert>, sqlx::Error>;
pub async fn count_matches(ch: &ClickHouse, project_id: Uuid, filters: &LogFilters, window: &Window) -> Result<u64, AppError>;
pub async fn apply_evaluation(pool: &PgPool, alert: &DueAlert, count: u64, started_at: DateTime<Utc>)
    -> Result<Option<Transition>, sqlx::Error>;
/// A failed count: only `last_error` / `last_error_at` change (R-P2-18).
pub async fn record_failure(pool: &PgPool, alert_id: Uuid, message: &str) -> Result<(), sqlx::Error>;
/// The incident seam (R-P2-27): the only code that opens or resolves a log-alert incident.
pub async fn open_alert_incident(conn: &mut PgConnection, alert: &DueAlert, count: u64, at: DateTime<Utc>)
    -> Result<Option<Transition>, sqlx::Error>;
pub async fn resolve_alert_incident(conn: &mut PgConnection, alert_id: Uuid, project_id: Uuid, name: &str,
    at: DateTime<Utc>, notify: bool) -> Result<Option<Transition>, sqlx::Error>;
/// One pass with its own budget of MAX_ALERT_EVALUATIONS (tests); `run_forever` uses `run_pass_with`
/// and one semaphore shared by every pass.
pub async fn run_pass(state: &AppState, notifications: &Notifications) -> Result<usize, AppError>;
pub async fn run_pass_with(state: &AppState, notifications: &Notifications, budget: &Arc<Semaphore>)
    -> Result<usize, AppError>;
/// run_pass, then waits for its notifications (tests).
pub async fn evaluate_due_alerts(state: &AppState) -> Result<usize, AppError>;
pub async fn run_forever(state: AppState);
```

- Routes: `GET/POST /projects/{id}/log-alerts` (viewer / member). `GET/PATCH/DELETE /projects/{id}/log-alerts/{alert_id}` (viewer / member / **admin**). `POST /projects/{id}/log-alerts/preview` (viewer), body `{query: LogQuery, window_seconds}` → `{"count": n, "from": .., "to": ..}`. Bodies: `CreateLogAlertRequest { name, #[serde(default)] query: LogQuery, threshold: i64, comparison: String, window_seconds: i32, interval_seconds: i32, #[serde(default = "yes")] enabled: bool }` and `PatchLogAlertRequest` with every field optional (merged with the stored alert, then validated), both `deny_unknown_fields`.

- [ ] **Step 1: Migration + failing migration test**

Two phase files (R-P2-1). NNNN is the next free number in `nami-engine/migrations/` at phase start; the logs file takes it and the incidents file the one after, because `incidents.log_alert_id` references `log_alerts`. No other task of this phase changes the schema.

`nami-engine/migrations/NNNN_logs_alerts.sql`:

```sql
-- Log alerts (Logs part 2): a count of matching log rows over a window,
-- compared with a threshold. The worker claims due alerts like monitors
-- (for update skip locked) and opens/resolves incidents through
-- incidents.log_alert_id.
create table log_alerts (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    -- logs::view::saved_query::LogQuery, validated by the engine.
    query jsonb not null default '{}'::jsonb check (jsonb_typeof(query) = 'object'),
    threshold bigint not null check (threshold between 0 and 1000000000000),
    comparison text not null check (comparison in ('gt', 'lt')),
    window_seconds int not null check (window_seconds between 60 and 86400),
    interval_seconds int not null check (interval_seconds between 60 and 86400),
    enabled boolean not null default true,
    state text not null default 'ok' check (state in ('ok', 'alerting')),
    last_evaluated_at timestamptz,
    last_count bigint,
    -- Why the last evaluation failed (fixed, user-safe text); null after a success.
    last_error text,
    last_error_at timestamptz,
    next_evaluation_at timestamptz not null default now(),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (comparison = 'gt' or threshold >= 1)
);

create index log_alerts_project_idx on log_alerts (project_id);
-- The worker's claim: due, enabled alerts, most overdue first.
create index log_alerts_due_idx on log_alerts (next_evaluation_at) where enabled;
```

`nami-engine/migrations/NNNN_incidents_log_alerts.sql` (after the logs file):

```sql
-- Incidents opened by log alerts (Logs part 2): logs::alert opens and
-- resolves them through incidents::model::{open,resolve}_for_log_alert.
alter table incidents add column log_alert_id uuid references log_alerts(id) on delete cascade;
-- At most one open incident per log alert (mirrors monitors and heartbeats).
create unique index incidents_one_open_per_log_alert on incidents (log_alert_id) where status = 'open';
-- Every incident of one log alert (the per-alert list and the delete cascade),
-- like incidents_monitor_idx and incidents_heartbeat_idx in 0004_incidents.sql.
create index incidents_log_alert_idx on incidents (log_alert_id) where log_alert_id is not null;
```

Test in `nami-engine/tests/core/migrations.rs`: `log_alerts_allow_one_open_incident_each`. Seed a project with `seed_source_row` (plain SQL). Insert an alert, insert an open incident for it (`insert into incidents (project_id, log_alert_id, title) values ($1, $2, 't')`), and expect a second open insert to fail with `23505`. Expect `comparison = 'lt'` with `threshold = 0` to be refused by the check. Add `"log_alerts"` to the table list of `all_tables_exist`.

Run: engine command with `--test core migrations::`. FAIL → add the two files → PASS.

- [ ] **Step 2: Failing unit tests**

In `nami-engine/src/uptime/alerts.rs` `mod tests` (its `transition(kind, name)` helper already sets `details: None` and `at` = 2026-10-14 17:46:40 UTC):

```rust
#[test]
fn log_alert_emails_say_triggered_and_resolved_and_link_to_the_alert() {
    let triggered = Transition {
        subject: Subject::LogAlert(Uuid::nil()),
        cause: Some("12 matching events in the last 5 min (threshold: > 10)".to_string()),
        ..transition(TransitionKind::Down, "Checkout errors")
    };
    let (subject, body) = render("http://ui.test", &triggered);
    assert_eq!(subject, "[Nami] Log alert Checkout errors triggered");
    assert!(body.contains("Condition: 12 matching events in the last 5 min (threshold: > 10)"), "{body}");
    let nil = Uuid::nil();
    assert!(body.contains(&format!("http://ui.test/projects/{nil}/logs/alerts/{nil}")), "{body}");
    let resolved = Transition { kind: TransitionKind::Recovered, ..triggered };
    let (subject, body) = render("http://ui.test", &resolved);
    assert_eq!(subject, "[Nami] Log alert Checkout errors resolved");
    assert!(body.contains("Resolved at: 2026-10-14 17:46:40 UTC"), "{body}");
    let evil = Transition { name: "x\r\nBcc: a@evil.test".into(), ..resolved };
    assert!(!render("http://ui.test", &evil).0.contains('\n'));
}
```

In `nami-engine/src/logs/alert/mod.rs` `mod tests`:

```rust
#[test]
fn conditions_compare_strictly() {
    assert!(condition_met("gt", 11, 10));
    assert!(!condition_met("gt", 10, 10));
    assert!(condition_met("lt", 0, 1));
    assert!(!condition_met("lt", 1, 1));
    assert!(!condition_met("eq", 5, 5), "unknown comparisons never fire");
}

#[test]
fn the_cause_names_count_window_and_threshold() {
    let alert = DueAlert { id: Uuid::nil(), project_id: Uuid::nil(), name: "n".into(),
        query: sqlx::types::Json(LogQuery::default()), threshold: 10, comparison: "gt".into(), window_seconds: 300,
        updated_at: chrono::DateTime::from_timestamp(1_792_000_000, 0).unwrap() };
    assert_eq!(describe(&alert, 12), "12 matching events in the last 5 min (threshold: > 10)");
    let hourly = DueAlert { comparison: "lt".into(), window_seconds: 3600, threshold: 1, ..alert.clone() };
    assert_eq!(describe(&hourly, 0), "0 matching events in the last 1 h (threshold: < 1)");
    let odd = DueAlert { window_seconds: 90, ..alert };
    assert_eq!(describe(&odd, 1), "1 matching events in the last 90 s (threshold: > 10)");
}
```

(`describe` writes the window as `N h` when it divides by 3600, `N min` when it divides by 60, otherwise `N s`. Drift (2026-10-24): the literal sets `updated_at`, which the plan's own `DueAlert` declares; without it the test would not compile. The assertions are unchanged.)

Run: `cargo test --lib -- uptime::alerts logs::alert` (engine environment). Expected: FAIL to compile, then PASS after Step 3.

- [ ] **Step 3: Implement `Subject::LogAlert`, its rendering, `condition_met` and `describe`**

`render` for `Subject::LogAlert(id)`: link `{base}/projects/{project}/logs/alerts/{id}`. `Down` → subject `[Nami] Log alert {name} triggered`, body `Log alert {name} triggered.\n\nCondition: {cause or "unknown"}\nSince: {at}\n\nDetails: {link}\n`. `Recovered` → subject `[Nami] Log alert {name} resolved`, body `Log alert {name} resolved.\n\nResolved at: {at}\n\nDetails: {link}\n`. Names and causes go through the same `is_unsafe_in_text` / `sanitize_cause` filters as monitor emails. `details` is ignored for log alerts. Fix every exhaustive `match` on `Subject`: `render` and the two in `still_down` (`nami-engine/src/uptime/alerts.rs:117-142`, see the Drift note above for the `LogAlert` arm).

- [ ] **Step 4: Write the failing API + worker tests** (`nami-engine/tests/logs/log_alerts.rs`)

```rust
// Log alerts: API, evaluation, incidents and email. Requires
// `docker compose up -d postgres clickhouse mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{app, call, logs::{post_json, seed_source}, public_app, seed_project, settled_mail, state_with_clickhouse, test_app, user_with_role};
use nami_engine::{logs::alert, core::state::AppState};
use serde_json::{Value, json};

fn alert_body(name: &str) -> Value {
    json!({ "name": name, "query": { "levels": ["error"] }, "threshold": 2, "comparison": "gt",
            "window_seconds": 300, "interval_seconds": 60 })
}

async fn make_due(pool: &sqlx::PgPool, alert: &str) {
    sqlx::query("update log_alerts set next_evaluation_at = now() - interval '1 second' where id = $1::uuid")
        .bind(alert).execute(pool).await.unwrap();
}

async fn alert_row(pool: &sqlx::PgPool, alert: &str) -> (String, Option<i64>) {
    sqlx::query_as("select state, last_count from log_alerts where id = $1::uuid").bind(alert).fetch_one(pool).await.unwrap()
}

async fn incidents(pool: &sqlx::PgPool, alert: &str) -> Vec<(String, String)> {
    sqlx::query_as("select status, title from incidents where log_alert_id = $1::uuid order by started_at")
        .bind(alert).fetch_all(pool).await.unwrap()
}

#[sqlx::test]
async fn roles_and_validation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign_source, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/log-alerts");

    let (status, created) = call(&app, "POST", &base, Some(&member), Some(alert_body("Errors"))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!((created["state"].clone(), created["enabled"].clone()), (json!("ok"), json!(true)));
    let one = format!("{base}/{}", created["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &base, Some(&viewer), None).await.1.as_array().unwrap().len(), 1);
    assert_eq!(call(&app, "POST", &base, Some(&viewer), Some(alert_body("x"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PATCH", &one, Some(&viewer), Some(json!({"threshold": 5}))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &one, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    let (status, patched) = call(&app, "PATCH", &one, Some(&member), Some(json!({"threshold": 5}))).await;
    assert_eq!((status, patched["threshold"].clone()), (StatusCode::OK, json!(5)));
    assert_eq!(call(&app, "DELETE", &one, Some(&member), None).await.0, StatusCode::FORBIDDEN, "admins delete alerts");

    for patch in [
        json!({"window_seconds": 59}), json!({"window_seconds": 86401}), json!({"interval_seconds": 59}),
        json!({"comparison": "eq"}), json!({"threshold": -1}), json!({"comparison": "lt", "threshold": 0}),
        json!({"threshold": 1_000_000_000_001_i64}), json!({"name": ""}),
        json!({"query": {"filters": [{"path":"a","op":"gt","value":"x"}]}}),
        json!({"query": {"source_ids": [foreign_source]}}),
    ] {
        let (status, body) = call(&app, "PATCH", &one, Some(&member), Some(patch.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{patch}: {body}");
    }
    assert_eq!(call(&app, "DELETE", &one, Some(&admin), None).await.0, StatusCode::NO_CONTENT);

    let (_, _, theirs) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (_, foreign) = call(&app, "POST", &format!("/projects/{other}/log-alerts"), Some(&theirs), Some(alert_body("Theirs"))).await;
    let foreign = format!("{base}/{}", foreign["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &foreign, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "DELETE", &foreign, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_preview_counts_only_this_projects_matching_rows(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!([{"level":"error","message":"a"},{"level":"error","message":"b"},{"message":"c"}])).await;
    post_json(&public, &foreign, json!([{"level":"error","message":"theirs"}])).await;
    let uri = format!("/projects/{project}/log-alerts/preview");
    let (status, body) = call(&app, "POST", &uri, Some(&viewer), Some(json!({"query":{"levels":["error"]},"window_seconds":300}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 2);
    let (status, _) = call(&app, "POST", &uri, Some(&viewer), Some(json!({"query":{},"window_seconds":30}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_alert_opens_and_resolves_its_incident_and_emails_once_each(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Checkout errors"))).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Another project's errors never count.
    post_json(&public, &foreign, json!(vec![json!({"level":"error","message":"x"}); 5])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), Some(0)));

    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    make_due(&pool, &id).await;
    assert_eq!(alert::evaluate_due_alerts(&state).await.unwrap(), 1);
    assert_eq!(alert_row(&pool, &id).await, ("alerting".to_string(), Some(3)));
    assert_eq!(incidents(&pool, &id).await, [("open".to_string(), "Log alert: Checkout errors".to_string())]);

    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(incidents(&pool, &id).await.len(), 1, "still alerting: no second incident");

    let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"threshold": 10}))).await;
    assert_eq!(status, StatusCode::OK);
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "ok");
    assert_eq!(incidents(&pool, &id).await[0].0, "resolved");

    let mail = settled_mail(&email, 2).await;
    let subjects: Vec<&str> = mail.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, ["[Nami] Log alert Checkout errors triggered", "[Nami] Log alert Checkout errors resolved"]);
    assert!(mail[0].1.contains("Condition: 3 matching events in the last 5 min (threshold: > 2)"), "{}", mail[0].1);

    let (_, listed) = call(&app, "GET", &format!("/projects/{project}/incidents"), Some(&member), None).await;
    assert_eq!(listed[0]["source_name"], "Checkout errors");
    assert_eq!(listed[0]["log_alert_id"], json!(id));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_failed_count_changes_nothing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("E"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "alerting");
    let evaluated: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select last_evaluated_at from log_alerts where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();

    // ClickHouse unreachable: AppState::for_test's client points at a database that does not exist.
    let broken = AppState::for_test(pool.clone());
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&broken).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("alerting".to_string(), Some(3)));
    assert_eq!(incidents(&pool, &id).await[0].0, "open");
    let (after, error): (Option<chrono::DateTime<chrono::Utc>>, Option<String>) =
        sqlx::query_as("select last_evaluated_at, last_error from log_alerts where id = $1::uuid")
            .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(after, evaluated);
    // Only the failure itself is recorded, as a fixed text (never ClickHouse's own message).
    assert_eq!(error.as_deref(), Some("the log store is unavailable"));
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    let error: Option<String> = sqlx::query_scalar("select last_error from log_alerts where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(error, None, "a successful evaluation clears it");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_project_wide_maintenance_window_holds_the_alert_state(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Deploy"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    sqlx::query("insert into maintenance_windows (project_id, title, kind, starts_at, ends_at)
                 values ($1, 'deploy', 'once', now() - interval '1 minute', now() + interval '1 hour')")
        .bind(project).execute(&pool).await.unwrap();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), Some(3)), "counted, but the state stands still");
    assert!(incidents(&pool, &id).await.is_empty());

    sqlx::query("delete from maintenance_windows where project_id = $1").bind(project).execute(&pool).await.unwrap();
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "alerting", "the first evaluation after the window applies");
    assert_eq!(settled_mail(&email, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_evaluation_of_an_edited_alert_is_dropped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member),
        Some(json!({ "name": "Quiet", "query": {}, "threshold": 1, "comparison": "lt", "window_seconds": 300, "interval_seconds": 60 }))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let claimed = alert::claim_due_alerts(&pool, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    // Edited while the count runs: the old settings ("< 1") must not open an incident.
    let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"name": "Renamed"}))).await;
    assert_eq!(status, StatusCode::OK);
    let applied = alert::apply_evaluation(&pool, &claimed[0], 0, chrono::Utc::now()).await.unwrap();
    assert!(applied.is_none());
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), None));
    assert!(incidents(&pool, &id).await.is_empty());
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn disabled_alerts_are_skipped_and_disabling_resolves_quietly(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Quiet"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"enabled": false}))).await;
    assert_eq!((status, body["state"].clone()), (StatusCode::OK, json!("ok")));
    assert_eq!(incidents(&pool, &id).await[0].0, "resolved");
    make_due(&pool, &id).await;
    assert_eq!(alert::evaluate_due_alerts(&state).await.unwrap(), 0, "a disabled alert is never claimed");
    assert_eq!(settled_mail(&email, 1).await.len(), 1, "only the trigger email: disabling sends nothing");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn overlapping_passes_evaluate_each_alert_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Once"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    let (a, b) = tokio::join!(alert::evaluate_due_alerts(&state), alert::evaluate_due_alerts(&state));
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(incidents(&pool, &id).await.len(), 1);
    assert_eq!(settled_mail(&email, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}
```

(`settled_mail(recipient, count)` waits until Mailpit holds `count` messages for the recipient and returns them oldest first. Check its exact semantics in `tests/common/mod.rs` and adapt the waits, not the assertions.)

- [ ] **Step 5: Run them to verify they fail**

Run: engine command with `--test logs log_alerts::`.
Expected: FAIL (routes 404, module missing).

- [ ] **Step 6: Implement models, worker and routes**

`claim_due_alerts`:

```rust
pub async fn claim_due_alerts(pool: &PgPool, limit: i64) -> Result<Vec<DueAlert>, sqlx::Error> {
    sqlx::query_as(
        "update log_alerts a
         set next_evaluation_at = now() + make_interval(secs => a.interval_seconds)
         where a.id in (
             select id from log_alerts
             where enabled and next_evaluation_at <= now()
             order by next_evaluation_at
             limit $1
             for update skip locked
         )
         returning a.id, a.project_id, a.name, a.query, a.threshold, a.comparison, a.window_seconds, a.updated_at",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
```

`count_matches`: `SELECT count() AS count FROM logs` + `push_where` + `push_window`, run with `READ_SETTINGS`, mapped by `search::user_error`, reading a `CountRow { count: u64 }`.

`apply_evaluation` (one transaction):

```rust
// Locked row; a deleted, disabled or edited alert, or a stale evaluation, changes nothing.
let row: Option<(String, bool, Option<DateTime<Utc>>, DateTime<Utc>)> = sqlx::query_as(
    "select state, enabled, last_evaluated_at, updated_at from log_alerts where id = $1 for update")
    .bind(alert.id).fetch_optional(&mut *tx).await?;
let Some((state, enabled, last, updated_at)) = row else { return Ok(None) };
if !enabled || updated_at != alert.updated_at || last.is_some_and(|last| last >= started_at) {
    return Ok(None);
}
// A project-wide maintenance window holds the state machine (R-P2-18).
let held = maintenance::active_windows(&mut *tx, alert.project_id, started_at).await?.covers_project();
let met = condition_met(&alert.comparison, count, alert.threshold);
let next_state = if held { state.as_str() } else if met { "alerting" } else { "ok" };
sqlx::query("update log_alerts set state = $2, last_count = $3, last_evaluated_at = $4, last_error = null,
             last_error_at = null where id = $1")
    .bind(alert.id).bind(next_state).bind(count as i64).bind(started_at).execute(&mut *tx).await?;
```

Then, unless `held`: `met && state == "ok"` → `open_alert_incident(&mut tx, alert, count, started_at)`. `!met && state == "alerting"` → `resolve_alert_incident(&mut tx, alert.id, alert.project_id, &alert.name, started_at, true)`. Otherwise `None`. Commit. The seam (R-P2-27): `open_alert_incident` calls `incident::open_for_log_alert(conn, project_id, alert.id, &format!("Log alert: {}", alert.name), Some(&describe(alert, count)), at)` and returns `Transition { kind: Down, subject: Subject::LogAlert(alert.id), project_id, name, cause, details: None, at }`. `resolve_alert_incident` calls `incident::resolve_for_log_alert` and returns the `Recovered` transition only when `notify`. No other code path calls the two `incident::*_for_log_alert` functions. `ActiveWindows::covers_project` is a new alias of `covers_heartbeats` in `nami-engine/src/uptime/maintenance/model.rs` (project-wide windows only). Imports: `crate::incidents::model as incident`, `crate::uptime::maintenance::model as maintenance`.

`record_failure`: `update log_alerts set last_error = $2, last_error_at = now() where id = $1` (nothing else).

`run_pass_with(state, notifications, budget)`: `let free = budget.available_permits().min(ALERT_CLAIM_BATCH as usize)`; `free == 0` → return 0 without claiming. Otherwise `claim_due_alerts(.., free as i64)` then, for each alert, in a `JoinSet`, holding an owned permit of `budget` (`acquire_owned`, which may briefly wait when another pass took permits meanwhile): take `started_at = Utc::now()`, convert `alert.query.to_filters()` (a stored query that no longer validates → `record_failure(.., "the alert's query is no longer valid")` and skip), set the window `[started_at − window_seconds, started_at]`, then `count_matches`. An error → `tracing::warn!(alert_id, error)`, `record_failure` with the `AppError::Validation` message or `"the log store is unavailable"`, and no state change (R-P2-18). Otherwise `apply_evaluation`. `run_pass` is `run_pass_with` over a fresh `Arc::new(Semaphore::new(MAX_ALERT_EVALUATIONS))`. Transitions are handed to `notifications.spawn(..)`, which calls `notify_transition` for each, as `uptime::worker::spawn_alerts` does (make that helper `pub(crate)` and reuse it). Returns the number of alerts claimed. `run_forever`: one `Arc<Semaphore>` of `MAX_ALERT_EVALUATIONS` and one `Notifications` for its lifetime, an interval of `ALERT_TICK` with `MissedTickBehavior::Delay`, each pass (`run_pass_with` over the shared semaphore) spawned so that a panic is logged and not fatal (as `run_checks`). `main.rs` spawns it inside the `uptime_worker_enabled` branch, logging `"log alert worker started"`.

`logs::alert::model::update` (transaction): lock the row (`for update`, scoped to the project, absent → `None`), merge and validate in the handler before the call, then write every field, `updated_at = now()` and `next_evaluation_at = now()`. When `enabled` becomes false and `state = 'alerting'`, set `state = 'ok'` and call `logs::alert::resolve_alert_incident(.., now, false)` in the same transaction (no transition). `create` follows `logs::view::model::create` (project row locked `for no key update`, cap `MAX_LOG_ALERTS_PER_PROJECT`, 409 `"a project may have at most 100 log alerts"`, `NoProject` → 404; alert names are not unique, so there is no `NameTaken`). `validate`: `validate_label("name", ..)`. `comparison ∈ {gt, lt}`. `threshold ∈ 0..=MAX_THRESHOLD`, and `>= 1` for `lt` (`"lt needs a threshold of at least 1"`). `window_seconds ∈ WINDOW_RANGE`, `interval_seconds ∈ INTERVAL_RANGE`, and `query.to_filters()` must pass, with the serialized query at most `saved_query::MAX_QUERY_BYTES` (Drift note above). The handler also calls `check_sources`. The routes are `router()` in `nami-engine/src/logs/alert/routes.rs`, merged in `build_router` (`nami-engine/src/core/router.rs`) beside the view routes.

Preview handler: `require_project_role(viewer)` → `window_seconds ∈ WINDOW_RANGE` (400) → `query.to_filters()` (400) → `check_sources` → `QueryPermit::acquire` (429) → `count_matches` over `[now − window, now]` → `{count, from, to}`.

`incidents::model`: `open_for_log_alert` / `resolve_for_log_alert` mirror `open_for_monitor` / `resolve_for_monitor` (not the heartbeat pair, which reopens recent incidents; Drift note above) with `on conflict (log_alert_id) where status = 'open' do nothing`. `list_for_project` adds `left join log_alerts la on la.id = i.log_alert_id`, selects `i.log_alert_id` and uses `coalesce(m.name, h.name, la.name) as source_name`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: engine command with `--test logs log_alerts::`, `--test incidents`, `--test uptime uptime_worker::`, `--test core migrations::`, `--lib uptime::alerts`.
Expected: PASS.

- [ ] **Step 8: Full engine suite + clippy, then commit**

```bash
git add nami-engine/migrations nami-engine/src nami-engine/tests/logs/log_alerts.rs nami-engine/tests/logs/main.rs nami-engine/tests/core/migrations.rs \
  docs/superpowers/specs/2026-10-13-log-management-design.md
git commit -m "feat(engine): log alerts with incident and email notifications (NAMI-LOG-14)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 15 (NAMI-LOG-15): Live-tail resume and the explorer's deferred fixes

On `feat/logs-part2`. Commits carry `(NAMI-LOG-15)`.

**Files:**
- Modify: `nami-engine/src/logs/query/tail.rs` (event ids, `RESUME_WINDOW`, `resume_cursor`, first-poll resume, `TailStats`), `nami-engine/src/logs/mod.rs` (`LogsState.tail_stats`), `nami-engine/src/logs/query/routes.rs` (read `Last-Event-ID`)
- Modify (UI): `nami-ui/src/lib/server/logs/log-proxy.ts` (+ test) `lastEventIdHeader`; `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/tail/+server.ts` (+ `server.test.ts`); `nami-ui/src/lib/logs/logs.ts` (+ test) `isLogEntry`, `mergeLiveRows`, `tailKey`; `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/+page.svelte`, `+page.server.ts`, `explorer.test.ts` (histogram/sources errors)
- Create (UI): `nami-ui/src/lib/logs/live-tail.ts` (+ `live-tail.test.ts`), R-P2-21
- Test: `nami-engine/tests/logs/logs_tail.rs` (helper extension + 3 tests), unit tests in `tail.rs`

**Drift (2026-10-24):**
- `tail::run` today takes `(state, session, project_id, filters, tx)` (`nami-engine/src/logs/query/tail.rs:330-336`) and the handler `tail_logs` (`nami-engine/src/logs/query/routes.rs:133-160`) has no `HeaderMap` extractor yet: add one and pass `resume` as the new fifth argument. The id poll uses `timings.batch` (`TailTimings.batch`, `TAIL_BATCH` by default), so the resume poll does too.
- The explorer's "Load more" keeps no `extraRows`: `rows` and `nextCursor` are writable `$derived` values of `data.logs` / `data.nextCursor` that reset on every navigation, and it fetches the BFF route `${path}/search` (`nami-ui/src/routes/(app)/projects/[id=uuid]/logs/+page.svelte:53-84`, `.../logs/search/+server.ts`). Keep that: the new `$effect` that reads `data.logs` only bumps `generation` and aborts `controller`; `loadMore` passes `signal` to its `fetch` and drops a result whose generation is stale.
- The message `<pre>` of an expanded row already wraps (`whitespace-pre-wrap wrap-anywhere`, `+page.svelte:528`); only the fields `<pre>` beside it (`:529-530`) does not. Give it the same two classes (Tailwind 4's `wrap-anywhere`, not `break-words`).
- The live notice already has `role="status"` (`+page.svelte:446`). Move `role="status" aria-live="polite"` to the element holding both the status text and the notice, so each change is announced once.
- Turning live off today sets the status to `off` but keeps the live rows (`+page.svelte:92-97`); clearing them is part of this task (R-P2-21).

**Interfaces:**
- Consumes: `query::Cursor` (`encode`/`decode`), the fix wave's two-step tail poll (`fetch_ids` then `fetch_rows`).
- Produces:

```rust
// logs::query::tail
pub const RESUME_WINDOW: Duration = Duration::from_secs(300);
/// A `Last-Event-ID` worth resuming from: a cursor dated within the last
/// RESUME_WINDOW (and not past FUTURE_ALLOWANCE ahead); anything else is None.
pub fn resume_cursor(header: Option<&str>, now: DateTime<Utc>) -> Option<Cursor>;
pub async fn run(state: AppState, session: Session, project_id: Uuid, filters: LogFilters,
    resume: Option<Cursor>, tx: EventSender);
// every `event: log` carries `id: Cursor { dt_ms, id }.encode()`
/// What the tails of this instance read (R-P2-25).
#[derive(Debug, Default)]
pub struct TailStats { pub id_polls: AtomicU64, pub rows_fetched: AtomicU64 }
// logs::LogsState gains `pub tail_stats: tail::TailStats`
```

```ts
// nami-ui lib/server/logs/log-proxy.ts
/** The Last-Event-ID to forward, or null when absent or not a plausible cursor. */
export function lastEventIdHeader(value: string | null): string | null;
// nami-ui lib/logs/logs.ts
export function isLogEntry(value: unknown): value is LogEntry;
/** `incoming` (oldest first) prepended newest-first to `existing`, without duplicate ids, at most `cap` rows. */
export function mergeLiveRows(existing: LogEntry[], incoming: LogEntry[], cap: number): LogEntry[];
/** What restarts the live tail: only the filters it sends. */
export function tailKey(state: ExplorerState): string;
```

- [ ] **Step 1: Write the failing engine tests**

Unit tests in `tail.rs`:

```rust
#[test]
fn only_recent_well_formed_cursors_resume() {
    let now = Utc::now();
    let at = |offset: chrono::Duration| Cursor { dt_ms: (now + offset).timestamp_millis(), id: Uuid::new_v4() }.encode();
    assert!(resume_cursor(Some(&at(-chrono::Duration::seconds(30))), now).is_some());
    assert!(resume_cursor(Some(&at(-chrono::Duration::minutes(6))), now).is_none(), "too old");
    assert!(resume_cursor(Some(&at(chrono::Duration::minutes(5))), now).is_none(), "in the future");
    assert!(resume_cursor(Some("not a cursor"), now).is_none());
    assert!(resume_cursor(Some(&"A".repeat(129)), now).is_none(), "over 128 bytes: not even decoded");
    assert!(resume_cursor(None, now).is_none());
}
```

In `tests/logs/logs_tail.rs`, extend the helper: `SseStream` gains `last_id: Option<String>` (set by `next` whenever a block has an `id:` line; `parse_block` returns the id alongside), and `open_with(app, uri, cookie, last_event_id: Option<&str>)` adds the `Last-Event-ID` header (`open` calls it with `None`). Then:

```rust
#[sqlx::test]
async fn a_reconnect_with_last_event_id_gets_the_rows_it_missed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState { logs: fast(|_| {}), ..state };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");

    let mut stream = SseStream::open(&app, &uri, Some(&viewer)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    post_json(&public, &token, json!({ "message": "seen" })).await;
    assert_eq!(stream.next_log(Duration::from_secs(5)).await.unwrap()["message"], "seen");
    let last = stream.last_id.clone().expect("log events carry an id");
    drop(stream);

    post_json(&public, &token, json!([{ "message": "missed 1" }, { "message": "missed 2" }])).await;
    let mut resumed = SseStream::open_with(&app, &uri, Some(&viewer), Some(&last)).await.unwrap();
    let first = resumed.next_log(Duration::from_secs(5)).await.unwrap();
    let second = resumed.next_log(Duration::from_secs(5)).await.unwrap();
    assert_eq!((first["message"].clone(), second["message"].clone()), (json!("missed 1"), json!("missed 2")));
    post_json(&public, &token, json!({ "message": "live again" })).await;
    assert_eq!(resumed.next_log(Duration::from_secs(5)).await.unwrap()["message"], "live again",
        "\"seen\" is not sent twice and the stream goes on live");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_unusable_last_event_id_starts_a_normal_tail(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState { logs: fast(|_| {}), ..state };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!({ "message": "old" })).await;
    post_json(&public, &foreign, json!({ "message": "theirs" })).await;
    let old = nami_engine::logs::query::Cursor {
        dt_ms: (Utc::now() - chrono::Duration::minutes(10)).timestamp_millis(), id: Uuid::nil() }.encode();
    let long = "A".repeat(4096);
    for header in ["garbage", long.as_str(), old.as_str()] {
        let mut stream = SseStream::open_with(&app, &format!("/projects/{project}/logs/tail"), Some(&viewer), Some(header))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        post_json(&public, &token, json!({ "message": "new" })).await;
        assert_eq!(stream.next_log(Duration::from_secs(5)).await.unwrap()["message"], "new",
            "nothing replayed for {header:?}, never another project's rows");
    }
    ch.drop_database().await.unwrap();
}
```

(A header with CR/LF cannot be built at all: `http` refuses it, so `"garbage"` stands for "not a cursor", and the 4 KiB value checks the length cap before decoding.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib logs::query::tail` and engine command with `--test logs logs_tail::`.
Expected: FAIL (`resume_cursor` missing; `last_id` stays `None`).

- [ ] **Step 3: Implement resume**

- Each `event: log` gets `.id(Cursor { dt_ms: row.dt.timestamp_millis(), id: row.id }.encode())`.
- The handler reads `headers.get("last-event-id").and_then(|v| v.to_str().ok())` → `tail::resume_cursor(.., Utc::now())` and passes it to `run`.
- In `run`, when `resume` is `Some(cursor)`, the first poll is different: `since = from_ms(cursor.dt_ms)`, and the id query adds a flag column (a fixed fragment, values bound):

```rust
sql.push("SELECT id, dt, toBool((dt, id) > (fromUnixTimestamp64Milli(")
    .value("Int64", cursor.dt_ms.to_string())
    .push(", 'UTC'), ")
    .value("UUID", cursor.id.to_string())
    .push(")) AS after FROM logs");
```

  It is followed by the usual `push_where` / range / `ORDER BY dt DESC, id DESC LIMIT batch`. Rows with `after` are fresh: fetch them with `fetch_rows(.., since = cursor dt, until)` and send them oldest first. All others are remembered. Then `primed = true`, `resume = None`, and polling continues as before. When the resume poll (ids or rows) fails, `resume` is kept and the next tick tries it again. `resume_cursor` returns `None` for a header over 128 bytes before calling `Cursor::decode`. The tuple comparison runs in ClickHouse, whose `UUID` order is the ids' generation order (swapped halves, Part 1 ruling). Rust's `Uuid` ordering is different and must not be used for this.
- Update the module doc (resume, its 5-minute window, at most `TAIL_BATCH` rows replayed, late rows at or before the cursor not replayed).

- [ ] **Step 4: Run them to verify they pass**

Run: same as Step 2. Expected: PASS, and the existing tail tests unchanged.

- [ ] **Step 4b: Pin "a poll reads only new rows" (R-P2-25; controller follow-up)**

Append to `tests/logs/logs_tail.rs`:

```rust
#[sqlx::test]
async fn polls_read_the_list_rows_of_new_ids_only(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = fast(|_| {});
    let state = AppState { logs: logs.clone(), ..state };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    // 100 rows already there, with large messages: re-reading them would show in the counter.
    post_json(&public, &token, json!((0..100).map(|n| json!({ "message": format!("{n} {}", "x".repeat(4000)) })).collect::<Vec<_>>())).await;

    let mut stream = SseStream::open(&app, &format!("/projects/{project}/logs/tail"), Some(&viewer)).await.unwrap();
    let polls = |n: u64| logs.tail_stats.id_polls.load(std::sync::atomic::Ordering::Relaxed) >= n;
    common::logs::eventually("five polls", Duration::from_secs(5), || polls(5)).await;
    assert_eq!(logs.tail_stats.rows_fetched.load(std::sync::atomic::Ordering::Relaxed), 0,
        "rows stored before the tail are remembered by id, never read");
    post_json(&public, &token, json!([{ "message": "new 1" }, { "message": "new 2" }])).await;
    stream.next_log(Duration::from_secs(5)).await.unwrap();
    stream.next_log(Duration::from_secs(5)).await.unwrap();
    let after = logs.tail_stats.id_polls.load(std::sync::atomic::Ordering::Relaxed);
    common::logs::eventually("five more polls", Duration::from_secs(5), || polls(after + 5)).await;
    assert_eq!(logs.tail_stats.rows_fetched.load(std::sync::atomic::Ordering::Relaxed), 2);
    ch.drop_database().await.unwrap();
}
```

Run: engine command with `--test logs logs_tail::`. Expected: FAIL to compile (`tail_stats` missing). Implement: `fetch_ids` bumps `id_polls` after each successful query, and `fetch_rows` adds the number of rows it returned to `rows_fetched`, both with `Ordering::Relaxed`. Run again: PASS.

- [ ] **Step 5: Write the failing UI tests**

`nami-ui/src/lib/server/logs/log-proxy.test.ts`:

```ts
describe('lastEventIdHeader', () => {
	it('forwards only a plausible cursor', () => {
		expect(lastEventIdHeader('MTc5MjAwMDAwMDAwMDpjMTNiYzEwYS1kMTdm')).toBe('MTc5MjAwMDAwMDAwMDpjMTNiYzEwYS1kMTdm');
		expect(lastEventIdHeader(null)).toBeNull();
		expect(lastEventIdHeader('')).toBeNull();
		expect(lastEventIdHeader('a b')).toBeNull();
		expect(lastEventIdHeader('x'.repeat(129))).toBeNull();
	});
});
```

`nami-ui/src/routes/(app)/projects/[id=uuid]/logs/tail/server.test.ts` (append):

```ts
it('forwards a valid Last-Event-ID and nothing else from the browser', async () => {
	const fetchMock = vi.fn().mockResolvedValue(new Response('', { status: 200, headers: { 'content-type': 'text/event-stream' } }));
	const url = `http://ui.test/projects/${PROJECT}/logs/tail`;
	// This file's fakeEvent returns { event, controller }.
	const { event, controller } = fakeEvent(fetchMock, url);
	event.request = new Request(url, {
		headers: { cookie: 'id=session', 'last-event-id': 'abc_DEF-1', 'x-other': 'no' },
		signal: controller.signal
	});
	await GET(event);
	const headers = new Headers((fetchMock.mock.calls[0][1] as RequestInit).headers);
	expect(headers.get('last-event-id')).toBe('abc_DEF-1');
	expect(headers.get('x-other')).toBeNull();
});
```

(Reuse the file's existing `fakeEvent`/`PROJECT`. `callEngine` copies `init.headers` into a fresh `Headers` and passes it on, so the assertion reads what the engine really receives.)

`nami-ui/src/lib/logs/logs.test.ts`:

```ts
const row = (id: string, message = id): LogEntry => ({
	id, source_id: 'c13bc10a-d17f-443b-8976-c6d9b54e2798', dt: '2026-10-19T08:00:00.000Z', level: 'info',
	message, fields: {}, trace_id: '', span_id: '', pattern_hash: '1', truncated: false
} as LogEntry);

describe('live rows', () => {
	it('validates incoming rows', () => {
		expect(isLogEntry(row('a'))).toBe(true);
		expect(isLogEntry({ id: 'a' })).toBe(false);
		expect(isLogEntry({ ...row('a'), message: 5 })).toBe(false);
		expect(isLogEntry(null)).toBe(false);
	});
	it('prepends newest first, drops duplicates and caps', () => {
		const merged = mergeLiveRows([row('b'), row('a')], [row('b'), row('c'), row('d')], 3);
		expect(merged.map((r) => r.id)).toEqual(['d', 'c', 'b']);
	});
	it('restarts the tail only for filter changes', () => {
		const base = parseExplorerState(new URLSearchParams('level=error&live=1'));
		expect(tailKey({ ...base, columns: ['a.b'] })).toBe(tailKey(base));
		expect(tailKey({ ...base, range: '7d' })).toBe(tailKey(base));
		expect(tailKey({ ...base, q: 'disk' })).not.toBe(tailKey(base));
	});
});
```

(`LogEntry.truncated` is optional in the UI type since the fix wave. `isLogEntry` accepts it absent or boolean.)

`nami-ui/src/lib/logs/live-tail.test.ts` (the lifecycle test LOG-8 deferred, R-P2-21):

```ts
import { describe, expect, it } from 'vitest';
import { startLiveTail, type LiveStatus } from './live-tail';

class FakeEventSource {
	static CLOSED = 2;
	static last: FakeEventSource;
	readyState = 0;
	closed = false;
	onopen: (() => void) | null = null;
	onerror: (() => void) | null = null;
	listeners = new Map<string, (event: MessageEvent) => void>();
	constructor(public url: string) {
		FakeEventSource.last = this;
	}
	addEventListener(type: string, listener: (event: MessageEvent) => void) {
		this.listeners.set(type, listener);
	}
	close() {
		this.closed = true;
		this.readyState = FakeEventSource.CLOSED;
	}
	emit(type: string, data: unknown) {
		this.listeners.get(type)?.({ data: typeof data === 'string' ? data : JSON.stringify(data) } as MessageEvent);
	}
}

const row = (id: string) => ({
	id, source_id: 'c13bc10a-d17f-443b-8976-c6d9b54e2798', dt: '2026-10-19T08:00:00.000Z', level: 'info',
	message: id, fields: {}, trace_id: '', span_id: '', pattern_hash: '1'
});

function start() {
	const seen: { status: LiveStatus[]; rows: string[][]; notice: (string | null)[] } = { status: [], rows: [], notice: [] };
	const stop = startLiveTail('/p/logs/tail?level=error', {
		EventSourceImpl: FakeEventSource as unknown as typeof EventSource,
		cap: 2,
		onStatus: (status, notice) => {
			seen.status.push(status);
			seen.notice.push(notice);
		},
		onRows: (rows) => seen.rows.push(rows.map((r) => r.id))
	});
	return { seen, stop, source: FakeEventSource.last };
}

describe('startLiveTail', () => {
	it('connects, validates, dedupes and caps live rows', () => {
		const { seen, source } = start();
		expect(source.url).toBe('/p/logs/tail?level=error');
		expect(seen.status.at(-1)).toBe('connecting');
		source.onopen?.();
		expect(seen.status.at(-1)).toBe('live');
		source.emit('log', row('a'));
		source.emit('log', { id: 'broken' });
		source.emit('log', 'not json');
		source.emit('log', row('a'));
		source.emit('log', row('b'));
		source.emit('log', row('c'));
		expect(seen.rows.at(-1)).toEqual(['c', 'b']);
	});

	it('lets the browser reconnect after a timeout but stops for a dead session', () => {
		const first = start();
		first.source.emit('end', { reason: 'timeout' });
		expect(first.source.closed).toBe(false);
		expect(first.seen.status.at(-1)).toBe('reconnecting');
		const second = start();
		second.source.emit('end', { reason: 'session' });
		expect(second.source.closed).toBe(true);
		expect(second.seen.status.at(-1)).toBe('ended');
	});

	it('stops cleanly', () => {
		const { stop, source } = start();
		stop();
		expect(source.closed).toBe(true);
	});
});
```

- [ ] **Step 6: Run them to verify they fail**

Run: `cd nami-ui && npx vitest run src/lib/logs src/lib/server/logs "src/routes/(app)/projects/[id=uuid]/logs"`.
Expected: FAIL (exports and `lib/logs/live-tail.ts` missing).

- [ ] **Step 7: Implement the UI changes**

- `lastEventIdHeader`: `value && /^[A-Za-z0-9_-]{1,128}$/.test(value) ? value : null`.
- Tail proxy: `const lastEventId = lastEventIdHeader(event.request.headers.get('last-event-id'))`. The engine request headers are `{ accept: 'text/event-stream', ...(lastEventId ? { 'last-event-id': lastEventId } : {}) }`.
- `isLogEntry`: an object with string `id`, `source_id`, `dt`, `level`, `message`, `trace_id`, `span_id`, `pattern_hash`, and `fields` an object.
- `mergeLiveRows`: `[...incoming].reverse()` concatenated with `existing`, keeping the first occurrence of each id, sliced to `cap`.
- `tailKey(state)`: `tailParams(state).toString()`.
- `lib/logs/live-tail.ts`: `export type LiveStatus = 'connecting' | 'live' | 'reconnecting' | 'ended'` and `startLiveTail(url, { EventSourceImpl = EventSource, cap = LIVE_ROW_CAP, onStatus(status, notice), onRows(rows) }): () => void`. It holds the live rows itself and moves today's `$effect` body there unchanged in behaviour: `log` events parsed, kept only when `isLogEntry`, merged with `mergeLiveRows(rows, [row], cap)` and handed to `onRows`; `end` with `session` closes and reports `ended`, any other `end` reports `reconnecting` and leaves the reconnect to `EventSource` (which resends `Last-Event-ID`); `onerror` as today, comparing `readyState` with `EventSourceImpl.CLOSED`; `pagehide` closes (registered only when `typeof window !== 'undefined'`: vitest runs in Node). The returned function removes the `pagehide` listener and closes the stream.
- `+page.svelte`:
  - `const liveKey = $derived(data.state.live ? tailKey(data.state) : null)`. The tail `$effect` reads only `liveKey` (`null` → status `off`, live rows cleared) and otherwise returns `startLiveTail(\`${path}/tail?${liveKey}\`, { onStatus, onRows })` as its cleanup.
  - LOG-8 leftovers: `+page.server.ts` returns `histogramError` and `sourcesError` (`engineErrorMessage` on a failed call, else null) and the page shows them where the chart and the source picker would be. The fields `<pre>` gets `whitespace-pre-wrap wrap-anywhere` (the message `<pre>` has them already). The live status line gets `role="status" aria-live="polite"`. Append to `explorer.test.ts`: a 500 histogram and a 500 sources call give `histogramError`/`sourcesError` strings and the page data still loads.
  - "Load more": `let generation = 0; let controller: AbortController | null = null;`. An `$effect` that reads `data.logs` (a new navigation) runs `generation += 1; controller?.abort()` (`rows` and `nextCursor` are writable `$derived` values and reset by themselves, see the Drift note). `loadMore` captures `const mine = generation`, creates a controller, passes `signal`, and drops the result if `mine !== generation`. An `AbortError` is silent.

- [ ] **Step 8: Run all checks; do a manual walkthrough; commit**

Run: engine command (all) + clippy, `cd nami-ui && npm run check && npx vitest run`.
Walkthrough (engine + nami-ui dev on spare ports, so other worktrees' servers are not disturbed):
1. Start live tail. Change the columns: the "Connecting…" state does not reappear.
2. Click "Load more", then immediately change the level: no stale rows appear.
3. Stop the engine for 10 s and restart it: after the reconnect, rows sent meanwhile appear once.

```bash
git add nami-engine/src/logs/query/tail.rs nami-engine/src/logs/mod.rs nami-engine/src/logs/query/routes.rs nami-engine/tests/logs/logs_tail.rs nami-ui/src
git commit -m "feat: live tail resumes after reconnects; explorer tail and load-more fixes (NAMI-LOG-15)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 16 (NAMI-LOG-16): UI — pipeline editor with live preview, 30-day usage chart

On `feat/logs-part2`. Commits carry `(NAMI-LOG-16)`.

**Files:**
- Create: `nami-ui/src/lib/logs/pipeline.ts` (+ `pipeline.test.ts`), `nami-ui/src/lib/server/core/json-relay.ts` (+ `json-relay.test.ts`; generic BFF code beside `relay-stream.ts`, T18 reuses it), `nami-ui/src/lib/components/logs/PipelineEditor.svelte`, `nami-ui/src/lib/components/UsageChart.svelte`
- Create: `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/sources/[sourceId=uuid]/pipeline/+page.server.ts`, `+page.svelte`, `pipeline.test.ts`, `pipeline/test/+server.ts`, `pipeline/test/server.test.ts`
- Modify: `.../logs/sources/[sourceId=uuid]/+page.server.ts`, `+page.svelte`, `source-detail.test.ts` (usage `days=30`, link to the pipeline page); `nami-ui/src/lib/logs/logs.ts` (+ test) `usageBars`
- Test: the files above

**Drift (2026-10-24):**
- `DailyUsage` (`nami-ui/src/lib/logs/logs.ts:32-40`) already carries every counter the engine returns (`day`, `events`, `bytes`, `events_dropped`, `events_sampled_out`, `requests_rate_limited`, `requests_over_quota`; engine `nami-engine/src/logs/ingest/usage.rs:57-62`). The usage endpoint answers the last `days` days oldest first and zero-filled (`usage.rs:273-296`), so `usage.at(-1)` is today. The source page asks `usage?days=1` and takes `[0]` today (`.../logs/sources/[sourceId=uuid]/+page.server.ts`, `source-detail.test.ts:24`).
- The Part 1 chart palette is gone: charts use design tokens (`nami-ui/src/routes/layout.css:53-59`, `--color-series-1..6`; `LogVolumeChart.svelte:22-26` uses `fill-danger`, `fill-warning`, `fill-accent-periwinkle`), and CLAUDE.md forbids raw hex in pages. `UsageChart` draws events with `fill-primary` and bytes with `fill-accent-cyan` (the colours of `--color-series-1` and `--color-series-2`), with a `Legend` of tones `primary` and `cyan` (`Legend` takes a `tone`, `#lib/components/ui/Legend.svelte:5`), the way `LogVolumeChart` pairs fill classes with tones, instead of `#2dd4bf` / `#737373`.

**Interfaces:**
- Consumes: engine `GET/PUT .../pipeline`, `POST .../pipeline/test` (T10), `GET .../usage?days=30` (Part 1).
- Produces (T18 reuses `relayJsonPost`):

```ts
// lib/logs/pipeline.ts
export type RuleKind = 'drop' | 'remove_fields' | 'redact' | 'rename' | 'sample';
export const RULE_KINDS: { value: RuleKind; label: string }[];
export type Match = { field?: string; equals?: string; message_regex?: string };
export type Rule = { type: RuleKind; match?: Match; paths?: string[]; keys?: string[]; regex?: string; from?: string; to?: string; n?: number };
export const MAX_RULES = 50;
export const MAX_REGEX_CHARS = 256;
export const MAX_REGEXES = 20;
export const MAX_TEST_EVENTS = 100;
export type RuleDraft = { type: RuleKind; matchMode: 'none' | 'field' | 'regex'; field: string; equals: string;
  messageRegex: string; paths: string; keys: string; regex: string; from: string; to: string; n: string };
export function emptyDraft(type: RuleKind): RuleDraft;
export function draftFromRule(rule: Rule): RuleDraft;
export function ruleFromDraft(draft: RuleDraft): { ok: true; rule: Rule } | { ok: false; message: string };
export function parseSampleEvents(text: string): { ok: true; events: Record<string, unknown>[] } | { ok: false; message: string };
export function describeRule(rule: Rule): string;
export type TestResult =
  | { outcome: 'kept'; event: { dt: string; level: string; message: string; fields: Record<string, unknown>; trace_id: string; span_id: string; pattern: string } }
  | { outcome: 'dropped' } | { outcome: 'sampled_out' };

// lib/server/core/json-relay.ts
export const MAX_RELAY_BYTES = 1024 * 1024;
/** Same-origin JSON POST → engine `path`, answered with the engine's status and JSON. */
export async function relayJsonPost(event: RequestEvent, path: string): Promise<Response>;

// lib/logs/logs.ts
export type UsageBar = { day: string; events: number; bytes: number; eventsShare: number; bytesShare: number;
  dropped: number; sampledOut: number; rejected: number };
export function usageBars(days: DailyUsage[]): UsageBar[];
```

`equals` is always sent as a string. The engine compares a string with a field's JSON text (R-P2-4), so `"200"` matches both `200` and `"200"`, and the UI needs no type picker.

- [ ] **Step 1: Write the failing lib tests** (`nami-ui/src/lib/logs/pipeline.test.ts`)

```ts
import { describe, expect, it } from 'vitest';
import { describeRule, draftFromRule, emptyDraft, parseSampleEvents, ruleFromDraft, type Rule } from './pipeline';

describe('rule drafts', () => {
	it('round-trip every kind', () => {
		const rules: Rule[] = [
			{ type: 'drop', match: { field: 'level', equals: 'debug' } },
			{ type: 'drop', match: { message_regex: '^healthz' } },
			{ type: 'remove_fields', paths: ['user.email', 'password'] },
			{ type: 'redact', keys: ['password', 'token'], regex: 'sk_[a-z0-9]+' },
			{ type: 'rename', from: 'msg', to: 'message' },
			{ type: 'sample', n: 10 },
			{ type: 'sample', n: 2, match: { field: 'level', equals: 'info' } }
		];
		for (const rule of rules) expect(ruleFromDraft(draftFromRule(rule))).toEqual({ ok: true, rule });
	});

	it('reject what the engine would reject, with a readable message', () => {
		const cases: [Partial<ReturnType<typeof emptyDraft>> & { type: Rule['type'] }, string][] = [
			[{ type: 'drop', matchMode: 'none' }, 'match'],
			[{ type: 'drop', matchMode: 'field', field: 'a..b', equals: 'x' }, 'path'],
			[{ type: 'redact' }, 'keys or a regex'],
			[{ type: 'redact', regex: 'a'.repeat(257) }, '256'],
			[{ type: 'sample', n: '1' }, '2'],
			[{ type: 'sample', n: '10001' }, '10000'],
			[{ type: 'remove_fields', paths: 'level' }, 'level'],
			[{ type: 'rename', from: '', to: 'x' }, 'from']
		];
		for (const [patch, needle] of cases) {
			const result = ruleFromDraft({ ...emptyDraft(patch.type), ...patch });
			expect(result.ok, JSON.stringify(patch)).toBe(false);
			if (!result.ok) expect(result.message.toLowerCase()).toContain(needle);
		}
	});

	it('describes rules in one line', () => {
		expect(describeRule({ type: 'sample', n: 10 })).toBe('Keep 1 in 10 events');
		expect(describeRule({ type: 'drop', match: { field: 'level', equals: 'debug' } })).toBe('Drop when level = debug');
	});
});

describe('sample events', () => {
	it('accept an array, one object or NDJSON', () => {
		expect(parseSampleEvents('[{"a":1},{"b":2}]')).toEqual({ ok: true, events: [{ a: 1 }, { b: 2 }] });
		expect(parseSampleEvents('{"a":1}')).toEqual({ ok: true, events: [{ a: 1 }] });
		expect(parseSampleEvents('{"a":1}\n\n{"b":2}\n')).toEqual({ ok: true, events: [{ a: 1 }, { b: 2 }] });
	});
	it('reject non-objects, bad JSON and more than 100 events', () => {
		expect(parseSampleEvents('[1]').ok).toBe(false);
		expect(parseSampleEvents('{nope').ok).toBe(false);
		expect(parseSampleEvents('').ok).toBe(false);
		expect(parseSampleEvents(JSON.stringify(Array(101).fill({ a: 1 }))).ok).toBe(false);
	});
});
```

`nami-ui/src/lib/logs/logs.test.ts` (append):

```ts
describe('usageBars', () => {
	it('scales each series to its own maximum and sums the rejections', () => {
		const day = (d: string, events: number, bytes: number) => ({
			day: d, events, bytes, events_dropped: 1, events_sampled_out: 2, requests_rate_limited: 3, requests_over_quota: 4
		});
		const bars = usageBars([day('2026-10-18', 50, 4000), day('2026-10-19', 100, 1000)]);
		expect(bars.map((b) => [b.eventsShare, b.bytesShare])).toEqual([[0.5, 1], [1, 0.25]]);
		expect(bars[0]).toMatchObject({ dropped: 1, sampledOut: 2, rejected: 7 });
		expect(usageBars([day('2026-10-19', 0, 0)])[0].eventsShare).toBe(0);
	});
});
```

(`DailyUsage` in `lib/logs/logs.ts` must carry every counter the engine returns; it already does, see the Drift note.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cd nami-ui && npx vitest run src/lib/logs/pipeline.test.ts src/lib/logs/logs.test.ts`.
Expected: FAIL (module missing).

- [ ] **Step 3: Implement `lib/logs/pipeline.ts` and `usageBars`**

Client checks mirror the engine (path rule = `isFieldPath`, regex length ≤ 256, at most 20 regexes in the whole list (`MAX_REGEXES`, shown on the "Add rule" button state), `n` 2-10000, ≤ 50 keys/paths, `level`/`message` not removable). Regex validity is left to the engine; the preview shows its 400. `paths` and `keys` drafts are comma- or newline-separated. `parseSampleEvents`: trimmed text; `[`… → `JSON.parse` array; `{`… → try one object, else split lines and parse each non-empty line. Every element must be a plain object, and there are at most `MAX_TEST_EVENTS`. `describeRule` texts: `Drop when <field> = <value>` / `Drop when message matches /<re>/` / `Remove <paths>` / `Redact keys <keys>` + ` and /<re>/` / `Rename <from> → <to>` / `Keep 1 in <n> events` (+ ` when …`).

- [ ] **Step 4: Run them to verify they pass**

Run: same as Step 2. Expected: PASS.

- [ ] **Step 5: Write the failing relay and page tests**

`nami-ui/src/lib/server/core/json-relay.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { MAX_RELAY_BYTES, relayJsonPost } from './json-relay';

function post(event: any, body: string, headers: Record<string, string> = {}) {
	event.request = new Request('http://ui.test/x', {
		method: 'POST',
		body,
		headers: { 'content-type': 'application/json', origin: 'http://ui.test', ...headers }
	});
	event.url = new URL('http://ui.test/x');
	return event;
}

describe('relayJsonPost', () => {
	it('forwards the JSON body and passes the answer through', async () => {
		const { event, calls } = fakeEvent({ respond: () => new Response('{"results":[]}', { status: 200 }) });
		const response = await relayJsonPost(post(event, '{"rules":[],"events":[]}'), `/projects/${PROJECT_ID}/sources/s/pipeline/test`);
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ results: [] });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/sources/s/pipeline/test`, body: '{"rules":[],"events":[]}' });
	});

	it('refuses other content types, other origins, bad JSON and large bodies', async () => {
		const cases: [string, Record<string, string>, number][] = [
			['{}', { 'content-type': 'text/plain' }, 415],
			['{}', { origin: 'http://evil.test' }, 403],
			['{nope', {}, 400],
			['"x"'.padEnd(MAX_RELAY_BYTES + 10, ' '), {}, 413]
		];
		for (const [body, headers, status] of cases) {
			const { event, calls } = fakeEvent();
			const response = await relayJsonPost(post(event, body, headers), '/x');
			expect(response.status, JSON.stringify(headers)).toBe(status);
			expect(calls()).toHaveLength(0);
		}
	});

	it('passes engine errors through with their message', async () => {
		for (const status of [400, 403, 404, 429]) {
			const { event } = fakeEvent({ respond: () => new Response('{"error":"rules[1]: bad"}', { status }) });
			const response = await relayJsonPost(post(event, '{}'), '/x');
			expect(response.status).toBe(status);
			expect(await response.json()).toEqual({ error: 'rules[1]: bad' });
		}
	});
});
```

`.../logs/sources/[sourceId=uuid]/pipeline/pipeline.test.ts` (page server):

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const SOURCE = '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69';
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const PATH = `/projects/${PROJECT_ID}/sources/${SOURCE}`;

describe('pipeline page', () => {
	it('loads the source and its rules', async () => {
		const rules = [{ type: 'sample', n: 2 }];
		const { event } = fakeEvent({
			params: { sourceId: SOURCE },
			respond: (url) => (url.pathname.endsWith('/pipeline') ? json({ rules, updated_at: 't' }) : json({ id: SOURCE, name: 'api' }))
		});
		expect(await load(event)).toMatchObject({ source: { name: 'api' }, rules });
	});

	it('saves the rules as JSON and shows the engine error beside the editor', async () => {
		const ok = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[{"type":"sample","n":2}]' }, respond: () => json({ rules: [], updated_at: 't' }) });
		expect(await actions.save(ok.event)).toEqual({ saved: true });
		expect(ok.calls()[0]).toMatchObject({ method: 'PUT', path: `${PATH}/pipeline`, body: '{"rules":[{"type":"sample","n":2}]}' });

		const bad = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[]' }, respond: () => json({ error: 'rules[0]: nope' }, 400) });
		expect(await actions.save(bad.event)).toMatchObject({ status: 400, data: { message: 'rules[0]: nope' } });

		const garbage = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '{nope' } });
		expect(await actions.save(garbage.event)).toMatchObject({ status: 400 });
		expect(garbage.calls()).toHaveLength(0);

		const viewer = fakeEvent({ params: { sourceId: SOURCE }, form: { rules: '[]' }, respond: () => json({ error: 'forbidden' }, 403) });
		expect(await actions.save(viewer.event)).toMatchObject({ status: 403 });
	});
});
```

`pipeline/test/server.test.ts`: a `POST` with a JSON body reaches `${PATH}/pipeline/test`; a non-uuid `sourceId` → 404 thrown before any call. (The relay rules themselves are covered by `json-relay.test.ts`.)

`source-detail.test.ts`: update the expectations to `${PATH}/usage?days=30`, `data.today` = the last element, and `data.usage` = the array.

- [ ] **Step 6: Run them to verify they fail**

Run: `cd nami-ui && npx vitest run src/lib/server/core/json-relay.test.ts "src/routes/(app)/projects/[id=uuid]/logs/sources"`.
Expected: FAIL.

- [ ] **Step 7: Implement the relay, routes and components**

`nami-ui/src/lib/server/core/json-relay.ts`:

```ts
import type { RequestEvent } from '@sveltejs/kit';
import { callEngine } from '#lib/server/core/engine';

export const MAX_RELAY_BYTES = 1024 * 1024;

const reply = (status: number, error: string) =>
	new Response(JSON.stringify({ error }), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });

/**
 * A small same-origin JSON POST (an editor's preview) relayed to nami-engine.
 * Only `application/json` is accepted: browsers cannot send that cross-site
 * without a CORS preflight, which this app never answers, and an `Origin`
 * from elsewhere is refused outright. The body is read once, bounded, and
 * checked to be JSON before the engine sees it.
 */
export async function relayJsonPost(event: RequestEvent, path: string): Promise<Response> {
	const type = event.request.headers.get('content-type') ?? '';
	if (!type.toLowerCase().startsWith('application/json')) return reply(415, 'expected a JSON body');
	const origin = event.request.headers.get('origin');
	if (origin && origin !== event.url.origin) return reply(403, 'cross-origin requests are not allowed');
	const declared = Number(event.request.headers.get('content-length') ?? '0');
	if (declared > MAX_RELAY_BYTES) return reply(413, 'the request is too large');
	const body = await event.request.text();
	if (new TextEncoder().encode(body).length > MAX_RELAY_BYTES) return reply(413, 'the request is too large');
	try {
		JSON.parse(body);
	} catch {
		return reply(400, 'the request body is not valid JSON');
	}
	let response: Response;
	try {
		response = await callEngine(event, path, { method: 'POST', body });
	} catch {
		return reply(502, 'the log service is unreachable');
	}
	return new Response(await response.text(), {
		status: response.status,
		headers: { 'content-type': response.headers.get('content-type') ?? 'application/json', 'cache-control': 'no-store' }
	});
}
```

(`callEngine` sets `content-type: application/json` for bodies. Check and keep its behaviour.)

`pipeline/test/+server.ts`: `POST` → `isUuid(id) && isUuid(sourceId)` else `error(404)` → `relayJsonPost(event, enginePath\`/projects/${id}/sources/${sourceId}/pipeline/test\`)`.

`pipeline/+page.server.ts`: the `load` GETs the source and the pipeline (401 → login, 403/404 → `error`) and returns `{ source, rules, updatedAt }`. `actions.save` parses the `rules` form field with `JSON.parse` (it must be an array, else `fail(400, { message: 'The rules are not valid JSON.' })`), PUTs `{ rules }`, and maps failures through `engineFailure`.

`PipelineEditor.svelte` (props: `rules: Rule[]`, `previewUrl: string`):
- An ordered list of rule cards. Each has a kind select, the fields of its kind, a match mode (none / field equals / message regex), move up/down and remove buttons, and "Add rule" (disabled at 50). Each card shows its `ruleFromDraft` error inline.
- The hidden `rules` input carries `JSON.stringify(valid rules)`. Save is disabled while any draft is invalid.
- "Sample events" textarea (prefilled with two example events) and a "Preview" panel. Preview runs automatically 500 ms after the last change (when the drafts and the samples are valid) with `fetch(previewUrl, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ rules, events }) })`. An in-flight preview is aborted by the next one. Each result is shown as a badge (`kept` / `dropped` / `sampled out`) with the stored event pretty-printed. An engine error is shown above the results, mapping `rules[i]` to "Rule i+1". Sample counters restart on each preview, which the panel states.
- All values render as text (no `{@html}`).

`+page.svelte`: the title `Pipeline · <source name>`, a short help text in spec order ("Rules run top to bottom after parsing and size caps, before patterns and storage; edits reach ingestion on the next request; redacted values are never stored; a redact rule covers what is there at its position, so put it after any rename that moves a secret"), the editor, and a form posting `?/save` with `use:enhance` and a submit guard.

Source detail page: the `load` asks for `usage?days=30`, `today = usage.at(-1) ?? null`, and also returns `usage`. A "Pipeline" link goes to `./<sourceId>/pipeline`. `UsageChart.svelte` (props `days: DailyUsage[]`) draws two rows of 30 bars (events per day, bytes per day, `formatBytes`). Each bar has a `title` tooltip with the UTC date, events, bytes, dropped, sampled out and rejected. A visually hidden `<table>` repeats the data for screen readers. Colours: events `fill-primary`, bytes `fill-accent-cyan`, with a `Legend` of tones `primary` / `cyan` (Drift note above).

- [ ] **Step 8: Run all checks; walkthrough; commit**

Run: `cd nami-ui && npm run check && npx vitest run`.
Expected: 0 errors / 0 warnings; all tests pass.
Walkthrough (engine + UI on spare ports):
1. As a member, add a redact rule and a drop rule, paste samples, and watch the preview update. Save.
2. Ingest with curl: the redacted value is absent in the explorer.
3. As a viewer, Save shows the 403 message.
4. The usage chart shows today's bar after the 10 s flush.

```bash
git add nami-ui/src
git commit -m "feat(ui): pipeline editor with live preview and 30-day source usage chart (NAMI-LOG-16)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17 (NAMI-LOG-17): UI — explorer Patterns tab, export and saved views

On `feat/logs-part2`. Commits carry `(NAMI-LOG-17)`.

**Files:**
- Modify: `nami-ui/src/lib/logs/logs.ts` (+ test), `nami-ui/src/lib/server/logs/log-proxy.ts` (+ test) `PATTERN_PARAMS`, `.../logs/+page.server.ts`, `.../logs/+page.svelte`, `.../logs/explorer.test.ts`
- Create: `nami-ui/src/lib/components/logs/PatternsTable.svelte`, `ExportMenu.svelte`, `SavedViews.svelte`. The full-entry proxy and the expand-to-full-row behaviour already shipped with the fix wave (`f3b358f`) and are reused unchanged.

**Drift (2026-10-24):**
- The UI conventions (CLAUDE.md, "UI conventions") came after this task was written. The Results | Patterns tabs use `SegmentedTabs` in link mode (`#lib/components/ui/SegmentedTabs.svelte`, as `logs/+layout.svelte` does for its sections), which renders the labelled `<nav>` R-P2-21 asks for; the pattern share bar and chips use tokens (`bg-primary`, `.badge-*`), never raw hex or stock Tailwind colours.
- Export already has its BFF route and allow-list from T12 (`.../logs/export/+server.ts`, `EXPORT_PARAMS` in `nami-ui/src/lib/server/logs/log-proxy.ts:28-35`); this task only adds the explorer side (`exportHref`, `exportWarning`, `ExportMenu`).

**Interfaces:**
- Consumes: engine `/logs/patterns` (T9), `/logs/export` via the T12 proxy, `/log-views` (T13).
- Produces:

```ts
// lib/logs/logs.ts
export type ExplorerTab = 'results' | 'patterns';
// ExplorerState gains: tab: ExplorerTab (URL `tab=patterns`; default results) and
// patterns: string[] (URL `pattern`, repeatable, at most 100, deduplicated; `isPatternHash`:
// /^\d{1,20}$/ and BigInt(value) <= 18446744073709551615n, the engine's u64 range)
export function isPatternHash(value: string): boolean;
// explorerParams appends `tab` (only when 'patterns') and then each `pattern` after `col`
// and before `live`.
export type PatternCount = { pattern_hash: string; pattern: string; count: number; first_seen: string; last_seen: string };
export const EXPORT_ROW_LIMIT = 100_000;
export type ExportFormat = 'csv' | 'ndjson';
export function exportHref(path: string, state: ExplorerState, window: { from: string; to: string }, format: ExportFormat): string;
/** The warning to show before an export of `total` rows, or null. */
export function exportWarning(total: number): string | null;
export type ViewQuery = { range: TimePreset | 'custom'; from?: string; to?: string; q?: string; in_fields?: boolean;
  levels?: LogLevel[]; source_ids?: string[]; filters?: FieldFilter[]; pattern_hashes?: string[]; columns?: string[] };
export type LogView = { id: string; name: string; query: ViewQuery; created_at: string; updated_at: string };
export function viewQueryFromState(state: ExplorerState): ViewQuery;   // never live/tab
export function stateFromViewQuery(query: ViewQuery): ExplorerState;  // through parseExplorerState's rules
```

`tailParams` (and so `engineParams`) appends one `pattern_hash` per `state.patterns`. `exportHref` = `${path}/export?` + `engineParams(state, window)` + `format` + one `columns` per `state.columns` (CSV only).

- [ ] **Step 1: Write the failing lib tests** (append to `nami-ui/src/lib/logs/logs.test.ts`)

```ts
describe('explorer state, part 2', () => {
	it('reads the tab and pattern filters from the URL', () => {
		// 18446744073709551615 is u64::MAX; one more is not a pattern hash (the engine would answer 400).
		const state = parseExplorerState(new URLSearchParams(
			'tab=patterns&pattern=123&pattern=123&pattern=x&pattern=18446744073709551615&pattern=18446744073709551616'
		));
		expect(state.tab).toBe('patterns');
		expect(state.patterns).toEqual(['123', '18446744073709551615']);
		expect(parseExplorerState(new URLSearchParams('tab=weird')).tab).toBe('results');
		expect(explorerParams(state).toString()).toBe('tab=patterns&pattern=123&pattern=18446744073709551615');
		expect(tailParams(state).getAll('pattern_hash')).toEqual(['123', '18446744073709551615']);
	});

	it('builds export links with the absolute window and the picked columns', () => {
		const state = parseExplorerState(new URLSearchParams('level=error&col=user.id&live=1&tab=patterns'));
		const window = { from: '2026-10-19T07:00:00.000Z', to: '2026-10-19T08:00:00.000Z' };
		const csv = new URL(exportHref('/projects/p/logs', state, window, 'csv'), 'http://ui.test');
		expect(csv.pathname).toBe('/projects/p/logs/export');
		expect(Object.fromEntries(csv.searchParams)).toMatchObject({ level: 'error', from: window.from, to: window.to, format: 'csv', columns: 'user.id' });
		expect(csv.searchParams.has('live') || csv.searchParams.has('tab')).toBe(false);
		const ndjson = new URL(exportHref('/projects/p/logs', state, window, 'ndjson'), 'http://ui.test');
		expect(ndjson.searchParams.has('columns')).toBe(false);
	});

	it('warns before a truncated export', () => {
		expect(EXPORT_ROW_LIMIT).toBe(100_000);
		expect(exportWarning(100_000)).toBeNull();
		expect(exportWarning(250_000)).toBe('Only the newest 100,000 of 250,000 matching rows will be exported.');
	});

	it('view state round-trips and stays relative', () => {
		const state = parseExplorerState(new URLSearchParams(
			'range=6h&q=disk&in_fields=1&level=error&source=c13bc10a-d17f-443b-8976-c6d9b54e2798&col=a.b&pattern=7&live=1&tab=patterns&filter=' +
				encodeURIComponent('{"path":"a","op":"gt","value":"5"}')
		));
		const query = viewQueryFromState(state);
		expect(query).toEqual({
			range: '6h', q: 'disk', in_fields: true, levels: ['error'], source_ids: ['c13bc10a-d17f-443b-8976-c6d9b54e2798'],
			filters: [{ path: 'a', op: 'gt', value: '5' }], pattern_hashes: ['7'], columns: ['a.b']
		});
		expect(stateFromViewQuery(query)).toEqual({ ...state, live: false, tab: 'results' });
		const custom = viewQueryFromState(parseExplorerState(new URLSearchParams('range=custom&from=2026-10-01T00:00:00.000Z&to=2026-10-02T00:00:00.000Z')));
		expect(custom).toEqual({ range: 'custom', from: '2026-10-01T00:00:00.000Z', to: '2026-10-02T00:00:00.000Z' });
		expect(stateFromViewQuery({ range: '1h', levels: ['loud' as any] }).levels).toEqual([]);
	});
});
```

- [ ] **Step 2: Run them to verify they fail; implement; run them to verify they pass**

Run: `cd nami-ui && npx vitest run src/lib/logs/logs.test.ts`. FAIL → implement in `lib/logs/logs.ts` → PASS. `stateFromViewQuery` builds a `URLSearchParams` from the query and calls `parseExplorerState` on it, so every validation rule stays in one place. `exportWarning` uses `toLocaleString('en-US')`.

- [ ] **Step 3: Write the failing load/action/proxy tests** (append to `logs/explorer.test.ts`; extend the `engine()` helper with `patterns` and `views` responses)

```ts
describe('explorer, part 2', () => {
	it('the patterns tab loads patterns instead of a results page', async () => {
		const { event, calls } = fakeEvent({
			url: 'http://ui.test/p?tab=patterns&level=error&pattern=5',
			respond: (url) => {
				if (url.pathname.endsWith('/logs/patterns')) return json({ patterns: [{ pattern_hash: '5', pattern: 'x <num>', count: 3, first_seen: 'a', last_seen: 'b' }] });
				if (url.pathname.endsWith('/log-views')) return json([]);
				return engine()(url);
			}
		});
		const data: any = await load(event);
		expect(data.patterns).toHaveLength(1);
		const paths = calls().map((c) => c.path);
		expect(paths).toContain(`/projects/${PROJECT_ID}/logs/patterns`);
		expect(paths).not.toContain(`/projects/${PROJECT_ID}/logs`);
		const patterns = calls().find((c) => c.path.endsWith('/logs/patterns'))!;
		const params = new URLSearchParams(patterns.search);
		expect(params.getAll('pattern_hash')).toEqual(['5']);
		expect(params.has('limit') || params.has('cursor')).toBe(false);
	});

	it('loads saved views, and a failed views call does not break the page', async () => {
		const views = [{ id: 'v1', name: 'Errors', query: { range: '1h' } }];
		const ok = fakeEvent({ url: 'http://ui.test/p', respond: (url) => (url.pathname.endsWith('/log-views') ? json(views) : engine()(url)) });
		expect(((await load(ok.event)) as any).views).toEqual(views);
		const down = fakeEvent({ url: 'http://ui.test/p', respond: (url) => (url.pathname.endsWith('/log-views') ? json({}, 500) : engine()(url)) });
		expect(((await load(down.event)) as any).views).toEqual([]);
	});

	it('saves the current state as a view, without live or tab', async () => {
		const state = 'range=24h&level=error&live=1&tab=patterns';
		const { event, calls } = fakeEvent({ form: { name: 'Errors', state }, respond: () => json({ id: 'v1' }, 201) });
		expect(await actions.saveView(event)).toEqual({ savedView: 'Errors' });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `/projects/${PROJECT_ID}/log-views` });
		expect(JSON.parse(calls()[0].body!)).toEqual({ name: 'Errors', query: { range: '24h', levels: ['error'] } });
	});

	it('updates and deletes views with checked ids and a confirmed delete', async () => {
		const VIEW = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
		const update = fakeEvent({ form: { view_id: VIEW, state: 'q=x' }, respond: () => json({ id: VIEW }) });
		expect(await actions.updateView(update.event)).toEqual({ updatedView: true });
		expect(update.calls()[0]).toMatchObject({ method: 'PATCH', path: `/projects/${PROJECT_ID}/log-views/${VIEW}` });
		const bare = fakeEvent({ form: { view_id: VIEW } });
		expect(await actions.deleteView(bare.event)).toEqual({ confirmDeleteView: VIEW });
		expect(bare.calls()).toHaveLength(0);
		// `new Response('', { status: 204 })` throws (204 is a null-body status): answer with a null body.
		const confirmed = fakeEvent({ form: { view_id: VIEW, confirm: 'yes' }, respond: () => new Response(null, { status: 204 }) });
		expect(await actions.deleteView(confirmed.event)).toEqual({ deletedView: true });
		const forged = fakeEvent({ form: { view_id: '../../sources', confirm: 'yes' } });
		expect(await actions.deleteView(forged.event)).toMatchObject({ status: 400 });
		expect(forged.calls()).toHaveLength(0);
	});
});
```

(Import `actions` alongside `load` at the top of the file.)

- [ ] **Step 4: Run them to verify they fail**

Run: `cd nami-ui && npx vitest run "src/routes/(app)/projects/[id=uuid]/logs"`.
Expected: FAIL.

- [ ] **Step 5: Implement the load, actions, proxy and components**

- `+page.server.ts`: on `state.tab === 'patterns'`, call `${base}/logs/patterns` + `pickLogParams(filters, PATTERN_PARAMS)` (`PATTERN_PARAMS = [...FILTER_PARAMS, 'from', 'to']`) instead of the search, and return `patterns` (with `patternsError` from `engineErrorMessage` on a 400/429). Views come from `${base}/log-views` (non-fatal, `[]` on failure), requested **after** the existing calls so the first test's `const [search, histogram, sources] = calls()` still holds. Actions:
  - `saveView`: `name` and `state`. `state` is the explorer query string, re-parsed with `parseExplorerState` and turned into a view query by `viewQueryFromState`. The engine is sent `{name, query}`.
  - `updateView`: `view_id` (`isUuid`, else `fail(400)`) and `state` → PATCH `{query}`.
  - `deleteView`: `view_id` (`isUuid`) and `confirm=yes`, else return `{ confirmDeleteView: id }` → DELETE.
  - Every failure goes through `engineFailure`.
- `+page.svelte`:
  - Tabs "Results | Patterns" as links that set `tab`. Patterns render `PatternsTable` (pattern in monospace as text, count, a share bar relative to the top count, first/last seen in UTC). A click on a pattern navigates with `patterns: [...state.patterns, hash]` and `tab: 'results'`. A note says "counts are approximate for very large windows".
  - Active pattern filters show as removable chips.
  - `ExportMenu` (CSV / NDJSON links built by `exportHref` with `download`). When `exportWarning(histogram.total)` is non-null, a click first asks `confirm(warning)`.
  - `SavedViews`: a select of views (option → `explorerHref(path, stateFromViewQuery(view.query))`), "Save as view" (name input + hidden `state` = `explorerParams(state).toString()`), "Update" for the selected view, and a two-step "Delete".

- [ ] **Step 6: Run all checks; walkthrough; commit**

Run: `cd nami-ui && npm run check && npx vitest run`.
Walkthrough:
1. Patterns tab, then click a pattern: the results show only that pattern.
2. Export CSV and NDJSON. Open the CSV in a spreadsheet: a `=1+1` message shows as text.
3. Make a window with more than 100,000 rows (or lower `EXPORT_ROW_LIMIT` temporarily in a scratch run): the warning appears.
4. Save a view on "Last 6 h", reload it an hour later or with a changed clock: the window is still the last 6 h.

```bash
git add nami-ui/src
git commit -m "feat(ui): explorer patterns tab, export and saved views (NAMI-LOG-17)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 18 (NAMI-LOG-18): UI — log alerts

On `feat/logs-part2`. Commits carry `(NAMI-LOG-18)`.

**Files:**
- Create: `nami-ui/src/lib/logs/log-alerts.ts` (+ test), `nami-ui/src/lib/server/logs/log-alert-form.ts` (+ test), `nami-ui/src/lib/components/logs/LogAlertForm.svelte`
- Create: `nami-ui/src/routes/(app)/projects/[id=uuid]/logs/alerts/+page.server.ts`, `+page.svelte`, `alerts.test.ts`; `alerts/new/+page.server.ts`, `+page.svelte`; `alerts/[alertId=uuid]/+page.server.ts`, `+page.svelte`, `alert-detail.test.ts`; `alerts/preview/+server.ts` (+ `server.test.ts`)
- Modify: `.../logs/+layout.svelte` (Alerts sub-tab), `.../logs/+page.svelte` ("Create alert from this search" link), `.../incidents/+page.svelte` and `nami-ui/src/lib/uptime/uptime.ts` (`log_alert_id`, link), the incidents tests (`.../incidents/incidents.test.ts`, `.../incidents/incidents-page.test.ts`)

**Drift (2026-10-24):**
- The incident type is `Incident` in `nami-ui/src/lib/uptime/uptime.ts:151-164` (`monitor_id`, `heartbeat_id`, `source_name`); it gains `log_alert_id: string | null`. The page builds its link in `.../incidents/+page.svelte:32-33` (monitor, then heartbeat); a third branch links `log_alert_id` to `/projects/<id>/logs/alerts/<log_alert_id>`. The typed `incident()` fixture in `.../incidents/incidents-page.test.ts:9` gains `log_alert_id: null`.
- The UI conventions forbid raw hex: the `alerting` badge is `Badge tone="danger"` and `ok` and "disabled" are `tone="neutral"` (`#lib/components/ui/Badge.svelte`), not `#d03b3b`.
- `parseFilter` exists unexported in `nami-ui/src/lib/logs/logs.ts:262-273`; export it as planned. `relayJsonPost` is imported from `#lib/server/core/json-relay` (T16).
- The logs layout already uses `SegmentedTabs` with an `Explorer | Sources` list (`.../logs/+layout.svelte:8-18`); Alerts is a third entry (`exact: false`).

**Interfaces:**
- Consumes: engine `/log-alerts*` (T14), `relayJsonPost` (T16), `parseExplorerState`/`FieldFilter`/`isFieldPath` (`lib/logs/logs.ts`).
- Produces:

```ts
// lib/logs/log-alerts.ts
export type LogAlertQuery = { q?: string; in_fields?: boolean; levels?: LogLevel[]; source_ids?: string[]; filters?: FieldFilter[]; pattern_hashes?: string[] };
export type LogAlert = { id: string; name: string; query: LogAlertQuery; threshold: number; comparison: 'gt' | 'lt';
  window_seconds: number; interval_seconds: number; enabled: boolean; state: 'ok' | 'alerting';
  last_evaluated_at: string | null; last_count: number | null; last_error: string | null;
  last_error_at: string | null; next_evaluation_at: string };
export const WINDOW_PRESETS: { seconds: number; label: string }[];   // 1, 5, 15 min, 1, 6, 24 h
export const INTERVAL_PRESETS: { seconds: number; label: string }[]; // 1, 5, 15 min, 1 h
export function formatSeconds(seconds: number): string;              // "5 min", "1 h", "90 s"
export function describeCondition(alert: Pick<LogAlert, 'comparison' | 'threshold' | 'window_seconds'>): string;
export function alertQueryFromExplorer(params: URLSearchParams): LogAlertQuery; // prefill from an explorer link

// lib/server/logs/log-alert-form.ts
export type LogAlertPayload = { name: string; query: LogAlertQuery; threshold: number; comparison: 'gt' | 'lt';
  window_seconds: number; interval_seconds: number; enabled: boolean };
export function logAlertPayloadFromForm(form: FormData): { ok: true; payload: LogAlertPayload } | { ok: false; message: string };
```

- [ ] **Step 1: Write the failing lib and form tests**

`nami-ui/src/lib/logs/log-alerts.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { alertQueryFromExplorer, describeCondition, formatSeconds } from './log-alerts';

describe('log alerts', () => {
	it('describes conditions in words', () => {
		expect(describeCondition({ comparison: 'gt', threshold: 10, window_seconds: 300 })).toBe('more than 10 matching events in 5 min');
		expect(describeCondition({ comparison: 'lt', threshold: 1, window_seconds: 3600 })).toBe('fewer than 1 matching event in 1 h');
		expect(formatSeconds(90)).toBe('90 s');
		expect(formatSeconds(86400)).toBe('24 h');
	});
	it('prefills from an explorer link, dropping window, columns and live', () => {
		const query = alertQueryFromExplorer(new URLSearchParams('range=7d&level=error&q=disk&col=a&live=1&pattern=5'));
		expect(query).toEqual({ q: 'disk', levels: ['error'], pattern_hashes: ['5'] });
	});
});
```

`nami-ui/src/lib/server/logs/log-alert-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { logAlertPayloadFromForm } from './log-alert-form';

const SOURCE = 'c13bc10a-d17f-443b-8976-c6d9b54e2798';

function form(fields: [string, string][]) {
	const data = new FormData();
	for (const [name, value] of fields) data.append(name, value);
	return data;
}

const base: [string, string][] = [
	['name', 'Checkout errors'], ['threshold', '10'], ['comparison', 'gt'], ['window_seconds', '300'], ['interval_seconds', '60'], ['enabled', 'on']
];

describe('logAlertPayloadFromForm', () => {
	it('builds the engine body, query included', () => {
		const result = logAlertPayloadFromForm(form([...base, ['q', 'disk'], ['level', 'error'], ['level', 'fatal'], ['source', SOURCE],
			['filter', '{"path":"http.status","op":"gte","value":"500"}'], ['pattern', '42']]));
		expect(result).toEqual({ ok: true, payload: {
			name: 'Checkout errors', threshold: 10, comparison: 'gt', window_seconds: 300, interval_seconds: 60, enabled: true,
			query: { q: 'disk', levels: ['error', 'fatal'], source_ids: [SOURCE], filters: [{ path: 'http.status', op: 'gte', value: '500' }], pattern_hashes: ['42'] }
		} });
		const disabled = logAlertPayloadFromForm(form(base.filter(([k]) => k !== 'enabled')));
		expect(disabled.ok && disabled.payload.enabled).toBe(false);
	});
	it('rejects what the engine would reject', () => {
		const set = (name: string, value: string) => form(base.map(([k, v]) => [k, k === name ? value : v] as [string, string]));
		for (const [name, value] of [['name', ''], ['threshold', '-1'], ['threshold', '1.5'], ['comparison', 'eq'],
			['window_seconds', '59'], ['window_seconds', '86401'], ['interval_seconds', '30']]) {
			expect(logAlertPayloadFromForm(set(name, value)).ok, `${name}=${value}`).toBe(false);
		}
		expect(logAlertPayloadFromForm(set('comparison', 'lt')).ok).toBe(true);
		expect(logAlertPayloadFromForm(form([...base.filter(([k]) => k !== 'threshold'), ['threshold', '0'], ['comparison', 'lt']])).ok).toBe(false);
		expect(logAlertPayloadFromForm(form([...base, ['source', 'not-a-uuid']])).ok).toBe(false);
		expect(logAlertPayloadFromForm(form([...base, ['filter', '{nope']])).ok).toBe(false);
	});
});
```

(The duplicate `comparison` key in the last-but-two case: `form` appends; the parser reads the *last* value of single-valued keys, so this checks `lt` with 0. If the parser reads the first value instead, build the form without the base `comparison`.)

- [ ] **Step 2: Run them to verify they fail; implement; run them to verify they pass**

Run: `cd nami-ui && npx vitest run src/lib/logs/log-alerts.test.ts src/lib/server/logs/log-alert-form.test.ts`. FAIL → implement → PASS. Export `parseFilter` from `lib/logs/logs.ts` for the form parser (it already exists, unexported). Single-valued keys read the last value. The name is trimmed (1-200 characters). The threshold is a non-negative integer ≤ 10^12, and `lt` needs ≥ 1. The window is 60-86400, the interval 60-86400. Levels must be in `LEVELS`. Sources must pass `isUuid`. Filters go through `parseFilter` (at most 10). Patterns go through T17's `isPatternHash` (u64 range).

- [ ] **Step 3: Write the failing route tests**

`alerts/alerts.test.ts`:
- The list `load` GETs `/projects/<id>/log-alerts` and `/sources`, and returns both. 403 → `error(403)`, 401 → redirect to `/login`.
- The `new` page `load` with `?level=error&q=disk` returns `initial.query = { q: 'disk', levels: ['error'] }`.
- The `new` page `create` action POSTs the parsed payload and redirects (303) to `/projects/<id>/logs/alerts/<new id>`. An invalid form → `fail(400)` without a call. An engine 400 → its message.

`alerts/[alertId=uuid]/alert-detail.test.ts`:
- `load` GETs the alert (404 → `error(404)`).
- `update` PATCHes the payload.
- `delete` without `confirm=yes` returns `{ confirmDelete: true }` and makes no call. With it, it DELETEs and redirects to the list. An engine 403 → `fail(403, { message: 'Only project admins can delete alerts.' })`.

`alerts/preview/server.test.ts`: a JSON POST reaches `/projects/<id>/log-alerts/preview`. A bad project id → 404.

Incidents test: an incident with `log_alert_id` links to `/projects/<id>/logs/alerts/<log_alert_id>`.

- [ ] **Step 4: Run them to verify they fail; implement; run them to verify they pass**

Run: `cd nami-ui && npx vitest run "src/routes/(app)/projects/[id=uuid]"`.

Implementation notes:
- `LogAlertForm.svelte` (props: `initial`, `sources`, `previewUrl`, `submitLabel`). Fields:
  - name;
  - "Alert when" comparison select (`more than` / `fewer than`);
  - threshold number;
  - "matching events in" window select from `WINDOW_PRESETS`;
  - "check every" interval select;
  - enabled checkbox;
  - query section: search text, "also search fields", level checkboxes, source checkboxes, field filters (repeatable rows of path/op/value, serialized into `filter` inputs as JSON), and active pattern chips (`pattern` inputs).
- The live preview count: 500 ms after the last change, a debounced `fetch(previewUrl, POST {query, window_seconds})` with abort-previous. It shows "N matching events in the last <window>", and "would trigger now" when `describeCondition` holds. 429 → "Too many queries; the preview will retry."
- List page: name, state badge (`alerting` `Badge tone="danger"`, `ok` neutral), condition text, last count, last evaluated (UTC), a disabled badge, and `last_error` (as text, with its UTC time) when set, so an alert that cannot evaluate is visible. "New alert" button.
- Detail page: the form (update), a two-step delete, and a link to the alert's incidents (`/projects/<id>/incidents`).
- Explorer: a "Create alert from this search" link → `/projects/<id>/logs/alerts/new?` + `explorerParams(state)`.
- Layout: sections `Explorer | Sources | Alerts`.

- [ ] **Step 5: All checks; walkthrough; commit**

Run: `cd nami-ui && npm run check && npx vitest run`.
Walkthrough:
1. From the explorer filtered to `level=error`, create an alert `> 2 in 5 min` and check that the preview count matches the histogram.
2. Ingest 3 errors and wait one interval: the alert shows `alerting`, an incident appears in Incidents linking back, and Mailpit has the "triggered" email.
3. Raise the threshold: the alert resolves and the "resolved" email arrives.
4. As a member, delete is refused with the admin message.

```bash
git add nami-ui/src
git commit -m "feat(ui): log alerts — list, editor with live preview count, incident links (NAMI-LOG-18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 19 (NAMI-LOG-19): Part 2 security review, fix wave and phase PR

**Covers:** R3.21.  **Depends on:** NAMI-LOG-18.

On `feat/logs-part2`. Per CLAUDE.md, every phase ends with a dedicated security review and fix wave, then the one phase PR. Fix commits carry `(NAMI-LOG-19)`.

1. **Whole-Part-2 review (opus)** over all of Part 2: what merged to `main` under the old flow (PRs #56, #63, #67, #75, #76; the range starts at the parent of #56's merge commit) and the branch diff `origin/main...feat/logs-part2` (NAMI-LOG-14 to NAMI-LOG-18), with engine + UI suites green and a live walkthrough of every feature as admin, member and viewer.
2. **Security focus list for that review** (each item names the test that should already pin it; a gap is a finding):
   - Pipeline: ReDoS (`catastrophic_patterns_run_in_linear_time`), empty-match and growth (`a_regex_that_matches_the_empty_string_is_rejected`, `redaction_that_grows_a_message_is_capped_again`), redaction completeness across message/fields/pattern/nested/mixed-case on HTTP and syslog (`a_redacted_value_never_reaches_clickhouse`, `syslog_frames_go_through_the_pipeline`), fail-closed (`a_stored_pipeline_that_does_not_compile_fails_closed`), redaction of numbers (R-P2-5), the regex budget and cache bound (`a_pipeline_holds_at_most_twenty_regexes`, R-P2-26), compile and preview off the runtime, cache freshness across instances with monotonic `updated_at` (R-P2-6), preview stores nothing, regex text never logged or echoed in errors (`Debug for CompiledPipeline`, `compile_regex` messages).
   - Query safety: every new SQL text (`logs/query/patterns.rs`, `logs/query/export.rs`, `logs/alert/mod.rs`, the tail resume query) is built only from `&'static str` fragments. Grep for `format!` near `Sql`. RESERVED_SETTINGS gains `http_write_exception_in_output_format`, and the drift test covers it.
   - Read budgets: patterns hold a query slot and use `READ_SETTINGS`-based settings. Export holds its own slot (2 per user, 8 per instance), streams with row/byte/line caps and a deadline that holds when the client stops reading (`a_stalled_export_gives_its_slot_back_at_the_deadline`), never writes a ClickHouse exception line into the file, never buffers in the engine or the BFF, and the BFF forwards only allow-listed headers. Alert counts use `READ_SETTINGS`. The per-project source cap bounds the export's name map.
   - Confinement: patterns, export, preview and alert counts never read another project's rows. Views and alerts reject foreign `source_ids`. Every id inside a project path is 404 when foreign. Roles: viewer/member/admin per the Global Constraints, 403 never 404.
   - Export output: the CSV formula guard on every cell (header included), RFC 4180 quoting, fixed `Content-Disposition`, `nosniff`, `no-store`.
   - Alerts: atomic claim (`overlapping_passes_evaluate_each_alert_once`), outage safety (`a_failed_count_changes_nothing`), email header-injection safety for alert names (`log_alert_emails_say_triggered_and_resolved_and_link_to_the_alert`), the evaluation-load bound (100 alerts per project, 8 concurrent across every pass of an instance, claims never beyond free capacity, 25 s query limit), edited-alert evaluations dropped (`an_evaluation_of_an_edited_alert_is_dropped`), project-wide maintenance holding the state (`a_project_wide_maintenance_window_holds_the_alert_state`), `last_error` carrying only fixed text, incidents only through the R-P2-27 seam. No outbound call other than `notify_transition`.
   - UI: JSON relays refuse cross-origin and non-JSON. Tail `Last-Event-ID` is validated. No `{@html}` on log, pattern, view or alert text. Two-step deletes. `[id=uuid]`/`isUuid` on every new id.
3. **Fix wave** on `feat/logs-part2` for every Important finding and for the Minor findings the reviewer marks fix-now, one or more commits per finding. Then a scoped re-review of the fixes, repeated until clean.
4. **Record:** commit the decisions doc `docs/superpowers/decisions/<date>-log-management-part2.md` (R-P2-1 to R-P2-27 with their final values, the Drift (2026-10-24) adaptations, the follow-ups below and the Part 1 deferred items decided under R-P2-22; same format as the Part 1 log), and tick R3.13 to R3.21 in `docs/superpowers/roadmap.md` §3 (see "Roadmap coverage").
5. **Phase PR:** push `feat/logs-part2` and open one PR (purpose and scope, the task list with commit ranges for NAMI-LOG-14 to NAMI-LOG-19 and the merged PRs of NAMI-LOG-9 to NAMI-LOG-13, implementation summary, tests run locally with results since no CI is configured, breaking changes, risks and follow-ups). It is auto-merged when the review is clean (pre-authorized, CLAUDE.md). Then delete the branch and the worktree.
6. **Known follow-ups and accepted risks to carry, not fix here:** fold `search::QuerySlots` and `slots::UserSlots` into one type (R-P2-15). The 512-connection caps (`core::serve::MAX_PUBLIC_CONNECTIONS`, `MAX_API_CONNECTIONS`) are global per listener: one client holding many connections can fill the public listener for everyone until the header deadline drops them. Per-IP limits and graceful shutdown → Platform. Alert no-data/confirmation periods → Metrics' shared evaluator. Escalation routing → Incident Management. "View trace" → Traces.
