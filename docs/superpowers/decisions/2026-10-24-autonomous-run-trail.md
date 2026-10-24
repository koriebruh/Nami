# Autonomous run trail (from 2026-10-24)

Owner directive (2026-10-24, before going offline): finish tidying every
plan, fill every gap, write engineering standards, then implement all plans,
in whatever order and parallelism works. Auto-merge on a clean review is
pre-authorized by CLAUDE.md. One row per decision, newest last.

| # | Decision | Why | Evidence |
|---|---|---|---|
| 1 | Roadmap items get stable IDs `R<section>.<nn>`; plans claim them in a `## Roadmap coverage` table. | Makes "nothing is missed" checkable by a script instead of by reading. | `scripts/check-plans.py`; baseline 105 unclaimed items, 724 lint hits in 4 live plans. |
| 2 | Logs Part 2 roadmap items stay unticked, with a status note naming what is on `main`. | CLAUDE.md ticks items only when the phase PR merges. | `docs/superpowers/roadmap.md` §3. |
| 3 | Old paths in live plans rewritten by a one-shot mapping script, the rest by per-plan agents. | 550 of 724 lint hits were mechanical renames from the restructure table. | Commit `d9b3697`; lint 724 to 172. |
| 4 | The 8 missing plans (INC P2, SP P2, MET P2, Traces, Errors, RUM, AI SRE, Platform) are written as `outline` plans: tasks, data shapes, endpoints, tests and acceptance fixed; step-level code written at phase start. | Step-level code against interfaces two or more phases away goes stale, as the 2026-10-13 plans already did. | `docs/superpowers/plan-conventions.md`. |
| 5 | Migrations in plans use `NNNN_<domain>_<change>.sql` placeholders, one file per domain per phase. | CLAUDE.md migration rules; the old reserved ranges are retired. | `docs/superpowers/decisions/2026-10-19-domain-restructure.md`. |
| 6 | `docs/STANDARDS.md` records existing engine patterns. Code changes from it are limited to two refactors that grep shows real repetition for: a `Role` enum for the RBAC string arguments (61 call sites) and sqlx's typed constraint-error helpers instead of SQLSTATE literals (8 sites). No speculative performance refactor. | Standards without measurements must not drive perf changes; typed roles turn a typo like `"Admin"` into a compile error. | `rg require_project_role`, `rg '"23503"'`. |
| 7 | `rustfmt` and `clippy` are not installed (Fedora system Rust, no sudo). Formatting and lint status are unknown until the owner runs `sudo dnf install -y rustfmt clippy`. | Cannot install system packages without a password. | `rpm -q clippy rustfmt`. |
