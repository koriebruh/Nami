# Log Management Part 2: decisions and carried-forward items

Part 2 is NAMI-LOG-9..19. Tasks 9-13 merged under the old one-PR-per-task flow (PRs #56, #63, #67, #75, #76). Tasks 14-19 shipped as one phase PR on `feat/logs-part2`, run autonomously on 2026-10-24/25 with subagents; the controller made the rulings below on the owner's behalf (see `2026-10-24-autonomous-run-trail.md`).

Plan: `docs/superpowers/plans/2026-10-13-log-management-part2.md`. Spec: `docs/superpowers/specs/2026-10-13-log-management-design.md`. Part 1 decisions: `2026-10-13-log-management-part1.md`.

## Rulings

- The phase branch also carries the plan-wide tidy (roadmap IDs, `scripts/check-plans.py`, `docs/plan-conventions.md`, eight new outline plans), `docs/STANDARDS.md`, and two standards refactors: a typed `Role` for RBAC checks and sqlx's typed constraint-error helpers. CLAUDE.md puts docs and small fixes on the current phase branch.
- Integration tests run with `--test-threads=8`. At one thread per core (20) the shared Postgres pool times out (`PoolTimedOut`).
- LOG-14: Task 14's migration split into `0006_logs_alerts.sql` and `0007_incidents_log_alerts.sql` (logs first, for the FK). `comparison` and `state` are enums (`Comparison`, `AlertState`, one transition function `AlertState::next`), parsed from request text so an unknown comparison stays a 400. A pass reserves evaluation places before claiming. Log-alert incidents do not reopen (copied from the monitor pair, not the heartbeat 5-minute reopen).
- LOG-15: the resume poll reads from `min(cursor dt, now - lookback)`, never more than `RESUME_WINDOW` (5 min) back. The BFF keeps its 502-JSON contract while the engine is down, so resume does not cover engine outages longer than the browser's retry delay. A quiet tail whose last row is older than 5 minutes at reconnect starts fresh.
- LOG-16: `level` matches with a whole-number value are sent as numbers (pino levels); every other `equals` is a string. The editor needs JavaScript; without it Save stays disabled, so a no-JS post cannot wipe the rules.
- LOG-17: a saved view opens through an explicit Open button (`?/openView`), not on select change. Viewers still see Save, Update and Delete until `role` arrives with NAMI-INC-7; a 403 shows a member-only message.
- LOG-18: incident rows link to their alert only when `log_alert_id` is a UUID. The preview label shows the window its count was taken over.
- Security wave (all fixed, each with a test):
  - M1: the pipeline preview takes a slot first: 2 per user, 8 per instance, 429 past either.
  - L1: `relayJsonPost` forwards the browser's abort signal.
  - L2: an alert PATCH reads, merges, validates and writes under `select ... for update`.
  - L3: a PATCH checks only the source ids it sends.
  - L4: at most 2 alert evaluations in flight per project per instance (window-ranked claim, still one `for update skip locked` statement).
  - L5: every constraint in `0006` is named.
  - L6: pinning tests for outsider 403s, foreign ids, the monitor-scoped maintenance window, preview slots and tail-resume confinement.
- Walkthrough fixes (found in the browser pass, fixed in this phase):
  - Verification and reset emails carried bare engine paths that 404 in the UI. They now carry `{UI_BASE_URL}/verify-email?token=` and `{UI_BASE_URL}/reset-password?token=`. The new `/verify-email` page confirms with a button, so email link scanners cannot spend the token.
  - Viewers saw a raw "forbidden" on source create and update; they now see a member-only message.

## Limits

| Limit | Value |
|---|---|
| Log alerts per project | 100 (409 past it) |
| Alert window | 60-86400 s |
| Alert interval | 60-86400 s |
| Alert threshold | 0-10^12 (`lt` needs >= 1) |
| Stored alert or view query | 16 KiB |
| Alert evaluations per instance / per project | 8 / 2 |
| Alert count query | 25 s server-side (`QUERY_MAX_EXECUTION_SECS`) |
| Pipeline previews per user / per instance | 2 / 8 |
| Tail resume window | 5 min, `Last-Event-ID` at most 128 bytes |
| Export | 100,000 rows |

## Verification

- Engine: 1161 tests at the phase start; at the end `--lib` 461, logs 158, incidents 10, uptime 448, core 73, projects 7, auth 34, all passing with `--test-threads=8`. `cargo build --all-targets` has no warnings.
- UI: `npm run check` 0 errors and 0 warnings; vitest 588 passing.
- Browser walkthrough on a throwaway database: every Part 2 feature passed (pipeline edit and preview, redaction and drop at ingest, usage chart, live tail, patterns, CSV/NDJSON export with formula escaping, saved views, log alert alerting and resolving with incident and emails, role messages).
- Not run: `cargo fmt` and `cargo clippy` (not installed on the dev machine; Platform's CI task adds them).

## Carried forward

- Alert email flap control (`ALERT_FLAP_WINDOW`, 5 min) applies to log alerts; Incidents Part 1 replaces it with escalation timing (ruling R31 there).
- Per-instance limits (alert evaluations, preview slots, tail and export slots) are not shared across engine instances. Platform.
- Viewers see write buttons on views until NAMI-INC-7 adds `role`.
- Engine-outage live-tail resume (above).
- A redact key containing a comma or edge spaces does not round-trip through the editor.
- Cyan byte bars on the usage chart are about 2.1:1 against white; a palette change is a design-system decision.
