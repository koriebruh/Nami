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
  metrics), accessed over its HTTP interface via `nami-engine/src/clickhouse.rs`

## Project structure

The project is decomposed into sequential sub-projects (BetterStack bundles
several independent products, too large for one spec/plan):

1. **Foundation: auth, org, RBAC, session** — done (see below)
2. Uptime monitoring (Phases 1-3) — in progress
3. Log management
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
`docs/superpowers/roadmap.md` — tick items there as their PRs merge. Every
sub-project ends with a dedicated security review + fix wave.

Completed sub-project: **Foundation** (14 tasks, PRs #1-#15, all merged to
`main`). Per-task review findings and fix rounds are in each PR's
description on GitHub; the working ledger used during execution was
scratch and has been deleted.
- Spec: `docs/superpowers/specs/2026-10-12-foundation-auth-rbac-design.md`
- Plan: `docs/superpowers/plans/2026-10-12-foundation-auth-rbac.md`

Current sub-project: **Uptime monitoring**
- Spec: `docs/superpowers/specs/2026-10-13-uptime-monitoring-design.md`
- Phase 2 done: plan `docs/superpowers/plans/2026-10-13-uptime-phase2.md`

Task IDs per sub-project: `NAMI-UP-<n>` (uptime), `NAMI-LOG-<n>` (logs),
`NAMI-INC-<n>` (incident/on-call), `NAMI-SP-<n>` (status page),
`NAMI-MET-<n>` (metrics), `NAMI-TR-<n>` (traces), `NAMI-ERR-<n>` (error
tracking), `NAMI-RUM-<n>` (RUM), `NAMI-AI-<n>` (AI SRE), `NAMI-PLT-<n>`
(platform/hardening), where `<n>` is the task number in that
sub-project's plan. Auto-merge on a clean review is pre-authorized for all
remaining sub-projects, 2-11 (user directive 2026-10-13: "jangan tunggu
kongfirmasii ... looping terus").

## Git branching workflow

This project uses one-task-per-branch, one-PR-per-task. Rules:

1. Never implement feature work directly on `main`.
2. Start every task from the latest `origin/main`, unless the task
   explicitly depends on another unmerged branch.
3. One task per branch, one reviewable pull request per task.
4. Branch naming: `feat/<TASK-ID>-<description>`, `fix/<TASK-ID>-<description>`,
   `refactor/...`, `test/...`, `docs/...`, `chore/...`.
5. Task IDs: `NAMI-<N>` where `<N>` is the task number from the active
   plan file (e.g. `NAMI-1` = Task 1 of the foundation plan). No external
   tracker is linked — never invent a different ID scheme without the
   user's explicit approval.
6. Use git worktrees for isolation — one worktree per task, under
   `.worktrees/NAMI-<N>/` (gitignored). Tasks in the current plan are
   mostly sequential (each builds on the previous task's interfaces), so
   worktrees are used for isolation/cleanliness, not necessarily run in
   parallel — only parallelize tasks confirmed independent.
7. Don't touch unrelated files, perform unrelated refactors, or overwrite
   another agent's work.
8. Agree on API contracts/schemas before parallel frontend/backend work.

**Per-task flow:** worktree + branch from latest `origin/main` → implement
(TDD per the plan's steps) → commit (Conventional Commits) → task review
(spec compliance + quality) → fix loop if needed → push → open PR (purpose/
scope, implementation summary, tests executed + results, breaking changes,
risks/follow-up) → **auto-merge once review is clean** (pre-authorized for
every remaining sub-project's plans — user directive 2026-10-13) → delete
branch + worktree → next task branches from
updated `origin/main`.

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
