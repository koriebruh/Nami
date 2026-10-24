# Plan conventions

How a plan under `docs/superpowers/plans/` is written and kept current.
`python3 scripts/check-plans.py` enforces the mechanical parts. Read
`CLAUDE.md`, `docs/STANDARDS.md`,
`docs/superpowers/decisions/2026-10-19-domain-restructure.md` and
`docs/superpowers/roadmap.md` before writing or editing a plan.

## Gate

`python3 scripts/check-plans.py -v` from the repository root must report no
lint hits for a live plan (any plan not marked `**Status:** done`), and every
unticked roadmap item must be claimed by some plan.

## Layout and naming

- Engine domains: `nami-engine/src/<domain>/` (`core`, `auth`, `projects`,
  `uptime`, `incidents`, `logs`, then `statuspages`, `metrics`, `traces`,
  `errors`, `rum`, `ai`, `platform` as each sub-project adds them). Each
  domain owns `model.rs`/`routes.rs` or areas `<area>/{mod,model,routes}.rs`.
  `core/router.rs` mounts every domain's routes. `core` has no domain logic.
- Module paths follow files: `crate::incidents::model::...`,
  `crate::logs::ingest::store::...`, `crate::core::clickhouse::...`.
- Tests: one target per domain, `nami-engine/tests/<domain>/main.rs`
  (declares `mod <file>;`), files `tests/<domain>/<file>.rs`, run with
  `cargo test --test <domain> <file>::`. A new domain gets a new target
  directory whose `main.rs` mounts `#[path = "../common/mod.rs"] mod common;`.
  Only a test that changes process-wide state is a standalone
  `tests/<name>.rs`.
- UI: `nami-ui/src/lib/<domain>/`, `nami-ui/src/lib/server/<domain>/`,
  components in `src/lib/components/` (design system in `components/ui/`).
  Imports use `#lib/...`, e.g. `#lib/server/core/engine`.
- Engine test command, with no machine-specific paths:
  `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <domain> <file>::`

## Migrations

- Never write a concrete number. Use `NNNN_<domain>_<change>.sql`, where
  NNNN is the next free number in `nami-engine/migrations/` when the phase
  starts.
- At most one file per domain per phase PR. When several tasks of a phase add
  tables to one domain, the first task creates the file and later tasks
  append to it (allowed because it has not merged). A task that changes
  another domain's tables (e.g. `monitors.escalation_policy_id`) adds to
  that domain's phase file.
- When files of one phase depend on each other through foreign keys, the
  plan states the order (e.g. the logs file before the incidents file).
- A migration test that names a file uses the placeholder rule. Historical
  facts name the baseline (`0005_logs.sql` now holds what LOG-10 added).
- ClickHouse DDL: entries in `core/clickhouse.rs` `MIGRATIONS`, grouped by
  domain, idempotent `CREATE TABLE IF NOT EXISTS`.

## Workflow

One phase is one plan file, one branch `feat/<subproject>-<phase>` in the
worktree `.worktrees/<subproject>-<phase>/`, from the latest `origin/main`.
Each task is one or more commits with its task ID. A task review follows
each task, with fix commits on the same branch. After the last task come the
phase security review, the fix wave, the decisions doc
`docs/superpowers/decisions/<date>-<subproject>-<phase>.md`, the roadmap
ticks and one phase PR, auto-merged when the review is clean. There is no CI
yet, so the PR lists the local test runs.

## Header block

Right after the title of every live plan:

```
> **Status:** <ready | in progress | outline>. <what is merged, what is next>.
> **Branch:** `feat/<subproject>-<phase>` in `.worktrees/<subproject>-<phase>/`.
> **Depends on:** <phases that must merge first>.
```

`outline` means the tasks, scope, data shapes, endpoints, files, tests and
acceptance criteria are fixed, and the step-level code is written at phase
start against the then-current `main`, because the interfaces it builds on
have not merged yet. A finished plan is marked `**Status:** done` and is
kept as a record.

## Roadmap coverage

Every live plan has a `## Roadmap coverage` section near the top, one row
per roadmap item it delivers, with the roadmap ID and the task IDs on the
same line:

```
| Roadmap | Item | Tasks |
|---|---|---|
| R4.01 | Incident lifecycle, manual incidents | NAMI-INC-1, NAMI-INC-2, NAMI-INC-12 |
```

An item split across parts appears in both, and each row says which half it
covers. "UI for all of the above" items list the UI tasks. The phase's
security review and fix wave is a task with its own ID.

## Task shape (outline plans)

Top matter mirrors the existing plans: Goal, Architecture, Tech Stack, Spec,
When to start, Global Constraints, Review Focus, Rulings (`R-*`). Then each
task:

```
### Task <n> (NAMI-<X>-<n>): <verb phrase>

**Covers:** R?.??  **Depends on:** NAMI-...
**Files:** create and modify lists with new-layout paths (engine, tests, UI).
**Data shape:** tables, columns, types, constraints, indexes; Rust types and
  enums (illegal states unrepresentable); JSON request and response shapes.
**API:** method, path, minimum role, status codes (403 authz, 404 foreign id,
  400 validation, 409 conflict, 429 limits).
**Behaviour:** numbered rules with the exact limits from the spec.
**Tests:** behavioural names, e.g. `a_viewer_cannot_delete_a_policy` in
  `tests/<domain>/<file>.rs`, unit tests in the module, UI vitest cases.
**Acceptance:** observable checks that end the task.
```

Every limit, role, status code and edge case in the spec lands in some task.
When the spec is silent on something a task needs, the plan records a ruling
with the default chosen and why. A plan adds no business feature beyond the
spec and the roadmap. Reused pieces of earlier phases are named by their real
paths, and a piece that is planned but not merged is marked "planned in
NAMI-<X>-<n>".
