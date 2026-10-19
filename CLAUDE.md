# Nami

Self-hosted BetterStack reimplementation (log management, uptime monitoring,
incident management/on-call, status pages, metrics) — avoids an ongoing
BetterStack subscription.

## Stack

- `nami-engine/` — Rust backend (Axum, SQLx/Postgres, tower-sessions, argon2,
  totp-rs, oauth2, lettre)
- `nami-ui/` — SvelteKit 5 + Tailwind 4 frontend, acts as a BFF (proxies all
  auth calls to `nami-engine`, owns the session cookie on its own origin)
- Postgres — users, sessions, RBAC
- Mailpit — local SMTP catcher for verification/reset emails in dev
- ClickHouse — time-series store (uptime check results, then logs and
  metrics), accessed over its HTTP interface via
  `nami-engine/src/core/clickhouse.rs`

## Project structure

The project is decomposed into sequential sub-projects (BetterStack bundles
several independent products, too large for one spec/plan):

1. **Foundation: auth, org, RBAC, session** — done
2. **Uptime monitoring** (Phases 1-3) — done
3. Log management — Part 1 done, Part 2 in progress
4. Incident management + on-call alerting
5. Status page
6. Metrics + dashboards (+ collector bundle)
7. Traces (OpenTelemetry)
8. Error tracking (Sentry-compatible)
9. Real user monitoring
10. AI SRE (bring your own LLM)
11. Platform + production hardening (last)

Each sub-project gets its own spec → plan → implementation cycle under
`docs/superpowers/specs/` and `docs/superpowers/plans/`. The feature-parity
checklist that defines "done" for the whole rebuild (11 sub-projects, incl.
traces, error tracking, RUM, AI SRE and a final production-hardening pass) is
`docs/superpowers/roadmap.md` — tick items there as their phase PR merges.
Every phase ends with a dedicated security review + fix wave. The rulings,
limits and deferred items of each finished phase are recorded in
`docs/superpowers/decisions/`.

