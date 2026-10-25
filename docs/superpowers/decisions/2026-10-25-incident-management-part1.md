# Incident Management Part 1: decisions and carried-forward items

Part 1 is NAMI-INC-1..15 plus its phase close (security review, fix wave, browser walkthrough), shipped as one phase PR on `feat/incidents-part1`. It ran autonomously on 2026-10-25 with subagents: each task had an implementer, then an independent review and fix round. The controller made the rulings below on the owner's behalf (see `2026-10-24-autonomous-run-trail.md`).

Plan: `docs/superpowers/plans/2026-10-13-incident-management-part1.md` (rulings R1-R33 live there). Spec: `docs/superpowers/specs/2026-10-13-incident-management-design.md`.

## Rulings made during the phase

- Migrations: `0008_incidents_part1.sql` (every incidents table of the phase), `0009_uptime_escalation.sql` and `0010_logs_escalation.sql` (policy, severity and `last_paged_at` on monitors, heartbeats and log alerts), applied in that order for the foreign keys. Escalation step targets reference schedules and channels with `deferrable initially deferred` keys, because a project delete cascades through them three levels deep; `incidents_severity_id_fkey` stays `on delete no action` (both proven by a test that empties every table last in turn).
- R31 (flap control moves into escalation) as implemented: the first step of an automatic open, or a heartbeat reopen, waits until 5 minutes after the source's `last_paged_at`. `last_paged_at` is written when a step notification is sent, so the hold is three primary-key reads (about 0.7 ms at 10,000 past incidents, down from about 22 ms). Manual reopen is never held. Resume is held too: the step waits for `max(resume + 5 min, last_paged_at + 5 min)`, so a source that recovered while paused resolves without paging. This supersedes the INC-10 review's "a person resumed, so no hold".
- Paused and maintenance sources: when a step comes due, a paused source stops escalation for the generation (timeline event), and a source covered by a maintenance window defers the step to the window end or 60 s, whichever comes first. Manual incidents are never held. Log alerts are covered by project-wide windows only.
- Heartbeat reopen takes the heartbeat's current policy and severity, falling back to the project defaults.
- "All members" pages project members with role member or admin and global admins who are members; viewers are never paged because they cannot acknowledge or resolve.
- A foreign policy or severity on a monitor, heartbeat or log alert is 404 (the plan's rule). Log alerts therefore answer 400 for a foreign log source and 404 for a foreign policy; a comment on `check_routing` says why.
- `OutboundPolicy` lives in `core::config` (R32). Public-only sends also refuse `240.0.0.0/4`, `::/96`, `fec0::/10`, the whole `64:ff9b::/32`, `2002::/16` and `2001:0::/32`.
- A destination refused at connect time fails at once ("destination not allowed") and is never retried.
- Deliveries: the lease is renewed before each send and checked against the attempt number; send slots are 40 per process shared by every drain; a drain waits at most 5 s and sends still running finish in the background. The 500-per-incident cap counts step notifications only; resolved notices are bounded by the pages sent.
- A page in flight at resolve time still gets its resolved notice; a reopen cancels resolved notices still queued.
- Notification text: Slack sections are capped at 3000 chars with `verbatim: true` (no auto mentions or links); Discord 2000, Telegram 4096; invisible and bidi characters are stripped; project names in notifications are capped at 200 chars, and project names are validated (200 chars, no control or bidi characters) at create.
- Action links (R30): signed, 24 h, single use (SHA-256 of the token stored), revoked by a password change, role rechecked at use, pruned hourly. The confirmation page acts only on POST. Confirm spawns the worker kick so it never waits for slow receivers.
- `validate_text` turns CRLF into LF before checking, so browser textareas work; a lone CR is refused.
- On-call: rotation 1-8760 hours; times bounded to 1970-2100; `start_date` within ±5 years on create or when a PUT changes it; overrides capped at 100 per schedule over the last ~121 days (shift range + override length), so `/shifts` stays bounded; "on call now" scans only change points (507 ms to 18 ms for 50 heavy schedules).
- `renderMarkdown` lives at `nami-ui/src/lib/markdown.ts` (later plans reuse it). A link label cannot contain `[`, which removed a quadratic render (4.2 s to 1 ms on a 100 KiB postmortem).
- UI: the email verification page and the action page confirm with a button so link scanners cannot spend tokens. A route-module guard test (`nami-ui/src/routes/route-modules.test.ts`) fails on any export SvelteKit rejects and on a `default` action mixed with named actions; the walkthrough found three such 500s that unit tests had missed.

## Verification

- Engine at the phase close: `--lib` 523, incidents 134, uptime 447, logs 158, core 90, projects 8, auth 34, all passing with `--test-threads=8`; `cargo build --all-targets` without warnings.
- UI: `npm run check` 0 errors and 0 warnings; vitest 805 passing; `vite build` passes.
- Independent whole-phase security review: 0 critical, 0 high, 1 medium (resume paging), 8 low; all fixed or ruled below. A re-review checked the fix wave.
- Browser walkthrough on a throwaway database: incident lifecycle, timeline, safe markdown, severity, roles, escalation paging, the R31 hold, acknowledge stopping later steps, email action links, channels (public-only refusal, Slack test failure, write-only secrets), on-call rotation and overrides, and maintenance holds. The three UI failures it found were fixed and re-walked.
- Not run: `cargo fmt` and `cargo clippy` (not installed on the dev machine; Platform's CI task adds them).

## Carried forward

- The monitors' `is_forbidden_destination` misses `64:ff9b:1::/48`, `::/96` and `2002::/16` (the public-only resolver already refuses them). Platform's single outbound policy (NAMI-PLT-10).
- Telegram and Teams may turn bare URLs in a cause into links; the URL shows its own text and cannot be disguised.
- A hung SMTP relay can fill the 40 send slots until each send times out.
- A maintenance window deleted early delays the page by up to 60 s.
- A resolved notice already sending when an incident reopens still goes out.
- Reordering schedule participants and restrictions needs JavaScript; without it the stored order is kept.
- The dev `nami` database must be recreated: migrations 0008-0010 changed while the phase was open.
