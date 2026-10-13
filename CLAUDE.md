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
- ClickHouse — reserved for the log-management/uptime sub-projects, unused
  by the current (foundation) sub-project

## Project structure

The project is decomposed into sequential sub-projects (BetterStack bundles
several independent products, too large for one spec/plan):

1. **Foundation: auth, org, RBAC, session** — in progress, see below
2. Uptime monitoring
3. Log management
4. Incident management + on-call alerting
5. Status page
6. Metrics/APM

Each sub-project gets its own spec → plan → implementation cycle under
`docs/superpowers/specs/` and `docs/superpowers/plans/`.

Current sub-project: **Foundation**
- Spec: `docs/superpowers/specs/2026-10-12-foundation-auth-rbac-design.md`
- Plan: `docs/superpowers/plans/2026-10-12-foundation-auth-rbac.md` (14 tasks)
- SDD ledger/progress: `.superpowers/sdd/2026-10-12-foundation-auth-rbac/progress.md`

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
risks/follow-up) → **auto-merge once review is clean** (user has
pre-authorized this for the current plan's execution — re-confirm if
starting a new plan) → delete branch + worktree → next task branches from
updated `origin/main`.

No CI is configured on this repo yet. "Required checks" currently means
the task's own test suite, run locally and verified from the diff during
task review — disclose this in every PR description.

## Conventions

- All infrastructure (Postgres, Mailpit, ClickHouse, and anything added
  later) runs as containers via `nami-engine/docker-compose.yaml` — never
  installed natively on the host. `docker compose up -d <service>` before
  running the engine or its tests.
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