Completed sub-projects:
- **Foundation** (PRs #1-#16). Spec
  `docs/superpowers/specs/2026-10-12-foundation-auth-rbac-design.md`, plan
  `docs/superpowers/plans/2026-10-12-foundation-auth-rbac.md`.
- **Uptime monitoring** (Phases 1-3, last fix wave #77). Spec
  `docs/superpowers/specs/2026-10-13-uptime-monitoring-design.md` (plus the
  phase 2 and 3 specs); decisions
  `docs/superpowers/decisions/2026-10-13-uptime-phase{1,2,3}.md`.

Current sub-project: **Log management, Part 2**
- Spec: `docs/superpowers/specs/2026-10-13-log-management-design.md`
- Plan: `docs/superpowers/plans/2026-10-13-log-management-part2.md`
- NAMI-LOG-9..13 are merged (PRs #56, #63, #67, #75, #76, under the old
  one-PR-per-task flow). NAMI-LOG-14..18 remain (log alerts, live-tail
  resume and explorer fixes, and the UI tasks 16-18), then the Part 2
  security review + fix wave. They ship as one phase PR (branch
  `feat/logs-part2`).
- Part 1 decisions: `docs/superpowers/decisions/2026-10-13-log-management-part1.md`.

Plans written before 2026-10-19 use the old file layout and migration
numbers: translate them with
`docs/superpowers/decisions/2026-10-19-domain-restructure.md`.

### Code layout (by domain)

- `nami-engine/src/`: `core/` (config, db, clickhouse, crypto, email,
  error, state, router, serve, middleware, shared validation; no domain
  logic), `auth/`, `projects/`, `uptime/` (`monitor/`, `heartbeat/`,
  `group/`, `maintenance/`, `checker/`, worker, store, alerts,
  availability, export, import), `incidents/`, `logs/` (`source/`, `view/`,
  `ingest/`, `query/`, `syslog/`). Each domain owns its models and routes;
  `core/router.rs` mounts every domain's routes. New sub-projects add a
  top-level domain module (`statuspages/`, `metrics/`, ...).
- `nami-engine/tests/`: one test target per domain,
  `tests/<domain>/main.rs`, which mounts `tests/common/` via
  `#[path = "../common/mod.rs"] mod common;` and declares one `mod` per test
  file. Only a test that changes process-wide state (environment variables,
  the global allocator) is a standalone `tests/<name>.rs`. Run one domain
  with `cargo test --test <domain>`.
- `nami-engine/migrations/`: Postgres migrations, see "Migrations" below.
- `nami-ui/src/lib/`: `core/`, `uptime/`, `logs/` (shared code),
  `server/{core,uptime,logs}/` (server-only BFF code), `components/`
  (Svelte components; `components/ui/` is the design system).

### Migrations

- `0001_auth.sql` … `0005_logs.sql` are the domain baselines (squashed on
  2026-10-19). New files are numbered sequentially after the newest file
  (the first is `0006_`) and named `NNNN_<domain>_<change>.sql`.
- At most one migration file per domain per phase PR; a phase that changes
  several domains adds one file for each. Never edit a merged migration.
- The old reserved number ranges (0022-0039, 0040-0059, 0060-0099,
  0100-0119, 0120-0139, ...) are retired.
- ClickHouse DDL lives in `core/clickhouse.rs` (`MIGRATIONS`), grouped by
  domain; statements are idempotent (`CREATE TABLE IF NOT EXISTS`).

Task IDs per sub-project: `NAMI-UP-<n>` (uptime), `NAMI-LOG-<n>` (logs),
`NAMI-INC-<n>` (incident/on-call), `NAMI-SP-<n>` (status page),
`NAMI-MET-<n>` (metrics), `NAMI-TR-<n>` (traces), `NAMI-ERR-<n>` (error
tracking), `NAMI-RUM-<n>` (RUM), `NAMI-AI-<n>` (AI SRE), `NAMI-PLT-<n>`
(platform/hardening), where `<n>` is the task number in that
sub-project's plan. Auto-merge on a clean review is pre-authorized for all
remaining sub-projects, 2-11 (user directive 2026-10-13: "jangan tunggu
kongfirmasii ... looping terus").

## Git branching workflow

One PR per sub-project phase (owner decision 2026-10-19: PRs were too small
and too many). Rules:

1. Never implement feature work directly on `main`.
2. A phase starts from the latest `origin/main`, on branch
   `feat/<subproject>-<phase>` (e.g. `feat/logs-part2`,
   `feat/incidents-part1`), in the worktree `.worktrees/<subproject>-<phase>/`
   (gitignored). The phase is everything one plan file covers, including its
   final security review and fix wave.
3. Each plan task is one or more commits on the phase branch, with the task
   ID in the commit message (e.g. `feat(engine): log alert worker
   (NAMI-LOG-14)`). Conventional Commits.
4. No separate PRs for plans, docs or small fixes: a plan is committed on
   the branch of the phase it plans (or on the previous phase's branch),
   and a fix goes on the branch of the phase it belongs to.
5. Tasks run in order on the one branch. Parallel work (e.g. engine and UI
   tasks confirmed independent) uses extra worktrees on short-lived local
   branches that are merged back into the phase branch before review; agree
   on API contracts/schemas first.
6. Don't touch unrelated files, perform unrelated refactors, or overwrite
   another agent's work.
7. Task IDs follow the scheme above. No external tracker is linked — never
   invent a different ID scheme without the user's explicit approval.

**Per-phase flow:** worktree + `feat/<subproject>-<phase>` from latest
`origin/main` → for each plan task: implement (TDD per the plan's steps) →
commit(s) with the task ID → task review (spec compliance + quality) → fix
commits on the same branch until the review is clean → next task. After the
last task: whole-phase security review → fix wave on the same branch →
re-review → commit the decisions doc
(`docs/superpowers/decisions/<date>-<subproject>-<phase>.md`) and the
roadmap ticks → push → open the phase PR (purpose/scope, task list with
commit ranges, implementation summary, tests executed + results, breaking
changes, risks/follow-up) → **auto-merge once review is clean**
(pre-authorized — user directive 2026-10-13) → delete branch + worktree →
next phase branches from updated `origin/main`.

No CI is configured on this repo yet. "Required checks" currently means
the task's own test suite, run locally and verified from the diff during
task review — disclose this in every PR description.

## Conventions

- All infrastructure (Postgres, Mailpit, ClickHouse, and anything added
  later) runs as containers via `nami-engine/docker-compose.yaml` — never
  installed natively on the host. `docker compose up -d <service>` before
  running the engine or its tests.
- Docker Postgres listens on host port **5433** (a native Homebrew Postgres
  on this machine owns 5432 and would silently capture `localhost:5432`
  connections). Run engine tests with
  `DATABASE_URL=postgres://nami:nami@localhost:5433/nami`.
- Commit messages: Conventional Commits (`feat(engine): ...`, `fix(ui): ...`,
  `chore: ...`).
- Rust: no ORM — SQLx with runtime-checked queries (`query`/`query_as`), no
  compile-time macros. Server-side sessions only (tower-sessions + Postgres
  store), never JWT.
- RBAC: two levels only — `global_role` on `users` (`admin`/`member`/`viewer`)
  and `project_role` on `project_members` (`admin`/`member`/`viewer`).
  Global `admin` bypasses project membership checks. Authorization failure
  is always `403`, never `404`.
- Auth error responses are always generic — no response shape, status, or
  timing difference may reveal whether an email is registered.
- GitHub auth: two accounts are logged into `gh` on this machine
  (`SaiNanamii`, `koriebruh`). This repo requires `koriebruh` (owner) —
  check `gh auth status` and `gh auth switch -h github.com -u koriebruh`
  before pushing/opening PRs if a push gets a 403.

## UI conventions

- Visual reference: `docs/superpowers/design/ui-reference-atom.png` (light
  "Atom" dashboard: grey-blue canvas, white cards, indigo primary, cyan and
  periwinkle accents, thin large headings, icon sidebar).
- All UI must use the design tokens in `nami-ui/src/routes/layout.css`
  (`@theme`: `canvas`, `surface`, `ink`, `muted`, `primary`, `success`/
  `danger`/`warning`/`info` with AA `-ink` and `-soft` variants, `status-*`
  for up/down/pending/paused/maintenance) and the components in
  `nami-ui/src/lib/components/ui/` (AppShell, Card, PageHeader,
  SegmentedTabs, StatCard, Sparkline, Legend, Badge, Button, Input, Select,
  EmptyState, Icon). Form controls that need raw attributes use the
  primitive classes (`.input`, `.select`, `.btn .btn-primary|secondary|ghost|danger`,
  `.alert-*`, `.badge-*`, `.table`). Never use raw hex values or Tailwind's
  stock palette in pages.
- No CDN assets: Inter is self-hosted via `@fontsource-variable/inter`;
  icons are inline SVG (`Icon.svelte`).
- Component APIs to reuse (all exported from `$lib/components/ui`):
  - `SegmentedTabs`: link mode (`items: {href,label,active}[]`, aria-current)
    or button mode (`items: {value,label}[]` + `bind:value` / `onselect`,
    aria-pressed); arrow keys/Home/End in both.
  - `CardMenu` and other menus: put `<a>` / `<button>` (form buttons too)
    inside with `.menu-item` (`.menu-item-danger` for destructive ones).
    Native `<details>` menus get `use:dismissible` (outside click + Escape).
  - `StatStrip cols={2..6|'auto'}`; `StatCard` has `unit`, `loading`, `hint`,
    optional `delta`/`direction` (only for real trends) and `spark`.
  - `Sparkline` draws a flat baseline below two finite points.
  - `CardGrid` (`wide` + `narrow` snippets, `ratio` 2:1 / 3:2 / 1:1) for the
    reference's wide-plus-narrow card rows.
  - Multi-series charts take colours from `seriesColor(i)` (tokens
    `--color-series-1..6`); chart primitives (bar, donut, axes) belong to the
    Metrics plan.
- Accessibility floor: text >= 4.5:1, focus rings and functional icons
  >= 3:1 (focus uses full `--color-primary`, icons use `muted`/`faint`),
  modal surfaces (the mobile drawer) trap focus via `inert` and return it.
