# Nami engineering standards

These rules describe how code in this repository is written. Most of them
record what the code already does. New code follows them, and reviews check
them. When a rule here conflicts with `CLAUDE.md`, `CLAUDE.md` wins. When it
conflicts with a generic standard (the `jcc:standards` defaults, a library's
README), this file wins.

Part 1 covers `nami-engine` (Rust). Part 2 holds the short UI rules that
`CLAUDE.md` does not already state.

## Part 1. nami-engine

### 1. Layout

- Code lives in its domain: `src/<domain>/`. Today the domains are `core`,
  `auth`, `projects`, `uptime`, `incidents` and `logs`. Each new sub-project
  adds one top-level domain (`statuspages`, `metrics`, `traces`, `errors`,
  `rum`, `ai`, `platform`).
- `core` is infrastructure only: config, db pool, ClickHouse client, crypto,
  email, errors, state, router, serve, middleware and shared validation. It
  never imports a domain module, apart from `core/router.rs` (which mounts
  every domain's routes) and `core/state.rs` (which holds domain state).
- A domain area is `<area>/mod.rs` (logic), `<area>/model.rs` (types and SQL)
  and `<area>/routes.rs` (HTTP). A small area can be one file. A file that
  grows a second area becomes a directory.
- Routes hold no SQL and no business rules. They extract, authorize,
  validate, call the model or logic function, and map the result to a
  response.
- Model functions hold the SQL. They take `&PgPool`, `impl PgExecutor<'_>`
  or `&mut PgConnection` (when the caller owns a transaction) and return
  domain types.
- Pure logic (planners, parsers, rule compilers, state machines) lives in
  functions with no I/O, so unit tests reach it without a database.
- Visibility is the smallest that works. Integration tests import through
  `nami_engine::<domain>::...`, so modules they use are `pub`. Everything
  else is `pub(crate)` or private.

### 2. Types

- Parse at the boundary, trust inside. A request body is a
  `#[derive(Deserialize)] #[serde(deny_unknown_fields)]` struct. A validator
  turns it into a validated value or `Err(String)`. Code after the
  validator takes the validated type, not the raw one.
- A closed set of values is an `enum`, never a `String`. Examples are roles
  (`core::middleware::rbac::Role`), incident states, monitor kinds and alert
  comparisons. Map them to Postgres `text` with a `check` constraint, and to
  serde with `#[serde(rename_all = "snake_case")]`.
- A state machine is an enum plus one transition function that returns the
  next state. Callers never set state fields by hand.
- A PATCH field that can be cleared is `Option<Option<T>>` with
  `#[serde(default, deserialize_with = "core::validate::nullable")]`.
  Absent keeps the value, `null` clears it, and a value sets it.
- Bounds are named constants next to the type they bound
  (`pub const MAX_POSITION: i32 = 1_000_000;`). A test names the constant,
  never the literal.
- IDs are `uuid::Uuid`, and new rows default to v7 where ordering matters.
  Times are `chrono::DateTime<Utc>`. Durations crossing an API are integer
  seconds with the unit in the field name (`window_seconds`).

### 3. Errors and HTTP status

- Every fallible handler returns `Result<_, core::error::AppError>`.
- Status rules:
  - 400 (`Validation`) for bad input, with a message a user can act on.
  - 401 (`Unauthorized`) for a missing or stale session.
  - 403 (`Forbidden`) for every authorization failure. Never answer 404 to
    hide a project the caller may not access.
  - 404 (`NotFound`) for an id that is not in an accessible project, or does
    not exist.
  - 409 (`Conflict`) for a unique-name clash or a quota-style conflict.
  - 429 (`RateLimited`) for any rate, slot or concurrency limit.
- `AppError::Database` and `AppError::ClickHouse` log the cause and answer a
  generic 500. Internal text (SQL, schema names, upstream bodies) never
  reaches a response.
- Auth responses stay generic. No status, shape or timing difference may
  reveal whether an email is registered.
- Recognise Postgres constraint errors through sqlx's typed helpers
  (`db_err.is_unique_violation()`, `db_err.is_foreign_key_violation()`,
  `db_err.is_check_violation()`), never SQLSTATE string literals.
- `unwrap()` and `expect()` are for tests and for invariants that cannot
  fail (a constant regex, a builder with constant input). An `expect`
  message says why it cannot fail.

### 4. Authorization

- The first line of every project-scoped handler is
  `require_project_role(&state.db, &user, project_id, Role::X).await?`, or
  `require_project_reader` when the response masks secrets for viewers.
- Roles per feature follow the existing pattern. Viewer reads, member creates
  and edits, admin deletes and manages members. A plan states any exception.
- An id from another project inside an accessible project path is a 404.
  An id of another project stored in a row (a view, an alert) is a 400.

### 5. Postgres

- SQLx runtime queries only (`query`, `query_as`, `query_scalar`). Never the
  compile-time macros, never an ORM.
- SQL is a string literal. A column list or expression shared by several
  queries is a `macro_rules!` that expands to a literal, used with
  `concat!` (see `logs::view::model::columns!` and
  `uptime::heartbeat::due_at!`). User values are always bound (`$1`), never
  formatted into SQL.
- Several writes that must agree run in one transaction. Quota checks
  (`uptime::quota::ProjectQuota`) run inside the same transaction as the
  insert.
- Worker claims are atomic. Use `update ... where id in (select ... for
  update skip locked limit $n) returning ...`, so two engine instances never
  run the same job.
- Migrations follow `CLAUDE.md`. Baselines are `0001`-`0005`, new files are
  `NNNN_<domain>_<change>.sql`, there is at most one file per domain per
  phase PR, and a merged file is never edited. Every constraint has a name.
  Every foreign key states `on delete`.

### 6. ClickHouse

- Access goes through `core::clickhouse::ClickHouse` over HTTP.
- Query text is assembled only from `&'static str` fragments
  (`logs::query::Sql::push`). Every value, JSON path segments included, is
  a bound parameter. The project id is bound from the path, never trusted
  from a body.
- Extra settings are code constants and are never names in
  `RESERVED_SETTINGS`.
- Reads cap rows, bytes and time (`max_result_bytes`,
  `max_execution_time`). The client caps the response body
  (`MAX_RESPONSE_BYTES`).
- DDL is an idempotent `CREATE TABLE IF NOT EXISTS` in `MIGRATIONS`, grouped
  by domain, with a per-source or per-table `TTL`.

### 7. Concurrency and performance

Correct and bounded first. Fast second, and only with a measurement.

- Everything that grows is bounded: request bodies, decoded sizes, batch
  rows, response bytes, in-flight requests per project or user (semaphores),
  queue lengths, retry counts and every outbound call's timeout. A new
  limit is a named constant and has a test at the limit and one past it.
- Never block the async runtime. CPU-heavy or blocking work (argon2,
  compiling user regexes, large decompression, sync file I/O) runs in
  `tokio::task::spawn_blocking` or behind a budget.
- Never hold a lock across `.await`. Use `std::sync::Mutex` for short
  critical sections with no await inside. Prefer per-key state in a
  `HashMap` behind one short lock over a lock held while working.
- Share read-mostly data with `Arc`, and borrow (`&str`, `&[T]`) instead of
  cloning in loops. Clone a `String` only when ownership must move.
- Hot loops (ingest, export, check workers) preallocate
  (`Vec::with_capacity`, `String::with_capacity`), reuse buffers, and stream
  instead of collecting. Large responses (export, tail) are streamed bodies,
  never buffered.
- Background loops are `tokio::spawn`ed once in `main.rs`, take their work
  through atomic claims, and survive a failed iteration (log it, sleep,
  continue).
- Performance changes need a number. State the metric, the baseline at
  `main` and the result. Do not micro-optimise unmeasured code. The release
  profile (`Cargo.toml`) uses thin LTO and one codegen unit.

### 8. Macros, generics and metaprogramming

- Derive macros (`serde`, `thiserror`, `sqlx::FromRow`) are the default for
  boilerplate.
- `macro_rules!` is allowed when it removes a real repetition (three or more
  identical shapes) or when the language needs a literal, such as SQL text
  for sqlx. Keep it small and local to the module that uses it, with a
  comment on why a function cannot do the job.
- Prefer a generic function or a trait when types alone carry the
  variation. A macro that generates items with names a reader cannot grep
  for is not allowed.
- No new proc-macro crate and no build script without a decision record in
  `docs/superpowers/decisions/`.
- Table-driven code beats repeated branches. A list of rules or kinds is a
  `const` slice or a `match` on an enum, in one place.

### 9. Security

- Secrets at rest are sealed with `core::crypto::Cipher` (AES-GCM) with a
  per-row AAD such as `sources.token:{id}`. Lookup tokens are stored as
  SHA-256 hashes. Comparisons of secrets are constant-time.
- Outbound requests to user-chosen hosts go through the guarded resolver
  and client (`uptime::checker::is_forbidden_destination` today). That means
  no link-local or metadata addresses, no private addresses unless the
  feature says so, capped bodies, timeouts, and no redirects unless the
  feature needs them.
- Text that reaches an email, a header or a log is checked for control and
  invisible characters (`core::validate`).
- Telemetry (log lines, error events, AI input) is untrusted data. It is
  never executed, never formatted into SQL, and escaped where it is shown.

### 10. Logging

- Use `tracing` with structured fields:
  `tracing::warn!(source_id = %id, error = %e, "ingest rejected")`. The
  message is a short constant, and the variables go in fields.
- `error` is for a failure an operator must look at (a 500, a worker
  iteration that failed). `warn` is for a handled abnormal case. `info` is
  for lifecycle events (start, listen, migrations).
- Never log secrets, tokens, passwords, session ids, full request bodies or
  ingest payloads.

### 11. Tests

- Integration tests use one target per domain, `tests/<domain>/main.rs`,
  with one `mod` per file. A test that changes process-wide state is a
  standalone `tests/<name>.rs`.
- A test name is a sentence about behaviour, such as
  `a_viewer_cannot_delete_a_group` or
  `a_failed_count_changes_nothing`. A test asserts what a client observes
  (status, JSON, rows, emails), not private internals.
- Pure logic has unit tests in the same file (`#[cfg(test)] mod tests`).
- ClickHouse tests create their own `nami_test_*` database and drop it.
  `AppState::for_test` points at a database that does not exist, so a test
  that forgets this fails loudly.
- Every limit has a test at the bound and one past it. Every role rule has a
  test for the lowest role that passes and the highest role that fails.
- Run commands:
  - One domain: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <domain>`.
  - All: the same command with `cargo test`.

### 12. Comments and docs

- A doc comment (`///`) on each `pub` item states its contract: what it
  returns, its limits and its errors.
- An inline comment explains a non-obvious *why* (a security reason, a
  library quirk, an ordering constraint). It never narrates what the next
  line does and never records history ("previously", "fix for").
- Module docs (`//!`) say what the module owns.

### 13. Dependencies and tooling

- Add a crate only when the standard library, an existing dependency or a
  few lines cannot do the job. The plan names the crate and the version and
  says why. Use `cargo add <crate>@<exact version>`.
- Formatting: `cargo fmt`. Lints: `cargo clippy --all-targets`. The crate's
  `[lints]` table in `Cargo.toml` sets the shared levels. Both tools need
  the `rustfmt` and `clippy` components (on Fedora:
  `sudo dnf install -y rustfmt clippy`).
- `unsafe` is denied crate-wide (`[lints.rust]`). The only exceptions are
  the standalone process-state test binaries, which allow it at the top of
  the file: the counting allocator in `tests/logs_decode_memory.rs` and
  `std::env::set_var` in `tests/clickhouse_proxy.rs` and
  `tests/uptime_checker_proxy.rs`.

### 14. Review checklist

- [ ] The code sits in the right domain, and routes hold no SQL.
- [ ] Input is parsed into typed values at the boundary
      (`deny_unknown_fields` on bodies).
- [ ] Closed sets are enums, including roles.
- [ ] Authorization is the first step. Failure is 403, and a foreign id is
      404.
- [ ] Every new limit is a named constant with tests at and past the bound.
- [ ] No lock is held across `.await`, and no blocking work runs on the
      runtime.
- [ ] SQL is a literal with bound values. ClickHouse SQL uses only static
      fragments.
- [ ] Errors do not leak internals, and auth errors stay generic.
- [ ] Secrets are sealed or hashed and never logged.
- [ ] Tests are named after behaviour and sit in the domain target.
- [ ] Any performance claim comes with a measured number.

## Part 2. nami-ui

`CLAUDE.md` ("UI conventions") holds the design-system rules. In addition:

- Every engine call goes through `callEngine` with `enginePath`. Modules
  under `#lib/server` are never imported by components.
- Route params use the `[id=uuid]` matchers and are checked with `isUuid`
  before they reach an engine path.
- Streams (export, tail) are relayed as bodies and never buffered in the BFF.
- Deletes are two-step (a confirm plus a `confirm=yes` field).
- Times are shown in UTC.
- Run `npm run check` (0 errors, 0 warnings) and `npx vitest run`.
