# Incident Management + On-call — Part 1 Implementation Plan

> **Status:** done. Shipped as one phase PR; decisions: `docs/superpowers/decisions/2026-10-25-incident-management-part1.md`.
> **Branch:** `feat/incidents-part1` in `.worktrees/incidents-part1/`.
> **Depends on:** Logs Part 2 (`feat/logs-part2`, NAMI-LOG-14..18 and its fix wave), whose Task 14 creates `NNNN_logs_alerts.sql` and `NNNN_incidents_log_alerts.sql` (the old 0044 file). Uptime Phase 3 is merged (fix wave #77); this plan builds on its code on `main`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Nami the core of BetterStack incident management and on-call. That means the incident lifecycle (open → acknowledged → resolved, reopen, manual incidents, comments, timeline, postmortem), severities, escalation policies (steps, delays, repeat, time-window conditions, fallback policy), on-call schedules (rotations, handoff, working-hours restrictions, overrides, "who is on call"), and notification channels (email, generic webhook, Slack, Microsoft Teams, Telegram, Discord), both personal and project-wide, with a durable delivery queue. It also adds signed acknowledge/resolve links in emails, and routes every monitor, heartbeat and log-alert incident through one internal open/resolve API, escalation and channels. nami-ui gets the screens for all of it.

**Architecture:** Every incident is opened, acknowledged, resolved and reopened only through `incidents::lifecycle`. That module writes the state change and its timeline event in the caller's transaction. It also arms escalation (`next_escalation_at`) and queues "resolved" notifications. Monitors, heartbeats, log alerts (Logs Part 2), manual incidents and Part 2's integrations and report form all call it with an `IncidentSource`. One engine loop (`incidents::worker`) runs two ticks. The escalation tick claims due incidents with `FOR UPDATE SKIP LOCKED`, resolves the current step's targets to users and channels, and inserts deduplicated rows into `notification_deliveries`. The delivery tick claims due deliveries and sends them through `incidents::send`. Chat and webhook requests go through one guarded outbound client (`outbound.rs`: no proxy, link-local and metadata refusal at DNS time, no redirects, 10 s timeout). Formatting is pure (`incidents::format`) and escapes per channel. On-call math is pure (`oncall::rotation`) and is tested across DST transitions. Channel secrets are sealed with the existing `core::crypto::Cipher` under per-row contexts and are never returned. Action links carry an HMAC token over `(incident, action, user, exp)`, are single-use, and are confirmed by a POST from a nami-ui page.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, chrono-tz 0.10.4), new direct crate `hmac 0.13.0` (already in the lock file through `hkdf`), Postgres 17, Mailpit, wiremock 0.6.5 (dev, already present), SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-incident-management-design.md`, including its "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §4 (the definition of done).

## Roadmap coverage

Rows marked *partial* are finished by Part 2 (NAMI-INC-16 onward, a separate plan), which also claims the §4 items this table leaves out.

| Roadmap | Item | Tasks |
|---|---|---|
| R4.01 | Incident lifecycle (open, acknowledged, resolved, reopen), manual incidents | NAMI-INC-1, NAMI-INC-2, NAMI-INC-12 |
| R4.02 | Incident timeline (events, comments), postmortem notes | NAMI-INC-1, NAMI-INC-2, NAMI-INC-12 |
| R4.03 | Escalation policies (multi-step, delays, repeat) per project | NAMI-INC-8, NAMI-INC-9, NAMI-INC-14 |
| R4.04 | Escalation step time-window conditions and fallback policy | NAMI-INC-6, NAMI-INC-8, NAMI-INC-9, NAMI-INC-14 |
| R4.05 | On-call schedules: rotations, handoff, overrides, "who's on call" | NAMI-INC-6, NAMI-INC-7, NAMI-INC-9, NAMI-INC-15 |
| R4.06 | *Partial:* the working-hours restriction only (shift reminders, the ICS feed and "take over shift" are Part 2) | NAMI-INC-6, NAMI-INC-7, NAMI-INC-15 |
| R4.07 | Notification channels: email, generic webhook, Slack, Telegram, Discord; per-user and per-policy targets | NAMI-INC-3, NAMI-INC-4, NAMI-INC-5, NAMI-INC-8, NAMI-INC-9, NAMI-INC-13 |
| R4.08 | Microsoft Teams channel (Adaptive Cards) | NAMI-INC-3, NAMI-INC-4, NAMI-INC-5, NAMI-INC-13 |
| R4.09 | Acknowledge/resolve from email links (signed, expiring, single-use tokens) | NAMI-INC-11 |
| R4.11 | Monitors, heartbeats and log alerts route through escalation policies | NAMI-INC-1, NAMI-INC-10, NAMI-INC-14 |
| R4.14 | *Partial:* seeded, read-only severities (project default, per source, per incident) and the `incidents.metadata` column; severity management and metadata editing are Part 2 | NAMI-INC-1, NAMI-INC-2, NAMI-INC-10, NAMI-INC-12, NAMI-INC-14 |
| R4.15 | *Partial:* list filters for status and source; the severity, date range and metadata filters are Part 2 | NAMI-INC-2, NAMI-INC-12 |
| R4.18 | *Partial:* the UI for Part 1's items | NAMI-INC-11, NAMI-INC-12, NAMI-INC-13, NAMI-INC-14, NAMI-INC-15 |
| (no roadmap ID) | Part 1 security review and fix wave | "Phase close", after NAMI-INC-15 in the same phase |

**When to start:** once Logs Part 2 (NAMI-LOG-9..18 and its fix wave, plan `docs/superpowers/plans/2026-10-13-log-management-part2.md`, branch `feat/logs-part2`) has merged to `main`. This plan assumes that end state. Until it merges, each item below is planned in NAMI-LOG-14: check it on `main` before the task that uses it.
   - Logs Part 2's migrations `NNNN_logs_alerts.sql` (`log_alerts`) and `NNNN_incidents_log_alerts.sql` (`incidents.log_alert_id`, FK with cascade, and the index `incidents_one_open_per_log_alert ... where status = 'open'`). They replace the old 0044 file of that plan.
   - `incidents::model::{open_for_log_alert, resolve_for_log_alert}`, `uptime::alerts::Subject::LogAlert`, `logs::alert::{claim_due_alerts, apply_evaluation}` (`apply_evaluation` returns `Option<Transition>`), and log-alert emails sent through `alerts::notify_transition`.
   - The R-P2-27 seam: `logs::alert::{open_alert_incident, resolve_alert_incident}` are the only callers of the two `*_for_log_alert` functions. `resolve_alert_incident(.., notify: false)` is what `logs::alert::model::update` calls when an alerting alert is disabled. Task 10 swaps the seam (R17).

Uptime Phase 3 is merged (NAMI-UP-20..32, fix wave #77). This plan consumes, from `main`:
   - `uptime::heartbeat::{Ping, PingKind, record_signal, failure_cause}` (`src/uptime/heartbeat/mod.rs`). A failure ping opens an incident titled `{name} reported a failure`.
   - `incidents.details text` (≤ 10 KiB + marker; the baseline `0004_incidents.sql` holds it now), `incidents::model::attach_details`, `IncidentListItem.details`, `alerts::Transition.details`, `uptime::heartbeat::sanitize_output`, and `tests/uptime/heartbeat_pings.rs::the_output_of_the_ping_that_opens_the_incident_is_attached_to_it`, which asserts that the DOWN email contains `Output:\n> …\n` lines.
   - The incidents page renders `details` in a `<details><summary>Job output</summary><pre>…</pre></details>` block, text only (`nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/+page.svelte:80-82`).
   - `uptime::availability::{for_monitor, for_heartbeat}` (`src/uptime/availability.rs:187`, `:210`; Part 2's SLA report only).
   - Two behaviours the first draft of this plan did not know: the heartbeat incident reopen window and the alert flap control. See "Drift (2026-10-24)" below.

Tasks that touch these shapes say so under **Interfaces → Consumes**.

Review fixes that land in the meantime may shift a line, a name or a test count. Implementers adapt such drift minimally (same behaviour, same tests) and note it in the phase PR. They never redesign around it.

**Part 2 follows** (a separate plan, NAMI-INC-16 onward). It covers:
- severity management (CRUD + settings UI);
- incident metadata editing and the full list filters (severity, date range, `metadata.<key>`, `q`);
- incoming integrations (generic webhook, Alertmanager, Grafana) on the public listener, with dedup and auto-resolve;
- the public incident report form (`/report/[token]`, honeypot, per-IP limits);
- Slack interactivity (signed Acknowledge/Resolve buttons);
- GitHub/Linear/Jira tickets;
- reporting (MTTA/MTTR, breakdowns, CSV) and the SLA report (needs Uptime Phase 3's availability function);
- shift reminder emails, the per-user ICS feed and "take over shift".

Heartbeat output (`incidents.details`, Uptime Phase 3) is **not** deferred. Part 1 keeps it everywhere it already shows: the list item, the email `Output:` block and the incidents page. It is also added to the detail page (R26).

Part 1 leaves room for these:
- `IncidentSource` gets `Integration { integration_id, dedup_key }` and `ReportForm` variants, each with its own conflict target.
- `incidents.metadata` (jsonb object) and the `metadata_updated`, `integration_event` and `ticket_created` event kinds already exist.
- `incident_severities` is seeded and read-only in Part 1.
- `incidents::oncall::rotation::shifts` is the function the ICS feed and the reminders call.
- `incidents::outbound::client()` is the shared client that the ticket integrations use.


**Drift (2026-10-24).** Checked against `main` after the domain restructure. Each item is repeated in the task it affects.
- **Heartbeat incidents reopen** (Task 1). `incidents::model::open_for_heartbeat` (`src/incidents/model.rs:94-156`) reopens the heartbeat's latest incident when it was resolved less than `HEARTBEAT_REOPEN_WINDOW_SECS` (5 min) before, instead of opening a new one: status back to `open`, `resolved_at` cleared, title and cause replaced, `details` cleared, id and `started_at` kept. `record_signal` and `sweep_overdue` rely on it (`src/uptime/heartbeat/mod.rs:263`, `:413`), and `tests/uptime/heartbeats.rs:3263`, `:3300` and `:3381` pin it. So that wrapper cannot become a one-line call: see Task 1.
- **Alert flap control** (Task 10). `alerts::notify_transition` (`src/uptime/alerts.rs:53-66`) first asks `AppState.alert_throttle` (`src/uptime/alert_throttle.rs`, `ALERT_FLAP_WINDOW` = 5 min, `src/core/state.rs:25-26`) and holds back a DOWN email inside the window. `send_deferred` / `run_deferred_alerts` (`src/uptime/alerts.rs:75`, `:102`), spawned on every instance in `src/main.rs:84-91`, send it later if the subject is still down. `tests/uptime/uptime_worker.rs:1663` and `tests/uptime/heartbeats.rs:3263` pin one DOWN + recovery pair per member per window. Task 10's routing replaces that email path, so the property needs a home in the escalation path: an **open question** for the controller, with a recommended default in Task 10.
- **`GuardedResolver` has a field** (Task 3): `struct GuardedResolver { ip_version: IpVersion }` (`src/uptime/checker/mod.rs:235-237`, `IpVersion` at `src/uptime/monitor/model.rs:60`), and it filters by family (`keep_family`, `src/uptime/checker/mod.rs:100`). It and `caused_by` (`:1362`) are private.
- **Test targets.** `tests/core/main.rs` and `tests/incidents/main.rs` mount `tests/common/` already. A test file starts with `use crate::common;` (never `mod common;`), and each new file is declared with `mod <file>;` in its domain's `main.rs`.
- **R18 test list.** On `main` the alert subjects are asserted in `tests/uptime/uptime_worker.rs`, `heartbeats.rs`, `recovery_threshold.rs` and `uptime_worker_timing.rs` (and `tests/logs/log_alerts.rs`, planned in NAMI-LOG-14). `tests/core/mailer.rs` only uses a subject literal of its own and does not change.
- **UI.** The account page is `nami-ui/src/routes/(app)/security/`; there is no `(app)/account/` folder yet (Task 13 creates `(app)/account/notifications/`).

**Migrations:** one incidents file for the phase plus one file per other domain it changes (R2).

**Verification status (2026-10-19, tidied 2026-10-24):** written against `main` at `95940ee` plus the Logs Part 2 plan's stated end state. A review on 2026-10-19 checked every signature against `origin/main` at `5da08a9` (NAMI-UP-20 merged) and against the planned shapes of Uptime Phase 3 NAMI-UP-21 and Logs Part 2 NAMI-LOG-14. It fixed the drift it found, for example the members list endpoint, `state_with_clickhouse(&pool)`, the monitor page's `?monitor_id=` list call, the tests that pin the old SMTP path, and `AppError` having no 410 variant. A tidy pass on 2026-10-24 moved the plan to the domain layout and the one-branch phase flow, merged its migrations into per-domain phase files, and rechecked the consumed interfaces against `main` ("Drift (2026-10-24)"). Unlike the Logs Part 1 and Uptime Phase 2 plans, the code here has **not** been applied to a scratch copy. Tests are given in full. Code that decides something is given in full: migrations, SQL, the escalation planner, rotation math, escaping, token signing and URL rules. Elsewhere the plan gives exact signatures, names and values, and the implementer writes the body (the Logs Part 2 convention). Run every step. A test that fails for a reason the plan did not predict is a plan bug: fix it minimally and say so in the phase PR.

**Execution:** per CLAUDE.md and `docs/superpowers/plan-conventions.md`. The whole phase is one branch, `feat/incidents-part1`, in the worktree `.worktrees/incidents-part1/`, from the latest `origin/main` once Logs Part 2 has merged. Each task is one or more Conventional Commits with its task ID, then a task review, then fix commits on the same branch until the review is clean. Tasks run in order (each consumes the interfaces of the ones before it). Two may run in parallel on a short-lived local branch in an extra worktree, merged back into `feat/incidents-part1` before its review: T4 depends only on T3 (the `hmac` dependency and nothing else), and T6 depends on nothing in this plan. After Task 15 comes "Phase close" (security review, fix wave, decisions doc, roadmap ticks, one phase PR). Engine tests need `docker compose up -d postgres clickhouse mailpit` (never restart running containers).

The **engine command** is used throughout:

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test <args>
```

**Test filters.** `cargo test` takes at most one positional filter, and that filter applies to *every* selected target. So:
- One test file: `--test <domain> <file>::`, for example `--test incidents incident_lifecycle::`.
- Several files or domains in one command: whole targets with no filter, for example `--test incidents --test core`.
- Unit tests by module go in a command of their own: `--lib -- <filter> <filter>`. Libtest takes several filters after `--`.
- Never combine `--test x` with a `--lib` filter in one command. The filter would also apply to target `x` and silently skip its tests.

Every "Run:" line below follows this rule. "engine command A, then B" means two invocations.

The **UI command** is `cd nami-ui && npm run check && npx vitest run <files>`.

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**, Mailpit 1025/8025, ClickHouse 8123). Never install anything natively. Never restart Docker.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`). No compile-time macros, no ORM. SQL strings are literals (`&'static str`), never `format!`.
- axum 0.8 path syntax `{param}`. Authorization failure is 403, never 404. An id of another project inside an accessible project path is 404. A personal resource of another user is 404.
- RBAC (spec "RBAC"):
  - Viewers read incidents, policies, schedules, severities and the project channel list (names, kinds, summaries).
  - Members create, acknowledge, resolve, reopen and comment on incidents, edit postmortems and change severity.
  - Admins manage policies, schedules, overrides and project channels.
  - Every user manages their own personal channels.
- Every state-changing API request carries a JSON body (`require_json_body`, 415 otherwise), including actions that take no input (`{}`).
- Incident status: `open` → `acknowledged` → `resolved`. Resolve is allowed from either unresolved state. Reopen is allowed only from `resolved`, returns to `open` and restarts escalation. Each source (monitor, heartbeat, log alert) has at most one **unresolved** incident.
- Text caps (spec):
  - title 1-255 chars, one line;
  - description/cause ≤ 10 KiB;
  - comment 1 B to 10 KiB;
  - postmortem ≤ 100 KiB (102,400 bytes);
  - timeline `message` ≤ 10 KiB.
- Severities (spec): `name` `^[a-z0-9_-]{1,32}$`, `rank` unique per project (1 = most severe), exactly one default. Every project gets `critical` (1), `high` (2, default), `low` (3). Severity appears in every notification.
- Escalation (spec):
  - `delay_minutes` is 0-1440, relative to the previous step (step 1 relative to open);
  - `repeat_count` is 0-10;
  - targets are `user`, `schedule`, `all_members`, `project_channel`;
  - acknowledge stops escalation, and resolve notifies everyone already notified;
  - an optional per-step condition `{weekdays [1-7], from "HH:MM", to "HH:MM", timezone IANA}` (a `from` after `to` spans midnight);
  - an optional same-project `fallback_policy_id` that is not the policy itself, followed one hop only.
- Default policy: every project, new and existing, gets a policy with one step, delay 0, target `all_members`.
- Schedules (spec): IANA timezone, ordered participants, rotation `daily` | `weekly` | custom hours, local `HH:MM` handoff, start date, `restrictions` (list of `{weekdays, from, to}`, empty = always), overrides (user + `starts_at`/`ends_at`, newest wins on overlap, record `created_by`). "On call now" and the next 14 days of shifts.
- Channels (spec):
  - kinds are `email`, `webhook`, `slack`, `teams`, `telegram`, `discord`;
  - config is sealed with AES-GCM and write-only, and responses never carry secrets;
  - every channel has a "send test" action;
  - webhook bodies are `{event, incident:{id,title,cause,status,source,severity,started_at,url}, project:{id,name}}` with `X-Nami-Timestamp: <unix seconds>` and `X-Nami-Signature: sha256=<lowercase hex HMAC-SHA256(secret, "{timestamp}.{body}")>` under a per-channel secret shown once at creation (R29, controller ruling: a signed timestamp, not the spec's body-only HMAC).
- Delivery (spec): durable queue, pending → sent | failed, up to **5 attempts** with exponential backoff (**30 s … 1 h**), **10 s** timeout, each attempt recorded in the incident timeline. A delivery that succeeded is never resent.
- Escaping (spec):
  - Slack: escape `&`, `<`, `>`.
  - Discord: `allowed_mentions: {"parse": []}` plus markdown escaping.
  - Telegram: HTML parse mode with `&`/`<`/`>` escaped.
  - Teams: an Adaptive Card v1.4, user text only in `TextBlock`s, with `*`, `_`, `[`, `]`, `(`, `)`, `` ` ``, `#`, `>`, `-`, `!`, `&`, `<`, `>` escaped. The only link is an `Action.OpenUrl` to the incident.
- Action links (spec):
  - they lead to a nami-ui confirmation page (`/incidents/action?token=…`) that POSTs, and a GET never mutates;
  - the token is HMAC-SHA256 over `(incident_id, action, user_id, exp)` with a key derived from `NAMI_ENCRYPTION_KEY` and expires after 24 h;
  - the POST needs no session and records the token's user as the actor.
- Outbound requests (R9):
  - Every chat/webhook send goes through one of two process-wide clients in `outbound`. Both use `.no_proxy()`, `redirect(Policy::none())`, `referer(false)`, `pool_max_idle_per_host(0)` (each request resolves again, so a DNS rebind is judged at connect time by the resolver, which returns the only addresses reqwest may connect to) and a 10 s timeout.
  - `outbound::client()` uses the monitors' `GuardedResolver`. It refuses link-local `169.254.0.0/16`/`fe80::/10`, `fd00:ec2::254`, `0.0.0.0/8`, `::` and their mapped/NAT64 forms, and allows private and loopback addresses. It serves only **project `webhook` channels** (admin-made, self-hosted receivers) and the Telegram API base (operator config).
  - `outbound::public_client()` uses `PublicOnlyResolver`. It refuses everything `GuardedResolver` refuses, plus loopback, RFC 1918, CGNAT `100.64.0.0/10`, `198.18.0.0/15`, ULA `fc00::/7`, multicast, broadcast and documentation ranges. It serves every **personal** channel (any logged-in user creates them, viewers included) and every Slack/Discord/Teams channel.
  - IP literals are checked against the same policy before the request (`check_url`).
  - Error text is a fixed vocabulary (`describe_error`), built from `reqwest::Error::without_url()`, and never contains a URL, body, address or token.
- Secrets in memory: no type that holds a channel secret, a signing secret or an HMAC key derives `Debug`. Each gets a manual `Debug` that prints only ids and kinds: `ChannelConfig`, `OpenedChannel`, `ChannelPatch`, and the create/patch request bodies of `incidents::channel::routes`. Keys and opened secrets are held in `zeroize::Zeroizing`. Never pass them to `tracing` fields or `#[instrument]` (use `skip_all`).
- Every new POST/PUT/PATCH route is added to the route list of `tests/core/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json`, in the task that adds the route. Every new table is added to `tests/core/migrations.rs::all_tables_exist`.
- Every new integration test file is a module of its domain target: `mod <file>;` in `nami-engine/tests/<domain>/main.rs`, and the file starts with `use crate::common;`. New engine modules live in the `incidents` domain (`src/incidents/`), never at the crate root, and are declared in `src/incidents/mod.rs` (or their area's `mod.rs`).
- nami-ui:
  - every engine call goes through `callEngine`, and every path through `enginePath`;
  - route params use `[...=uuid]` matchers;
  - form actions return `engineFailure(...)` on engine errors;
  - deletes take two steps (`confirm=yes`);
  - pages that show or accept secrets set `cache-control: private, no-store`;
  - follow SvelteKit 3 conventions (`#lib` alias, `process.env` in server code, runes, `page` from `$app/state`), use `defaultValue`/`defaultChecked` for initial input values, and copy the dark theme classes from the existing pages.
- Tests: test binaries run one after another (`cargo test`). Mailpit assertions filter by their own recipient and never delete messages.
- Commits: Conventional Commits on `feat/incidents-part1` with the task ID (e.g. `(NAMI-INC-3)`), whose message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **A DST change must not move a handoff or a working-hours window by an hour.** Example: a weekly rotation in `Europe/Berlin` handing off Monday 09:00 must hand off at 09:00 local both before and after the spring-forward Sunday. A 02:30 handoff on that day (a local time that does not exist) happens at 03:00, never at 01:30 and never a day late → Task 6 (`weekly_handoff_keeps_local_time_across_spring_forward`, `a_handoff_inside_the_spring_forward_gap_happens_at_the_first_valid_instant`, `fall_back_repeats_no_shift`).
- **A flapping source, or many incidents opening at once, must not flood a channel.** Example: a monitor that goes down and up every minute, or 50 monitors behind one dead switch. Deliveries to one channel are capped per minute (the excess waits, it is not dropped), each incident is capped in total, and a repeat pass never starts sooner than 5 minutes after the previous one → Task 5 (`a_channel_never_receives_more_than_its_per_minute_cap`) and Task 9 (`the_per_incident_delivery_cap_stops_a_storm`, `repeats_wait_at_least_the_minimum_gap`).
- **An acknowledgement that races an escalation tick must win.** Example: a step-2 notification that is already queued when someone acknowledges. That notification must not go out, and no later step may run → Task 9 (`an_acknowledge_cancels_queued_step_notifications`).
- **Nobody on call must never mean nobody notified, silently.** Example: a schedule target outside its working hours, or a schedule with no participants. That pass falls through to the fallback policy. Without a fallback, the timeline records `escalation_exhausted` with the reason, so the gap is visible on the incident → Task 9 (`a_pass_that_notifies_nobody_moves_to_the_fallback_once`, `without_a_fallback_an_empty_pass_is_recorded_as_exhausted`).
- **A user who can only make personal channels must not reach internal services through Nami.** Example: a global viewer with no project creates a personal webhook to `http://127.0.0.1:5432/` or to a name that resolves to `10.0.0.5`, then presses "send test". It is refused at create time (an IP literal) or at connect time (a name, `PublicOnlyResolver`), and the reply is "destination not allowed", never a status code from the internal service → Task 3 (`public_only_destinations_refuse_every_internal_address`, `personal_channels_belong_to_their_user_only`) and Task 5 (`the_strict_policy_refuses_internal_and_off_allowlist_destinations_at_send_time`).
- **One crashed send must not wedge the queue.** Example: the engine dies mid-send on a delivery's 5th attempt. The next claim fails that row with `delivery interrupted` and drains the rest → Task 5 (`a_lease_that_expired_after_the_last_attempt_fails_the_row_instead_of_wedging_the_queue`).
- **Someone removed from a project must stop getting its alerts, and their links must stop working.** This applies to schedule participants, user targets, personal channels and outstanding action links → Task 9 (`a_removed_member_is_not_notified_even_when_on_call`) and Task 11 (`a_token_of_a_user_no_longer_in_the_project_is_refused`).

## Decisions the spec leaves open (rulings)

- **R1 Split.** The spec has 19 roadmap bullets. They do not fit one plan of at most 16 tasks, so this is Part 1, the core named in the header. Part 2's scope is listed there.
- **R2 Migrations.** One file per domain for the phase (`docs/superpowers/plan-conventions.md`, "Migrations"). NNNN is the next free number in `nami-engine/migrations/` when the phase starts, taken in the order below.
  - `NNNN_incidents_part1.sql` (incidents domain). Task 1 creates it, later tasks append their part, in this order:
    1. Task 1: severities, the new incident columns, `incident_events`, the generalised unique indexes (R4).
    2. Task 3: `notification_channels`.
    3. Task 5: `notification_deliveries` (references channels).
    4. Task 7: `on_call_schedules`, `on_call_participants`, `on_call_overrides`.
    5. Task 8: `escalation_policies`, `escalation_steps`, `escalation_step_targets` (target schedules and channels), the incident escalation columns, the default policy and the R6 update.
    6. Task 11: `incident_action_token_uses`.
  - `NNNN_uptime_escalation.sql` (Task 10): `escalation_policy_id` and `severity_id` on `monitors` and `heartbeats`.
  - `NNNN_logs_escalation.sql` (Task 10): the same two columns on `log_alerts`.
  - **Foreign-key order.** Logs Part 2's `NNNN_logs_alerts.sql` and `NNNN_incidents_log_alerts.sql` are merged before this phase starts. `NNNN_incidents_part1.sql` takes the lowest new number, because it drops `incidents_one_open_per_log_alert` and reads `incidents.log_alert_id`. `NNNN_uptime_escalation.sql` and then `NNNN_logs_escalation.sql` come after it, because both reference `escalation_policies` and `incident_severities`.
  - Appending to the unmerged file changes its checksum, so recreate the dev database after each such task (`sqlx::test` databases are fresh per test and need nothing). Never edit a merged migration.
- **R3 Table names** (the spec leaves these to the plan): `escalation_policies`, `escalation_steps`, `escalation_step_targets`, `on_call_schedules`, `on_call_participants`, `on_call_overrides`, `notification_channels`, `notification_deliveries`, `incident_events`, `incident_severities`, `incident_action_token_uses`.
- **R4 One unresolved incident per source.** Task 1's part of `NNNN_incidents_part1.sql` drops `incidents_one_open_per_monitor`, `incidents_one_open_per_heartbeat` (baseline `0004_incidents.sql`) and `incidents_one_open_per_log_alert` (Logs Part 2's `NNNN_incidents_log_alerts.sql`), all `where status = 'open'`. It recreates them as `..._one_unresolved_per_...` with `where status <> 'resolved'`, so an acknowledged incident still blocks a second one. `lifecycle::open` names the matching conflict target per source. Each target is a separate SQL literal, because a `match` on the variant picks the statement.
- **R5 Seeding by trigger.** Default severities and the default policy are created by `after insert on projects` triggers, plus a backfill for existing projects. A project created in any way gets them, including directly in SQL as the tests' `seed_project` does.
- **R6 Existing open incidents at deploy time.** They get the default policy with `next_escalation_at = null`. The old email path already alerted for them, and a deploy must not page everyone again. They still receive "resolved" notifications. Their earlier emails were not recorded as deliveries, so the recipients are resolved from the default policy, which is all members, exactly who was emailed.
- **R7 Who gets notified.**
  - A `user` target gets their implicit email (only if `email_verified_at` is set) plus every **enabled** personal channel.
  - `schedule` resolves to whoever is on call when the step runs.
  - `all_members` means every `project_members` row.
  - `project_channel` means that channel if it is enabled.
  - A user who is not a project member when the step runs is skipped, even if they are on call or named directly. Global admins are no exception: add them as members to page them.
  - Recipients are deduplicated within a step by `target_key` (`email:{user_id}` / `channel:{channel_id}`).
  - A person may be notified again by a later step or pass. That is the point of escalating.
- **R8 Personal channel kinds.** `webhook`, `slack`, `teams`, `telegram` and `discord` are allowed. A personal `email` channel is not: the implicit verified address covers it, and an unverified extra address would let anyone mail third parties through Nami. Project `email` channels (admin) take any address, for example a team list. **Controller ruling (2026-10-19): confirmed.**
- **R9 URL rules per kind.** These are checked at create/update and again before each send. **Controller ruling (2026-10-19): kept.** Personal webhooks and every chat-host webhook (Slack, Discord, Teams) reach public addresses only, checked again on every connection.
  - webhook: `http` or `https`, no userinfo, host required.
    - A **project** webhook (admin) may point at private and loopback hosts (self-hosted receivers). It is sent with `outbound::client()`, the same DNS guard as monitors.
    - A **personal** webhook must resolve to public addresses only. It is sent with `outbound::public_client()`. Personal channels can be made by any logged-in user, a global viewer with no project included. Without this rule, "send test" would hand such a user a POST to any internal service, and its status line ("HTTP 404" or "connection failed") would let them scan ports. The create/update check refuses a private or loopback IP literal with 400 `personal channels must point at a public address`. Names are judged by `PublicOnlyResolver` when the request connects.
  - slack: `https://hooks.slack.com/` exactly. Public-only.
  - discord: `https://discord.com/api/webhooks/…`, also `discordapp.com`, `ptb.discord.com`, `canary.discord.com`. Public-only.
  - teams: any `https` host (Workflows URLs are tenant-specific). Public-only, for project channels too.
  - telegram: no URL. A bot token `^\d{1,20}:[A-Za-z0-9_-]{30,64}$` and a chat id `^-?\d{1,20}$` or `^@[A-Za-z0-9_]{5,32}$`, sent to `TELEGRAM_API_BASE_URL` (default `https://api.telegram.org`, like `RDAP_BASE_URL`) with `outbound::client()`, because the base URL is operator config, possibly a local Bot API server.
  - **Test seam.** `Config.outbound_policy: outbound::OutboundPolicy` is `Strict` in production. `Config::from_env` never reads it from the environment, so no deployment can turn it off. Only `Config::from_env_for_test` sets `Relaxed`. Under `Relaxed`, the send-time check keeps the scheme/userinfo/host-present rules and the monitors' forbidden-address set, but skips the per-kind host allowlists, the `https`-only rule and the public-only rule. That lets integration tests send Slack/Discord/Teams payloads and personal channels to wiremock on `127.0.0.1`. Create/update validation through the API is always strict, whatever the policy. Integration tests insert channels through the model (`common::insert_channel`), which skips validation. One test per rule sets `state.config.outbound_policy = Strict` and proves the send-time refusal (Task 5).
- **R10 Secrets at rest.** `notification_channels.config_enc` is sealed under `notification_channels.config:{id}` and `signing_secret_enc` under `notification_channels.signing_secret:{id}` (ids generated in Rust before the insert). A non-secret `summary` column holds what the UI shows: the webhook URL's scheme and host, `hooks.slack.com`, `discord.com`, the Teams host, `Telegram chat <chat_id>`, or the email address. A channel whose secret no longer decrypts (key changed) fails each delivery with `channel configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)` and is never sent anywhere.
- **R11 Errors recorded in the timeline.** Viewers can read the timeline, so it records only kinds and status codes: `HTTP 404`, `timeout`, `connection failed`, `destination not allowed`, `email not delivered`. It never records a response body, URL, address or token. The Telegram bot token is part of the request path, so `describe_error` always calls `without_url()`.
- **R12 Retry policy.**
  - Retryable: timeouts, connection errors, HTTP 5xx and 429 (`Retry-After` honoured up to 1 h), and SMTP failures.
  - Any other 4xx fails at once (the URL or token is wrong; retrying cannot help).
  - Backoff after failed attempt n is 30 s × 4^(n−1), capped at 1 h: 30 s, 2 min, 8 min, 32 min. After attempt 5 the delivery is `failed`.
  - A claimed delivery holds a 60 s lease. A crash between "sent" and recording it can resend once (at-least-once at the transport level). Webhooks carry `X-Nami-Delivery: <delivery id>` so receivers can drop that duplicate.
  - **Poison rows.** The claim increments `attempts`. A row whose lease expired after its 5th attempt (the process crashed or panicked mid-send) is never reclaimed. The claim transaction first marks it `failed` with `delivery interrupted`. Otherwise `attempts + 1` would break the `attempts between 0 and 5` check, and that would fail the whole claim statement and wedge the queue for every incident.
- **R13 Storm caps.**
  - At most 30 deliveries per channel per rolling minute, and at most 20 emails per user per minute. Excess deliveries are rescheduled 60 s later, not counted as an attempt and not dropped.
  - The caps are enforced **at claim time, inside the claim transaction, under one transaction-level advisory lock**. The counts are "sent in the last minute" plus "currently leased" per bucket, and each candidate gets a rank within its bucket (Task 5 gives the SQL). The claim itself is serialised across every engine instance, and claimed rows count against the cap at once. So the cap is exact even with ten concurrent sends per drain and several instances. A per-row "count, then send" check would let every concurrent task see the same count. Over-cap candidates are deferred in the same statement, so a busy channel can never starve the others at the head of the queue.
  - At most 500 deliveries per incident, after which one `notification_failed` event says the cap was reached.
  - At most 100 recipients per step.
  - A repeat pass starts no sooner than 5 minutes after the previous pass ended (`MIN_REPEAT_GAP`).
  - One tick runs at most 20 consecutive zero-delay steps of one incident.
- **R14 Escalation cursor.**
  - Columns: `escalation_policy_id`, `escalation_step` (0-based, the next step to run), `escalation_repeat`, `escalation_on_fallback`, `escalation_notified` (recipients notified in the current pass) and `next_escalation_at`.
  - A step's run time `ran_at` is the previous step's *scheduled* time plus its delay, not "now", so a slow tick does not drift the policy. One exception: `ran_at = max(next_escalation_at, now − 60 s)`. A step overdue by more than a minute (the worker was down) runs once, now, and the policy's clock restarts from now. An outage therefore never replays every missed step and repeat as one burst.
  - The step's **targets and condition are evaluated at the tick's `now`**, not at `ran_at`. After an outage, the person on call *now* is paged, and a "business hours" step is judged by the current time.
  - The decision after each step is the pure `escalation::plan::after_step` (Task 8).
  - Deleting the policy of a running incident ends its escalation with `escalation_exhausted` ("policy deleted").
- **R15 Ack/resolve cancels queued step notifications.** A `triggered` delivery whose incident is no longer `open` when it is claimed becomes `cancelled`. `resolved` deliveries always go out.
- **R16 Action links only in email.** Each email carries the recipient's own acknowledge and resolve links. Chat messages carry only the incident link, because a "personal" Slack webhook may post into a shared room.
  - Format: `base64url(payload) "." base64url(mac)`, payload `v1|<incident>|<ack|resolve>|<user>|<exp unix>|<nonce>`. The nonce is 9 random bytes, base64url (12 chars). Every link is distinct, so two emails sent in the same second never share one single-use key.
  - The MAC is HMAC-SHA256 under `Zeroizing::new(cipher.derive_subkey("nami/incident-action/v1"))`. It is checked with `Mac::verify_slice` (constant time) before the payload is parsed.
  - Tokens are **single-use**. The first successful confirm stores SHA-256(token) in `incident_action_token_uses`, and a second confirm gets 410.
  - The confirm order inside one transaction is: verify, lock the incident row, check the role, refuse a token already used (410), apply the action (409 if it no longer applies; a fresh token is not consumed), then insert the use (`on conflict do nothing`; no row → roll back, 410). Two concurrent confirms are serialised by the row lock, and the primary key backs that up.
  - The user must still be a project `member` or `admin` (or a global admin) when the link is used. A password change revokes every outstanding link. The token never creates a session and grants nothing else.
  - The engine endpoints take the token in a JSON body (never a URL), so it stays out of the engine's request logs. They sit behind their own per-IP governor (burst 20, one token every 3 s), not the login one (burst 5, one a minute), because a responder may act on several links in a row.
  - The nami-ui page sends `referrer-policy: no-referrer` and `cache-control: no-store`. Its `load` only inspects, so mail scanners and link previews that GET it change nothing. Its form action is protected by SvelteKit's built-in origin check (`csrf.checkOrigin`, on by default; never disable it). It links only to `/projects/{project_id}/incidents/{id}`, built from ids that pass `isUuid`, so it has no redirect parameter to abuse.
- **R17 Routing.**
  - `alerts::notify_transition` keeps its signature, so the uptime worker, the heartbeat endpoint and Logs Part 2's alert worker do not change.
  - Its body no longer emails anyone. It calls `incidents::worker::kick(state)`, which runs one escalation tick and one delivery drain at once. The uptime worker's `Notifications` set bounds its own calls.
  - The heartbeat ping handler (`src/uptime/heartbeat/routes.rs:482`) calls `notify_transition` from a bare `tokio::spawn` per transition. So `kick` bounds itself: a process-wide `static KICKS: tokio::sync::Semaphore = Semaphore::const_new(2)` permits at most two kicks at once, and the rest *wait* for a permit, so no work is lost. Every tick and drain is bounded (`ESCALATION_BATCH`, `DRAIN_BATCH`), and a kick that finds nothing due costs two indexed queries.
  - `kick` does nothing when `config.incidents_worker_enabled` is false (R23).
  - The worker loop (every 2 s) is the durable backstop.
  - The source wrappers (`incidents::model::open_for_monitor` and the others) keep their signatures and call `lifecycle::open` with the source's `escalation_policy_id`/`severity_id`.
  - Log alerts keep the R-P2-27 seam. `logs::alert::open_alert_incident` is unchanged: it calls the `open_for_log_alert` wrapper. In `resolve_alert_incident(.., notify)`, `notify: true` calls the `resolve_for_log_alert` wrapper (resolved notices go out). **Controller ruling (2026-10-19):** `notify: false` (an alerting alert being disabled) calls `lifecycle::resolve_source_with_reason(conn, &IncidentSource::LogAlert(id), at, "alert disabled")`. That resolves the incident like any automatic resolution: everyone already paged gets the normal "Resolved" notice, which carries `Reason: alert disabled`. The `resolved` event's message is `alert disabled` and its metadata is `{"reason": "alert disabled"}`. `resolve_alert_incident` still returns `None` (no transition, so no `notify_transition`). The route that disables the alert calls `incidents::worker::kick(&state)` after its transaction commits, and the worker loop is the backstop. **This supersedes Logs Part 2's "disabling resolves without a notification" behaviour (R-P2-27, NAMI-LOG-14) once NAMI-INC-10 lands.** Task 10 updates Logs Part 2's disable test to match.
- **R18 Email subjects change** with routing (Task 10). **Controller ruling (2026-10-19): accepted.** A step notification is `[Nami] [<severity>] <title>`. A resolution is `[Nami] Resolved: <title>`. The existing worker, heartbeat, maintenance and log-alert tests that assert the old subjects (`… is DOWN`, `… is back UP`, `Log alert … triggered/resolved`) are updated in Task 10. Their counts stay the same (one email per member per transition). A handful of `tests/uptime/uptime_worker.rs` tests pin the *mechanics* of the old SMTP path: sequential per-recipient sends, giving up after one timeout, its log lines. Task 10 lists them with the property each keeps.
- **R19 Time windows** reuse `uptime::maintenance::model::Schedule::Weekly::covers` and `weekday_from_iso` (Phase 2, already DST-tested). `from == to` means a full 24 h, as for maintenance windows. Wall-clock semantics apply: a window inside a skipped hour does not happen that day, and one across a repeated hour lasts an hour longer.
- **R20 Rotation math.**
  - `daily`/`weekly` shifts are calendar arithmetic in the schedule's zone. Shift k starts at local `(start_date + k days|weeks) handoff` and is converted with `local_to_utc`: a nonexistent time moves to the first valid minute after it, and an ambiguous one takes the earlier instant.
  - Custom rotations (`rotation_hours` 1-8760) are absolute: `epoch + k × hours`.
  - Before the epoch nobody is on call.
  - Restrictions blank the rotation outside their windows. Overrides apply regardless (spec).
  - `shifts(from, to)` evaluates `on_call_at` at every UTC minute boundary and at every override edge in the range, then merges runs of the same user. The range is at most 70 days, which covers the 14-day view and Part 2's ICS window.
- **R21 Incident list.** `GET /projects/{id}/incidents` returns `{"incidents": [...], "next_cursor": string|null}`, newest first, `limit` 1-100 (default 50). It takes `status` (`open`, `acknowledged`, `resolved`, or `unresolved`; repeatable) and `source` (repeatable). The response shape changes from a bare array, and Task 2 updates the existing nami-ui loader in the same task. Part 2 adds the remaining filters.
- **R22 Postmortem rendering.** nami-ui renders postmortems with a small in-repo renderer (`#lib/incidents/markdown`). It HTML-escapes everything first, then applies a fixed subset: headings, emphasis, inline code, fenced code, lists, paragraphs, and `http(s)` links with `rel="noopener noreferrer nofollow"`. No raw HTML passes, and there is no new dependency.
- **R23 Worker switch. Controller ruling (2026-10-19):** the incidents worker has its own flag, `INCIDENTS_WORKER_ENABLED` (default `true`, parsed with `env_flag` like `UPTIME_WORKER_ENABLED`). It is `Config.incidents_worker_enabled` and is documented in `nami-engine/.env.example` (Task 5). It does not reuse `UPTIME_WORKER_ENABLED`. When it is false, `main` does not spawn `incidents::worker::run_forever`, and `kick` returns at once. Every claim uses `skip locked` (escalation) or a transaction-level advisory lock (deliveries), so several instances with the flag on are safe.
- **R24 Not in Part 1** (Part 2 or later): everything listed under "Part 2 follows". SMS, phone and push are spec non-goals (they would need a paid provider), so this plan has none.
- **R25 Overrides are admin-only in Part 1. Controller ruling (2026-10-19): accepted.** The spec's RBAC already lists overrides under admins. Members get "take over shift" (an override for themselves only) in Part 2.
- **R26 Heartbeat output stays visible.** Uptime Phase 3 stores a heartbeat failure's output in `incidents.details`. The incident list item and the detail keep `details` (Task 2). The email keeps the `Output:` block, each line prefixed `> ` (Task 4). The incidents page keeps NAMI-UP-29's `<details>` block, and the detail page gets the same block (Task 12). Chat messages and the webhook body do not carry it: the spec fixes the webhook body, and chat stays short.
- **R27 Routing switches in Task 10 only.** The phase ships as one PR, so nothing deploys between tasks.
  - At deploy, R6's update (Task 8's part of `NNNN_incidents_part1.sql`) leaves every unresolved incident unarmed (`next_escalation_at` is a new column, null) with `escalation_legacy = true`. That covers the monitor, heartbeat and log-alert incidents the old path emailed, so no deploy pages for them twice.
  - On the branch, the incidents worker is spawned from `main.rs` only in Task 10, when routing switches. Before that, deliveries go out only where a test calls `kick` ("send test" is synchronous), so Tasks 5-9 never page anyone alongside the old email path.
  - The per-task-PR draft also had a Task 10 statement that disarmed unresolved source incidents again. With one PR it repeated R6's update, so it is dropped.
- **R29 Signed webhook timestamp. Controller ruling (2026-10-19).** Every webhook request carries `X-Nami-Timestamp: <unix seconds>` (decimal, no fraction) and `X-Nami-Signature: sha256=<lowercase hex>`. The hex is HMAC-SHA256 with the channel's signing secret (the 43-char string shown once, as its ASCII bytes) over the exact bytes `<timestamp>` `.` `<raw body>`. Example: `1792000000.{"event":...}`. `X-Nami-Delivery: <delivery uuid>` stays. Receiver guidance, shown on the channel page right after creation (Task 13) and in the spec's webhook section: recompute the HMAC over `{X-Nami-Timestamp}.{raw body}` and compare in constant time; reject a timestamp more than 5 minutes old (or in the future); store `X-Nami-Delivery` ids for at least 5 minutes and drop a repeat. The timestamp is taken once per attempt, just before sending, so a retry has a fresh one. This supersedes the spec's "HMAC of body"; the spec's webhook bullet is updated in Task 4's commit.
- **R30 Resolution reasons.** A resolution may carry a short reason (≤ 200 chars, one line), stored as the `resolved` event's message and `metadata.reason`. Part 1 sets it only for a log alert disabled while alerting (`alert disabled`, R17). The delivery drain reads the reason of the incident's latest `resolved` event when it builds a `resolved` message (`IncidentSummary.resolution_reason`).
- **R28 Project members and roles for the UI.** The engine has no members list today (`/projects/{id}/members` takes POST only), and the project layout returns no role. Task 7 adds `GET /projects/{id}/members` (viewer) → `[{user_id, email, project_role}]`, by email. Schedule participants, override users and policy user targets are chosen from it. **Controller ruling (2026-10-19):** Task 7 also makes `GET /projects/{id}` return the caller's effective role, `role: "admin" | "member" | "viewer"` (a global admin is `"admin"`, member or not). The project layout (`(app)/projects/[id=uuid]/+layout.server.ts`) passes it on as `data.role`. The UI hides admin-only forms (channels, policies, schedules, overrides) for non-admins and member-only actions (incident actions, comments, postmortem, new incident) for viewers. The engine stays the authority: every hidden action still answers 403, and the pages still show an engine 403 as the form message. Tests cover both sides: the engine route × role tests, and UI render tests with `role: 'viewer'` (no form) and `role: 'admin'` (form).

- **R31 Flap control after Task 10 (controller ruling, 2026-10-24).** Task 10 removes `uptime::alert_throttle::AlertThrottle` and the deferred email sender. The flap property moves into escalation: the first escalation step of an incident is not run until 5 minutes after the same source's previous paging pass (monitor, heartbeat or log alert). A reopened heartbeat incident (`lifecycle::reopen_source_within`) re-arms escalation under the same 5-minute rule, so a flapping source pages at most once per 5 minutes. The two flap tests (`tests/uptime/uptime_worker.rs`, `tests/uptime/heartbeats.rs`) keep their assertions with the new subjects.
- **R32 `OutboundPolicy` lives in `core::config` (controller ruling, 2026-10-24).** It is a configuration switch, not domain logic, and `docs/STANDARDS.md` keeps `core` free of domain imports. `incidents::outbound` uses `crate::core::config::OutboundPolicy`. The drift note in Task 3 that offered both places is settled by this ruling.
- **R33 No separate disarm statement in Task 10 (controller ruling, 2026-10-24).** The phase ships as one PR, so nothing is deployed between tasks; R6's update already leaves every unresolved incident unarmed at deploy.

## File Structure

```
nami-engine/
  Cargo.toml (+ Cargo.lock)            T3 hmac 0.13.0 (direct)
  .env.example                         T5 TELEGRAM_API_BASE_URL, INCIDENTS_WORKER_ENABLED
  migrations/
    NNNN_incidents_part1.sql           T1 creates; T3, T5, T7, T8, T11 append (R2)
    NNNN_uptime_escalation.sql         T10 monitors/heartbeats escalation_policy_id, severity_id
    NNNN_logs_escalation.sql           T10 log_alerts escalation_policy_id, severity_id
  src/
    main.rs                            T10 spawn incidents::worker::run_forever under INCIDENTS_WORKER_ENABLED (R27)
    core/
      config.rs                        T3 outbound_policy (Strict; Relaxed only in from_env_for_test);
                                       T5 telegram_api_base_url, incidents_worker_enabled
      error.rs                         T11 AppError::Gone(String) → 410
      router.rs                        T2, T3, T7, T8, T11 merge the new routers (T11: own governor)
      validate.rs                      T2, T3, T7, T8 shared checks
    incidents/
      mod.rs                           module list (T1, T2, T3, T5, T6, T7, T8, T11 add theirs)
      model.rs                         T1 wrappers over lifecycle; T2 list/detail/events; T10 source policy/severity
      routes.rs                        T2 list/create/detail/ack/resolve/reopen/comments/postmortem/severity
      lifecycle.rs                     T1 IncidentSource, OpenIncident, open/acknowledge/resolve/reopen/comment/record_event;
                                       T8 arms escalation; T9 queues resolved notifications
      outbound.rs                      T3 client(), public_client(), PublicOnlyResolver, OutboundPolicy,
                                       URL rules, describe_error
      timewindow.rs                    T6 TimeWindow (wraps uptime::maintenance::model::Schedule::Weekly)
      severity/{mod,model,routes}.rs   T2 list/find
      channel/{mod,model,routes}.rs    T3 Channel, ChannelConfig, seal/open, CRUD, project + personal routes; T5 test send
      format.rs                        T4 per-channel rendering + escaping, webhook signature
      send.rs                          T5 senders per kind, SendError, retry classification
      deliveries.rs                    T5 queue claim/drain/backoff/caps
      worker.rs                        T5 loop (drain); T9 + escalation tick, kick
      oncall/mod.rs, rotation.rs       T6 pure rotation math
      oncall/model.rs, routes.rs       T7 schedules, participants, overrides
      escalation/mod.rs, plan.rs       T8 pure planner
      escalation/model.rs, routes.rs   T8 policies, steps, targets, validation
      escalation/run.rs                T9 the tick
      action/mod.rs, token.rs          T11 sign/verify, single use
      action/routes.rs                 T11 inspect/confirm (no session)
    projects/model.rs, routes.rs       T7 GET /projects/{id}/members; GET /projects/{id} gains `role` (R28)
    uptime/
      checker/mod.rs                   T3 `GuardedResolver` (and its field) and `caused_by` become pub(crate) (no move, no behaviour change)
      alerts.rs                        T10 notify_transition → incidents::worker::kick; render removed; flap control: open question (Task 10)
      monitor/{model,request,routes}.rs, heartbeat/{model,routes}.rs  T10 escalation_policy_id, severity_id
      worker.rs, heartbeat/mod.rs      unchanged: policy/severity come through the incidents::model wrappers
    logs/alert/mod.rs                  T10 resolve_alert_incident(notify: false) → lifecycle::resolve_source_with_reason(.., "alert disabled") (R17)
    logs/alert/model.rs, routes.rs     T10 escalation_policy_id, severity_id; PATCH kicks the worker
  tests/
    common/mod.rs                      T1 open_test_incident; T5 run_incident_ticks, insert_channel; T9 make_escalation_due, set_policy
    incidents/main.rs                  `mod <file>;` for each new file below
    incidents/incident_lifecycle.rs    T1
    incidents/incidents_api.rs         T2
    incidents/notification_channels.rs T3
    incidents/notification_deliveries.rs T5
    incidents/schedules_api.rs         T7
    incidents/escalation_policies_api.rs T8
    incidents/escalation_engine.rs     T9
    incidents/incident_routing.rs      T10
    incidents/incident_actions.rs      T11
    incidents/incidents.rs             T1 one test renamed (status_is_limited_to_the_lifecycle_states)
    core/migrations.rs                 T1 (+ `use crate::common;`), T3, T5, T7, T8, T10, T11
    core/json_bodies.rs                T2, T3, T5, T7, T8, T11 new state-changing routes
    uptime/uptime_read_api.rs, uptime/heartbeats.rs  T2 list response shape {incidents, next_cursor}
    uptime/uptime_worker.rs, uptime/heartbeats.rs, uptime/recovery_threshold.rs,
    uptime/uptime_worker_timing.rs, logs/log_alerts.rs  T10 subject assertions (R18), the old-SMTP-mechanics
                                       tests and the flap-control tests (Task 10)

nami-ui/src/
  lib/incidents/incidents.ts (+ .test.ts)  T2 types; T12 status/source/severity helpers, timeline labels
  lib/incidents/markdown.ts (+ .test.ts)   T12 safe postmortem renderer
  lib/incidents/oncall.ts (+ .test.ts)     T15 shift rows, local time input
  lib/server/incidents/channel-form.ts (+ .test)  T13 channel form → engine body (write-only secrets)
  lib/server/incidents/policy-form.ts (+ .test)   T14 policy editor form → engine body
  lib/server/incidents/schedule-form.ts (+ .test) T15 schedule/override form → engine body
  lib/components/SeverityBadge.svelte  T12
  lib/components/ChannelList.svelte    T13 shared by personal + project pages
  routes/(app)/projects/[id=uuid]/
    +layout.server.ts, section.test.ts T7 passes `role` (R28)
    +layout.svelte                     T14 "Escalation", T15 "On-call" tabs
    +page.svelte, +page.server.ts      T15 "On call now" card
    incidents/+page.*                  T2 loader shape; T12 list, tabs, new incident (keeps the Job output block, LOG-18 link)
    monitors/[monitorId=uuid]/+page.server.ts  T2 reads `.incidents` of the new list shape
    incidents/[incidentId=uuid]/+page.* T12 detail, timeline, actions, comments, postmortem
    settings/notifications/+page.*     T13 project channels
    escalation/+page.*, escalation/[policyId=uuid]/+page.*  T14
    oncall/+page.*, oncall/[scheduleId=uuid]/+page.*        T15
    monitors/new, monitors/[monitorId=uuid]/edit, heartbeats/..., logs/alerts/...  T14 policy/severity selects
  routes/(app)/account/notifications/+page.*  T13 personal channels (new folder; the security page is `(app)/security/`)
  routes/incidents/action/+page.*      T11 confirmation page (outside (app), no login)
```

---
### Task 1 (NAMI-INC-1): Incident lifecycle schema and the unified open/resolve API

**Files:**
- Create: `nami-engine/migrations/NNNN_incidents_part1.sql` (the phase's incidents file, see R2), `nami-engine/src/incidents/lifecycle.rs`
- Modify: `nami-engine/src/incidents/mod.rs` (`pub mod lifecycle;`; the module exists), `nami-engine/src/incidents/model.rs` (the six source wrappers delegate to `lifecycle`), `nami-engine/tests/common/mod.rs` (`open_test_incident`), `nami-engine/tests/core/migrations.rs`
- Test: `nami-engine/tests/incidents/incident_lifecycle.rs` (`mod incident_lifecycle;` in `tests/incidents/main.rs`). The existing `tests/incidents/incidents.rs` must stay green unchanged, except `status_is_limited_to_open_and_resolved`, which is renamed `status_is_limited_to_the_lifecycle_states` and also accepts `acknowledged`.

**Interfaces:**
- Consumes: `incidents` (baseline `0004_incidents.sql`, which holds `details`, plus Logs Part 2's `NNNN_incidents_log_alerts.sql` with `log_alert_id`), `uptime::checker::is_unsafe_in_text`. `incidents::model` already holds Logs Part 2's `*_for_log_alert` wrappers (planned in NAMI-LOG-14) and Uptime Phase 3's `attach_details` and `IncidentListItem.details`. Keep `attach_details` unchanged: `record_signal` calls it right after `open_for_heartbeat`, in the same transaction.
- **Drift (2026-10-24): heartbeat reopen.** `open_for_heartbeat` (`src/incidents/model.rs:94-156`) is not a plain insert. Inside `HEARTBEAT_REOPEN_WINDOW_SECS` (5 min, `src/incidents/model.rs:99`) after the heartbeat's latest incident resolved, it reopens that incident (status `open`, `resolved_at` cleared, title and cause replaced, `details` cleared, id and `started_at` kept) and returns its id. `tests/uptime/heartbeats.rs:3263` (`a_flapping_heartbeat_keeps_one_incident_and_sends_one_pair_of_emails`), `:3300` and `:3381` pin it, and `src/uptime/availability.rs:10-13` counts on it. So the heartbeat wrapper keeps the reopen and routes it through `lifecycle`: add `pub async fn reopen_source_within(conn, source: &IncidentSource, window_secs: i32, title: &str, cause: Option<&str>, at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>`, which runs the existing reopen `update` (plus `resolved_by`, `acknowledged_at`, `acknowledged_by` cleared) and records a `reopened` event with no actor. `open_for_heartbeat` calls it first and `lifecycle::open` only when it returns `None`. From Task 8 on it re-arms like `reopen` (generation + 1); how often a flapping source may page is the Task 10 open question. The other five wrappers are one-line calls.
- Produces (`nami_engine::incidents::lifecycle`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IncidentSource {
    Monitor(Uuid),
    Heartbeat(Uuid),
    LogAlert(Uuid),
    Manual { created_by: Uuid },
}
impl IncidentSource { pub fn as_str(&self) -> &'static str }   // "monitor" | "heartbeat" | "log_alert" | "manual"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind { Opened, Acknowledged, Resolved, Reopened, Escalated, NotificationSent, NotificationFailed,
    Comment, PostmortemUpdated, IntegrationEvent, StepSkipped, EscalationFallback, EscalationExhausted,
    SeverityChanged, MetadataUpdated, TicketCreated }
impl EventKind { pub fn as_str(self) -> &'static str }          // snake_case, e.g. "notification_sent"

#[derive(Debug, Clone)]
pub struct OpenIncident<'a> {
    pub project_id: Uuid,
    pub source: IncidentSource,
    pub title: &'a str,
    pub cause: Option<&'a str>,
    /// `None` or an id of another project: the project's default severity.
    pub severity_id: Option<Uuid>,
    pub metadata: Option<&'a serde_json::Map<String, serde_json::Value>>,
    pub started_at: DateTime<Utc>,
}

pub enum Reopen { Reopened, NotResolved, SourceHasUnresolvedIncident, NotFound }

/// Opens an incident unless the source already has an unresolved one (then `None`).
/// Records an `opened` event in the same transaction.
pub async fn open(conn: &mut PgConnection, req: &OpenIncident<'_>) -> Result<Option<Uuid>, sqlx::Error>;
/// open → acknowledged. `false` when it was not open.
pub async fn acknowledge(conn: &mut PgConnection, incident_id: Uuid, actor: Uuid, at: DateTime<Utc>) -> Result<bool, sqlx::Error>;
/// open|acknowledged → resolved. `actor: None` = automatic. `false` when already resolved.
pub async fn resolve(conn: &mut PgConnection, incident_id: Uuid, actor: Option<Uuid>, at: DateTime<Utc>) -> Result<bool, sqlx::Error>;
/// Resolves the source's unresolved incident (automatic). Returns its id.
pub async fn resolve_source(conn: &mut PgConnection, source: &IncidentSource, at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>;
pub async fn reopen(conn: &mut PgConnection, incident_id: Uuid, actor: Uuid, at: DateTime<Utc>) -> Result<Reopen, sqlx::Error>;
/// Automatic reopen (Drift 2026-10-24): the source's latest incident, if it was resolved less than
/// `window_secs` before `at`, goes back to `open` with the new title and cause (details, resolver and
/// acknowledgement cleared, id and started_at kept) and gets a `reopened` event with no actor. `None` otherwise.
pub async fn reopen_source_within(conn: &mut PgConnection, source: &IncidentSource, window_secs: i32, title: &str,
    cause: Option<&str>, at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error>;
pub async fn comment(conn: &mut PgConnection, incident_id: Uuid, actor: Uuid, body: &str) -> Result<i64, sqlx::Error>;
pub async fn record_event(conn: &mut PgConnection, incident_id: Uuid, kind: EventKind, actor: Option<Uuid>,
    message: Option<&str>, metadata: serde_json::Value) -> Result<i64, sqlx::Error>;
```

`incidents::model::{open_for_monitor, resolve_for_monitor, open_for_heartbeat, resolve_for_heartbeat, open_for_log_alert, resolve_for_log_alert}` keep their exact signatures and become calls into `lifecycle` (one line each, except `open_for_heartbeat`'s reopen, see the drift note above). Later sub-projects add `IncidentSource` variants (Part 2: `Integration`, `ReportForm`; Metrics: `MetricAlert`) with their own conflict target.

- [ ] **Step 1: Write the migration**

Create `nami-engine/migrations/NNNN_incidents_part1.sql` (NNNN is the next free number in `nami-engine/migrations/` when the phase starts; R2):

```sql
-- Incident management, part 1 (NAMI-INC-1): severities, the incident
-- lifecycle (open -> acknowledged -> resolved, reopen), the timeline.

create table incident_severities (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null check (name ~ '^[a-z0-9_-]{1,32}$'),
    rank int not null check (rank between 1 and 1000),
    is_default boolean not null default false,
    unique (project_id, name),
    unique (project_id, rank)
);
create unique index incident_severities_one_default on incident_severities (project_id) where is_default;

-- Every project, however it is created, starts with the spec's three.
create function seed_incident_severities() returns trigger language plpgsql as $$
begin
    insert into incident_severities (project_id, name, rank, is_default)
    values (new.id, 'critical', 1, false), (new.id, 'high', 2, true), (new.id, 'low', 3, false);
    return new;
end
$$;
create trigger projects_seed_incident_severities after insert on projects
    for each row execute function seed_incident_severities();

insert into incident_severities (project_id, name, rank, is_default)
select p.id, s.name, s.rank, s.is_default
from projects p
cross join (values ('critical', 1, false), ('high', 2, true), ('low', 3, false)) as s(name, rank, is_default);

alter table incidents drop constraint incidents_status_check;
alter table incidents add constraint incidents_status_check
    check (status in ('open', 'acknowledged', 'resolved'));

alter table incidents
    add column source text not null default 'manual' check (source in
        ('monitor', 'heartbeat', 'log_alert', 'metric_alert', 'manual', 'integration', 'report_form')),
    add column severity_id uuid references incident_severities(id),
    add column metadata jsonb not null default '{}'::jsonb check (jsonb_typeof(metadata) = 'object'),
    add column postmortem text check (octet_length(postmortem) <= 102400),
    add column acknowledged_at timestamptz,
    add column acknowledged_by uuid references users(id) on delete set null,
    add column resolved_by uuid references users(id) on delete set null,
    add column created_by uuid references users(id) on delete set null,
    add constraint incidents_acknowledged_has_time check (status <> 'acknowledged' or acknowledged_at is not null),
    add constraint incidents_resolved_has_time check (status <> 'resolved' or resolved_at is not null);

update incidents set source = case
    when monitor_id is not null then 'monitor'
    when heartbeat_id is not null then 'heartbeat'
    when log_alert_id is not null then 'log_alert'
    else 'manual' end;

-- A row inserted without `source` (existing tests seed incidents in plain SQL
-- with only monitor_id, for example) still gets the source its foreign key
-- says, never the 'manual' default.
create function incidents_derive_source() returns trigger language plpgsql as $$
begin
    new.source := case
        when new.monitor_id is not null then 'monitor'
        when new.heartbeat_id is not null then 'heartbeat'
        when new.log_alert_id is not null then 'log_alert'
        else new.source end;
    return new;
end
$$;
create trigger incidents_derive_source before insert on incidents
    for each row execute function incidents_derive_source();
update incidents i set severity_id = s.id
from incident_severities s where s.project_id = i.project_id and s.is_default;
-- A resolved row without a time cannot come from the worker (it always
-- sets resolved_at), but a hand-written row could: give it one.
update incidents set resolved_at = started_at where status = 'resolved' and resolved_at is null;

-- One unresolved incident per source: an acknowledged incident blocks a new
-- one as much as an open one does (the old indexes only covered 'open').
drop index incidents_one_open_per_monitor;
drop index incidents_one_open_per_heartbeat;
drop index incidents_one_open_per_log_alert;
create unique index incidents_one_unresolved_per_monitor on incidents (monitor_id) where status <> 'resolved';
create unique index incidents_one_unresolved_per_heartbeat on incidents (heartbeat_id) where status <> 'resolved';
create unique index incidents_one_unresolved_per_log_alert on incidents (log_alert_id) where status <> 'resolved';

create table incident_events (
    id bigint generated always as identity primary key,
    incident_id uuid not null references incidents(id) on delete cascade,
    kind text not null check (kind in (
        'opened', 'acknowledged', 'resolved', 'reopened', 'escalated', 'notification_sent',
        'notification_failed', 'comment', 'postmortem_updated', 'integration_event', 'step_skipped',
        'escalation_fallback', 'escalation_exhausted', 'severity_changed', 'metadata_updated',
        'ticket_created')),
    actor_id uuid references users(id) on delete set null,
    message text check (octet_length(message) <= 10240),
    metadata jsonb not null default '{}'::jsonb check (jsonb_typeof(metadata) = 'object'),
    created_at timestamptz not null default now()
);
create index incident_events_incident_idx on incident_events (incident_id, id);

-- History for incidents that predate the timeline.
insert into incident_events (incident_id, kind, created_at)
select id, 'opened', started_at from incidents;
insert into incident_events (incident_id, kind, created_at)
select id, 'resolved', resolved_at from incidents where status = 'resolved';
```

- [ ] **Step 2: Write the failing migration test**

Add to `nami-engine/tests/core/migrations.rs` (in the style of its existing table tests). `tests/core/main.rs` mounts `common` already (Drift (2026-10-24)): add `use crate::common;` at the top of `migrations.rs` unless a Logs Part 2 task already has. Also add `"incident_severities"` and `"incident_events"` to `all_tables_exist`.

```rust
#[sqlx::test]
async fn every_project_gets_the_three_default_severities(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let rows: Vec<(String, i32, bool)> = sqlx::query_as(
        "select name, rank, is_default from incident_severities where project_id = $1 order by rank",
    )
    .bind(project)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [("critical".into(), 1, false), ("high".into(), 2, true), ("low".into(), 3, false)]
    );
    let second_default = sqlx::query(
        "insert into incident_severities (project_id, name, rank, is_default) values ($1, 'x', 9, true)",
    )
    .bind(project)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(second_default.as_database_error().unwrap().code().as_deref(), Some("23505"));
}

#[sqlx::test]
async fn an_acknowledged_incident_still_blocks_a_second_one_for_its_monitor(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let monitor = common::seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    sqlx::query(
        "insert into incidents (project_id, monitor_id, title, status, acknowledged_at, source)
         values ($1, $2, 'down', 'acknowledged', now(), 'monitor')",
    )
    .bind(project)
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap();
    let second = sqlx::query(
        "insert into incidents (project_id, monitor_id, title, source) values ($1, $2, 'down', 'monitor')",
    )
    .bind(project)
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(second.as_database_error().unwrap().code().as_deref(), Some("23505"));
}

#[sqlx::test]
async fn a_plain_insert_takes_its_source_from_its_foreign_key(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let monitor = common::seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let source: String = sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title) values ($1, $2, 'down') returning source",
    )
    .bind(project)
    .bind(monitor)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(source, "monitor");
}
```

Run: engine command `--test core migrations::`. Expected: FAIL (no table `incident_severities`), then PASS once the migration file exists.

- [ ] **Step 3: Write the failing lifecycle tests**

`nami-engine/tests/incidents/incident_lifecycle.rs`:

```rust
// The incident lifecycle (incidents::lifecycle). Requires `docker compose up -d postgres`.
use crate::common;

use chrono::{DateTime, Utc};
use common::{seed_http_monitor, seed_project, seed_user, unique_email};
use nami_engine::incidents::lifecycle::{self, IncidentSource, OpenIncident, Reopen};
use sqlx::PgPool;
use uuid::Uuid;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_792_000_000 + seconds, 0).unwrap()
}

async fn open(pool: &PgPool, project: Uuid, source: IncidentSource) -> Option<Uuid> {
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::open(
        &mut conn,
        &OpenIncident {
            project_id: project,
            source,
            title: "api is down",
            cause: Some("connection refused"),
            severity_id: None,
            metadata: None,
            started_at: at(0),
        },
    )
    .await
    .unwrap()
}

async fn events(pool: &PgPool, incident: Uuid) -> Vec<(String, Option<Uuid>)> {
    sqlx::query_as("select kind, actor_id from incident_events where incident_id = $1 order by id")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn status(pool: &PgPool, incident: Uuid) -> (String, Option<Uuid>, Option<Uuid>) {
    sqlx::query_as("select status, acknowledged_by, resolved_by from incidents where id = $1")
        .bind(incident)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn opening_records_source_default_severity_and_an_opened_event(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let id = open(&pool, project, IncidentSource::Monitor(monitor)).await.unwrap();

    let (source, severity, monitor_id): (String, String, Option<Uuid>) = sqlx::query_as(
        "select i.source, s.name, i.monitor_id from incidents i
         join incident_severities s on s.id = i.severity_id where i.id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((source.as_str(), severity.as_str(), monitor_id), ("monitor", "high", Some(monitor)));
    assert_eq!(events(&pool, id).await, [("opened".to_string(), None)]);
}

#[sqlx::test]
async fn a_severity_of_another_project_falls_back_to_the_default(pool: PgPool) {
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign: Uuid = sqlx::query_scalar(
        "select id from incident_severities where project_id = $1 and name = 'critical'",
    )
    .bind(other)
    .fetch_one(&pool)
    .await
    .unwrap();
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let mut conn = pool.acquire().await.unwrap();
    let id = lifecycle::open(
        &mut conn,
        &OpenIncident {
            project_id: project,
            source: IncidentSource::Manual { created_by: user },
            title: "manual",
            cause: None,
            severity_id: Some(foreign),
            metadata: None,
            started_at: at(0),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let (name, created_by): (String, Option<Uuid>) = sqlx::query_as(
        "select s.name, i.created_by from incidents i join incident_severities s on s.id = i.severity_id
         where i.id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((name.as_str(), created_by), ("high", Some(user)));
}

#[sqlx::test]
async fn a_source_has_at_most_one_unresolved_incident_even_while_acknowledged(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let first = open(&pool, project, IncidentSource::Monitor(monitor)).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert!(lifecycle::acknowledge(&mut conn, first, user, at(10)).await.unwrap());
    assert_eq!(open(&pool, project, IncidentSource::Monitor(monitor)).await, None);
    assert_eq!(
        lifecycle::resolve_source(&mut conn, &IncidentSource::Monitor(monitor), at(20)).await.unwrap(),
        Some(first)
    );
    assert!(open(&pool, project, IncidentSource::Monitor(monitor)).await.is_some());
}

#[sqlx::test]
async fn acknowledge_only_from_open_and_resolve_from_either_state(pool: PgPool) {
    let project = seed_project(&pool).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let id = open(&pool, project, IncidentSource::Manual { created_by: user }).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();

    assert!(lifecycle::acknowledge(&mut conn, id, user, at(5)).await.unwrap());
    assert!(!lifecycle::acknowledge(&mut conn, id, user, at(6)).await.unwrap(), "already acknowledged");
    assert_eq!(status(&pool, id).await, ("acknowledged".into(), Some(user), None));

    assert!(lifecycle::resolve(&mut conn, id, Some(user), at(7)).await.unwrap());
    assert!(!lifecycle::resolve(&mut conn, id, Some(user), at(8)).await.unwrap(), "already resolved");
    assert!(!lifecycle::acknowledge(&mut conn, id, user, at(9)).await.unwrap(), "resolved is not open");
    assert_eq!(status(&pool, id).await, ("resolved".into(), Some(user), Some(user)));
    assert_eq!(
        events(&pool, id).await,
        [("opened".into(), None), ("acknowledged".into(), Some(user)), ("resolved".into(), Some(user))]
    );
}

#[sqlx::test]
async fn automatic_resolution_has_no_resolver(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let id = open(&pool, project, IncidentSource::Monitor(monitor)).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::resolve_source(&mut conn, &IncidentSource::Monitor(monitor), at(30)).await.unwrap();
    assert_eq!(status(&pool, id).await, ("resolved".into(), None, None));
    assert_eq!(events(&pool, id).await.last().unwrap(), &("resolved".to_string(), None));
}

#[sqlx::test]
async fn reopen_returns_to_open_clears_the_acknowledgement_and_respects_the_source(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let first = open(&pool, project, IncidentSource::Monitor(monitor)).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();

    assert!(matches!(lifecycle::reopen(&mut conn, first, user, at(1)).await.unwrap(), Reopen::NotResolved));
    lifecycle::acknowledge(&mut conn, first, user, at(2)).await.unwrap();
    lifecycle::resolve(&mut conn, first, Some(user), at(3)).await.unwrap();
    assert!(matches!(lifecycle::reopen(&mut conn, first, user, at(4)).await.unwrap(), Reopen::Reopened));
    let (status, ack_by, resolved_by) = status(&pool, first).await;
    assert_eq!((status.as_str(), ack_by, resolved_by), ("open", None, None));
    let resolved_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("select resolved_at from incidents where id = $1").bind(first).fetch_one(&pool).await.unwrap();
    assert_eq!(resolved_at, None);

    // A newer incident of the same monitor blocks reopening the old one.
    lifecycle::resolve(&mut conn, first, None, at(5)).await.unwrap();
    let second = open(&pool, project, IncidentSource::Monitor(monitor)).await.unwrap();
    assert_ne!(first, second);
    assert!(matches!(
        lifecycle::reopen(&mut conn, first, user, at(6)).await.unwrap(),
        Reopen::SourceHasUnresolvedIncident
    ));
    assert!(matches!(lifecycle::reopen(&mut conn, Uuid::new_v4(), user, at(7)).await.unwrap(), Reopen::NotFound));
}

#[sqlx::test]
async fn comments_are_events_with_their_author(pool: PgPool) {
    let project = seed_project(&pool).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let id = open(&pool, project, IncidentSource::Manual { created_by: user }).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::comment(&mut conn, id, user, "looking into it").await.unwrap();
    let (kind, actor, message): (String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "select kind, actor_id, message from incident_events where incident_id = $1 order by id desc limit 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((kind.as_str(), actor, message.as_deref()), ("comment", Some(user), Some("looking into it")));
}

#[sqlx::test]
async fn racing_opens_for_one_source_create_one_incident(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let (a, b) = tokio::join!(
        open(&pool, project, IncidentSource::Monitor(monitor)),
        open(&pool, project, IncidentSource::Monitor(monitor))
    );
    assert_eq!(a.is_some() as u8 + b.is_some() as u8, 1);
    let count: i64 = sqlx::query_scalar("select count(*) from incidents where monitor_id = $1")
        .bind(monitor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
```

Run: engine command `--test incidents incident_lifecycle::`. Expected: FAIL to compile (`incidents::lifecycle` missing).

- [ ] **Step 4: Implement `incidents::lifecycle`**

The open statements, one literal per conflict target. `$5` is the requested severity, kept only if it belongs to the project:

```rust
const OPEN_MONITOR: &str = "insert into incidents
        (project_id, monitor_id, source, title, cause, severity_id, metadata, started_at)
    values ($1, $2, 'monitor', $3, $4,
        coalesce((select id from incident_severities where id = $5 and project_id = $1),
                 (select id from incident_severities where project_id = $1 and is_default)),
        coalesce($6, '{}'::jsonb), $7)
    on conflict (monitor_id) where status <> 'resolved' do nothing
    returning id";
// OPEN_HEARTBEAT: heartbeat_id / 'heartbeat' / `on conflict (heartbeat_id) where status <> 'resolved'`.
// OPEN_LOG_ALERT: log_alert_id / 'log_alert' / `on conflict (log_alert_id) where status <> 'resolved'`.
// OPEN_MANUAL: column created_by = $2, source 'manual', no `on conflict` clause.
```

- `open` runs the statement for the variant. When it returns an id, `open` calls `record_event(.., Opened, None, None, json!({}))`. A manual incident's `opened` event carries `actor_id = created_by`.
- `acknowledge` runs `update incidents set status = 'acknowledged', acknowledged_at = $3, acknowledged_by = $2 where id = $1 and status = 'open'`. On one row it records `Acknowledged` with the actor.
- `resolve` runs `update ... set status = 'resolved', resolved_at = $3, resolved_by = $2 where id = $1 and status <> 'resolved'` and records `Resolved`.
- `resolve_source` selects the source's unresolved id `for update` (by `monitor_id` / `heartbeat_id` / `log_alert_id`), then calls `resolve(.., None, at)`. For `Manual` it returns `Ok(None)`.
- `reopen` locks the row and returns `NotFound` or `NotResolved` as appropriate. Otherwise it runs `update ... set status = 'open', resolved_at = null, resolved_by = null, acknowledged_at = null, acknowledged_by = null where id = $1` inside a savepoint (`conn.begin()` on the connection). A unique violation `23505` rolls the savepoint back and returns `SourceHasUnresolvedIncident`. On success it records `Reopened`.
- `comment` calls `record_event(.., Comment, Some(actor), Some(body), json!({}))` and returns the event id. Length and character checks belong to the route (Task 2).

Then rewrite the wrappers in `src/incidents/model.rs` as calls, for example:

```rust
pub async fn open_for_monitor(conn: &mut PgConnection, project_id: Uuid, monitor_id: Uuid, title: &str,
    cause: Option<&str>, started_at: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::open(conn, &OpenIncident { project_id, source: IncidentSource::Monitor(monitor_id), title, cause,
        severity_id: None, metadata: None, started_at }).await
}
pub async fn resolve_for_monitor(conn: &mut PgConnection, monitor_id: Uuid, resolved_at: DateTime<Utc>)
    -> Result<Option<Uuid>, sqlx::Error> {
    lifecycle::resolve_source(conn, &IncidentSource::Monitor(monitor_id), resolved_at).await
}
```

`open_for_heartbeat` first calls `lifecycle::reopen_source_within(conn, &IncidentSource::Heartbeat(heartbeat_id), HEARTBEAT_REOPEN_WINDOW_SECS, title, cause, started_at)` and calls `lifecycle::open` only when that returns `None` (Drift (2026-10-24) above). Add to `incident_lifecycle.rs`: `a_heartbeat_incident_resolved_moments_ago_is_reopened_with_an_event`, which seeds a heartbeat with a plain SQL insert (`tests/common/mod.rs` has no heartbeat seeder; `seed_heartbeat` is local to `tests/uptime/heartbeats.rs`), resolves its incident, opens again 60 s later through `open_for_heartbeat`, and asserts the same id, status `open`, the new title, `details` null and a last event `("reopened", None)`.

`list_for_project`'s `status` filter keeps working; Task 2 replaces it.

Add to `tests/common/mod.rs`:

```rust
/// Opens a manual incident in `project` (title "test incident") and returns its id.
pub async fn open_test_incident(pool: &sqlx::PgPool, project: Uuid, created_by: Uuid) -> Uuid
```

- [ ] **Step 5: Run the tests**

Run: engine command `--test incidents --test core --test uptime --test logs` (whole targets, no filter). Expected: PASS (the email-based tests are untouched by this task).

- [ ] **Step 6: Run the full engine suite and clippy**

Run: engine command (no args), then `cargo clippy --all-targets -- -D warnings` with the same env. Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/NNNN_incidents_part1.sql nami-engine/src/incidents \
  nami-engine/tests/incidents nami-engine/tests/uptime \
  nami-engine/tests/core/migrations.rs nami-engine/tests/common/mod.rs
git commit -m "feat(engine): incident lifecycle, severities and timeline (NAMI-INC-1)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2 (NAMI-INC-2): Incident API — list, manual create, detail with timeline, ack/resolve/reopen, comments, postmortem, severity

**Files:**
- Create: `nami-engine/src/incidents/severity/mod.rs`, `nami-engine/src/incidents/severity/model.rs`, `nami-engine/src/incidents/severity/routes.rs`
- Modify: `nami-engine/src/incidents/mod.rs` (`pub mod severity;`), `nami-engine/src/incidents/model.rs` (list with cursor, `IncidentDetail`, `IncidentEvent`), `nami-engine/src/incidents/routes.rs`, `nami-engine/src/core/validate.rs`, `nami-engine/src/core/router.rs`, `nami-ui/src/lib/incidents/incidents.ts` (new: types), `nami-ui/src/lib/uptime/uptime.ts` (re-export `Incident` from `incidents.ts` for existing imports), `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/+page.server.ts` (+ its tests: read `.incidents` from the new shape), `nami-ui/src/routes/(app)/projects/[id=uuid]/monitors/[monitorId=uuid]/+page.server.ts` (+ its test: it calls `/incidents?monitor_id=` today and reads a bare array)
- Modify (existing engine tests on the old bare-array list): `tests/uptime/uptime_read_api.rs::incidents_are_newest_first_and_filterable` (read `body["incidents"]`; `?status=open` keeps meaning "status open"; `?monitor_id=` keeps working) and the list assertion in `tests/uptime/heartbeats.rs` (line 470 on `main`, `list[0]["heartbeat_id"]` → `list["incidents"][0]["heartbeat_id"]`). Add the five new write routes to `tests/core/json_bodies.rs`.
- Test: `nami-engine/tests/incidents/incidents_api.rs`, the existing `incidents-page.test.ts` / `incidents.test.ts` updated

**Interfaces:**
- Consumes: Task 1 `lifecycle::*`.
- Produces:

```rust
// incidents::severity::model
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Severity { pub id: Uuid, pub name: String, pub rank: i32, pub is_default: bool }
pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Severity>, sqlx::Error>; // by rank
pub async fn belongs_to<'e>(executor: impl PgExecutor<'e>, project_id: Uuid, severity_id: Uuid) -> Result<bool, sqlx::Error>;

// incidents::model
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IncidentListItem { pub id: Uuid, pub project_id: Uuid, pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>, pub log_alert_id: Option<Uuid>, pub source: String,
    pub source_name: Option<String>, pub title: String, pub cause: Option<String>, pub status: String,
    pub severity: Option<String>, pub severity_rank: Option<i32>, pub started_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>, pub acknowledged_by_email: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// NAMI-UP-21's heartbeat output (R26); text, never markup.
    pub details: Option<String> }
pub struct ListFilter { pub statuses: Vec<String>, pub sources: Vec<String>, pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>, pub log_alert_id: Option<Uuid>, pub before: Option<(DateTime<Utc>, Uuid)>, pub limit: i64 }
pub async fn list(pool: &PgPool, project_id: Uuid, filter: &ListFilter) -> Result<Vec<IncidentListItem>, sqlx::Error>;
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IncidentEvent { pub id: i64, pub kind: String, pub actor_id: Option<Uuid>, pub actor_email: Option<String>,
    pub message: Option<String>, pub metadata: serde_json::Value, pub created_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize)]
pub struct IncidentDetail { #[serde(flatten)] pub item: IncidentListItem, pub severity_id: Option<Uuid>,
    pub metadata: serde_json::Value, pub postmortem: Option<String>, pub resolved_by_email: Option<String>,
    pub created_by_email: Option<String>, pub events: Vec<IncidentEvent> }
pub async fn detail(pool: &PgPool, project_id: Uuid, incident_id: Uuid) -> Result<Option<IncidentDetail>, sqlx::Error>;
```

- Routes (all JSON; writes take a JSON body):

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `GET /projects/{id}/severities` | viewer | — | `200 [Severity]` | |
| `GET /projects/{id}/incidents?status=&source=&monitor_id=&heartbeat_id=&log_alert_id=&limit=&cursor=` | viewer | — | `200 {"incidents", "next_cursor"}` | 400 bad status/source/limit/cursor/id |
| `POST /projects/{id}/incidents` | member | `{title, description?, severity_id?}` | `201 IncidentDetail` | 400 caps, 404 foreign severity |
| `GET /projects/{id}/incidents/{incident_id}` | viewer | — | `200 IncidentDetail` | 404 |
| `POST .../acknowledge` | member | `{}` | `200 IncidentDetail` | 409 `incident is not open` |
| `POST .../resolve` | member | `{}` | `200 IncidentDetail` | 409 `incident is already resolved` |
| `POST .../reopen` | member | `{}` | `200 IncidentDetail` | 409 `incident is not resolved` / `this source already has an unresolved incident` |
| `POST .../comments` | member | `{body}` | `201 IncidentEvent` | 400 |
| `PUT .../postmortem` | member | `{postmortem: string or null}` | `200 IncidentDetail` | 400 > 102,400 bytes |
| `PATCH /projects/{id}/incidents/{incident_id}` | member | `{severity_id}` | `200 IncidentDetail` | 404 foreign severity |

The cursor is opaque: base64url of `<started_at RFC 3339 micros>|<id>`, and a malformed one is a 400. The list orders by `(started_at desc, id desc)` and fetches `limit + 1` rows to decide `next_cursor`. The SQL takes `(cardinality($2::text[]) = 0 or status = any($2))` and the same for `source`, plus `($n::uuid is null or monitor_id = $n)` for each of the three source ids. The monitor page's existing `?monitor_id=` call keeps working. `unresolved` expands to `open` + `acknowledged`. Every incident path checks `incidents.project_id = {id}` (404 otherwise). Request bodies are `#[serde(deny_unknown_fields)]`.

Validation rules:
- title: trimmed, 1-255 chars, no char with `is_unsafe_in_text`.
- description and comment body: ≤ 10,240 bytes; the only control characters allowed are `\n` and `\t`.
- postmortem: ≤ 102,400 bytes, same character rule.

A `severity_changed` event carries `{"from": "<name>", "to": "<name>"}`. `postmortem_updated` carries no text, so the timeline stays small. Detail returns at most the latest 1,000 events, oldest first.

- [ ] **Step 1: Write the failing API tests**

`nami-engine/tests/incidents/incidents_api.rs`:

```rust
// The incident API. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, open_test_incident, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

fn url(project: Uuid, rest: &str) -> String {
    format!("/projects/{project}/incidents{rest}")
}

#[sqlx::test]
async fn members_open_manual_incidents_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, member_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, _) = call(&app, "POST", &url(project, ""), Some(&viewer), Some(json!({"title": "x"}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, created) = call(&app, "POST", &url(project, ""), Some(&member),
        Some(json!({"title": "  Checkout slow  ", "description": "p95 > 2s\nsince 10:00"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["title"], "Checkout slow");
    assert_eq!(created["cause"], "p95 > 2s\nsince 10:00");
    assert_eq!(created["source"], "manual");
    assert_eq!(created["status"], "open");
    assert_eq!(created["severity"], "high");
    assert_eq!(created["created_by_email"], member_email);
    assert_eq!(created["events"][0]["kind"], "opened");
    assert_eq!(created["events"][0]["actor_id"], json!(member_id));

    let (status, listed) = call(&app, "GET", &url(project, ""), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["incidents"][0]["id"], created["id"]);
    assert_eq!(listed["next_cursor"], Value::Null);
}

#[sqlx::test]
async fn outsiders_get_403_and_foreign_ids_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let foreign = open_test_incident(&pool, other, member_id).await;

    let (status, _) = call(&app, "GET", &url(project, ""), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, "GET", &url(project, &format!("/{foreign}")), Some(&member), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST", &url(project, &format!("/{foreign}/acknowledge")), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let foreign_severity: Uuid = sqlx::query_scalar("select id from incident_severities where project_id = $1 limit 1")
        .bind(other).fetch_one(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", &url(project, ""), Some(&member),
        Some(json!({"title": "x", "severity_id": foreign_severity}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn ack_resolve_reopen_follow_the_lifecycle(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, member_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open_test_incident(&pool, project, member_id).await;
    let action = |name: &str| url(project, &format!("/{id}/{name}"));

    let (status, _) = call(&app, "POST", &action("acknowledge"), Some(&viewer), Some(json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&app, "POST", &action("acknowledge"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["status"].as_str(), body["acknowledged_by_email"].as_str()), (Some("acknowledged"), Some(member_email.as_str())));
    let (status, _) = call(&app, "POST", &action("acknowledge"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "POST", &action("reopen"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = call(&app, "POST", &action("resolve"), Some(&member), Some(json!({}))).await;
    assert_eq!((status, body["status"].as_str()), (StatusCode::OK, Some("resolved")));
    let (status, _) = call(&app, "POST", &action("resolve"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = call(&app, "POST", &action("reopen"), Some(&member), Some(json!({}))).await;
    assert_eq!((status, body["status"].as_str()), (StatusCode::OK, Some("open")));
    let kinds: Vec<&str> = body["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["opened", "acknowledged", "resolved", "reopened"]);
}

#[sqlx::test]
async fn comments_postmortems_and_severity_changes_are_capped_and_logged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, member_id).await;

    let (status, event) = call(&app, "POST", &url(project, &format!("/{id}/comments")), Some(&member),
        Some(json!({"body": "rolled back\nwatching"}))).await;
    assert_eq!((status, event["kind"].as_str()), (StatusCode::CREATED, Some("comment")));
    for bad in [json!({"body": ""}), json!({"body": "x".repeat(10_241)}), json!({"body": "a\u{1b}[31m"})] {
        let (status, _) = call(&app, "POST", &url(project, &format!("/{id}/comments")), Some(&member), Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let (status, body) = call(&app, "PUT", &url(project, &format!("/{id}/postmortem")), Some(&member),
        Some(json!({"postmortem": "# Cause\n<script>alert(1)</script>"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["postmortem"], "# Cause\n<script>alert(1)</script>", "stored verbatim; the UI escapes it");
    let (status, _) = call(&app, "PUT", &url(project, &format!("/{id}/postmortem")), Some(&member),
        Some(json!({"postmortem": "x".repeat(102_401)}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let critical: Uuid = sqlx::query_scalar("select id from incident_severities where project_id = $1 and name = 'critical'")
        .bind(project).fetch_one(&pool).await.unwrap();
    let (status, body) = call(&app, "PATCH", &url(project, &format!("/{id}")), Some(&member),
        Some(json!({"severity_id": critical}))).await;
    assert_eq!((status, body["severity"].as_str()), (StatusCode::OK, Some("critical")));
    let last = body["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["kind"], "severity_changed");
    assert_eq!(last["metadata"], json!({"from": "high", "to": "critical"}));
}

#[sqlx::test]
async fn titles_are_one_line_and_capped(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for title in ["", "   ", "a\nb", "a\u{202e}b", &"x".repeat(256)] {
        let (status, _) = call(&app, "POST", &url(project, ""), Some(&member), Some(json!({"title": title}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{title:?}");
    }
    let (status, _) = call(&app, "POST", &url(project, ""), Some(&member), Some(json!({"title": "x", "extra": 1}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown fields are refused");
}

#[sqlx::test]
async fn the_list_filters_by_status_and_source_and_pages_without_gaps(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(open_test_incident(&pool, project, member_id).await);
    }
    // Same started_at for all five: paging must still return each exactly once.
    sqlx::query("update incidents set started_at = '2026-10-19T00:00:00Z' where project_id = $1")
        .bind(project).execute(&pool).await.unwrap();
    call(&app, "POST", &url(project, &format!("/{}/acknowledge", ids[0])), Some(&member), Some(json!({}))).await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor { Some(c) => format!("?limit=2&cursor={c}"), None => "?limit=2".into() };
        let (status, page) = call(&app, "GET", &url(project, &q), Some(&member), None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        seen.extend(page["incidents"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()));
        match page["next_cursor"].as_str() { Some(c) => cursor = Some(c.to_string()), None => break }
    }
    seen.sort();
    let mut expected: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    expected.sort();
    assert_eq!(seen, expected);

    let (_, acked) = call(&app, "GET", &url(project, "?status=acknowledged"), Some(&member), None).await;
    assert_eq!(acked["incidents"].as_array().unwrap().len(), 1);
    let (_, unresolved) = call(&app, "GET", &url(project, "?status=unresolved&source=manual"), Some(&member), None).await;
    assert_eq!(unresolved["incidents"].as_array().unwrap().len(), 5);
    for bad in ["?status=bogus", "?source=x", "?limit=0", "?limit=101", "?cursor=%%%"] {
        let (status, _) = call(&app, "GET", &url(project, bad), Some(&member), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[sqlx::test]
async fn severities_are_listed_by_rank_for_viewers(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (status, body) = call(&app, "GET", &format!("/projects/{project}/severities"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["critical", "high", "low"]);
}
```

If the router answers an unknown field with 400 rather than 422 (check how `Json` rejections are mapped today), assert what it does now. Do not change the global mapping.

Run: engine command `--test incidents incidents_api::`. Expected: FAIL (routes missing).

- [ ] **Step 2: Implement** the models and routes as specified in the Interfaces block. Every write handler runs in one transaction: lock the incident row (`select ... where id = $1 and project_id = $2 for update`), call `lifecycle`, then load the detail after commit.

- [ ] **Step 3: Update the nami-ui loader**

In `nami-ui/src/lib/incidents/incidents.ts`, define `Incident` (mirrors `IncidentListItem`, including `details: string | null` and Logs Part 2's `log_alert_id`), `IncidentDetail`, `IncidentEvent`, `Severity` and `IncidentPage = { incidents: Incident[]; next_cursor: string | null }`. The existing page's `+page.server.ts` keeps its open/resolved toggle and reads `(await response.json()).incidents`, with `status=unresolved` for the "open" tab. Update its two test files to the new response shape. The monitor detail loader (`monitors/[monitorId=uuid]/+page.server.ts`) reads `.incidents` as well, and so does its test.

Run: UI command with `src/routes/(app)/projects/[id=uuid]/incidents "src/routes/(app)/projects/[id=uuid]/monitors"`. Expected: PASS, 0 errors / 0 warnings.

- [ ] **Step 4: Run** engine command `--test incidents --test uptime --test core`, then the full engine suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src nami-engine/tests nami-ui/src/lib/incidents/incidents.ts nami-ui/src/lib/uptime/uptime.ts \
  "nami-ui/src/routes/(app)/projects/[id=uuid]/incidents" "nami-ui/src/routes/(app)/projects/[id=uuid]/monitors"
git commit -m "feat(engine,ui): incident API with timeline, actions, comments and postmortems (NAMI-INC-2)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 3 (NAMI-INC-3): Guarded outbound client and notification channels (personal + project, sealed, write-only)

**Files:**
- Create: `nami-engine/src/incidents/outbound.rs`, `nami-engine/src/incidents/channel/mod.rs`, `nami-engine/src/incidents/channel/model.rs`, `nami-engine/src/incidents/channel/routes.rs`
- Append: `nami-engine/migrations/NNNN_incidents_part1.sql` (channels)
- Modify: `nami-engine/src/uptime/checker/mod.rs` (make `GuardedResolver` and `caused_by` `pub(crate)`; nothing moves, behaviour unchanged; `names_forbidden_address` and `allowed_addresses` are already `pub(crate)`), `nami-engine/src/core/config.rs` (`outbound_policy`, R9), `nami-engine/src/incidents/mod.rs` (`pub mod outbound; pub mod channel;`), `nami-engine/src/core/validate.rs`, `nami-engine/src/core/router.rs`, `nami-engine/Cargo.toml` (`cargo add hmac@0.13.0`, already in the lock file through `hkdf 0.13`, same `digest 0.11` as `sha2 0.11`), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (project and personal channel POST/PATCH)
- Test: `nami-engine/tests/incidents/notification_channels.rs` (+ unit tests in `incidents/outbound.rs` and `incidents/channel/model.rs`)

**Interfaces:**
- Consumes: `core::crypto::Cipher`, `uptime::checker::{is_forbidden_destination, allowed_addresses, names_forbidden_address, GuardedResolver, caused_by, DestinationNotAllowed}`, `core::validate::validate_name`, `core::middleware::rbac::{require_project_role, CurrentUser}`.
- Produces:

```rust
// outbound
pub const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// No proxy, GuardedResolver (the monitors' policy), no redirects, no referer, no idle pool,
/// 10 s total timeout, UA "Nami-Notify/<version>". Built once (`OnceLock`), cloned per call.
/// Only for project webhooks and the Telegram API base (R9).
pub fn client() -> reqwest::Client;
/// The same, with `PublicOnlyResolver`. Every personal channel and every Slack/Discord/Teams send.
pub fn public_client() -> reqwest::Client;
/// `is_forbidden_destination(ip)` or loopback, RFC 1918, CGNAT 100.64/10, 198.18/15, ULA fc00::/7,
/// multicast, broadcast, documentation (192.0.2/24, 198.51.100/24, 203.0.113/24, 2001:db8::/32),
/// judged after unwrapping IPv4-mapped and NAT64 forms.
pub fn is_non_public(ip: IpAddr) -> bool;
/// Like checker::allowed_addresses with `is_non_public`: Err(DestinationNotAllowed) when every
/// resolved address is refused.
pub(crate) fn public_addresses(resolved: impl IntoIterator<Item = SocketAddr>) -> Result<Vec<SocketAddr>, DestinationNotAllowed>;
pub(crate) struct PublicOnlyResolver;   // reqwest::dns::Resolve, as GuardedResolver with public_addresses
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub use crate::core::config::OutboundPolicy;   // enum OutboundPolicy { Strict, Relaxed } (R9, R32)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach { MonitorPolicy, PublicOnly }  // which client and which literal check
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRule { HttpOrHttps, Https, HttpsHosts(&'static [&'static str]), HttpsHostPrefix { host: &'static [&'static str], path_prefix: &'static str } }
/// Parses and checks a destination: scheme, host present, no userinfo, the rule, and no
/// forbidden IP literal (`names_forbidden_address`, plus `is_non_public` for `Reach::PublicOnly`).
/// Always strict: used by create/update validation. The error is a user-facing message.
pub fn check_url(raw: &str, rule: UrlRule, reach: Reach) -> Result<reqwest::Url, String>;
/// The send-time check: `check_url` under `Strict`; under `Relaxed` only HttpOrHttps +
/// `names_forbidden_address` (R9 test seam).
pub fn check_url_for_send(raw: &str, rule: UrlRule, reach: Reach, policy: OutboundPolicy) -> Result<reqwest::Url, String>;
/// "timeout" | "connection failed" | "destination not allowed" | "HTTP <code>" | "request failed".
/// Never contains the URL (uses `without_url`) or a response body.
pub fn describe_error(error: &reqwest::Error) -> String;

// incidents::channel::model
pub const KINDS: [&str; 6] = ["email", "webhook", "slack", "teams", "telegram", "discord"];
pub const PERSONAL_KINDS: [&str; 5] = ["webhook", "slack", "teams", "telegram", "discord"];
pub const MAX_PROJECT_CHANNELS: i64 = 50;
pub const MAX_PERSONAL_CHANNELS: i64 = 10;
pub const CONFIG_CONTEXT: &str = "notification_channels.config";
pub const SIGNING_SECRET_CONTEXT: &str = "notification_channels.signing_secret";
pub fn config_context(id: Uuid) -> String;           // "notification_channels.config:{id}"
pub fn signing_secret_context(id: Uuid) -> String;   // "notification_channels.signing_secret:{id}"

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "config", rename_all = "lowercase")]
pub enum ChannelConfig {
    Email { address: String },
    Webhook { url: String },
    Slack { webhook_url: String },
    Teams { webhook_url: String },
    Telegram { bot_token: String, chat_id: String },
    Discord { webhook_url: String },
}
impl fmt::Debug for ChannelConfig { /* prints the kind only */ }
pub enum Owner { Project(Uuid), User(Uuid) }
impl ChannelConfig {
    pub fn kind(&self) -> &'static str;
    /// R9 rules for this owner (a personal webhook/any Slack/Discord/Teams is `Reach::PublicOnly`);
    /// always strict. The error is a 400 message.
    pub fn validate(&self, owner: &Owner) -> Result<(), String>;
    /// The client and literal policy a send uses (R9).
    pub fn reach(&self, owner: &Owner) -> Reach;
    /// R10 display string, never secret.
    pub fn summary(&self) -> String;
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Channel { pub id: Uuid, pub project_id: Option<Uuid>, pub user_id: Option<Uuid>, pub name: String,
    pub kind: String, pub summary: String, pub enabled: bool, pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
#[derive(Serialize)]
pub struct CreatedChannel { #[serde(flatten)] pub channel: Channel, #[serde(skip_serializing_if = "Option::is_none")] pub signing_secret: Option<String> }

pub enum Inserted { Created(CreatedChannel), Full }
/// Seals config (and a fresh 32-byte base64url signing secret for webhooks). Does NOT run `validate`
/// (routes do), so tests can point Slack/Discord/Teams at wiremock.
pub async fn insert(pool: &PgPool, cipher: &Cipher, owner: Owner, name: &str, config: &ChannelConfig,
    enabled: bool, created_by: Uuid) -> Result<Inserted, sqlx::Error>;
pub async fn list(pool: &PgPool, owner: &Owner) -> Result<Vec<Channel>, sqlx::Error>;
pub async fn find(pool: &PgPool, owner: &Owner, id: Uuid) -> Result<Option<Channel>, sqlx::Error>;
pub struct ChannelPatch { pub name: Option<String>, pub enabled: Option<bool>, pub config: Option<ChannelConfig> }
pub async fn update(pool: &PgPool, cipher: &Cipher, owner: &Owner, id: Uuid, patch: ChannelPatch) -> Result<Option<Channel>, AppError>;
pub enum Deleted { Deleted, NotFound, InUse { policy: String } }
pub async fn delete(pool: &PgPool, owner: &Owner, id: Uuid) -> Result<Deleted, sqlx::Error>;
/// Everything a sender needs, opened. `Err(DecryptError)` when the key changed.
/// No `Debug` derive: a manual impl prints id, kind and owner only.
pub struct OpenedChannel { pub id: Uuid, pub kind: String, pub name: String, pub owner: Owner, pub enabled: bool,
    pub config: ChannelConfig, pub signing_secret: Option<Zeroizing<Vec<u8>>> }
pub async fn open_for_send(pool: &PgPool, cipher: &Cipher, id: Uuid) -> Result<Option<Result<OpenedChannel, DecryptError>>, sqlx::Error>;
```

- Routes:
  - `GET /projects/{id}/notification-channels` (viewer) and `POST` (admin): `{name, kind, config, enabled?}`.
  - `PATCH` (admin): `{name?, enabled?, kind?+config?}`. A config replaces the old one as a whole and must keep its kind; a kind change is a 400.
  - `DELETE /projects/{id}/notification-channels/{channel_id}` (admin): 409 `channel is used by escalation policy "<name>"` once Task 8 exists (until then, always deletable).
  - `GET|POST /account/notification-channels` and `PATCH|DELETE /account/notification-channels/{channel_id}` (any logged-in user, own channels only; another user's id is 404; `email` kind → 400).
  - Create returns `201 CreatedChannel`. The signing secret (webhooks only) appears in that one response and never again. Over the limit → 409 `channel limit reached (50 per project)` / `(10 per user)`.

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_incidents_part1.sql`:

```sql
-- Notification channels (NAMI-INC-3). A channel belongs to a project (a team
-- room, admin-managed) or to one user (personal). Secrets are sealed with
-- core::crypto::Cipher under per-row contexts; `summary` is what the UI may show.
create table notification_channels (
    id uuid primary key,
    project_id uuid references projects(id) on delete cascade,
    user_id uuid references users(id) on delete cascade,
    name text not null check (char_length(name) between 1 and 100),
    kind text not null check (kind in ('email', 'webhook', 'slack', 'teams', 'telegram', 'discord')),
    config_enc bytea not null,
    signing_secret_enc bytea,
    summary text not null,
    enabled boolean not null default true,
    last_tested_at timestamptz,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check ((project_id is null) <> (user_id is null)),
    check ((kind = 'webhook') = (signing_secret_enc is not null)),
    check (user_id is null or kind <> 'email')
);
create index notification_channels_project_idx on notification_channels (project_id) where project_id is not null;
create index notification_channels_user_idx on notification_channels (user_id) where user_id is not null;
```

Migration test `a_channel_belongs_to_exactly_one_owner`: an insert with both owners null and one with both set each fail with `23514`; a personal `email` row fails with `23514`.

- [ ] **Step 2: Write the failing unit tests**

In `outbound.rs` `mod tests`:

```rust
use Reach::{MonitorPolicy, PublicOnly};

#[test]
fn urls_follow_their_rule() {
    assert!(check_url("https://hooks.slack.com/services/T/B/x", SLACK, PublicOnly).is_ok());
    for bad in ["http://hooks.slack.com/services/x", "https://hooks.slack.com.evil.test/x",
                "https://evil.test/hooks.slack.com/", "https://user:pw@hooks.slack.com/x"] {
        assert!(check_url(bad, SLACK, PublicOnly).is_err(), "{bad}");
    }
    assert!(check_url("https://discord.com/api/webhooks/1/abc", DISCORD, PublicOnly).is_ok());
    assert!(check_url("https://discordapp.com/api/webhooks/1/abc", DISCORD, PublicOnly).is_ok());
    assert!(check_url("https://discord.com/other/1/abc", DISCORD, PublicOnly).is_err());
    assert!(check_url("http://10.0.0.5:8080/hook", UrlRule::HttpOrHttps, MonitorPolicy).is_ok(),
        "private receivers are allowed for project webhooks");
    for bad in ["ftp://x.test/", "http://169.254.169.254/latest", "http://[fe80::1]/", "http://0.0.0.0/",
                "http://[::ffff:169.254.169.254]/", "javascript:alert(1)", "http:///nohost"] {
        assert!(check_url(bad, UrlRule::HttpOrHttps, MonitorPolicy).is_err(), "{bad}");
    }
    assert!(check_url("http://teams.example/x", UrlRule::Https, PublicOnly).is_err());
}

#[test]
fn public_only_destinations_refuse_every_internal_address() {
    for bad in ["http://127.0.0.1/", "http://10.0.0.5/", "http://172.16.0.1/", "http://192.168.1.1/",
                "http://100.64.0.1/", "http://[::1]/", "http://[fc00::1]/", "http://[fd12::1]/",
                "http://[::ffff:10.0.0.1]/", "http://[64:ff9b::a00:1]/", "http://224.0.0.1/",
                "http://255.255.255.255/", "http://2130706433/"] {
        assert!(check_url(bad, UrlRule::HttpOrHttps, PublicOnly).is_err(), "{bad}");
    }
    assert!(check_url("https://203.0.114.10/x", UrlRule::HttpOrHttps, PublicOnly).is_ok());
    let loopback = [std::net::SocketAddr::from(([127, 0, 0, 1], 0)), std::net::SocketAddr::from(([10, 1, 2, 3], 0))];
    assert!(public_addresses(loopback).is_err(), "a name resolving only to internal addresses is refused at connect");
    let mixed = [std::net::SocketAddr::from(([10, 1, 2, 3], 0)), std::net::SocketAddr::from(([93, 184, 216, 34], 0))];
    assert_eq!(public_addresses(mixed).unwrap().len(), 1, "only the public address is ever connected to");
}

#[test]
fn the_relaxed_send_check_exists_only_for_tests_and_still_refuses_metadata() {
    let wiremock = "http://127.0.0.1:4567/services/x";
    assert!(check_url_for_send(wiremock, SLACK, PublicOnly, OutboundPolicy::Strict).is_err());
    assert!(check_url_for_send(wiremock, SLACK, PublicOnly, OutboundPolicy::Relaxed).is_ok());
    assert!(check_url_for_send("http://169.254.169.254/", SLACK, PublicOnly, OutboundPolicy::Relaxed).is_err());
    assert_eq!(crate::core::config::Config::from_env_for_test().outbound_policy, OutboundPolicy::Relaxed);
}

#[tokio::test]
async fn the_client_never_follows_redirects() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/"))
        .mount(&server).await;
    let response = client().post(server.uri()).send().await.unwrap();
    assert_eq!(response.status(), 302, "the redirect is returned, not followed");
}

#[test]
fn the_resolver_refuses_names_that_only_resolve_to_forbidden_addresses() {
    // `GuardedResolver` is the checker's, reused in place (not moved); its address filter
    // (`allowed_addresses`) is already unit-tested in checker.rs. This pins that the
    // notification client really uses it: a name resolving only to link-local fails.
    let resolved = [std::net::SocketAddr::from(([169, 254, 169, 254], 0))];
    assert!(crate::uptime::checker::allowed_addresses(resolved).is_err());
}

#[tokio::test]
async fn errors_never_carry_the_url_or_its_secrets() {
    let error = client().post("http://127.0.0.1:9/bot123456:SECRETSECRETSECRETSECRETSECRETSE/sendMessage")
        .send().await.unwrap_err();
    let text = describe_error(&error);
    assert_eq!(text, "connection failed");
    assert!(!text.contains("SECRET"));
}
```

`SLACK` and `DISCORD` are `pub const` rules in `outbound` (also used by `ChannelConfig::validate`):

```rust
pub const SLACK: UrlRule = UrlRule::HttpsHostPrefix { host: &["hooks.slack.com"], path_prefix: "/" };
pub const DISCORD: UrlRule = UrlRule::HttpsHostPrefix {
    host: &["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com"],
    path_prefix: "/api/webhooks/",
};
```

Host comparison is exact and case-insensitive, after the URL parser's normalisation. Prefix comparison is on the parsed path.

In `incidents/channel/model.rs` `mod tests`:

```rust
#[test]
fn telegram_tokens_and_chat_ids_are_shaped() {
    let ok = ChannelConfig::Telegram { bot_token: format!("123456:{}", "A".repeat(35)), chat_id: "-1001234".into() };
    assert!(ok.validate(&Owner::User(Uuid::nil())).is_ok());
    for (token, chat) in [("123456:short", "1"), ("abc:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/../x", "1"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "@ab"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1; drop")] {
        let c = ChannelConfig::Telegram { bot_token: token.into(), chat_id: chat.into() };
        assert!(c.validate(&Owner::User(Uuid::nil())).is_err(), "{token} {chat}");
    }
}

#[test]
fn summaries_and_debug_output_never_show_secrets() {
    let slack = ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/T0/B0/SECRET".into() };
    assert_eq!(slack.summary(), "hooks.slack.com");
    assert!(!format!("{slack:?}").contains("SECRET"));
    let hook = ChannelConfig::Webhook { url: "https://ops.example:8443/in?key=SECRET".into() };
    assert_eq!(hook.summary(), "https://ops.example:8443");
    let tg = ChannelConfig::Telegram { bot_token: format!("1:{}", "S".repeat(35)), chat_id: "42".into() };
    assert_eq!(tg.summary(), "Telegram chat 42");
    assert!(!format!("{tg:?}").contains("SSSS"));
}

#[test]
fn the_contexts_name_the_column_and_the_row() {
    let id = Uuid::nil();
    assert_eq!(config_context(id), format!("notification_channels.config:{id}"));
    assert_eq!(signing_secret_context(id), format!("notification_channels.signing_secret:{id}"));
}
```

- [ ] **Step 3: Write the failing API tests**

`nami-engine/tests/incidents/notification_channels.rs`:

```rust
// Notification channels: personal and project, sealed and write-only.
// Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, seed_project, test_app, test_cipher, user_with_role};
use nami_engine::incidents::channel::model::{config_context, signing_secret_context, ChannelConfig};
use serde_json::{json, Value};
use uuid::Uuid;

const SLACK_URL: &str = "https://hooks.slack.com/services/T000/B000/XXXXSECRETXXXX";

fn assert_no_secrets(body: &Value) {
    let text = body.to_string();
    for needle in ["XXXXSECRETXXXX", "config_enc", "signing_secret_enc", "bot_token", "webhook_url"] {
        assert!(!text.contains(needle), "{needle} leaked: {text}");
    }
}

#[sqlx::test]
async fn admins_create_project_channels_that_are_sealed_and_never_echoed(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let path = format!("/projects/{project}/notification-channels");
    let body = json!({"name": "ops room", "kind": "slack", "config": {"webhook_url": SLACK_URL}});

    let (status, _) = call(&app, "POST", &path, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!((created["kind"].as_str(), created["summary"].as_str()), (Some("slack"), Some("hooks.slack.com")));
    assert_eq!(created["signing_secret"], Value::Null, "only webhooks get a signing secret");
    assert_no_secrets(&created);

    let (status, listed) = call(&app, "GET", &path, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_secrets(&listed);

    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let sealed: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("SECRET"));
    let opened = test_cipher().decrypt(&config_context(id), &sealed).unwrap();
    let config: ChannelConfig = serde_json::from_slice(&opened).unwrap();
    assert_eq!(config, ChannelConfig::Slack { webhook_url: SLACK_URL.into() });
    assert!(test_cipher().decrypt(&config_context(Uuid::new_v4()), &sealed).is_err(), "bound to its row");
}

#[sqlx::test]
async fn webhook_channels_reveal_their_signing_secret_once(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    let (status, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "pager", "kind": "webhook", "config": {"url": "http://10.0.0.5:8080/in?key=abc"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let secret = created["signing_secret"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), 43);
    assert_eq!(created["summary"], "http://10.0.0.5:8080");
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let sealed: Vec<u8> = sqlx::query_scalar("select signing_secret_enc from notification_channels where id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(test_cipher().decrypt(&signing_secret_context(id), &sealed).unwrap(), secret.as_bytes());
    let (_, listed) = call(&app, "GET", &path, Some(&admin), None).await;
    assert!(!listed.to_string().contains(&secret));
}

#[sqlx::test]
async fn destinations_are_validated_per_kind(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    for (kind, config) in [
        ("slack", json!({"webhook_url": "https://evil.test/services/x"})),
        ("discord", json!({"webhook_url": "https://discord.com.evil.test/api/webhooks/1/x"})),
        ("teams", json!({"webhook_url": "http://prod-00.westus.logic.azure.com/workflows/x"})),
        ("webhook", json!({"url": "http://169.254.169.254/latest/meta-data"})),
        ("webhook", json!({"url": "file:///etc/passwd"})),
        ("telegram", json!({"bot_token": "nope", "chat_id": "1"})),
        ("email", json!({"address": "not an address"})),
        ("sms", json!({"to": "+1"})),
    ] {
        let (status, body) = call(&app, "POST", &path, Some(&admin), Some(json!({"name": "x", "kind": kind, "config": config}))).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY, "{kind}: {status} {body}");
    }
}

#[sqlx::test]
async fn personal_channels_belong_to_their_user_only(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, alice) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, bob) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, created) = call(&app, "POST", "/account/notification-channels", Some(&alice),
        Some(json!({"name": "my phone", "kind": "telegram",
                    "config": {"bot_token": format!("123456:{}", "A".repeat(35)), "chat_id": "42"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["summary"], "Telegram chat 42");
    let id = created["id"].as_str().unwrap();

    let (_, mine) = call(&app, "GET", "/account/notification-channels", Some(&alice), None).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    let (_, theirs) = call(&app, "GET", "/account/notification-channels", Some(&bob), None).await;
    assert!(theirs.as_array().unwrap().is_empty());
    let (status, _) = call(&app, "PATCH", &format!("/account/notification-channels/{id}"), Some(&bob), Some(json!({"enabled": false}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &format!("/account/notification-channels/{id}"), Some(&bob), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST", "/account/notification-channels", Some(&alice),
        Some(json!({"name": "mail", "kind": "email", "config": {"address": "a@b.test"}}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no personal email channels (R8)");
    for url in ["http://10.0.0.5:8080/in", "http://127.0.0.1:9/x", "https://[::1]/x"] {
        let (status, body) = call(&app, "POST", "/account/notification-channels", Some(&alice),
            Some(json!({"name": "internal", "kind": "webhook", "config": {"url": url}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "personal webhooks are public-only (R9): {url} {body}");
    }
}

#[sqlx::test]
async fn a_global_viewer_without_projects_manages_only_personal_channels(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let email = common::unique_email("gviewer");
    common::seed_user(&pool, &email, "viewer").await;
    let viewer = common::login(&app, &email).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}/notification-channels"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, "POST", "/account/notification-channels", Some(&viewer),
        Some(json!({"name": "hook", "kind": "webhook", "config": {"url": "https://hooks.example.org/in"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "any logged-in user owns personal channels");
}

#[sqlx::test]
async fn patch_keeps_omitted_config_and_replaces_given_config(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    let (_, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "room", "kind": "slack", "config": {"webhook_url": SLACK_URL}}))).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let before: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();

    let (status, renamed) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin), Some(json!({"name": "war room", "enabled": false}))).await;
    assert_eq!((status, renamed["name"].as_str(), renamed["enabled"].as_bool()), (StatusCode::OK, Some("war room"), Some(false)));
    let after: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after, "a rename never touches the sealed config");

    let (status, _) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"kind": "discord", "config": {"webhook_url": "https://discord.com/api/webhooks/1/x"}}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the kind cannot change");
    let (status, _) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"kind": "slack", "config": {"webhook_url": "https://hooks.slack.com/services/T/B/NEW"}}))).await;
    assert_eq!(status, StatusCode::OK);
    let replaced: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_ne!(before, replaced);
}
```

Run: engine command `--test incidents notification_channels::`, then engine command `--lib -- incidents::outbound incidents::channel::model`. Expected: FAIL.

- [ ] **Step 4: Implement** `incidents/outbound.rs`, `incidents/channel/{mod,model,routes}.rs` as specified.
- `outbound::client()`: a `static CLIENT: OnceLock<reqwest::Client>` holding `reqwest::Client::builder().no_proxy().dns_resolver(GuardedResolver { ip_version: IpVersion::Any }).redirect(Policy::none()).referer(false).timeout(OUTBOUND_TIMEOUT).connect_timeout(CONNECT_TIMEOUT).pool_max_idle_per_host(0).user_agent(concat!("Nami-Notify/", env!("CARGO_PKG_VERSION")))`. `public_client()` is the same with `dns_resolver(PublicOnlyResolver)`. Both are built once, because building a client loads the TLS roots.
- **Drift (2026-10-24):** `GuardedResolver` is `struct GuardedResolver { ip_version: IpVersion }` (`src/uptime/checker/mod.rs:235-237`), not a unit struct, and drops addresses of the excluded family first (`keep_family`, `:100`). Make the struct and its field `pub(crate)` (or add a `pub(crate) fn any_family()` constructor) and build it with `IpVersion::Any` (`src/uptime/monitor/model.rs:60`), which keeps both families. `PublicOnlyResolver` needs no family field. `Config.outbound_policy` makes `core::config` import `incidents::outbound::OutboundPolicy`, the one place `core` names a domain type; moving the enum into `core::config` is the alternative if review prefers it.
- `PublicOnlyResolver::resolve` mirrors `GuardedResolver::resolve` with `public_addresses`. reqwest connects only to the addresses the resolver returns, and with no idle pool every request resolves again. That is the DNS-rebind defence: the address is judged where it is connected to, not at validation time.
- `describe_error` order: `is_timeout` → "timeout"; the error chain contains `DestinationNotAllowed` (`checker::caused_by`) → "destination not allowed"; `is_connect` → "connection failed"; `status()` → `HTTP {code}`; else "request failed". It never formats the error itself (`%e` would print the URL, which holds the Telegram bot token and webhook keys).
- `ChannelConfig::validate(owner)` uses `check_url` with `self.reach(owner)` (`SLACK`, `DISCORD`, `UrlRule::Https` for Teams, `UrlRule::HttpOrHttps` for webhooks). A personal webhook is `Reach::PublicOnly`, a project webhook `Reach::MonitorPolicy`, and Slack/Discord/Teams are always `PublicOnly`. It also uses the Telegram regexes from R9, an email check via `lettre::Address::from_str` (a bare address only, no display name), and lengths (URL ≤ 2,048 bytes, address ≤ 254).
- Routes refuse a `kind` not in the list with 400 `unknown channel kind`.
- Every config is serialised to JSON, then `cipher.encrypt(&config_context(id), ..)`. The request body structs (`CreateChannelRequest`, `PatchChannelRequest`) get manual `Debug` impls that print the kind only (Global Constraints).
- `Config.outbound_policy`: `from_env_defaults_only` sets `Strict`. `from_env_for_test` returns `Config { outbound_policy: OutboundPolicy::Relaxed, ..Self::from_env_defaults_only() }`. No environment variable reads it.

- [ ] **Step 5: Run** engine command `--test incidents --test core --test uptime --test uptime_checker_proxy`, then engine command `--lib -- incidents::outbound incidents::channel::model uptime::checker`, then the full suite and clippy. Expected: green (the checker tests prove the visibility change altered nothing).

- [ ] **Step 6: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/migrations/NNNN_incidents_part1.sql \
  nami-engine/src nami-engine/tests/incidents/notification_channels.rs nami-engine/tests/core/migrations.rs
git commit -m "feat(engine): guarded outbound client and sealed notification channels (NAMI-INC-3)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4 (NAMI-INC-4): Notification formatting with per-channel escaping and webhook signatures

**Files:**
- Create: `nami-engine/src/incidents/format.rs`
- Modify: `nami-engine/src/incidents/mod.rs`
- Test: unit tests in `format.rs`

**Interfaces:**
- Consumes: `uptime::checker::is_unsafe_in_text`, `uptime::heartbeat::sanitize_output` (NAMI-UP-21), `hmac 0.13` (added in Task 3), `sha2`.
- Produces (`nami_engine::incidents::format`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent { Triggered, Resolved, Test }
impl NotifyEvent { pub fn webhook_name(self) -> &'static str }  // "incident.triggered" | "incident.resolved" | "test"

#[derive(Debug, Clone)]
pub struct IncidentSummary { pub id: Uuid, pub title: String, pub cause: Option<String>, pub status: String,
    pub source: String, pub severity: String, pub started_at: DateTime<Utc>,
    /// `incidents.details` (heartbeat output, R26): email only.
    pub details: Option<String>,
    /// R30: the latest `resolved` event's reason ("alert disabled"), `Resolved` messages only.
    pub resolution_reason: Option<String> }
#[derive(Debug, Clone)]
pub struct ActionLinks { pub acknowledge: String, pub resolve: String }
#[derive(Debug, Clone)]
pub struct Message { pub event: NotifyEvent, pub incident: IncidentSummary, pub project_id: Uuid,
    pub project_name: String, pub incident_url: String, pub actions: Option<ActionLinks> }

pub fn email(m: &Message) -> (String, String);             // (subject, plain-text body)
pub fn slack(m: &Message) -> serde_json::Value;
pub fn discord(m: &Message) -> serde_json::Value;
pub fn telegram(m: &Message, chat_id: &str) -> serde_json::Value;
pub fn teams(m: &Message) -> serde_json::Value;
pub fn webhook_body(m: &Message) -> Vec<u8>;
/// R29: "sha256=" + lowercase hex HMAC-SHA256(secret, "{timestamp}." + body).
pub fn signature(secret: &[u8], timestamp: i64, body: &[u8]) -> String;
/// Lowercase hex HMAC-SHA256(secret, bytes); `signature` is built on it.
pub fn hmac_hex(secret: &[u8], bytes: &[u8]) -> String;
pub fn escape_slack(s: &str) -> String;
pub fn escape_discord(s: &str) -> String;
pub fn escape_telegram(s: &str) -> String;
pub fn escape_teams(s: &str) -> String;
pub const MAX_CHAT_CAUSE_CHARS: usize = 1000;
```

Content rules:
- **Every user text is first made one line.** Title and project name go through `is_unsafe_in_text` → space. The cause keeps `\n` (shown as a block) but loses every other unsafe char, and is cut to 1,000 chars with `…` for chat (the email keeps 10 KiB).
- **Headline.** Triggered: `[<severity>] <title>`. Resolved: `Resolved: <title>`. Test: `Test notification from Nami`.
- **Resolution reason (R30).** When `resolution_reason` is set on a `Resolved` message: the email body adds `Reason: <reason>` right after `Status:`. Slack, Discord, Telegram and Teams append ` · Reason: <reason>` (escaped per channel) to their second line. The webhook body is unchanged (the spec fixes it).
- **Email.** Subject `[Nami] [<severity>] <title>` / `[Nami] Resolved: <title>` / `[Nami] Test notification`. Body:

  ```
  <headline>

  Project: <project>
  Severity: <severity>
  Status: <status>
  Started: <%Y-%m-%d %H:%M:%S UTC>
  Cause: <cause or "unknown">

  View: <incident_url>
  ```

  When `actions` is set, the body adds `Acknowledge: <url>` and `Resolve: <url>` lines after `View:`, with a blank line before them.
  - **No forged lines.** Every cause line after the first is indented by two spaces (`Cause: a\n  b`). User text can then never start a line of its own, so a cause cannot fake a `View:`, `Acknowledge:` or `Resolve:` line pointing elsewhere.
  - **Output block (R26).** When `details` is non-empty, `Output:\n` and then each line of `sanitize_output(details.as_bytes())` as `> {line}\n` follow the `Cause:` line, before the blank line and `View:`. The `> ` prefix is exactly NAMI-UP-21's, so its integration test's `Output:\n> …\n` assertion keeps holding.
  - The body is `SinglePart::plain` (the existing `Mailer::send`), so markup is inert. The subject is one line, and lettre encodes it.
- **Slack.** `{"text": <escaped headline>, "unfurl_links": false, "unfurl_media": false, "blocks": [section mrkdwn "*<escaped headline>*\n<escaped project> · <escaped severity> · <status>", section mrkdwn "<escaped cause>" (when any), section mrkdwn "<url|View incident>"]}`. `escape_slack` replaces `&`→`&amp;`, `<`→`&lt;`, `>`→`&gt;` (so `<!channel>`, `<!here>`, `<@U…>`, `<#C…>` and `<http://x|y>` become inert). The only `<url|…>` is built from `incident_url`, which is ids only, under the operator's `UI_BASE_URL`.
- **Discord.** `{"content": "<escaped headline>\n<escaped project> · <severity> · <status>\n<escaped cause>\n<incident_url>", "allowed_mentions": {"parse": []}}`, cut to 2,000 chars. `escape_discord` puts a backslash before `\ * _ ~ ` ` | > # - [ ] ( ) @ :` and before `<`.
- **Telegram.** `{"chat_id": .., "text": "<b>headline</b>\n...\n<a href=\"<incident_url>\">View incident</a>", "parse_mode": "HTML", "disable_web_page_preview": true}`. `escape_telegram` handles `&`, `<`, `>` (and `"` for the attribute). The text is cut to 4,096 chars before the link line is appended.
- **Teams.** `{"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {"$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "type": "AdaptiveCard", "version": "1.4", "body": [TextBlock headline (weight bolder, wrap), TextBlock "<project> · <severity> · <status>", TextBlock cause (wrap)], "actions": [{"type": "Action.OpenUrl", "title": "View incident", "url": incident_url}]}}]}`. `escape_teams` puts a backslash before each of `\ * _ [ ] ( ) ` # > - !` and replaces `&`, `<`, `>` with entities.
- **Webhook.** `{"event": .., "incident": {"id", "title", "cause", "status", "source", "severity", "started_at", "url"}, "project": {"id", "name"}}`, serialised once. `signature(secret, timestamp, body)` is computed over `{timestamp}.` followed by exactly those bytes (R29).

- [ ] **Step 1: Write the failing tests** in `format.rs` `mod tests`:

```rust
use super::*;

fn message(title: &str, cause: Option<&str>) -> Message {
    Message {
        event: NotifyEvent::Triggered,
        incident: IncidentSummary { id: Uuid::nil(), title: title.into(), cause: cause.map(Into::into),
            status: "open".into(), source: "monitor".into(), severity: "critical".into(),
            started_at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(), details: None,
            resolution_reason: None },
        project_id: Uuid::nil(), project_name: "Shop".into(),
        incident_url: format!("http://ui.test/projects/{0}/incidents/{0}", Uuid::nil()),
        actions: None,
    }
}

const HOSTILE: &str = "<!channel> <@U123> @everyone @here <script>alert(1)</script> *bold* [x](http://evil.test) <at>ceo</at> &amp;";

#[test]
fn email_subject_and_body_carry_severity_and_links_on_single_lines() {
    let mut m = message("API\r\nBcc: x@evil.test", Some("refused"));
    m.actions = Some(ActionLinks { acknowledge: "http://ui.test/incidents/action?token=a".into(),
                                   resolve: "http://ui.test/incidents/action?token=r".into() });
    let (subject, body) = email(&m);
    assert_eq!(subject, "[Nami] [critical] API  Bcc: x@evil.test");
    assert!(body.starts_with("[critical] API  Bcc: x@evil.test\n\nProject: Shop\nSeverity: critical\n"), "{body}");
    assert!(body.contains("Started: 2026-10-14 17:46:40 UTC\nCause: refused\n"), "{body}");
    assert!(body.contains("\nAcknowledge: http://ui.test/incidents/action?token=a\nResolve: http://ui.test/incidents/action?token=r"), "{body}");
    let resolved = Message { event: NotifyEvent::Resolved, ..message("API", None) };
    assert_eq!(email(&resolved).0, "[Nami] Resolved: API");
}

#[test]
fn a_cause_cannot_forge_a_link_line_and_job_output_is_quoted() {
    let mut m = message("Nightly backup reported a failure", Some("boom\nAcknowledge: http://evil.test/x\r\nView: http://evil.test"));
    m.incident.details = Some("<script>alert(1)</script>\nBcc: x@evil.test\n\u{1b}[31mdisk full".into());
    let (_, body) = email(&m);
    for line in body.lines() {
        assert!(!line.starts_with("Acknowledge: http://evil") && !line.starts_with("View: http://evil"), "{body}");
    }
    assert!(body.contains("Cause: boom\n  Acknowledge: http://evil.test/x"), "{body}");
    assert!(body.contains("Output:\n> <script>alert(1)</script>\n> Bcc: x@evil.test\n> [31mdisk full\n"), "{body}");
    assert!(body.find("Output:").unwrap() < body.find("View: ").unwrap(), "{body}");
}

#[test]
fn slack_text_cannot_mention_or_link() {
    let v = slack(&message(HOSTILE, Some(HOSTILE)));
    let text = v.to_string();
    for bad in ["<!channel>", "<@U123>", "<script>", "<http://evil"] {
        assert!(!text.contains(bad), "{bad} in {text}");
    }
    assert!(text.contains("&lt;!channel&gt;"));
    assert!(text.contains("&amp;amp;"));
    let links: Vec<_> = text.match_indices("<http").collect();
    assert_eq!(links.len(), 1, "only the incident link: {text}");
}

#[test]
fn discord_never_parses_mentions_and_escapes_markdown() {
    let v = discord(&message(HOSTILE, None));
    assert_eq!(v["allowed_mentions"], serde_json::json!({"parse": []}));
    let content = v["content"].as_str().unwrap();
    assert!(content.contains("\\*bold\\*"), "{content}");
    assert!(content.contains("\\[x\\]\\(http\\:"), "{content}");
    assert!(content.contains("\\@everyone"), "{content}");
    assert!(content.chars().count() <= 2000);
    let long = discord(&message(&"x".repeat(300), Some(&"y".repeat(5000))));
    assert!(long["content"].as_str().unwrap().chars().count() <= 2000);
}

#[test]
fn telegram_uses_html_with_everything_escaped() {
    let v = telegram(&message(HOSTILE, Some("a < b && c > d")), "-100");
    assert_eq!(v["parse_mode"], "HTML");
    assert_eq!(v["chat_id"], "-100");
    assert_eq!(v["disable_web_page_preview"], true);
    let text = v["text"].as_str().unwrap();
    assert!(!text.contains("<script>") && !text.contains("<at>"), "{text}");
    assert!(text.contains("a &lt; b &amp;&amp; c &gt; d"), "{text}");
    assert_eq!(text.matches("<a href=").count(), 1);
}

#[test]
fn teams_is_an_adaptive_card_whose_only_link_is_the_incident() {
    let v = teams(&message(HOSTILE, Some(HOSTILE)));
    let card = &v["attachments"][0]["content"];
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(card["version"], "1.4");
    let blocks = card["body"].as_array().unwrap();
    assert!(blocks.iter().all(|b| b["type"] == "TextBlock"));
    let texts: String = blocks.iter().map(|b| b["text"].as_str().unwrap()).collect();
    for bad in ["<at>", "<script>", "[x](", "*bold*"] {
        assert!(!texts.contains(bad), "{bad} in {texts}");
    }
    assert!(texts.contains("\\[x\\]\\(http"));
    let actions = card["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["type"], "Action.OpenUrl");
    assert_eq!(actions[0]["url"], format!("http://ui.test/projects/{0}/incidents/{0}", Uuid::nil()));
}

#[test]
fn every_escaper_handles_each_control_character() {
    assert_eq!(escape_slack("a&<>b"), "a&amp;&lt;&gt;b");
    assert_eq!(escape_telegram("a&<>\"b"), "a&amp;&lt;&gt;&quot;b");
    assert_eq!(escape_teams("*_[]()`#>-!&<>\\"), "\\*\\_\\[\\]\\(\\)\\`\\#\\>\\-\\!&amp;&lt;&gt;\\\\");
    assert_eq!(escape_discord("@here *x*"), "\\@here \\*x\\*");
}

#[test]
fn webhook_bodies_are_signed_with_hmac_sha256_over_the_exact_bytes() {
    let body = webhook_body(&message("API down", Some("refused")));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["event"], "incident.triggered");
    assert_eq!(v["incident"]["severity"], "critical");
    assert_eq!(v["project"]["name"], "Shop");
    assert_eq!(v.as_object().unwrap().len(), 3);
    // RFC 4231 test case 2.
    assert_eq!(hmac_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    // R29: the signed bytes are "{timestamp}.{body}", never the body alone.
    let mut signed = b"1792000000.".to_vec();
    signed.extend_from_slice(&body);
    assert_eq!(signature(b"k", 1_792_000_000, &body), format!("sha256={}", hmac_hex(b"k", &signed)));
    assert_ne!(signature(b"k", 1_792_000_000, &body), signature(b"k", 1_792_000_001, &body), "the timestamp is signed");
    assert_ne!(signature(b"k", 1_792_000_000, &body), format!("sha256={}", hmac_hex(b"k", &body)));
}

#[test]
fn a_resolution_reason_reaches_every_format_escaped() {
    let mut m = Message { event: NotifyEvent::Resolved, ..message("Log alert: Checkout errors", None) };
    m.incident.resolution_reason = Some("alert disabled".into());
    let (subject, body) = email(&m);
    assert_eq!(subject, "[Nami] Resolved: Log alert: Checkout errors");
    assert!(body.contains("Status: open\nReason: alert disabled\n"), "{body}");
    assert!(slack(&m).to_string().contains("Reason: alert disabled"));
    assert!(discord(&m)["content"].as_str().unwrap().contains("Reason: alert disabled"));
    assert!(telegram(&m, "1")["text"].as_str().unwrap().contains("Reason: alert disabled"));
    assert!(teams(&m).to_string().contains("Reason: alert disabled"));
    let v: serde_json::Value = serde_json::from_slice(&webhook_body(&m)).unwrap();
    assert!(v["incident"].get("reason").is_none(), "the webhook body stays as the spec fixes it");
}
```

Run: engine command `--lib incidents::format`. Expected: FAIL to compile.

- [ ] **Step 2: Implement** `format.rs` to the rules above. `hmac_hex` uses `Hmac::<Sha256>::new_from_slice(secret)`, `update(bytes)`, then `hex::encode(finalize().into_bytes())`. `signature` feeds `timestamp.to_string()`, `b"."` and the body into one MAC (no intermediate copy needed). The same commit updates the webhook bullet of `docs/superpowers/specs/2026-10-13-incident-management-design.md` to R29's format and receiver guidance, noting the controller ruling.

- [ ] **Step 3: Run** engine command `--lib incidents::format`, then clippy. Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src/incidents
git commit -m "feat(engine): per-channel notification formatting with escaping and signatures (NAMI-INC-4)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 5 (NAMI-INC-5): Durable delivery queue, senders per channel, "send test", and the incidents worker loop

**Files:**
- Append: `nami-engine/migrations/NNNN_incidents_part1.sql` (deliveries; references Task 3's channels)
- Create: `nami-engine/src/incidents/send.rs`, `nami-engine/src/incidents/deliveries.rs`, `nami-engine/src/incidents/worker.rs`
- Modify: `nami-engine/src/incidents/mod.rs`, `nami-engine/src/core/config.rs` (`telegram_api_base_url`, env `TELEGRAM_API_BASE_URL`, default `https://api.telegram.org`, must be an absolute http(s) URL with no path, query or userinfo; and `incidents_worker_enabled`, env `INCIDENTS_WORKER_ENABLED`, `env_flag(.., true)`, R23), `nami-engine/.env.example` (both variables, documented like `UPTIME_WORKER_ENABLED` and `RDAP_BASE_URL`), `nami-engine/src/incidents/channel/routes.rs` (the two `/test` routes), `nami-engine/tests/common/mod.rs` (`run_incident_ticks`, `insert_channel`), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (the two `/test` routes). `main.rs` is **not** touched here: Task 10 spawns the worker (R27).
- Test: `nami-engine/tests/incidents/notification_deliveries.rs`

**Interfaces:**
- Consumes: Task 3 `outbound::{client, public_client, check_url_for_send, describe_error, OutboundPolicy}`, `incidents::channel::model::{open_for_send, OpenedChannel, ChannelConfig}`. Task 4 `format::*`. Task 1 `lifecycle::{record_event, EventKind}`. `core::email::Mailer::send` (`async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError>`; it logs the SMTP failure kind itself, never the address).
- Produces:

```rust
// incidents::send
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError { pub message: String, pub retry: Retry }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry { Never, After(Option<Duration>) }   // After(Some(d)) = Retry-After from a 429
pub struct Senders { pub telegram_base: String, pub policy: OutboundPolicy }
impl Senders { pub fn from_config(config: &Config) -> Senders }  // read per kick, so tests can change config
/// One request (or one email). Re-validates the destination (R9, `check_url_for_send` with the
/// channel's `reach` and `policy`) and picks `client()` or `public_client()` by `reach` before sending.
pub async fn send_to_channel(s: &Senders, mailer: &Mailer, channel: &OpenedChannel, message: &format::Message,
    delivery_id: Uuid) -> Result<(), SendError>;
pub async fn send_email(mailer: &Mailer, to: &str, message: &format::Message) -> Result<(), SendError>;

// incidents::deliveries
pub const MAX_ATTEMPTS: i32 = 5;
pub const LEASE: Duration = Duration::from_secs(60);
pub const CHANNEL_PER_MINUTE: i64 = 30;
pub const EMAIL_PER_USER_PER_MINUTE: i64 = 20;
pub const DRAIN_BATCH: i64 = 50;
pub const MAX_SENDS_IN_FLIGHT: usize = 10;
/// 30 s × 4^(attempt-1), capped at 1 h (attempt is 1-based, the one that just failed).
pub fn backoff(attempt: i32) -> Duration;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryEvent { Triggered, Resolved }
pub struct NewDelivery { pub incident_id: Uuid, pub event: DeliveryEvent, pub round: String,
    pub target: Target }
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target { Email { user_id: Uuid }, Channel { channel_id: Uuid, user_id: Option<Uuid> } }
impl Target { pub fn key(&self) -> String }   // "email:{user}" | "channel:{channel}"
/// Inserts, ignoring a duplicate (incident, round, target). Returns how many rows were new.
pub async fn enqueue(conn: &mut PgConnection, deliveries: &[NewDelivery]) -> Result<u64, sqlx::Error>;
/// Claims due deliveries and sends them, at most MAX_SENDS_IN_FLIGHT at once. Returns how many were attempted.
pub async fn drain(state: &AppState, senders: &Senders) -> Result<usize, AppError>;

// incidents::worker
pub const TICK: Duration = Duration::from_secs(2);
pub const MAX_CONCURRENT_KICKS: usize = 2;
pub async fn run_forever(state: AppState);
/// One drain now (Task 9 adds one escalation tick before it). Used by notify_transition (Task 10) and tests.
/// Returns at once when `!state.config.incidents_worker_enabled`. Otherwise waits for one of the
/// MAX_CONCURRENT_KICKS permits of a process-wide `static KICKS: Semaphore` (R17), then runs.
pub async fn kick(state: &AppState);
```

- Routes:
  - `POST /projects/{id}/notification-channels/{channel_id}/test` (admin) and `POST /account/notification-channels/{channel_id}/test` (owner), body `{}`.
  - Each sends a `NotifyEvent::Test` message synchronously (not queued) and answers `200 {"ok": true}`, or `502 {"error": "<describe_error text>"}` on failure.
  - At most one test per channel per 10 s → 429 `wait a few seconds before testing again`. The limit is enforced atomically on `last_tested_at`: `update notification_channels set last_tested_at = now() where id = $1 and (last_tested_at is null or last_tested_at < now() - interval '10 seconds') returning id`.
  - A test sends to a disabled channel too (that is how it is checked before enabling).

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_incidents_part1.sql`:

```sql
-- The durable notification queue (NAMI-INC-5). One row per (incident,
-- round, target): `round` names the escalation pass and step that queued it
-- (e.g. 'p0r0s1', see incidents::escalation) or 'resolved', so re-running a
-- step never notifies the same target twice and a sent row is never resent.
create table notification_deliveries (
    id uuid primary key default gen_random_uuid(),
    incident_id uuid not null references incidents(id) on delete cascade,
    event text not null check (event in ('triggered', 'resolved')),
    round text not null check (char_length(round) between 1 and 32),
    target_key text not null,
    user_id uuid references users(id) on delete cascade,
    channel_id uuid references notification_channels(id) on delete cascade,
    status text not null default 'pending'
        check (status in ('pending', 'sending', 'sent', 'failed', 'cancelled')),
    attempts int not null default 0 check (attempts between 0 and 5),
    next_attempt_at timestamptz not null default now(),
    locked_until timestamptz,
    last_error text check (char_length(last_error) <= 200),
    created_at timestamptz not null default now(),
    sent_at timestamptz,
    unique (incident_id, round, target_key),
    check (user_id is not null or channel_id is not null),
    check (status <> 'sent' or sent_at is not null)
);
create index notification_deliveries_due_idx on notification_deliveries (next_attempt_at)
    where status in ('pending', 'sending');
-- Rate caps look at recent sends per channel and per user.
create index notification_deliveries_channel_sent_idx on notification_deliveries (channel_id, sent_at)
    where channel_id is not null and status = 'sent';
create index notification_deliveries_email_sent_idx on notification_deliveries (user_id, sent_at)
    where channel_id is null and status = 'sent';
```

Migration test `a_target_gets_one_delivery_per_incident_round`: a second insert with the same `(incident_id, round, target_key)` fails with `23505`. Add `"notification_deliveries"` (and Task 3's `"notification_channels"`) to `all_tables_exist`.

- [ ] **Step 2: Write the failing tests**

`deliveries.rs` `mod tests`:

```rust
#[test]
fn backoff_is_30s_times_4_to_the_attempt_capped_at_an_hour() {
    let secs: Vec<u64> = (1..=6).map(|a| backoff(a).as_secs()).collect();
    assert_eq!(secs, [30, 120, 480, 1920, 3600, 3600]);
}
```

`nami-engine/tests/incidents/notification_deliveries.rs`:

```rust
// The notification queue and its senders. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{call, insert_channel, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app,
    user_with_role};
use nami_engine::{
    incidents::deliveries::{self, DeliveryEvent, NewDelivery, Target},
    incidents::channel::model::ChannelConfig,
    core::state::AppState,
};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::{matchers::{header_exists, method, path}, Mock, MockServer, ResponseTemplate};

async fn queue(pool: &sqlx::PgPool, incident: Uuid, round: &str, target: Target) -> u64 {
    let mut conn = pool.acquire().await.unwrap();
    deliveries::enqueue(&mut conn, &[NewDelivery { incident_id: incident, event: DeliveryEvent::Triggered,
        round: round.into(), target }]).await.unwrap()
}

// From Task 9 on, `kick` also runs the incident's default policy, which queues
// emails of its own (round "g0p0r0s0"). These helpers look only at the rows and
// events this file queues itself (round "p0r0s0", channel messages).
async fn delivery(pool: &sqlx::PgPool, incident: Uuid) -> (String, i32, Option<String>) {
    sqlx::query_as("select status, attempts, last_error from notification_deliveries where incident_id = $1 and round = 'p0r0s0'")
        .bind(incident).fetch_one(pool).await.unwrap()
}

async fn timeline(pool: &sqlx::PgPool, incident: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as("select kind, message from incident_events where incident_id = $1 and kind like 'notification_%'
                    and message not like 'Email:%' order by id")
        .bind(incident).fetch_all(pool).await.unwrap()
}

/// Makes every pending/sending delivery due now (skips the backoff wait).
async fn make_due(pool: &sqlx::PgPool) {
    sqlx::query("update notification_deliveries set next_attempt_at = now(), locked_until = null
                 where status in ('pending', 'sending')").execute(pool).await.unwrap();
}

#[sqlx::test]
async fn a_slack_delivery_is_sent_once_and_recorded(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let app = test_app(pool.clone());
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).and(path("/services/T/B/x")).respond_with(ResponseTemplate::new(200))
        .expect(1).mount(&slack).await;
    let channel = insert_channel(&pool, Some(project), None, "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/T/B/x", slack.uri()) }).await;

    assert_eq!(queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await, 1);
    assert_eq!(queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await, 0, "deduplicated");
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("sent".into(), 1, None));
    assert_eq!(timeline(&pool, incident).await, [("notification_sent".into(), Some("Slack: room".into()))]);
    let body: Value = serde_json::from_slice(&slack.received_requests().await.unwrap()[0].body).unwrap();
    assert!(body["text"].as_str().unwrap().contains("[high] test incident"));
}

#[sqlx::test]
async fn a_failing_receiver_is_retried_with_backoff_then_marked_failed(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(503).set_body_string("token=SECRET"))
        .mount(&hook).await;
    let channel = insert_channel(&pool, Some(project), None, "hook",
        ChannelConfig::Webhook { url: format!("{}/in?key=SECRET", hook.uri()) }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;

    run_incident_ticks(&state).await;
    let (status, attempts, error) = delivery(&pool, incident).await;
    assert_eq!((status.as_str(), attempts, error.as_deref()), ("pending", 1, Some("HTTP 503")));
    let wait: f64 = sqlx::query_scalar("select extract(epoch from next_attempt_at - now())::float8 from notification_deliveries where round = 'p0r0s0'")
        .fetch_one(&pool).await.unwrap();
    assert!((25.0..=31.0).contains(&wait), "first retry after ~30 s, got {wait}");
    for _ in 0..4 {
        make_due(&pool).await;
        run_incident_ticks(&state).await;
    }
    assert_eq!(delivery(&pool, incident).await.0, "failed");
    assert_eq!(hook.received_requests().await.unwrap().len(), 5);
    let events = timeline(&pool, incident).await;
    assert_eq!(events.len(), 5, "one event per attempt");
    for (_, message) in &events {
        let message = message.as_deref().unwrap();
        assert!(!message.contains("SECRET") && !message.contains("127.0.0.1"), "{message}");
    }
}

#[sqlx::test]
async fn a_rejected_request_is_not_retried(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let discord = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(404)).mount(&discord).await;
    let channel = insert_channel(&pool, Some(project), None, "d",
        ChannelConfig::Discord { webhook_url: format!("{}/api/webhooks/1/x", discord.uri()) }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("failed".into(), 1, Some("HTTP 404".into())));
}

#[sqlx::test]
async fn webhooks_are_signed_with_a_timestamp_and_carry_the_delivery_id(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST")).and(header_exists("x-nami-signature")).and(header_exists("x-nami-timestamp"))
        .and(header_exists("x-nami-delivery"))
        .respond_with(ResponseTemplate::new(204)).expect(1).mount(&hook).await;
    let channel = insert_channel(&pool, Some(project), None, "hook", ChannelConfig::Webhook { url: hook.uri() }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;

    let request = &hook.received_requests().await.unwrap()[0];
    let sealed: Vec<u8> = sqlx::query_scalar("select signing_secret_enc from notification_channels where id = $1")
        .bind(channel).fetch_one(&pool).await.unwrap();
    let secret = state.cipher.decrypt(&nami_engine::incidents::channel::model::signing_secret_context(channel), &sealed).unwrap();
    let timestamp: i64 = request.headers.get("x-nami-timestamp").unwrap().to_str().unwrap().parse().unwrap();
    assert!((chrono::Utc::now().timestamp() - timestamp).abs() <= 60, "a fresh unix-seconds timestamp");
    let expected = nami_engine::incidents::format::signature(&secret, timestamp, &request.body);
    assert_eq!(request.headers.get("x-nami-signature").unwrap().to_str().unwrap(), expected);
    // What a receiver does (R29): HMAC over "{timestamp}.{raw body}".
    let mut signed = format!("{timestamp}.").into_bytes();
    signed.extend_from_slice(&request.body);
    assert_eq!(expected, format!("sha256={}", nami_engine::incidents::format::hmac_hex(&secret, &signed)));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["event"], "incident.triggered");
    assert_eq!(body["incident"]["id"], json!(incident));
}

#[sqlx::test]
async fn telegram_goes_to_the_configured_api_and_never_logs_the_token(pool: sqlx::PgPool) {
    let telegram = MockServer::start().await;
    let token = format!("123456:{}", "T".repeat(35));
    Mock::given(method("POST")).and(path(format!("/bot{token}/sendMessage")))
        .respond_with(ResponseTemplate::new(500)).mount(&telegram).await;
    let mut state = AppState::for_test(pool.clone());
    state.config.telegram_api_base_url = telegram.uri();
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let channel = insert_channel(&pool, None, Some(user), "phone",
        ChannelConfig::Telegram { bot_token: token.clone(), chat_id: "42".into() }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: Some(user) }).await;
    let logs = common::LogCapture::default();
    let _guard = logs.install();
    run_incident_ticks(&state).await;
    assert_eq!(telegram.received_requests().await.unwrap().len(), 1);
    assert!(!logs.lines().join("\n").contains(&"T".repeat(35)), "the bot token never reaches a log line");
    assert!(!timeline(&pool, incident).await.iter().any(|(_, m)| m.as_deref().unwrap_or("").contains("TTTT")));
}

#[sqlx::test]
async fn implicit_email_goes_to_the_user_through_the_mailer(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [high] test incident");
}

#[sqlx::test]
async fn a_channel_never_receives_more_than_its_per_minute_cap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&slack).await;
    let channel = insert_channel(&pool, Some(project), None, "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", slack.uri()) }).await;
    for _ in 0..(deliveries::CHANNEL_PER_MINUTE + 10) {
        let incident = open_test_incident(&pool, project, user).await;
        queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    }
    for _ in 0..3 {
        run_incident_ticks(&state).await;
    }
    assert_eq!(slack.received_requests().await.unwrap().len() as i64, deliveries::CHANNEL_PER_MINUTE);
    let (pending, attempts): (i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(attempts), 0) from notification_deliveries where status = 'pending' and channel_id = $1")
        .bind(channel)
        .fetch_one(&pool).await.unwrap();
    assert_eq!((pending, attempts), (10, 0), "the excess waits without spending an attempt");
}

#[sqlx::test]
async fn a_delivery_whose_channel_no_longer_decrypts_fails_without_sending(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let channel = insert_channel(&pool, Some(project), None, "room",
        ChannelConfig::Slack { webhook_url: "http://127.0.0.1:9/x".into() }).await;
    sqlx::query("update notification_channels set config_enc = '\\x00'::bytea where id = $1").bind(channel).execute(&pool).await.unwrap();
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;
    let (status, _, error) = delivery(&pool, incident).await;
    assert_eq!(status, "failed");
    assert!(error.unwrap().contains("could not be decrypted"));
}

#[sqlx::test]
async fn send_test_reports_the_outcome_and_is_rate_limited(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let teams = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(202)).expect(1).mount(&teams).await;
    let channel = insert_channel(&pool, Some(project), None, "teams",
        ChannelConfig::Teams { webhook_url: format!("{}/workflows/x", teams.uri()) }).await;
    let test_path = format!("/projects/{project}/notification-channels/{channel}/test");

    let (status, _) = call(&app, "POST", &test_path, Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&app, "POST", &test_path, Some(&admin), Some(json!({}))).await;
    assert_eq!((status, body["ok"].as_bool()), (StatusCode::OK, Some(true)), "{body}");
    let (status, _) = call(&app, "POST", &test_path, Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let card: Value = serde_json::from_slice(&teams.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(card["attachments"][0]["content"]["type"], "AdaptiveCard");
}

#[sqlx::test]
async fn the_strict_policy_refuses_internal_and_off_allowlist_destinations_at_send_time(pool: sqlx::PgPool) {
    let mut state = AppState::for_test(pool.clone());
    state.config.outbound_policy = nami_engine::incidents::outbound::OutboundPolicy::Strict;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let receiver = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&receiver).await;
    // A personal webhook at 127.0.0.1 (public-only) and a Slack channel off hooks.slack.com: both refused.
    let personal = insert_channel(&pool, None, Some(user), "mine", ChannelConfig::Webhook { url: receiver.uri() }).await;
    let slack = insert_channel(&pool, Some(project), None, "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", receiver.uri()) }).await;
    // A project webhook may reach a private receiver (R9).
    let project_hook = insert_channel(&pool, Some(project), None, "hook", ChannelConfig::Webhook { url: receiver.uri() }).await;
    for (round, channel, owner) in [("p0r0s0", personal, Some(user)), ("p0r0s1", slack, None), ("p0r0s2", project_hook, None)] {
        queue(&pool, incident, round, Target::Channel { channel_id: channel, user_id: owner }).await;
    }
    run_incident_ticks(&state).await;
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "select round, status, last_error from notification_deliveries where incident_id = $1 and round like 'p0r0s%' order by round")
        .bind(incident).fetch_all(&pool).await.unwrap();
    assert_eq!(rows, [
        ("p0r0s0".into(), "failed".into(), Some("destination not allowed".into())),
        ("p0r0s1".into(), "failed".into(), Some("destination not allowed".into())),
        ("p0r0s2".into(), "sent".into(), None),
    ]);
    assert_eq!(receiver.received_requests().await.unwrap().len(), 1, "only the project webhook was contacted");
}

#[sqlx::test]
async fn a_lease_that_expired_after_the_last_attempt_fails_the_row_instead_of_wedging_the_queue(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    // A crash mid-send of attempt 5 leaves this behind.
    sqlx::query("insert into notification_deliveries (incident_id, event, round, target_key, user_id, status, attempts, locked_until)
                 values ($1, 'triggered', 'stuck', 'email:x', $2, 'sending', 5, now() - interval '1 minute')")
        .bind(incident).bind(user).execute(&pool).await.unwrap();
    queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    run_incident_ticks(&state).await;
    let stuck: (String, Option<String>) = sqlx::query_as(
        "select status, last_error from notification_deliveries where round = 'stuck'").fetch_one(&pool).await.unwrap();
    assert_eq!(stuck, ("failed".into(), Some("delivery interrupted".into())));
    assert_eq!(delivery(&pool, incident).await.0, "sent", "the rest of the queue still drains");
    assert!(!settled_mail(&email, 1).await.is_empty());
}
```

`tests/common/mod.rs` gains:

```rust
/// Seals and inserts a channel without the host allowlist (R9), so tests can target wiremock.
pub async fn insert_channel(pool: &sqlx::PgPool, project: Option<Uuid>, user: Option<Uuid>, name: &str,
    config: nami_engine::incidents::channel::model::ChannelConfig) -> Uuid
/// One escalation tick (from Task 9 on) and one delivery drain, as the worker would run them.
pub async fn run_incident_ticks(state: &AppState) { nami_engine::incidents::worker::kick(state).await }
```

`insert_channel` needs a `created_by`. It uses `user`, or for project channels it seeds an admin through `seed_user`.

Run: engine command `--test incidents notification_deliveries::`, then engine command `--lib incidents::deliveries`. Expected: FAIL.

- [ ] **Step 3: Implement**

`deliveries::drain`:

1. Claim, in **one transaction** (R12, R13). Each step is one literal statement:

```sql
-- (a) one claimer at a time, across every engine instance; released at commit
select pg_advisory_xact_lock(hashtext('nami.notification_deliveries.claim'));

-- (b) poison rows: a lease that expired after the last attempt (R12)
update notification_deliveries set status = 'failed', locked_until = null, last_error = 'delivery interrupted'
where status = 'sending' and locked_until < now() and attempts >= 5;

-- (c) claim with the storm caps ($1 = DRAIN_BATCH, $2 = CHANNEL_PER_MINUTE, $3 = EMAIL_PER_USER_PER_MINUTE)
with candidates as (
    select id, channel_id, user_id, next_attempt_at,
           case when channel_id is not null then 'c:' || channel_id::text else 'u:' || user_id::text end as bucket
    from notification_deliveries
    where ((status = 'pending' and next_attempt_at <= now())
        or (status = 'sending' and locked_until < now()))
      and attempts < 5
    order by next_attempt_at, id
    limit $1
    for update skip locked
), recent as (
    select case when channel_id is not null then 'c:' || channel_id::text else 'u:' || user_id::text end as bucket,
           count(*) as used
    from notification_deliveries
    where ((status = 'sent' and sent_at > now() - interval '1 minute')
        or (status = 'sending' and locked_until >= now()))
      and (channel_id in (select channel_id from candidates where channel_id is not null)
        or (channel_id is null and user_id in (select user_id from candidates where channel_id is null)))
    group by 1
), ranked as (
    select c.id,
           coalesce(r.used, 0) + row_number() over (partition by c.bucket order by c.next_attempt_at, c.id) as nth,
           case when c.bucket like 'c:%' then $2::bigint else $3::bigint end as cap
    from candidates c left join recent r on r.bucket = c.bucket
), deferred as (
    update notification_deliveries d
    set status = 'pending', locked_until = null, next_attempt_at = now() + interval '60 seconds'
    from ranked r where d.id = r.id and r.nth > r.cap
    returning d.id
)
update notification_deliveries d
set status = 'sending', locked_until = now() + interval '60 seconds', attempts = d.attempts + 1
from ranked r
where d.id = r.id and r.nth <= r.cap
returning d.id, d.incident_id, d.event, d.round, d.user_id, d.channel_id, d.attempts
```

   Commit. The lock serialises only the claim, which takes milliseconds. Sends happen after the commit, outside any transaction. Deferred rows keep their attempt count. `skip locked` stays, so a claimer never waits on a row another transaction is updating (a record step, for example).

2. Per claimed row, in a task bounded by a `Semaphore(MAX_SENDS_IN_FLIGHT)`:
   a. **Cancel (R15).** If `event = 'triggered'` and the incident's status is not `open`: `status = 'cancelled'`, no event.
   b. *(The rate caps are already applied by the claim.)*
   c. **Membership.** If `user_id` is set and that user is no longer a member of the incident's project: `cancelled`.
   d. **Build the message.** Load the incident summary (title, cause, status, source, severity name, started_at) and the project name. `incident_url = {ui_base_url}/projects/{project}/incidents/{id}`. Event: `triggered` → `NotifyEvent::Triggered`, `resolved` → `Resolved`. `actions` stays `None` until Task 11.
   e. **Send.** Email target: the user's address if `email_verified_at` is set (otherwise `failed`, "no verified email"), via `send_email`. Channel target: `open_for_send`. A missing or disabled channel → `cancelled`; a `DecryptError` → `failed` with R10's message; otherwise `send_to_channel`.
   f. **Record.**
      - Success: `status = 'sent', sent_at = now(), last_error = null`, plus a `notification_sent` event (`"<Kind>: <channel name>"`, or `"Email: <user email>"` for implicit email).
      - Failure with `Retry::Never`, or `attempts = MAX_ATTEMPTS`: `failed`.
      - Otherwise: `pending` with `next_attempt_at = now() + max(backoff(attempts), retry_after)`.
      - Every failure adds a `notification_failed` event with `"<Kind>: <name>: <error>"` and sets `last_error` (both ≤ 200 chars).
   The email address appears in the event message only for that user's own implicit email (members can see each other's addresses on the project already).

`send_to_channel` per kind (method POST, JSON body, 10 s from the client):
- webhook: `webhook_body`, headers `content-type: application/json`, `x-nami-timestamp: <unix seconds, taken just before this attempt>`, `x-nami-signature: sha256=<hex>` over `{timestamp}.{body}` (R29), `x-nami-delivery: <id>`, `x-nami-event`.
- slack / discord / teams: their JSON.
- telegram: `{telegram_base}/bot{token}/sendMessage` with `format::telegram`.
- email (project channel): `send_email` to the configured address.

Before every request it re-runs the destination rules: `check_url_for_send` with the kind's rule, `channel.config.reach(&channel.owner)` and `senders.policy`. A refusal is `Retry::Never` with "destination not allowed". It then sends with `outbound::public_client()` for `Reach::PublicOnly` and `outbound::client()` for `Reach::MonitorPolicy` (project webhooks, the Telegram base). Responses: 2xx is success; 429 is `After(Retry-After capped at 1 h)`; 408 and 5xx are `After(None)`; any other 4xx is `Never`. Transport errors are `After(None)` with `describe_error`. Mailer errors are `After(None)` with "email not delivered". The response body is read only to drain it (at most 64 KiB) and is never stored.

`worker::run_forever`: every `TICK` (missed ticks delayed), spawn one iteration in its own task (a panic is logged, never fatal) that calls `kick`. `kick` returns at once when `!state.config.incidents_worker_enabled`. Otherwise it takes a `KICKS` permit (`static KICKS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_KICKS)`), builds `Senders::from_config(&state.config)` (cheap; read per call so tests can point it at wiremock), calls `deliveries::drain` once, and logs errors with `tracing::error!` (no secrets: never a URL, address or reqwest error `Display`). A permit is held only for one bounded tick plus one bounded drain. `sqlx::test` databases are per test, so tests that run in parallel share the permits but never each other's rows.

- [ ] **Step 4: Run** engine command `--test incidents --test core`, then engine command `--lib -- incidents`, then the full suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/NNNN_incidents_part1.sql nami-engine/src nami-engine/.env.example \
  nami-engine/tests/incidents/notification_deliveries.rs nami-engine/tests/common/mod.rs nami-engine/tests/core/migrations.rs
git commit -m "feat(engine): durable notification deliveries with retries, caps and channel tests (NAMI-INC-5)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6 (NAMI-INC-6): On-call rotation math and time windows (pure, DST-tested)

**Files:**
- Create: `nami-engine/src/incidents/timewindow.rs`, `nami-engine/src/incidents/oncall/mod.rs`, `nami-engine/src/incidents/oncall/rotation.rs`
- Modify: `nami-engine/src/incidents/mod.rs` (`pub mod timewindow; pub mod oncall;`)
- Test: unit tests in both files

**Interfaces:**
- Consumes: `uptime::maintenance::model::{Schedule, weekday_from_iso}` (R19), `chrono-tz`.
- Produces:

```rust
// timewindow
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeWindow { pub weekdays: Vec<u8>, pub from: String, pub to: String }   // "HH:MM"
#[derive(Debug, Clone, PartialEq)]
pub struct ValidWindow { schedule: Schedule /* Schedule::Weekly */ }
impl TimeWindow {
    /// weekdays: 1-7, non-empty, no duplicates (sorted on output); from/to: "HH:MM" 00:00-23:59.
    pub fn validate(&self, timezone: Tz) -> Result<ValidWindow, String>;
}
impl ValidWindow { pub fn covers(&self, at: DateTime<Utc>) -> bool }  // maintenance::Schedule::covers
pub fn parse_timezone(name: &str) -> Result<Tz, String>;               // "unknown time zone \"x\""

// oncall::rotation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationLength { Daily, Weekly, Hours(u32) }  // Hours: 1..=8760
#[derive(Debug, Clone)]
pub struct Rotation { pub timezone: Tz, pub start_date: NaiveDate, pub handoff: NaiveTime,
    pub length: RotationLength, pub participants: Vec<Uuid>, pub restrictions: Vec<ValidWindow> }
#[derive(Debug, Clone, PartialEq)]
pub struct Override { pub id: Uuid, pub user_id: Uuid, pub starts_at: DateTime<Utc>, pub ends_at: DateTime<Utc>,
    pub created_at: DateTime<Utc> }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shift { pub user_id: Uuid, pub starts_at: DateTime<Utc>, pub ends_at: DateTime<Utc>, pub override_id: Option<Uuid> }
pub const MAX_SHIFT_RANGE: chrono::Duration = chrono::Duration::days(70);
pub fn local_to_utc(tz: Tz, local: NaiveDateTime) -> DateTime<Utc>;
pub fn shift_start(r: &Rotation, k: i64) -> DateTime<Utc>;
/// Index of the rotation shift containing `at`; None before the first shift.
pub fn shift_index(r: &Rotation, at: DateTime<Utc>) -> Option<i64>;
pub fn rotation_user_at(r: &Rotation, at: DateTime<Utc>) -> Option<Uuid>;
pub fn on_call_at(r: &Rotation, overrides: &[Override], at: DateTime<Utc>) -> Option<(Uuid, Option<Uuid>)>; // (user, override id)
/// Merged timeline in [from, to); panics never, clamps to MAX_SHIFT_RANGE.
pub fn shifts(r: &Rotation, overrides: &[Override], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Shift>;
```

The algorithms are given here because the signatures and tests do not fix them:

```rust
pub fn local_to_utc(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    // Ambiguous (fall back): the earlier instant. Nonexistent (spring forward):
    // the first valid minute after the requested local time.
    let mut probe = local;
    for _ in 0..=24 * 60 {
        match tz.from_local_datetime(&probe) {
            chrono::LocalResult::Single(t) => return t.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(earlier, _) => return earlier.with_timezone(&Utc),
            chrono::LocalResult::None => probe += chrono::Duration::minutes(1),
        }
    }
    Utc.from_utc_datetime(&local) // unreachable for real zones; never panic
}

pub fn shift_start(r: &Rotation, k: i64) -> DateTime<Utc> {
    let epoch_local = r.start_date.and_time(r.handoff);
    match r.length {
        RotationLength::Daily => local_to_utc(r.timezone, epoch_local + chrono::Days::new_signed(k)),
        RotationLength::Weekly => local_to_utc(r.timezone, epoch_local + chrono::Days::new_signed(7 * k)),
        RotationLength::Hours(h) => local_to_utc(r.timezone, epoch_local) + chrono::Duration::hours(h as i64 * k),
    }
}
// (`Days::new_signed` does not exist in chrono: write a small helper that adds or subtracts
// `Days::new(k.unsigned_abs())`. Only k >= 0 occurs, because shift_index returns None before the epoch.)

pub fn shift_index(r: &Rotation, at: DateTime<Utc>) -> Option<i64> {
    let epoch = shift_start(r, 0);
    if at < epoch { return None; }
    let nominal = match r.length { RotationLength::Daily => 86_400, RotationLength::Weekly => 604_800,
        RotationLength::Hours(h) => h as i64 * 3600 };
    let mut k = (at - epoch).num_seconds() / nominal;
    while k > 0 && shift_start(r, k) > at { k -= 1; }      // DST made the estimate overshoot
    while shift_start(r, k + 1) <= at { k += 1; }          // ...or undershoot
    Some(k)
}
```

`rotation_user_at`:
1. Return None when there are no participants, when `shift_index` is None, or when restrictions are non-empty and none of them covers `at`.
2. Otherwise return `participants[k mod n]`.

`on_call_at`:
1. The override with the latest `created_at` among those with `starts_at <= at < ends_at` wins (ties: larger id).
2. Otherwise `rotation_user_at`, with no override id.

`shifts` (R20):
1. Clamp `to` to `from + MAX_SHIFT_RANGE`.
2. Collect sample points: `from`, then every whole UTC minute in `(from, to)`, plus each override's `starts_at`/`ends_at` inside `(from, to)`. Sort and dedupe them.
3. Evaluate `on_call_at` at each point.
4. Merge consecutive equal `(user, override)` values into shifts that end at the next differing point (or `to`). Drop `None` stretches.

70 days are ~100k evaluations, fine for a request. The tests pin correctness, not speed. Two notes:
- The list routes compute `on_call_now`/`until` for up to 50 schedules (≈ 20k evaluations each for 14 days). So routes call `shifts` inside `tokio::task::spawn_blocking`, never directly on the async runtime.
- `TimeWindow.weekdays` are `u8`, and `maintenance::weekday_from_iso` takes an `i16`: convert with `i16::from(d)`. `ValidWindow` builds `Schedule::Weekly { days, start, end, timezone }`, the field names on `main`.

- [ ] **Step 1: Write the failing tests**

`timewindow.rs` `mod tests`:

```rust
use super::*;
use chrono::TimeZone;

fn utc(s: &str) -> DateTime<Utc> { s.parse().unwrap() }

#[test]
fn windows_validate_days_times_and_zones() {
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    assert!(TimeWindow { weekdays: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "17:00".into() }.validate(tz).is_ok());
    for bad in [
        TimeWindow { weekdays: vec![], from: "09:00".into(), to: "17:00".into() },
        TimeWindow { weekdays: vec![0], from: "09:00".into(), to: "17:00".into() },
        TimeWindow { weekdays: vec![8], from: "09:00".into(), to: "17:00".into() },
        TimeWindow { weekdays: vec![1, 1], from: "09:00".into(), to: "17:00".into() },
        TimeWindow { weekdays: vec![1], from: "9:00".into(), to: "17:00".into() },
        TimeWindow { weekdays: vec![1], from: "24:00".into(), to: "17:00".into() },
    ] {
        assert!(bad.validate(tz).is_err(), "{bad:?}");
    }
    assert!(parse_timezone("Mars/Olympus").is_err());
}

#[test]
fn a_window_from_after_to_spans_midnight_into_the_next_day() {
    let tz: Tz = "Asia/Jakarta".parse().unwrap(); // UTC+7, no DST
    let night = TimeWindow { weekdays: vec![5], from: "22:00".into(), to: "06:00".into() }.validate(tz).unwrap();
    assert!(night.covers(utc("2026-10-23T15:30:00Z")), "Friday 22:30 local");
    assert!(night.covers(utc("2026-10-23T22:59:00Z")), "Saturday 05:59 local, carried over");
    assert!(!night.covers(utc("2026-10-23T23:00:00Z")), "Saturday 06:00 local");
    assert!(!night.covers(utc("2026-10-24T15:30:00Z")), "Saturday 22:30 local is not listed");
}

#[test]
fn windows_follow_the_wall_clock_across_dst() {
    let tz: Tz = "America/New_York".parse().unwrap();
    let mornings = TimeWindow { weekdays: vec![7], from: "09:00".into(), to: "10:00".into() }.validate(tz).unwrap();
    // 2026-03-08 is the spring-forward Sunday: 09:00 EDT = 13:00Z (the day before it was 14:00Z).
    assert!(mornings.covers(utc("2026-03-08T13:30:00Z")));
    assert!(!mornings.covers(utc("2026-03-08T14:30:00Z")));
    // 2026-11-01 is the fall-back Sunday: 09:00 EST = 14:00Z.
    assert!(mornings.covers(utc("2026-11-01T14:30:00Z")));
    assert!(!mornings.covers(utc("2026-11-01T13:30:00Z")));
}
```

`rotation.rs` `mod tests`:

```rust
use super::*;
use crate::incidents::timewindow::TimeWindow;

fn utc(s: &str) -> DateTime<Utc> { s.parse().unwrap() }
fn users(n: usize) -> Vec<Uuid> { (0..n).map(|i| Uuid::from_u128(i as u128 + 1)).collect() }

fn rotation(tz: &str, start: &str, handoff: &str, length: RotationLength, n: usize) -> Rotation {
    Rotation { timezone: tz.parse().unwrap(), start_date: start.parse().unwrap(),
        handoff: NaiveTime::parse_from_str(handoff, "%H:%M").unwrap(), length, participants: users(n),
        restrictions: vec![] }
}

#[test]
fn daily_rotation_cycles_through_participants_at_the_handoff() {
    let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 3);
    let u = users(3);
    assert_eq!(rotation_user_at(&r, utc("2026-10-19T08:59:59Z")), None, "before the first shift");
    assert_eq!(rotation_user_at(&r, utc("2026-10-19T09:00:00Z")), Some(u[0]));
    assert_eq!(rotation_user_at(&r, utc("2026-10-20T08:59:59Z")), Some(u[0]));
    assert_eq!(rotation_user_at(&r, utc("2026-10-20T09:00:00Z")), Some(u[1]));
    assert_eq!(rotation_user_at(&r, utc("2026-10-22T09:00:00Z")), Some(u[0]), "wraps around");
}

#[test]
fn weekly_handoff_keeps_local_time_across_spring_forward() {
    // Europe/Berlin springs forward on Sunday 2026-03-29 (02:00 -> 03:00).
    let r = rotation("Europe/Berlin", "2026-03-23", "09:00", RotationLength::Weekly, 2);
    assert_eq!(shift_start(&r, 0), utc("2026-03-23T08:00:00Z"), "09:00 CET");
    assert_eq!(shift_start(&r, 1), utc("2026-03-30T07:00:00Z"), "09:00 CEST");
    let u = users(2);
    assert_eq!(rotation_user_at(&r, utc("2026-03-30T06:59:00Z")), Some(u[0]));
    assert_eq!(rotation_user_at(&r, utc("2026-03-30T07:00:00Z")), Some(u[1]));
}

#[test]
fn a_handoff_inside_the_spring_forward_gap_happens_at_the_first_valid_instant() {
    let r = rotation("Europe/Berlin", "2026-03-28", "02:30", RotationLength::Daily, 2);
    assert_eq!(shift_start(&r, 0), utc("2026-03-28T01:30:00Z"), "02:30 CET the day before");
    assert_eq!(shift_start(&r, 1), utc("2026-03-29T01:00:00Z"), "02:30 does not exist: 03:00 CEST");
    assert_eq!(shift_start(&r, 2), utc("2026-03-30T00:30:00Z"), "02:30 CEST");
}

#[test]
fn fall_back_repeats_no_shift() {
    // Europe/Berlin falls back on Sunday 2026-10-25 (03:00 -> 02:00): 02:30 happens twice.
    let r = rotation("Europe/Berlin", "2026-10-24", "02:30", RotationLength::Daily, 2);
    assert_eq!(shift_start(&r, 1), utc("2026-10-25T00:30:00Z"), "the earlier 02:30 (CEST)");
    assert_eq!(shift_start(&r, 2), utc("2026-10-26T01:30:00Z"));
    let u = users(2);
    assert_eq!(rotation_user_at(&r, utc("2026-10-25T01:30:00Z")), Some(u[1]), "the repeated 02:30 is mid-shift");
    let s = shifts(&r, &[], utc("2026-10-24T00:00:00Z"), utc("2026-10-27T00:00:00Z"));
    assert_eq!(s.iter().map(|s| s.starts_at).collect::<Vec<_>>(),
        [utc("2026-10-24T00:30:00Z"), utc("2026-10-25T00:30:00Z"), utc("2026-10-26T01:30:00Z")],
        "nobody before the epoch; a 25-hour shift on the fall-back day");
}

#[test]
fn custom_hour_rotations_are_absolute_time() {
    let r = rotation("Europe/Berlin", "2026-03-28", "20:00", RotationLength::Hours(12), 2);
    assert_eq!(shift_start(&r, 0), utc("2026-03-28T19:00:00Z"));
    assert_eq!(shift_start(&r, 1), utc("2026-03-29T07:00:00Z"), "12 real hours, not 08:00 local");
}

#[test]
fn restrictions_blank_the_rotation_outside_working_hours_but_overrides_still_apply() {
    let tz: Tz = "UTC".parse().unwrap();
    let mut r = rotation("UTC", "2026-10-19", "00:00", RotationLength::Weekly, 1);
    r.restrictions = vec![TimeWindow { weekdays: vec![1, 2, 3, 4, 5], from: "09:00".into(), to: "17:00".into() }
        .validate(tz).unwrap()];
    assert_eq!(rotation_user_at(&r, utc("2026-10-19T10:00:00Z")), Some(users(1)[0]));
    assert_eq!(rotation_user_at(&r, utc("2026-10-19T18:00:00Z")), None);
    assert_eq!(rotation_user_at(&r, utc("2026-10-24T10:00:00Z")), None, "Saturday");
    let stand_in = Uuid::from_u128(99);
    let o = Override { id: Uuid::from_u128(500), user_id: stand_in, starts_at: utc("2026-10-19T17:00:00Z"),
        ends_at: utc("2026-10-19T20:00:00Z"), created_at: utc("2026-10-18T00:00:00Z") };
    assert_eq!(on_call_at(&r, &[o.clone()], utc("2026-10-19T18:00:00Z")), Some((stand_in, Some(o.id))));
}

#[test]
fn the_newest_override_wins_on_overlap() {
    let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 1);
    let (a, b) = (Uuid::from_u128(10), Uuid::from_u128(11));
    let older = Override { id: Uuid::from_u128(1), user_id: a, starts_at: utc("2026-10-19T10:00:00Z"),
        ends_at: utc("2026-10-19T14:00:00Z"), created_at: utc("2026-10-01T00:00:00Z") };
    let newer = Override { id: Uuid::from_u128(2), user_id: b, starts_at: utc("2026-10-19T12:00:00Z"),
        ends_at: utc("2026-10-19T13:00:00Z"), created_at: utc("2026-10-02T00:00:00Z") };
    let all = [older.clone(), newer.clone()];
    assert_eq!(on_call_at(&r, &all, utc("2026-10-19T11:00:00Z")).unwrap().0, a);
    assert_eq!(on_call_at(&r, &all, utc("2026-10-19T12:30:00Z")).unwrap().0, b);
    assert_eq!(on_call_at(&r, &all, utc("2026-10-19T13:30:00Z")).unwrap().0, a);
    let s = shifts(&r, &all, utc("2026-10-19T09:00:00Z"), utc("2026-10-19T15:00:00Z"));
    let users_in_order: Vec<Uuid> = s.iter().map(|s| s.user_id).collect();
    assert_eq!(users_in_order, [users(1)[0], a, b, a, users(1)[0]]);
    assert_eq!(s[2].starts_at, utc("2026-10-19T12:00:00Z"));
    assert_eq!(s[2].ends_at, utc("2026-10-19T13:00:00Z"));
}

#[test]
fn shifts_merge_runs_and_are_clamped_to_70_days() {
    let r = rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 1);
    let s = shifts(&r, &[], utc("2026-10-19T09:00:00Z"), utc("2027-10-19T09:00:00Z"));
    assert_eq!(s.len(), 1, "one participant: one merged shift");
    assert_eq!(s[0].ends_at, utc("2026-10-19T09:00:00Z") + MAX_SHIFT_RANGE);
    assert!(shifts(&rotation("UTC", "2026-10-19", "09:00", RotationLength::Daily, 0), &[],
        utc("2026-10-19T00:00:00Z"), utc("2026-10-20T00:00:00Z")).is_empty(), "no participants: nobody");
}

#[test]
fn shift_index_agrees_with_shift_start_for_a_year_of_daily_shifts_in_a_dst_zone() {
    let r = rotation("America/Santiago", "2026-01-01", "00:00", RotationLength::Daily, 7);
    for k in 0..400 {
        let start = shift_start(&r, k);
        assert_eq!(shift_index(&r, start), Some(k), "k={k}");
        assert_eq!(shift_index(&r, start - chrono::Duration::seconds(1)), if k == 0 { None } else { Some(k - 1) });
    }
}
```

(America/Santiago changes its offset at local midnight, which is the hardest case for a 00:00 handoff: some days start at 01:00 local.)

Run: engine command `--lib -- incidents::timewindow incidents::oncall::rotation`. Expected: FAIL to compile.

- [ ] **Step 2: Implement** both modules as specified.
- [ ] **Step 3: Run** engine command `--lib -- incidents::timewindow incidents::oncall::rotation uptime::maintenance::model`, then clippy. Expected: PASS.
- [ ] **Step 4: Commit**

```bash
git add nami-engine/src/incidents
git commit -m "feat(engine): on-call rotation math and time windows, DST-safe (NAMI-INC-6)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7 (NAMI-INC-7): On-call schedules API — schedules, participants, restrictions, overrides, "on call now", shifts

**Files:**
- Append: `nami-engine/migrations/NNNN_incidents_part1.sql` (schedules)
- Create: `nami-engine/src/incidents/oncall/model.rs`, `nami-engine/src/incidents/oncall/routes.rs`
- Modify: `nami-engine/src/core/validate.rs`, `nami-engine/src/core/router.rs`, `nami-engine/src/projects/model.rs` + `nami-engine/src/projects/routes.rs` (the members list and the caller's `role` on `GET /projects/{id}`, R28), `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.server.ts` (return `role` with `project`; default `'viewer'` when the field is missing) + `section.test.ts` (the layout passes `role` through), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (schedule POST/PUT, override POST)
- Test: `nami-engine/tests/incidents/schedules_api.rs`

**Interfaces:**
- Consumes: Task 6 `oncall::rotation::*`, `timewindow::{TimeWindow, parse_timezone}`.
- Produces:

```rust
// projects::model (R28)
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MemberView { pub user_id: Uuid, pub email: String, pub project_role: String }
pub async fn list_members(pool: &PgPool, project_id: Uuid) -> Result<Vec<MemberView>, sqlx::Error>; // by email
// projects::routes: add `.get(list_members)` to the existing
// `.route("/projects/{id}/members", post(add_member))` line (one route, two methods), viewer.
// GET /projects/{id} (R28, controller ruling) returns `ProjectView`, the project plus the caller's
// effective role; `projects::model::Project` itself is unchanged (the list route keeps its shape).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectView { #[serde(flatten)] pub project: Project, pub role: String }  // "admin" | "member" | "viewer"
/// Global admin → "admin" (member or not); otherwise the `project_members.project_role`.
/// Called after `require_project_role(.., "viewer")` passed, so a row exists for non-admins.
pub async fn effective_role(pool: &PgPool, user: &CurrentUser, project_id: Uuid) -> Result<String, sqlx::Error>;

// incidents::oncall::model
pub const MAX_SCHEDULES_PER_PROJECT: i64 = 50;
pub const MAX_PARTICIPANTS: usize = 50;
pub const MAX_RESTRICTIONS: usize = 14;
pub const MAX_OVERRIDE_DAYS: i64 = 90;
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleView { pub id: Uuid, pub project_id: Uuid, pub name: String, pub timezone: String,
    pub rotation: String, pub rotation_hours: Option<i32>, pub handoff_time: String, pub start_date: NaiveDate,
    pub restrictions: Vec<TimeWindow>, pub participants: Vec<Participant>, pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Participant { pub user_id: Uuid, pub email: String, pub is_member: bool }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct ScheduleInput { pub name: String, pub timezone: String, pub rotation: String, pub rotation_hours: Option<i32>,
    pub handoff_time: String, pub start_date: NaiveDate, #[serde(default)] pub restrictions: Vec<TimeWindow>,
    pub participants: Vec<Uuid> }
pub struct ValidSchedule { /* name, tz, length, handoff, start_date, restrictions (validated), participants */ }
pub fn validate(input: ScheduleInput) -> Result<ValidSchedule, String>;
pub async fn create(pool: &PgPool, project_id: Uuid, v: &ValidSchedule) -> Result<Created<ScheduleView>, AppError>;
pub async fn replace(pool: &PgPool, project_id: Uuid, id: Uuid, v: &ValidSchedule) -> Result<Option<ScheduleView>, AppError>;
pub async fn list(pool: &PgPool, project_id: Uuid) -> Result<Vec<ScheduleView>, sqlx::Error>;
pub async fn get(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Option<ScheduleView>, sqlx::Error>;
pub enum Deleted { Deleted, NotFound, InUse { policy: String } }
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Deleted, sqlx::Error>;
/// The rotation + overrides of one schedule, for evaluation (also used by the escalation engine).
pub async fn load_rotation(conn: &mut PgConnection, schedule_id: Uuid) -> Result<Option<(Rotation, Vec<Override>)>, sqlx::Error>;
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OverrideView { pub id: Uuid, pub user_id: Uuid, pub email: String, pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>, pub created_by: Option<Uuid>, pub created_at: DateTime<Utc> }
pub async fn add_override(pool: &PgPool, project_id: Uuid, schedule_id: Uuid, user_id: Uuid, starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>, created_by: Uuid) -> Result<Option<OverrideView>, AppError>;
pub async fn list_overrides(pool: &PgPool, project_id: Uuid, schedule_id: Uuid) -> Result<Option<Vec<OverrideView>>, sqlx::Error>; // ending after now()
pub async fn delete_override(pool: &PgPool, project_id: Uuid, schedule_id: Uuid, override_id: Uuid) -> Result<bool, sqlx::Error>;
pub enum Created<T> { Created(T), Full }
```

- Routes:

| Method + path | Role | Notes |
|---|---|---|
| `GET /projects/{id}/schedules` | viewer | each with `on_call_now: {user_id, email, until} \| null` |
| `POST /projects/{id}/schedules` | admin | `ScheduleInput` → 201 |
| `GET /projects/{id}/schedules/{schedule_id}` | viewer | + `on_call_now` |
| `PUT /projects/{id}/schedules/{schedule_id}` | admin | replaces fields + participants |
| `DELETE /projects/{id}/schedules/{schedule_id}` | admin | 409 if an escalation step targets it (from Task 8) |
| `GET /projects/{id}/schedules/{schedule_id}/shifts?from=&to=` | viewer | default now → now + 14 days; `to - from` ≤ 31 days else 400 |
| `GET /projects/{id}/schedules/{schedule_id}/overrides` | viewer | upcoming and current |
| `POST /projects/{id}/schedules/{schedule_id}/overrides` | admin | `{user_id, starts_at, ends_at}` |
| `DELETE /projects/{id}/schedules/{schedule_id}/overrides/{override_id}` | admin | |
| `GET /projects/{id}/on-call` | viewer | `[{schedule_id, name, on_call_now}]` for the dashboard card |

Validation rules:
- `name` via `core::validate::validate_name`, unique per project (409).
- `rotation` is `daily` | `weekly` | `custom`; `rotation_hours` is required for `custom` (1-8760) and must be absent otherwise.
- `handoff_time` is `HH:MM`.
- `participants` are 1-50 distinct users who are current members of the project (a non-member → 400 `user <id> is not a member of this project`).
- `restrictions` hold at most 14 windows, each passing `TimeWindow::validate`.
- `start_date` lies within ±5 years of today.
- Override: `ends_at > starts_at`, `ends_at > now()`, length ≤ 90 days, the user is a project member.

`until` in `on_call_now` is the end of the current merged shift (`shifts(now, now + 14 days)[0].ends_at`). Shifts show each user's email and `is_member` (R7: non-members are listed but flagged, and never paged).

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_incidents_part1.sql`:

```sql
-- On-call schedules (NAMI-INC-7). The rotation is evaluated by
-- oncall::rotation (pure); restrictions are TimeWindow objects in the
-- schedule's zone.
create table on_call_schedules (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    timezone text not null,
    rotation text not null check (rotation in ('daily', 'weekly', 'custom')),
    rotation_hours int check (rotation_hours between 1 and 8760),
    handoff_time time not null,
    start_date date not null,
    restrictions jsonb not null default '[]'::jsonb check (jsonb_typeof(restrictions) = 'array'),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, name),
    check ((rotation = 'custom') = (rotation_hours is not null))
);

create table on_call_participants (
    schedule_id uuid not null references on_call_schedules(id) on delete cascade,
    position int not null check (position >= 0),
    user_id uuid not null references users(id) on delete cascade,
    primary key (schedule_id, position),
    unique (schedule_id, user_id)
);

create table on_call_overrides (
    id uuid primary key default gen_random_uuid(),
    schedule_id uuid not null references on_call_schedules(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    starts_at timestamptz not null,
    ends_at timestamptz not null,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    check (ends_at > starts_at)
);
create index on_call_overrides_schedule_idx on on_call_overrides (schedule_id, ends_at);
```

A deleted user's participant row is removed by cascade, which shifts the later positions' meaning. `replace` always rewrites positions 0..n-1, so a gap only means the cycle skips that slot until the next save. `load_rotation` reads participants ordered by position, so a gap never yields a wrong user.

Migration test `a_custom_rotation_needs_hours_and_others_must_not_have_them` (two failing inserts, `23514`). Add the three tables to `all_tables_exist`.

- [ ] **Step 2: Write the failing API tests**

`nami-engine/tests/incidents/schedules_api.rs`:

```rust
// On-call schedules. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::{call, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

fn schedule_body(participants: &[Uuid]) -> Value {
    json!({"name": "primary", "timezone": "Europe/Berlin", "rotation": "weekly", "handoff_time": "09:00",
           "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": participants,
           "restrictions": [{"weekdays": [1,2,3,4,5,6,7], "from": "00:00", "to": "00:00"}]})
}

#[sqlx::test]
async fn admins_manage_schedules_and_viewers_see_who_is_on_call(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let path = format!("/projects/{project}/schedules");

    let (status, _) = call(&app, "POST", &path, Some(&member), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["participants"][0]["email"], alice_email);
    assert_eq!(created["on_call_now"]["user_id"], json!(alice));
    let id = created["id"].as_str().unwrap();

    let (status, shifts) = call(&app, "GET", &format!("{path}/{id}/shifts"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{shifts}");
    assert!(!shifts.as_array().unwrap().is_empty());
    let (status, board) = call(&app, "GET", &format!("/projects/{project}/on-call"), Some(&viewer), None).await;
    assert_eq!((status, board[0]["on_call_now"]["email"].as_str()), (StatusCode::OK, Some(alice_email.as_str())));
}

#[sqlx::test]
async fn viewers_list_project_members_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let path = format!("/projects/{project}/members");
    let (status, members) = call(&app, "GET", &path, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{members}");
    let entry = members.as_array().unwrap().iter().find(|m| m["user_id"] == json!(alice)).unwrap().clone();
    assert_eq!(entry, json!({"user_id": alice, "email": alice_email, "project_role": "member"}));
    let (status, _) = call(&app, "GET", &path, Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn the_project_tells_the_caller_its_effective_role(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    for role in ["admin", "member", "viewer"] {
        let (_, _, cookie) = user_with_role(&app, &pool, project, Some(role)).await;
        let (status, body) = call(&app, "GET", &format!("/projects/{project}"), Some(&cookie), None).await;
        assert_eq!((status, body["role"].as_str()), (StatusCode::OK, Some(role)), "{body}");
    }
    let email = common::unique_email("root");
    common::seed_user(&pool, &email, "admin").await;
    let root = common::login(&app, &email).await;
    let (_, body) = call(&app, "GET", &format!("/projects/{project}"), Some(&root), None).await;
    assert_eq!(body["role"], "admin", "a global admin without membership is admin");
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}"), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn schedules_are_validated(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (stranger, _, _) = user_with_role(&app, &pool, other, Some("member")).await;
    let path = format!("/projects/{project}/schedules");
    let mut cases = Vec::new();
    for (key, value) in [("timezone", json!("Mars/Olympus")), ("rotation", json!("monthly")),
                         ("handoff_time", json!("25:00")), ("participants", json!([])),
                         ("participants", json!([stranger])), ("participants", json!([alice, alice])),
                         ("restrictions", json!([{"weekdays": [9], "from": "09:00", "to": "17:00"}]))] {
        let mut body = schedule_body(&[alice]);
        body[key] = value;
        cases.push(body);
    }
    let mut custom = schedule_body(&[alice]);
    custom["rotation"] = json!("custom");
    cases.push(custom.clone());
    custom["rotation_hours"] = json!(0);
    cases.push(custom);
    for body in cases {
        let (status, response) = call(&app, "POST", &path, Some(&admin), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response}");
    }
}

#[sqlx::test]
async fn overrides_put_someone_else_on_call_and_foreign_ids_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (admin_id, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, other_admin) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let overrides = format!("/projects/{project}/schedules/{id}/overrides");
    let window = json!({"user_id": bob, "starts_at": Utc::now() - Duration::minutes(1), "ends_at": Utc::now() + Duration::hours(2)});

    let (status, _) = call(&app, "POST", &overrides, Some(&member), Some(window.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only admins manage overrides in Part 1");
    let (status, o) = call(&app, "POST", &overrides, Some(&admin), Some(window)).await;
    assert_eq!(status, StatusCode::CREATED, "{o}");
    assert_eq!(o["created_by"], json!(admin_id));
    let (_, now) = call(&app, "GET", &format!("/projects/{project}/schedules/{id}"), Some(&admin), None).await;
    assert_eq!(now["on_call_now"]["user_id"], json!(bob));

    for bad in [json!({"user_id": bob, "starts_at": Utc::now(), "ends_at": Utc::now() - Duration::hours(1)}),
                json!({"user_id": bob, "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::days(91)}),
                json!({"user_id": Uuid::new_v4(), "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::hours(1)})] {
        let (status, _) = call(&app, "POST", &overrides, Some(&admin), Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = call(&app, "GET", &format!("/projects/{other}/schedules/{id}"), Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &format!("/projects/{other}/schedules/{id}/overrides/{}", o["id"].as_str().unwrap()),
        Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn shift_ranges_are_capped(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let id = created["id"].as_str().unwrap();
    let from = Utc::now();
    let to = from + Duration::days(32);
    let q = format!("/projects/{project}/schedules/{id}/shifts?from={}&to={}",
        urlencode(&from.to_rfc3339()), urlencode(&to.to_rfc3339()));
    let (status, _) = call(&app, "GET", &q, Some(&admin), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn urlencode(s: &str) -> String { url::form_urlencoded::byte_serialize(s.as_bytes()).collect() }
```

Run: engine command `--test incidents schedules_api::`, then `--test core migrations::`. Expected: FAIL.

- [ ] **Step 3: Implement** the model and routes. Shift computations run in `spawn_blocking` (Task 6 note). Writes run in one transaction. Participants are rewritten as positions 0..n-1. `updated_at = now()` on replace. The `InUse` check is added in Task 8 (until then `delete` never returns it).
- [ ] **Step 4: Run** engine command `--test incidents --test core`, then the full suite and clippy. Expected: green.
- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/NNNN_incidents_part1.sql nami-engine/src nami-engine/tests/incidents/schedules_api.rs nami-engine/tests/core/migrations.rs
git commit -m "feat(engine): on-call schedules with restrictions, overrides and shifts (NAMI-INC-7)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8 (NAMI-INC-8): Escalation policies — schema, default policy, pure planner, API

**Files:**
- Append: `nami-engine/migrations/NNNN_incidents_part1.sql` (policies; their targets reference Task 3's channels and Task 7's schedules)
- Create: `nami-engine/src/incidents/escalation/mod.rs`, `nami-engine/src/incidents/escalation/plan.rs`, `nami-engine/src/incidents/escalation/model.rs`, `nami-engine/src/incidents/escalation/routes.rs`
- Modify:
  - `nami-engine/src/incidents/lifecycle.rs`: `OpenIncident.escalation_policy_id: Option<Uuid>`. `open`, `reopen`, `acknowledge` and `resolve` arm or disarm escalation. Every construction site in Task 1/2 code and tests sets the new field to `None`.
  - `nami-engine/src/incidents/channel/model.rs` and `nami-engine/src/incidents/oncall/model.rs`: `delete` returns `InUse` when a step targets the row.
  - `nami-engine/src/incidents/routes.rs`: manual create accepts `escalation_policy_id` (a foreign id → 404).
  - `nami-engine/src/core/validate.rs`, `nami-engine/src/core/router.rs`, `nami-engine/tests/common/mod.rs` (`policy_body`), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (policy POST/PUT, `/default`).
- Test: `nami-engine/tests/incidents/escalation_policies_api.rs` (+ unit tests in `plan.rs`)

**Interfaces:**
- Consumes: Task 3 channels, Task 6 `timewindow`, Task 7 schedules.
- Produces:

```rust
// incidents::escalation::plan (pure)
pub const MIN_REPEAT_GAP: chrono::Duration = chrono::Duration::minutes(5);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyShape { pub id: Uuid, pub delays_minutes: Vec<u32>, pub repeat_count: u32 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor { pub step: usize, pub repeat: u32, pub on_fallback: bool, pub notified_in_pass: u32 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted { NobodyNotified, RepeatsDone, NoPolicy }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next { Step { cursor: Cursor, at: DateTime<Utc> }, Exhausted(Exhausted) }
/// The step after the one that just ran. `active` is the policy the cursor is walking (the
/// incident's own, or its fallback when `cursor.on_fallback`); `fallback` is the incident's policy's
/// fallback (ignored when already on it: one hop). `ran_at` is the step's scheduled time (R14).
pub fn after_step(active: &PolicyShape, fallback: Option<&PolicyShape>, cursor: Cursor, ran_at: DateTime<Utc>,
    newly_notified: u32) -> Next;
/// The round label of a step (dedup key part, ≤ 32 chars): "g{generation}p{0|1}r{repeat}s{step}".
pub fn round(generation: i32, cursor: &Cursor) -> String;

// incidents::escalation::model
pub const MAX_POLICIES_PER_PROJECT: i64 = 50;
pub const MAX_STEPS: usize = 20;
pub const MAX_TARGETS_PER_STEP: usize = 20;
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetInput { User { id: Uuid }, Schedule { id: Uuid }, AllMembers, ProjectChannel { id: Uuid } }
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)] #[serde(deny_unknown_fields)]
pub struct StepCondition { pub weekdays: Vec<u8>, pub from: String, pub to: String, pub timezone: String }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct StepInput { pub delay_minutes: i32, #[serde(default)] pub condition: Option<StepCondition>, pub targets: Vec<TargetInput> }
#[derive(Debug, Clone, Deserialize)] #[serde(deny_unknown_fields)]
pub struct PolicyInput { pub name: String, #[serde(default)] pub repeat_count: i32,
    #[serde(default)] pub fallback_policy_id: Option<Uuid>, pub steps: Vec<StepInput> }
#[derive(Debug, Clone, Serialize)]
pub struct TargetView { #[serde(flatten)] pub target: TargetInput, pub label: String }  // email / schedule name / "<channel> (<kind>)" / "All members"
#[derive(Debug, Clone, Serialize)]
pub struct StepView { pub position: i32, pub delay_minutes: i32, pub condition: Option<StepCondition>, pub targets: Vec<TargetView> }
#[derive(Debug, Clone, Serialize)]
pub struct PolicyView { pub id: Uuid, pub project_id: Uuid, pub name: String, pub repeat_count: i32,
    pub fallback_policy_id: Option<Uuid>, pub is_default: bool, pub steps: Vec<StepView>, pub updated_at: DateTime<Utc> }
/// Shape checks (counts, ranges, duplicates, condition window/zone). Same-project checks happen in `save`.
pub fn validate(input: &PolicyInput) -> Result<(), String>;
pub enum Saved { Saved(PolicyView), NotFound, Full, Invalid(String) }
pub async fn create(pool: &PgPool, project_id: Uuid, input: &PolicyInput) -> Result<Saved, sqlx::Error>;
pub async fn replace(pool: &PgPool, project_id: Uuid, id: Uuid, input: &PolicyInput) -> Result<Saved, sqlx::Error>;
pub async fn list(pool: &PgPool, project_id: Uuid) -> Result<Vec<PolicyView>, sqlx::Error>;
pub async fn get(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Option<PolicyView>, sqlx::Error>;
pub async fn make_default(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>;
pub enum Deleted { Deleted, NotFound, IsDefault }
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Deleted, sqlx::Error>;
pub async fn belongs_to<'e>(executor: impl PgExecutor<'e>, project_id: Uuid, policy_id: Uuid) -> Result<bool, sqlx::Error>;
```

- Routes:
  - `GET /projects/{id}/escalation-policies` and `GET .../{policy_id}` (viewer).
  - `POST /projects/{id}/escalation-policies` (admin, `PolicyInput` → 201).
  - `PUT .../{policy_id}` (admin, replaces name, repeat, fallback and all steps).
  - `POST .../{policy_id}/default` (admin, `{}`).
  - `DELETE .../{policy_id}` (admin; the default → 409 `the default policy cannot be deleted; make another policy the default first`).
  - Channel or schedule delete while targeted → 409 `used by escalation policy "<name>"`.

`save` checks (inside the write transaction, all 400 except where noted):
- `fallback_policy_id` must be in the project (else 400 `fallback policy not found in this project`) and must not be the policy itself.
- Every `user` is a current project member.
- Every `schedule` is in the project.
- Every `project_channel` is a project channel of this project (personal channels cannot be policy targets).
- Name unique per project (409).
- Duplicate targets in one step are refused (400).

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_incidents_part1.sql`:

```sql
-- Escalation policies (NAMI-INC-8) and the incident escalation cursor
-- (R14 in the plan). The engine (incidents::escalation) walks the cursor.
create table escalation_policies (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    repeat_count int not null default 0 check (repeat_count between 0 and 10),
    fallback_policy_id uuid references escalation_policies(id) on delete set null,
    is_default boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, name),
    check (fallback_policy_id is null or fallback_policy_id <> id)
);
create unique index escalation_policies_one_default on escalation_policies (project_id) where is_default;

create table escalation_steps (
    id uuid primary key default gen_random_uuid(),
    policy_id uuid not null references escalation_policies(id) on delete cascade,
    position int not null check (position between 0 and 19),
    delay_minutes int not null check (delay_minutes between 0 and 1440),
    condition jsonb check (condition is null or jsonb_typeof(condition) = 'object'),
    unique (policy_id, position)
);

create table escalation_step_targets (
    id uuid primary key default gen_random_uuid(),
    step_id uuid not null references escalation_steps(id) on delete cascade,
    kind text not null check (kind in ('user', 'schedule', 'all_members', 'project_channel')),
    user_id uuid references users(id) on delete cascade,
    -- NO ACTION (the default), not RESTRICT: both refuse removing a targeted
    -- schedule or channel directly (23503), but NO ACTION is checked at the
    -- end of the statement, so removing the whole project, which cascades to
    -- the policies' targets and to the schedules/channels in one statement,
    -- still works. RESTRICT is checked at once and could fail that cascade,
    -- depending on the order the cascades run in.
    schedule_id uuid references on_call_schedules(id),
    channel_id uuid references notification_channels(id),
    check (
        (kind = 'user' and user_id is not null and schedule_id is null and channel_id is null) or
        (kind = 'schedule' and schedule_id is not null and user_id is null and channel_id is null) or
        (kind = 'project_channel' and channel_id is not null and user_id is null and schedule_id is null) or
        (kind = 'all_members' and user_id is null and schedule_id is null and channel_id is null))
);
create index escalation_step_targets_step_idx on escalation_step_targets (step_id);
create index escalation_step_targets_schedule_idx on escalation_step_targets (schedule_id) where schedule_id is not null;
create index escalation_step_targets_channel_idx on escalation_step_targets (channel_id) where channel_id is not null;

alter table incidents
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column escalation_generation int not null default 0,
    add column escalation_step int not null default 0,
    add column escalation_repeat int not null default 0,
    add column escalation_on_fallback boolean not null default false,
    add column escalation_notified int not null default 0,
    add column escalation_legacy boolean not null default false,
    add column next_escalation_at timestamptz;
create index incidents_escalation_due_idx on incidents (next_escalation_at)
    where status = 'open' and next_escalation_at is not null;

create function seed_default_escalation_policy(target_project uuid) returns void language plpgsql as $$
declare
    policy uuid;
    step uuid;
begin
    insert into escalation_policies (project_id, name, is_default) values (target_project, 'Default', true)
        returning id into policy;
    insert into escalation_steps (policy_id, position, delay_minutes) values (policy, 0, 0) returning id into step;
    insert into escalation_step_targets (step_id, kind) values (step, 'all_members');
end
$$;
create function projects_seed_default_escalation_policy() returns trigger language plpgsql as $$
begin
    perform seed_default_escalation_policy(new.id);
    return new;
end
$$;
create trigger projects_seed_default_escalation_policy after insert on projects
    for each row execute function projects_seed_default_escalation_policy();
select seed_default_escalation_policy(id) from projects;

-- R6: incidents open at deploy time were already emailed by the old path.
-- They get the default policy but no escalation; "legacy" makes their
-- resolution notify every member, as the old path would have.
update incidents i set escalation_policy_id = p.id, escalation_legacy = (i.status <> 'resolved')
from escalation_policies p where p.project_id = i.project_id and p.is_default;
```

Migration tests:
- `every_project_gets_a_default_policy_that_pages_all_members`: one policy, one step, delay 0, one `all_members` target.
- `a_policy_cannot_fall_back_to_itself` (`23514`).
- `a_targeted_schedule_cannot_be_deleted_from_sql` (`23503`).
- `removing_a_project_removes_its_policies_schedules_and_channels`: a project whose policy targets one of its schedules and one of its project channels (inserted in SQL) is removed with a plain SQL delete of the `projects` row without error, and no row of the five tables is left for it.
- Add the three new tables to `all_tables_exist`.

- [ ] **Step 2: Write the failing planner tests** in `plan.rs` `mod tests`:

```rust
use super::*;

fn t(minutes: i64) -> DateTime<Utc> { DateTime::from_timestamp(1_792_000_000 + minutes * 60, 0).unwrap() }
fn policy(delays: &[u32], repeat: u32) -> PolicyShape {
    PolicyShape { id: Uuid::from_u128(delays.len() as u128 * 100 + repeat as u128), delays_minutes: delays.to_vec(), repeat_count: repeat }
}
fn cursor(step: usize, repeat: u32, on_fallback: bool, notified: u32) -> Cursor {
    Cursor { step, repeat, on_fallback, notified_in_pass: notified }
}

#[test]
fn steps_follow_each_other_after_their_delay_from_the_scheduled_time() {
    let p = policy(&[0, 10, 30], 0);
    assert_eq!(after_step(&p, None, cursor(0, 0, false, 0), t(0), 2),
        Next::Step { cursor: cursor(1, 0, false, 2), at: t(10) });
    assert_eq!(after_step(&p, None, cursor(1, 0, false, 2), t(10), 0),
        Next::Step { cursor: cursor(2, 0, false, 2), at: t(40) });
}

#[test]
fn a_pass_that_notified_someone_repeats_after_at_least_the_minimum_gap() {
    let p = policy(&[0, 10], 2);
    assert_eq!(after_step(&p, None, cursor(1, 0, false, 1), t(10), 0),
        Next::Step { cursor: cursor(0, 1, false, 0), at: t(15) }, "delay 0 still waits MIN_REPEAT_GAP");
    let slow = policy(&[20], 1);
    assert_eq!(after_step(&slow, None, cursor(0, 0, false, 0), t(20), 3),
        Next::Step { cursor: cursor(0, 1, false, 0), at: t(40) });
    assert_eq!(after_step(&p, None, cursor(1, 2, false, 1), t(50), 0), Next::Exhausted(Exhausted::RepeatsDone));
}

#[test]
fn a_pass_that_notified_nobody_moves_to_the_fallback_at_once() {
    let p = policy(&[0, 5], 3);
    let f = policy(&[0], 0);
    assert_eq!(after_step(&p, Some(&f), cursor(1, 0, false, 0), t(5), 0),
        Next::Step { cursor: cursor(0, 0, true, 0), at: t(5) });
    assert_eq!(after_step(&p, None, cursor(1, 0, false, 0), t(5), 0), Next::Exhausted(Exhausted::NobodyNotified));
}

#[test]
fn the_last_repeat_ending_unacknowledged_moves_to_the_fallback_after_the_gap() {
    let p = policy(&[0], 1);
    let f = policy(&[2], 0);
    assert_eq!(after_step(&p, Some(&f), cursor(0, 1, false, 0), t(30), 1),
        Next::Step { cursor: cursor(0, 0, true, 0), at: t(35) });
}

#[test]
fn a_fallback_is_followed_one_hop_only() {
    let f = policy(&[0], 0);
    let ff = policy(&[0], 0);
    assert_eq!(after_step(&f, Some(&ff), cursor(0, 0, true, 0), t(0), 0), Next::Exhausted(Exhausted::NobodyNotified));
    assert_eq!(after_step(&f, Some(&ff), cursor(0, 0, true, 0), t(0), 1), Next::Exhausted(Exhausted::RepeatsDone));
}

#[test]
fn every_path_terminates_within_a_bounded_number_of_steps() {
    // Exhaustive over small policies: walking `after_step` from step 0 always reaches
    // Exhausted within (repeat+1) * steps * 2 + 2 calls, whatever gets notified.
    for steps in 1..=4usize {
        for repeat in 0..=3u32 {
            for notify_pattern in 0..16u32 {
                let p = policy(&vec![0; steps], repeat);
                let f = policy(&vec![1; steps], repeat);
                let mut c = cursor(0, 0, false, 0);
                let mut at = t(0);
                let mut calls = 0;
                loop {
                    calls += 1;
                    assert!(calls <= ((repeat as usize + 1) * steps * 2 + 2), "no termination");
                    let notified = (notify_pattern >> (calls % 4)) & 1;
                    let active = if c.on_fallback { &f } else { &p };
                    match after_step(active, Some(&f), c, at, notified) {
                        Next::Step { cursor, at: next } => { assert!(next >= at); c = cursor; at = next; }
                        Next::Exhausted(_) => break,
                    }
                }
            }
        }
    }
}

#[test]
fn rounds_are_short_and_distinct() {
    let a = round(0, &cursor(0, 0, false, 0));
    let b = round(0, &cursor(0, 0, true, 0));
    let c = round(1, &cursor(0, 0, false, 0));
    assert_eq!(a, "g0p0r0s0");
    assert!(a != b && a != c && b != c);
    assert!(round(i32::MAX, &cursor(19, 10, true, 0)).len() <= 32);
}
```

The planner is pure and fully determined by these tests:

```rust
pub fn after_step(active: &PolicyShape, fallback: Option<&PolicyShape>, cursor: Cursor, ran_at: DateTime<Utc>,
    newly_notified: u32) -> Next {
    let notified = cursor.notified_in_pass.saturating_add(newly_notified);
    let minutes = |m: u32| chrono::Duration::minutes(m as i64);
    if let Some(&delay) = active.delays_minutes.get(cursor.step + 1) {
        return Next::Step { cursor: Cursor { step: cursor.step + 1, notified_in_pass: notified, ..cursor }, at: ran_at + minutes(delay) };
    }
    let fallback = fallback.filter(|f| !cursor.on_fallback && !f.delays_minutes.is_empty());
    let to_fallback = |gap: chrono::Duration| {
        let f = fallback.expect("checked");
        Next::Step { cursor: Cursor { step: 0, repeat: 0, on_fallback: true, notified_in_pass: 0 },
            at: ran_at + minutes(f.delays_minutes[0]).max(gap) }
    };
    if notified == 0 {
        return if fallback.is_some() { to_fallback(chrono::Duration::zero()) } else { Next::Exhausted(Exhausted::NobodyNotified) };
    }
    if cursor.repeat < active.repeat_count {
        return Next::Step { cursor: Cursor { step: 0, repeat: cursor.repeat + 1, notified_in_pass: 0, ..cursor },
            at: ran_at + minutes(active.delays_minutes[0]).max(MIN_REPEAT_GAP) };
    }
    if fallback.is_some() { to_fallback(MIN_REPEAT_GAP) } else { Next::Exhausted(Exhausted::RepeatsDone) }
}
```

Run: engine command `--lib incidents::escalation`. Expected: FAIL, then PASS once `plan.rs` exists.

- [ ] **Step 3: Write the failing API tests**

`nami-engine/tests/incidents/escalation_policies_api.rs`:

```rust
// Escalation policies. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, insert_channel, seed_project, test_app, user_with_role};
use nami_engine::incidents::channel::model::ChannelConfig;
use serde_json::{json, Value};
use uuid::Uuid;

#[sqlx::test]
async fn a_project_starts_with_a_default_policy_paging_all_members(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (status, body) = call(&app, "GET", &format!("/projects/{project}/escalation-policies"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "Default");
    assert_eq!(body[0]["is_default"], true);
    assert_eq!(body[0]["steps"][0]["delay_minutes"], 0);
    assert_eq!(body[0]["steps"][0]["targets"][0], json!({"type": "all_members", "label": "All members"}));
}

#[sqlx::test]
async fn admins_build_multi_step_policies_with_conditions_and_a_fallback(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let room = insert_channel(&pool, Some(project), None, "ops", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/x".into() }).await;
    let path = format!("/projects/{project}/escalation-policies");
    let (_, list) = call(&app, "GET", &path, Some(&admin), None).await;
    let default_id = list[0]["id"].as_str().unwrap().to_string();
    let body = json!({
        "name": "Business hours", "repeat_count": 2, "fallback_policy_id": default_id,
        "steps": [
            {"delay_minutes": 0, "targets": [{"type": "user", "id": alice}, {"type": "project_channel", "id": room}],
             "condition": {"weekdays": [1,2,3,4,5], "from": "09:00", "to": "17:00", "timezone": "Asia/Jakarta"}},
            {"delay_minutes": 15, "targets": [{"type": "all_members"}]}
        ]});
    let (status, _) = call(&app, "POST", &path, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["steps"][0]["targets"][0]["label"], alice_email);
    assert_eq!(created["steps"][0]["targets"][1]["label"], "ops (slack)");
    assert_eq!(created["steps"][1]["delay_minutes"], 15);
    let id = created["id"].as_str().unwrap();

    let (status, _) = call(&app, "DELETE", &format!("/projects/{project}/notification-channels/{room}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "a targeted channel cannot be deleted");
    let (status, _) = call(&app, "POST", &format!("{path}/{id}/default"), Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "DELETE", &format!("{path}/{id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "the default cannot be deleted");
    let (status, _) = call(&app, "DELETE", &format!("{path}/{default_id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK, "the old default can go now");
}

#[sqlx::test]
async fn policies_are_validated_and_scoped_to_their_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (stranger, _, _) = user_with_role(&app, &pool, other, Some("member")).await;
    let foreign_room = insert_channel(&pool, Some(other), None, "x", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/y".into() }).await;
    let personal = insert_channel(&pool, None, Some(alice), "mine", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/z".into() }).await;
    let other_policy: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1").bind(other).fetch_one(&pool).await.unwrap();
    let path = format!("/projects/{project}/escalation-policies");
    let step = |targets: Value| json!([{"delay_minutes": 0, "targets": targets}]);
    let cases = [
        json!({"name": "a", "steps": []}),
        json!({"name": "a", "steps": step(json!([]))}),
        json!({"name": "a", "repeat_count": 11, "steps": step(json!([{"type": "all_members"}]))}),
        json!({"name": "a", "steps": [{"delay_minutes": 1441, "targets": [{"type": "all_members"}]}]}),
        json!({"name": "a", "steps": step(json!([{"type": "user", "id": stranger}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "project_channel", "id": foreign_room}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "project_channel", "id": personal}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "all_members"}, {"type": "all_members"}]))}),
        json!({"name": "a", "fallback_policy_id": other_policy, "steps": step(json!([{"type": "all_members"}]))}),
        json!({"name": "a", "steps": [{"delay_minutes": 0, "targets": [{"type": "all_members"}],
               "condition": {"weekdays": [1], "from": "09:00", "to": "17:00", "timezone": "Nowhere/Land"}}]}),
    ];
    for body in cases {
        let (status, response) = call(&app, "POST", &path, Some(&admin), Some(body.clone())).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY, "{body} -> {status} {response}");
    }
    let (status, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "self", "steps": step(json!([{"type": "all_members"}]))}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let (status, _) = call(&app, "PUT", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"name": "self", "fallback_policy_id": id, "steps": step(json!([{"type": "all_members"}]))}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no fallback to itself");
    let (_, _, other_admin) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{other}/escalation-policies/{id}"), Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn opening_an_incident_arms_escalation_on_its_policy(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin),
        Some(json!({"name": "slow", "steps": [{"delay_minutes": 7, "targets": [{"type": "all_members"}]}]}))).await;
    let (_, incident) = call(&app, "POST", &format!("/projects/{project}/incidents"), Some(&member),
        Some(json!({"title": "x", "escalation_policy_id": created["id"]}))).await;
    let (policy, step, next, started): (Uuid, i32, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select escalation_policy_id, escalation_step, next_escalation_at, started_at from incidents where id = $1::uuid")
            .bind(incident["id"].as_str().unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(policy.to_string(), created["id"].as_str().unwrap());
    assert_eq!(step, 0);
    assert_eq!(next, Some(started + chrono::Duration::minutes(7)));

    let id = incident["id"].as_str().unwrap();
    call(&app, "POST", &format!("/projects/{project}/incidents/{id}/acknowledge"), Some(&member), Some(json!({}))).await;
    let next: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select next_escalation_at from incidents where id = $1::uuid")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(next, None, "acknowledge stops escalation");
}
```

Run: engine command `--test incidents escalation_policies_api::`, then `--test core migrations::`. Expected: FAIL.

- [ ] **Step 4: Implement**

Lifecycle arming, a private `arm(conn, incident_id, requested_policy: Option<Uuid>, base: DateTime<Utc>)`:

```sql
update incidents i set
    escalation_policy_id = p.id, escalation_step = 0, escalation_repeat = 0,
    escalation_on_fallback = false, escalation_notified = 0, escalation_legacy = false,
    next_escalation_at = $3 + make_interval(mins => s.delay_minutes)
from escalation_policies p
join escalation_steps s on s.policy_id = p.id and s.position = 0
where i.id = $1
  and p.id = coalesce(
      (select id from escalation_policies where id = $2 and project_id = i.project_id),
      (select id from escalation_policies where project_id = i.project_id and is_default))
```

- `open` calls `arm(id, req.escalation_policy_id, req.started_at)` after the insert.
- `reopen` runs `escalation_generation = escalation_generation + 1`, then `arm(id, Some(current policy), at)`, so the incident keeps its own policy and restarts at step 1 (spec).
- `acknowledge` and `resolve` set `next_escalation_at = null` in the same update.

Then write the model and routes as specified.

- [ ] **Step 5: Run** engine command `--test incidents --test core`, then engine command `--lib -- incidents`, then the full suite and clippy. Expected: green.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/NNNN_incidents_part1.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): escalation policies, default policy and the escalation planner (NAMI-INC-8)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 9 (NAMI-INC-9): Escalation engine — targets to recipients, steps, repeats, fallback, resolve notifications

**Files:**
- Create: `nami-engine/src/incidents/escalation/run.rs`
- Modify: `nami-engine/src/incidents/escalation/mod.rs`, `nami-engine/src/incidents/worker.rs` (`kick` = escalation tick, then drain), `nami-engine/src/incidents/lifecycle.rs` (`resolve` queues "resolved" deliveries), `nami-engine/tests/common/mod.rs` (`make_escalation_due`, `set_policy`)
- Test: `nami-engine/tests/incidents/escalation_engine.rs`

**Interfaces:**
- Consumes: Task 5 `deliveries::{enqueue, NewDelivery, Target, DeliveryEvent}`. Task 6 `oncall::rotation::on_call_at`. Task 7 `incidents::oncall::model::load_rotation`. Task 8 `plan::{after_step, round, PolicyShape, Cursor, Next, Exhausted}`. `timewindow`.
- Produces:

```rust
// incidents::escalation::run
pub const ESCALATION_BATCH: usize = 50;
pub const MAX_STEPS_PER_TICK: usize = 20;
pub const MAX_RECIPIENTS_PER_STEP: usize = 100;
pub const MAX_DELIVERIES_PER_INCIDENT: i64 = 500;
/// Runs every due step of up to ESCALATION_BATCH incidents. Returns how many steps ran or were skipped.
pub async fn tick(state: &AppState) -> Result<usize, AppError>;
/// R7: the targets of one step at `at`, deduplicated, members only, capped.
pub async fn recipients(conn: &mut PgConnection, project_id: Uuid, step_id: Uuid, at: DateTime<Utc>)
    -> Result<Vec<Target>, sqlx::Error>;

// incidents::lifecycle (changed)
// resolve(): after the state change, enqueues one `resolved` delivery per target that has a
// `sent` triggered delivery in the current generation (rounds `like 'g' || generation || 'p%'`;
// the `p` keeps g1 from matching g10), or, for `escalation_legacy` incidents, one email per
// project member, round "resolved:g{generation}".
/// R17/R30: `resolve_source` with a reason (≤ 200 chars, unsafe chars → space). Queues the normal
/// `resolved` notices; the `resolved` event's message is the reason, metadata {"reason": ..}.
/// Only Logs Part 2's disable path (`resolve_alert_incident(.., notify: false)`) uses it (Task 10).
pub async fn resolve_source_with_reason(conn: &mut PgConnection, source: &IncidentSource, at: DateTime<Utc>,
    reason: &str) -> Result<Option<Uuid>, sqlx::Error>;
```

`tests/common/mod.rs` gains:

```rust
/// Makes the incident's next escalation step due now, only if one is scheduled:
/// `update incidents set next_escalation_at = now() where id = $1 and next_escalation_at is not null`.
/// (Unconditional, it would resurrect an exhausted escalation and break the "fallback runs once" test.)
pub async fn make_escalation_due(pool: &sqlx::PgPool, incident: Uuid)
/// Replaces the project's default policy (PUT) through the API as `admin`; returns its id.
pub async fn set_policy(app: &Router, project: Uuid, admin: &HeaderValue, body: serde_json::Value) -> Uuid
```

**The tick** runs one transaction per incident:
1. `select ... from incidents where status = 'open' and next_escalation_at <= now() order by next_escalation_at limit 1 for update skip locked`. Stop when none is left or after `ESCALATION_BATCH` incidents.
2. Load the incident's policy, its fallback and the `active` one (fallback when `escalation_on_fallback`).
   - A missing policy (deleted) → `next_escalation_at = null` + `escalation_exhausted` "escalation policy was deleted".
   - A cursor past the active policy's last step (the policy was shortened) runs as the end of the pass with 0 newly notified.
3. Run the step scheduled at `ran_at = max(next_escalation_at, now − 60 s)` (R14). `now` is read once per incident with `select now()` inside the transaction.
   - **Condition false.** If the step has a condition that does not cover `now`, record a `step_skipped` event (`"Step <n> skipped: outside <weekdays> <from>-<to> <timezone>"`) with 0 newly notified.
   - **Recipients.** Otherwise call `recipients(project, step, now)`, so the person on call now is paged (R14). Then cap at `MAX_DELIVERIES_PER_INCIDENT - existing` (when the cap is hit, add one `notification_failed` event "notification cap reached (500 per incident)", once per incident, detected by `metadata->>'cap' = 'true'`).
   - **Enqueue.** `enqueue` with `event = Triggered`, `round = round(generation, cursor)`. The newly notified count is the inserted row count.
   - **Event.** Record `escalated` with `"Step <n> of <policy name>: <k> recipient(s)"` and metadata `{"step", "repeat", "fallback", "recipients": k}`.
4. `after_step(active, fallback, cursor, ran_at, newly)`:
   - `Step { cursor, at }` writes the cursor columns and `next_escalation_at = at`. When `on_fallback` turns true, record `escalation_fallback` (`"No one acknowledged: continuing with <fallback name>"`, or `"No one could be notified: continuing with <fallback name>"` when the pass notified nobody).
   - When `at <= now()` and fewer than `MAX_STEPS_PER_TICK` steps have run for this incident in this tick, loop to step 3.
   - `Exhausted(reason)` sets `next_escalation_at = null` and records `escalation_exhausted` with:
     - `NobodyNotified`: "No one could be notified (nobody on call, no members or enabled channels) and there is no fallback policy".
     - `RepeatsDone`: "Escalation finished: every step and repeat ran without an acknowledgement".
     - `NoPolicy`: as in step 2.
5. Commit, then do the next incident.

**`recipients`** reads the step's targets:
- `user` → that user if they are a project member (`project_members` row). A user target yields `Target::Email { user_id }` when `email_verified_at` is set, plus `Target::Channel { channel_id, user_id: Some(user) }` for each of their enabled personal channels.
- `schedule` → `load_rotation` + `on_call_at(at)`, then the same as a user target.
- `all_members` → every member, each expanded as a user target.
- `project_channel` → `Target::Channel { channel_id, user_id: None }` if enabled.

Deduplicate by `Target::key()`, keep the first `MAX_RECIPIENTS_PER_STEP`, and order deterministically (by key).

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/incidents/escalation_engine.rs`:

```rust
// The escalation engine. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::{HeaderValue, StatusCode};
use chrono::{Duration, Utc};
use common::{call, insert_channel, make_escalation_due, run_incident_ticks, seed_project, set_policy,
    settled_mail, test_app, user_with_role};
use nami_engine::{incidents::escalation::run, incidents::channel::model::ChannelConfig, core::state::AppState};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

async fn open(app: &axum::Router, project: Uuid, member: &HeaderValue) -> String {
    let (status, body) = call(app, "POST", &format!("/projects/{project}/incidents"), Some(member),
        Some(json!({"title": "db down"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn act(app: &axum::Router, project: Uuid, member: &HeaderValue, id: &str, action: &str) {
    let (status, body) = call(app, "POST", &format!("/projects/{project}/incidents/{id}/{action}"), Some(member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{action}: {body}");
}

async fn kinds(pool: &sqlx::PgPool, id: &str) -> Vec<String> {
    sqlx::query_scalar("select kind from incident_events where incident_id = $1::uuid order by id")
        .bind(id).fetch_all(pool).await.unwrap()
}

fn uid(id: &str) -> Uuid { id.parse().unwrap() }

fn one_step(delay: i64, targets: Value) -> Value {
    json!({"delay_minutes": delay, "targets": targets})
}

#[sqlx::test]
async fn the_default_policy_emails_every_member_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, alice, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, viewer_email, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    for email in [&alice, &viewer_email] {
        let mail = settled_mail(email, 1).await;
        assert_eq!(mail.len(), 1, "{email}");
        assert_eq!(mail[0].0, "[Nami] [high] db down");
    }
    assert!(kinds(&pool, &id).await.contains(&"escalated".to_string()));
}

#[sqlx::test]
async fn a_later_step_waits_for_its_delay_and_an_acknowledge_stops_it(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [
        one_step(0, json!([{"type": "user", "id": alice}])),
        one_step(10, json!([{"type": "user", "id": bob}]))]})).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1);
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "step 2 waits 10 minutes");
    act(&app, project, &member, &id, "acknowledge").await;
    make_escalation_due(&pool, uid(&id)).await;
    run_incident_ticks(&state).await;
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "acknowledged: no step 2");
}

#[sqlx::test]
async fn an_acknowledge_cancels_queued_step_notifications(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (bob, bob_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]})).await;
    let id = open(&app, project, &member).await;
    run::tick(&state).await.unwrap();                     // queued, not yet sent
    act(&app, project, &member, &id, "acknowledge").await;
    run_incident_ticks(&state).await;                     // the drain finds it acknowledged
    let status: String = sqlx::query_scalar("select status from notification_deliveries where incident_id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(status, "cancelled");
    assert!(settled_mail(&bob_email, 0).await.is_empty());
}

#[sqlx::test]
async fn a_schedule_target_pages_whoever_is_on_call_when_the_step_runs(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, schedule) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(json!({
        "name": "primary", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
        "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice]}))).await;
    let sid = schedule["id"].as_str().unwrap();
    call(&app, "POST", &format!("/projects/{project}/schedules/{sid}/overrides"), Some(&admin), Some(json!({
        "user_id": bob, "starts_at": Utc::now() - Duration::minutes(5), "ends_at": Utc::now() + Duration::hours(1)}))).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([{"type": "schedule", "id": sid}]))]})).await;
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&bob_email, 1).await.len(), 1, "the override is on call");
    assert!(settled_mail(&alice_email, 0).await.is_empty());
}

#[sqlx::test]
async fn a_user_target_also_uses_enabled_personal_channels_without_duplicates(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(1).mount(&slack).await;
    insert_channel(&pool, None, Some(alice), "phone", ChannelConfig::Slack { webhook_url: format!("{}/a", slack.uri()) }).await;
    let off = insert_channel(&pool, None, Some(alice), "old", ChannelConfig::Slack { webhook_url: format!("{}/b", slack.uri()) }).await;
    sqlx::query("update notification_channels set enabled = false where id = $1").bind(off).execute(&pool).await.unwrap();
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([
        {"type": "user", "id": alice}, {"type": "all_members"}]))]})).await;
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1, "named twice, emailed once");
}

#[sqlx::test]
async fn a_pass_that_notifies_nobody_moves_to_the_fallback_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    // Alice is on call only in a window that is never now.
    let (_, schedule) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(json!({
        "name": "nights", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
        "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice],
        "restrictions": [window_not_now()]}))).await;
    let sid = schedule["id"].as_str().unwrap();
    let (_, fallback) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin), Some(json!({
        "name": "backup", "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]}))).await;
    let fid = fallback["id"].as_str().unwrap();
    let main = set_policy(&app, project, &admin, json!({"name": "Default", "fallback_policy_id": fid,
        "steps": [one_step(0, json!([{"type": "schedule", "id": sid}]))]})).await;
    // The fallback points back at the main policy: it must not loop.
    call(&app, "PUT", &format!("/projects/{project}/escalation-policies/{fid}"), Some(&admin), Some(json!({
        "name": "backup", "fallback_policy_id": main, "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]}))).await;
    let id = open(&app, project, &member).await;
    for _ in 0..5 {
        make_escalation_due(&pool, uid(&id)).await;
        run_incident_ticks(&state).await;
    }
    assert_eq!(settled_mail(&bob_email, 1).await.len(), 1, "the fallback ran once");
    let k = kinds(&pool, &id).await;
    assert_eq!(k.iter().filter(|k| *k == "escalation_fallback").count(), 1, "{k:?}");
    assert_eq!(k.iter().filter(|k| *k == "escalation_exhausted").count(), 1, "{k:?}");
    let next: Option<chrono::DateTime<Utc>> = sqlx::query_scalar("select next_escalation_at from incidents where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(next, None, "nothing left to run");
}

/// A UTC window two to three hours from now: never covers the present.
fn window_not_now() -> Value {
    let from = (Utc::now() + Duration::hours(2)).format("%H:%M").to_string();
    let to = (Utc::now() + Duration::hours(3)).format("%H:%M").to_string();
    json!({"weekdays": [1,2,3,4,5,6,7], "from": from, "to": to})
}

#[sqlx::test]
async fn without_a_fallback_an_empty_pass_is_recorded_as_exhausted(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut condition = window_not_now();
    condition["timezone"] = json!("UTC");
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [
        {"delay_minutes": 0, "condition": condition, "targets": [{"type": "all_members"}]}]})).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    let k = kinds(&pool, &id).await;
    assert_eq!(&k[k.len() - 2..], ["step_skipped", "escalation_exhausted"], "{k:?}");
    let message: String = sqlx::query_scalar(
        "select message from incident_events where incident_id = $1::uuid and kind = 'escalation_exhausted'")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert!(message.contains("no fallback policy"), "{message}");
}

#[sqlx::test]
async fn repeats_wait_at_least_the_minimum_gap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "repeat_count": 3,
        "steps": [one_step(0, json!([{"type": "all_members"}]))]})).await;
    let id = open(&app, project, &member).await;
    run::tick(&state).await.unwrap();
    let wait: f64 = sqlx::query_scalar("select extract(epoch from next_escalation_at - started_at)::float8 from incidents where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert!((299.0..=301.0).contains(&wait), "repeat after 5 minutes, got {wait}s");
}

#[sqlx::test]
async fn resolve_notifies_everyone_already_notified_once_and_reopen_restarts(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [
        one_step(0, json!([{"type": "user", "id": alice}])), one_step(30, json!([{"type": "user", "id": bob}]))]})).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    act(&app, project, &member, &id, "resolve").await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["[Nami] [high] db down", "[Nami] Resolved: db down"]);
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "never notified, so not told about the resolution");

    act(&app, project, &member, &id, "reopen").await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 3).await.len(), 3, "a reopened incident pages step 1 again");
}

#[sqlx::test]
async fn a_removed_member_is_not_notified_even_when_on_call(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, schedule) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(json!({
        "name": "primary", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
        "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice]}))).await;
    let sid = schedule["id"].as_str().unwrap();
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([
        {"type": "schedule", "id": sid}, {"type": "user", "id": alice}]))]})).await;
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2").bind(project).bind(alice).execute(&pool).await.unwrap();
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    assert!(settled_mail(&alice_email, 0).await.is_empty());
}

#[sqlx::test]
async fn the_per_incident_delivery_cap_stops_a_storm(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("member")).await;
    let id = open(&app, project, &member).await;
    sqlx::query("insert into notification_deliveries (incident_id, event, round, target_key, user_id, status, sent_at)
                 select $1::uuid, 'triggered', 'old', 'email:' || g, (select user_id from project_members where project_id = $2 limit 1),
                        'sent', now() from generate_series(1, 499) g")
        .bind(&id).bind(project).execute(&pool).await.unwrap();
    run::tick(&state).await.unwrap();
    let total: i64 = sqlx::query_scalar("select count(*) from notification_deliveries where incident_id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(total, 500);
    let capped: i64 = sqlx::query_scalar("select count(*) from incident_events where incident_id = $1::uuid and kind = 'notification_failed'")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(capped, 1);
}

#[sqlx::test]
async fn racing_ticks_notify_each_target_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open(&app, project, &member).await;
    let (a, b) = tokio::join!(run::tick(&state), run::tick(&state));
    a.unwrap();
    b.unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from notification_deliveries where incident_id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 2, "two members, one delivery each");
}

#[sqlx::test]
async fn an_outage_never_replays_missed_steps_as_a_burst(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "repeat_count": 10,
        "steps": [one_step(0, json!([{"type": "all_members"}]))]})).await;
    let id = open(&app, project, &member).await;
    // The worker was down for three hours: the first step is long overdue.
    sqlx::query("update incidents set next_escalation_at = now() - interval '3 hours' where id = $1::uuid")
        .bind(&id).execute(&pool).await.unwrap();
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&email, 1).await.len(), 1, "one pass now, not one per missed repeat");
    let wait: f64 = sqlx::query_scalar("select extract(epoch from next_escalation_at - now())::float8 from incidents where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert!((180.0..=301.0).contains(&wait), "the next repeat is ~5 min from now, got {wait}s");
}
```

Run: engine command `--test incidents escalation_engine::`. Expected: FAIL.

- [ ] **Step 2: Implement** `run.rs`, the `resolve` hook, `resolve_source_with_reason` and `kick` as specified above. The drain fills `IncidentSummary.resolution_reason` for `resolved` deliveries from `select message from incident_events where incident_id = $1 and kind = 'resolved' order by id desc limit 1` (R30). `kick` takes its `KICKS` permit (Task 5), then runs `escalation::run::tick` and then `deliveries::drain`, logging each error separately: a failed tick never skips the drain. Add a test in `tests/incidents/escalation_engine.rs`: `a_resolution_with_a_reason_notifies_the_paged_with_it`. A monitor incident whose step already emailed alice, resolved with `resolve_source_with_reason(.., "alert disabled")`, queues one `resolved:g0` delivery for alice. After `run_incident_ticks` her second email is `[Nami] Resolved: <title>` and its body contains `Reason: alert disabled`. The last event is `resolved` with message `alert disabled` and metadata `{"reason": "alert disabled"}`.
- [ ] **Step 3: Run** engine command `--test incidents`, then engine command `--lib -- incidents`, then the full suite and clippy. Expected: green.
- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests/incidents/escalation_engine.rs nami-engine/tests/common/mod.rs
git commit -m "feat(engine): escalation engine with schedules, repeats, fallback and resolve notices (NAMI-INC-9)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10 (NAMI-INC-10): Route monitors, heartbeats and log alerts through escalation policies and channels

**Files:**
- Create: `nami-engine/migrations/NNNN_uptime_escalation.sql`, `nami-engine/migrations/NNNN_logs_escalation.sql` (both numbered after `NNNN_incidents_part1.sql`, R2)
- Modify:
  - `nami-engine/src/incidents/model.rs`: the six wrappers read the source's `escalation_policy_id`/`severity_id` and pass them to `lifecycle::open` (and the heartbeat reopen of Task 1).
  - `nami-engine/src/uptime/alerts.rs`: `notify_transition` calls `incidents::worker::kick`. `render` and its unit tests are removed, including Logs Part 2's `log_alert_emails_say_triggered_and_resolved_and_link_to_the_alert` (planned in NAMI-LOG-14) and Uptime Phase 3's `heartbeat_output_is_quoted_line_by_line_between_since_and_details` / `details_are_sanitized_again_when_rendered`. Their properties live in `incidents::format`'s tests (Task 4). `sanitize_cause`, `Transition` (with its `details`) and `Subject` stay: the worker's `log_dropped` and the call sites use them. What happens to the flap control is the open question below.
  - `nami-engine/src/logs/alert/mod.rs`: the R-P2-27 seam. `resolve_alert_incident(.., notify: false)` calls `lifecycle::resolve_source_with_reason(.., "alert disabled")` instead of the wrapper (R17, controller ruling). `open_alert_incident` and the `notify: true` path are unchanged. `src/logs/alert/routes.rs` (planned in NAMI-LOG-14): the PATCH handler calls `incidents::worker::kick(&state).await` after the update commits.
  - `docs/superpowers/plans/2026-10-13-log-management-part2.md` is not edited (its phase has merged before this one starts). The Part 1 decisions doc records that R17 supersedes R-P2-27's silent disable.
  - `nami-engine/src/main.rs`: spawn `incidents::worker::run_forever(state.clone())` when `state.config.incidents_worker_enabled`, next to the uptime worker block (`src/main.rs:107-109`), with the same two log lines (`"incidents worker started"` / `"incidents worker disabled (INCIDENTS_WORKER_ENABLED=false)"`). R23, R27.
  - `nami-engine/src/uptime/monitor/{model,request,routes}.rs`, `nami-engine/src/uptime/heartbeat/{model,routes}.rs` and `nami-engine/src/logs/alert/{model,routes}.rs`: the two new optional fields in create/patch bodies and views.
  - The existing tests that assert alert subjects (R18, list as of `main` in Drift (2026-10-24)): `tests/uptime/uptime_worker.rs`, `tests/uptime/heartbeats.rs`, `tests/uptime/recovery_threshold.rs`, `tests/uptime/uptime_worker_timing.rs`, `tests/logs/log_alerts.rs` (planned in NAMI-LOG-14). `tests/uptime/heartbeat_pings.rs` keeps its `Output:` body assertion; `tests/uptime/maintenance_windows.rs` and `tests/uptime/checker_http_config.rs` count mails and keep their counts.
  - The `tests/uptime/uptime_worker.rs` tests that pin the old SMTP path's mechanics (Step 4 lists them), and the flap-control tests (open question below).
  - `nami-engine/tests/core/migrations.rs`.
- Test: `nami-engine/tests/incidents/incident_routing.rs` (`mod incident_routing;` in `tests/incidents/main.rs`)

**Drift (2026-10-24): flap control. Open question for the controller.** Since this plan was drafted, Uptime Phase 3 added per-subject flap control to the email path. `notify_transition` (`src/uptime/alerts.rs:53-66`) asks `state.alert_throttle` (`src/uptime/alert_throttle.rs`, `ALERT_FLAP_WINDOW` = 5 min at `:40`) and holds a DOWN email back when one went out for that monitor or heartbeat inside the window. `send_deferred` / `run_deferred_alerts` (`src/uptime/alerts.rs:75`, `:102`), spawned on every instance at `src/main.rs:84-91`, send it later if the subject is still down. Two tests pin the property "each member gets at most one DOWN and one recovery email per subject per window, and a subject that stays down is reported one window late": `tests/uptime/uptime_worker.rs:1663` (`a_flapping_monitor_sends_one_pair_per_window_and_reports_staying_down_later`) and `tests/uptime/heartbeats.rs:3263` (`a_flapping_heartbeat_keeps_one_incident_and_sends_one_pair_of_emails`). Routing through escalation replaces that email path. Without a decision, every flap of a monitor opens an armed incident that pages, so Step 4's "fix the code, not the test" rule cannot be met. **Recommended default, to confirm before Task 10 starts:** move the window into escalation arming. `lifecycle::open` and `reopen_source_within` arm the first step no sooner than `ALERT_FLAP_WINDOW` after the start of the same source's previous triggered pass (read from `notification_deliveries` of that source's incidents). An incident that resolves before its delayed step has notified nobody, so it sends no "Resolved" notice either; one that stays down is paged once the window has passed. `AlertThrottle`, `send_deferred`, `run_deferred_alerts`, `AppState.alert_throttle` and the `main.rs` spawn are then removed with `render`, and the two tests keep their property with the new subjects (one `[Nami] [high] … is down` and one `[Nami] Resolved: …` per member per window, the stay-down notice after the window via `make_escalation_due` and `run_incident_ticks`). Manual incidents are not throttled. Whatever the controller rules is recorded as a new ruling (R31) and in the decisions doc.

**Interfaces:**
- Consumes: Tasks 1-9. Logs Part 2 (planned in NAMI-LOG-14): `log_alerts`, `logs::alert::{claim_due_alerts, apply_evaluation, resolve_alert_incident}`. Uptime Phase 3: `incidents.details` and `attach_details` (unchanged; `format::email` renders it, Task 4).
- Produces:
  - `Monitor`/`MonitorView`, `Heartbeat`/`HeartbeatView` and `LogAlert` gain `escalation_policy_id: Option<Uuid>` and `severity_id: Option<Uuid>`.
  - Create bodies accept them as optional fields. Patch bodies accept them as `Option<Option<Uuid>>` (`null` clears, so the project default applies; deserialized with `core::validate::nullable`, like `group_id`).
  - An id from another project is rejected exactly the way a foreign `group_id` is today in `create_monitor` (`src/uptime/monitor/routes.rs:163-165`): `404` from a `belongs_to(&state.db, project_id, id)` check before the write, plus the existing `23503 → 404` mapping for a row deleted in between. Use `incidents::escalation::model::belongs_to` / `incidents::severity::model::belongs_to`, both generic over `impl PgExecutor` like `uptime::group::model::belongs_to` (`src/uptime/group/model.rs:95`). A policy or severity can never move between projects, so the check cannot go stale.
  - `alerts::notify_transition(state, transition)` keeps its signature. Its body is now `crate::incidents::worker::kick(state).await`, because the incident and its deliveries were already written in the source's transaction.

- [ ] **Step 1: Write the migrations**

`nami-engine/migrations/NNNN_uptime_escalation.sql`:

```sql
-- Monitors and heartbeats pick an escalation policy and a severity for the
-- incidents they open (NAMI-INC-10). Null = the project default; deleting the
-- policy or severity falls back to the default. Runs after
-- NNNN_incidents_part1.sql, which creates both referenced tables.
alter table monitors
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
alter table heartbeats
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
```

`nami-engine/migrations/NNNN_logs_escalation.sql`:

```sql
-- Log alerts pick an escalation policy and a severity for the incidents they
-- open (NAMI-INC-10), as monitors and heartbeats do.
alter table log_alerts
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
```

No disarm statement: R6's update already leaves every unresolved incident unarmed at deploy (R27). Migration test `deleting_a_policy_clears_it_on_monitors`.

- [ ] **Step 2: Write the failing routing tests**

`nami-engine/tests/incidents/incident_routing.rs`:

```rust
// Monitors, heartbeats and log alerts open incidents through escalation.
// Requires `docker compose up -d postgres clickhouse mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, set_policy, settled_mail, state_with_clickhouse, test_app,
    user_with_role};
use nami_engine::{logs::alert, uptime::{alerts as uptime_alerts, worker}};
use serde_json::json;
use uuid::Uuid;
use wiremock::{matchers::any, Mock, MockServer, ResponseTemplate};

async fn severity(pool: &sqlx::PgPool, project: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar("select id from incident_severities where project_id = $1 and name = $2")
        .bind(project).bind(name).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn a_monitor_pages_through_its_own_policy_with_its_severity(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, policy) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin), Some(json!({
        "name": "api team", "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]}))).await;
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    let critical = severity(&pool, project, "critical").await;
    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/monitors/{monitor}"), Some(&admin),
        Some(json!({"escalation_policy_id": policy["id"], "severity_id": critical}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["severity_id"], json!(critical));

    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let mail = settled_mail(&alice_email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [critical] Checkout API is down");
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "not on this policy");
    let (source, policy_id): (String, Uuid) = sqlx::query_as(
        "select source, escalation_policy_id from incidents where monitor_id = $1").bind(monitor).fetch_one(&pool).await.unwrap();
    assert_eq!((source.as_str(), policy_id.to_string()), ("monitor", policy["id"].as_str().unwrap().to_string()));

    server.reset().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail[1].0, "[Nami] Resolved: Checkout API is down");
}

#[sqlx::test]
async fn an_acknowledged_monitor_incident_blocks_a_second_one_until_it_recovers(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let monitor = seed_http_monitor(&pool, project, "api", &server.uri(), 1).await;
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let id: Uuid = sqlx::query_scalar("select id from incidents where monitor_id = $1").bind(monitor).fetch_one(&pool).await.unwrap();
    call(&app, "POST", &format!("/projects/{project}/incidents/{id}/acknowledge"), Some(&member), Some(json!({}))).await;
    // Force the monitor through up -> down again while the incident is acknowledged.
    sqlx::query("update monitors set current_status = 'up' where id = $1").bind(monitor).execute(&pool).await.unwrap();
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from incidents where monitor_id = $1").bind(monitor).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn a_missed_heartbeat_pages_the_default_policy(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, hb) = call(&app, "POST", &format!("/projects/{project}/heartbeats"), Some(&member),
        Some(json!({"name": "nightly backup", "period_seconds": 60, "grace_seconds": 0}))).await;
    assert_eq!(status, StatusCode::CREATED, "{hb}");
    sqlx::query("update heartbeats set created_at = now() - interval '1 hour' where id = $1::uuid")
        .bind(hb["id"].as_str().unwrap()).execute(&pool).await.unwrap();
    worker::sweep_heartbeats(&state).await.unwrap();
    let mail = settled_mail(&email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [high] nightly backup missed its heartbeat");
}

#[sqlx::test]
async fn a_log_alert_opens_its_incident_on_its_policy_and_severity(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let policy = set_policy(&app, project, &admin, json!({"name": "Default",
        "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]})).await;
    let low = severity(&pool, project, "low").await;
    let alert: Uuid = sqlx::query_scalar(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds, severity_id)
         values ($1, 'Checkout errors', 10, 'gt', 300, 60, $2) returning id")
        .bind(project).bind(low).fetch_one(&pool).await.unwrap();
    let due = alert::claim_due_alerts(&pool, 10).await.unwrap();
    let transition = alert::apply_evaluation(&pool, &due[0], 12, chrono::Utc::now()).await.unwrap().unwrap();
    uptime_alerts::notify_transition(&state, &transition).await;
    let mail = settled_mail(&alice_email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [low] Log alert: Checkout errors");
    let (source, policy_id): (String, Uuid) = sqlx::query_as(
        "select source, escalation_policy_id from incidents where log_alert_id = $1").bind(alert).fetch_one(&pool).await.unwrap();
    assert_eq!((source.as_str(), policy_id), ("log_alert", policy));
}

#[sqlx::test]
async fn foreign_policies_and_severities_are_refused_on_sources(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let foreign_policy: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1").bind(other).fetch_one(&pool).await.unwrap();
    let foreign_severity = severity(&pool, other, "low").await;
    for body in [json!({"escalation_policy_id": foreign_policy}), json!({"severity_id": foreign_severity})] {
        let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/monitors/{monitor}"), Some(&admin), Some(body)).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND, "{status}");
    }
}
```

`state_with_clickhouse(&pool)` takes a reference and returns `(AppState, ClickHouse)` bound to a fresh per-test database. Each test above that calls it ends with `ch.drop_database().await.unwrap();` (the convention in `tests/uptime/uptime_worker.rs`). `make_due` and the heartbeat create body follow the existing helpers and API in `tests/common/mod.rs` and `tests/uptime/heartbeats.rs`. Where a field name differs (for example `grace_seconds`), use the existing one.

Run: engine command `--test incidents incident_routing::`, then `--test core migrations::`. Expected: FAIL. (After Step 3, also run `--test logs --test uptime` and expect only the Step 4 failures.)

- [ ] **Step 3: Implement**

The wrappers fetch `(escalation_policy_id, severity_id)` from `monitors` / `heartbeats` / `log_alerts` by id on the same connection, then call `lifecycle::open`. `notify_transition`:

```rust
/// Incidents are opened and resolved, and their notifications queued, inside the
/// transaction that changed the source's state (incidents::lifecycle). This only
/// wakes the incidents worker so they go out now instead of at its next tick.
pub async fn notify_transition(state: &AppState, _transition: &Transition) {
    crate::incidents::worker::kick(state).await;
}
```

Delete `render` and its unit tests. The escaping they covered (CR/LF in names never reaching a subject) is covered by `incidents::format` (`email_subject_and_body_carry_severity_and_links_on_single_lines`). The heartbeat output quoting is covered by `a_cause_cannot_forge_a_link_line_and_job_output_is_quoted`. Fix the uses of `render` that the compiler reports. Logs Part 2's `log_alert_emails_say_triggered_and_resolved...` test and NAMI-UP-21's two `render` tests go with it.

The log-alert seam (R17, controller ruling): in `logs::alert::resolve_alert_incident`, `notify: false` calls `lifecycle::resolve_source_with_reason(conn, &IncidentSource::LogAlert(alert_id), at, "alert disabled")` and returns `None`. Logs Part 2's "no email on disable" behaviour is **superseded** from this task on. Its disable test in `tests/logs/log_alerts.rs` (the alert disabled while alerting resolves its incident) is updated in Step 4 to expect, after `common::run_incident_ticks`, exactly one `[Nami] Resolved: Log alert: <name>` email per member who got the triggered email, with `Reason: alert disabled` in the body. A member never paged gets nothing.

Add to `tests/incidents/incident_routing.rs`:

```rust
#[sqlx::test]
async fn disabling_an_alerting_log_alert_resolves_and_tells_the_paged_why(pool: sqlx::PgPool) {
    let state = nami_engine::core::state::AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default",
        "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]})).await;
    let alert: Uuid = sqlx::query_scalar(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds)
         values ($1, 'Checkout errors', 10, 'gt', 300, 60) returning id")
        .bind(project).fetch_one(&pool).await.unwrap();
    let due = alert::claim_due_alerts(&pool, 10).await.unwrap();
    let transition = alert::apply_evaluation(&pool, &due[0], 12, chrono::Utc::now()).await.unwrap().unwrap();
    uptime_alerts::notify_transition(&state, &transition).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1);

    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{alert}"), Some(&admin),
        Some(json!({"enabled": false}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    common::run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail[1].0, "[Nami] Resolved: Log alert: Checkout errors");
    assert!(mail[1].1.contains("Reason: alert disabled"), "{}", mail[1].1);
    let (status, reason): (String, Option<String>) = sqlx::query_as(
        "select i.status, e.message from incidents i join incident_events e on e.incident_id = i.id and e.kind = 'resolved'
         where i.log_alert_id = $1").bind(alert).fetch_one(&pool).await.unwrap();
    assert_eq!((status.as_str(), reason.as_deref()), ("resolved", Some("alert disabled")));
}
```

- [ ] **Step 4: Update the existing assertions (R18)**

Run the full engine suite. Expect two kinds of failure.

**(a) Subject strings.** Rewrite each subject to the new form. Counts stay the same: one email per member per transition, because the default policy pages `all_members` once, and a resolution notifies each of them once.
- `"[Nami] X is DOWN"` → `"[Nami] [high] X is down"`
- `"[Nami] X is back UP"` → `"[Nami] Resolved: X is down"`
- heartbeats (sweep): `"[Nami] [high] X missed its heartbeat"` / `"[Nami] Resolved: X missed its heartbeat"`
- heartbeats (failure ping, NAMI-UP-20): `"[Nami] [high] X reported a failure"` / `"[Nami] Resolved: X reported a failure"`
- log alerts: `"[Nami] [high] Log alert: X"` / `"[Nami] Resolved: Log alert: X"`
- Logs Part 2's disable test: from "no email" to one `Resolved` email with `Reason: alert disabled` per paged member (R17, superseding R-P2-27's silent disable).
- `a_name_written_around_the_api_cannot_inject_headers_or_recipients`: unsafe characters now become spaces instead of being dropped, so the subject is `format!("[Nami] [high] API  Bcc: {victim}  Cc: {victim} is down")`. Its recipient/header assertions stay as they are.

Body assertions that looked for `Cause: …` keep working (the format keeps that line). NAMI-UP-21's `Output:\n> …\n` assertion keeps working (Task 4).

**(b) Tests that pin the old SMTP path's mechanics** (`tests/uptime/uptime_worker.rs`). The old path sent one email after another from the alert task and gave up after one timeout. The new path queues deliveries, sends up to `MAX_SENDS_IN_FLIGHT` at once and retries per R12. Rewrite exactly these tests to keep the property named here. Do not weaken anything else.
- `a_hung_smtp_server_does_not_hold_up_the_checks`: keep the property "two passes finish in < 5 s while SMTP never answers, and the monitor ends up". Drop the `notifications.reap() == 2` count: the second kick may find nothing to send.
- `alerts_to_a_hung_smtp_server_are_given_up_on_after_the_timeout`: keep "`run_due_checks` ends (well under 20 s) although SMTP is hung". The two recipients' sends now run concurrently: assert `elapsed >= 700 ms` (one mailer timeout) instead of `>= 1,300 ms`. Both deliveries end `pending` with `attempts = 1` and `last_error = 'email not delivered'`.
- `a_failed_alert_is_logged_once_and_without_the_recipients_address`: keep "no log line contains the recipient's address" and "the mailer logs the cause (`failed to send email`) once". Replace the `alert email not delivered to every recipient` summary assertion with: the delivery is `pending` with `attempts = 1`, and there is exactly one `notification_failed` event whose message is `Email: <recipient>: email not delivered` (R11 allows the user's own address in their own event).
- `a_pass_sends_all_its_alerts_from_one_task` and `alerts_are_dropped_and_logged_when_too_many_batches_are_still_sending` test the `Notifications` set, which is unchanged. They keep passing as they are. In the second one, the refused batch means no kick runs, so the armed incident sends nothing until the worker loop, which tests do not run. `settled_mail(&recipient, 0)` still sees no email.
- `an_alert_does_not_wait_for_the_rest_of_its_pass`: unchanged apart from the subject (`"[Nami] [high] Refused API is down"`).

**(c) The flap-control tests** (`tests/uptime/uptime_worker.rs:1663`, `tests/uptime/heartbeats.rs:3263`, and the reopen tests at `tests/uptime/heartbeats.rs:3300` and `:3381` where they count emails). Rewrite them only to the property the controller rules on (the open question above); without a ruling, stop before this step.

Any other failure is a behaviour change: stop and fix the code, not the test.

- [ ] **Step 5: Run** the full engine suite and clippy. Expected: green.
- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/NNNN_uptime_escalation.sql nami-engine/migrations/NNNN_logs_escalation.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): monitors, heartbeats and log alerts page through escalation policies (NAMI-INC-10)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11 (NAMI-INC-11): Signed, single-use acknowledge/resolve links in emails, and the confirmation page

**Files:**
- Append: `nami-engine/migrations/NNNN_incidents_part1.sql` (token uses)
- Create: `nami-engine/src/incidents/action/mod.rs`, `nami-engine/src/incidents/action/token.rs`, `nami-engine/src/incidents/action/routes.rs`, `nami-ui/src/routes/incidents/action/+page.server.ts`, `nami-ui/src/routes/incidents/action/+page.svelte`, `nami-ui/src/routes/incidents/action/action.test.ts`
- Modify:
  - `nami-engine/src/incidents/deliveries.rs`: implicit-email deliveries of `triggered` events carry `ActionLinks` for that user when they are a `member`/`admin` of the project or a global admin.
  - `nami-engine/src/incidents/worker.rs`: prune token uses older than 2 days, once an hour.
  - `nami-engine/src/core/router.rs`: merge behind a governor of its own, `rate_limited_actions(router)`, the same shape as `rate_limited_with_governor` with `SmartIpKeyExtractor`, `.per_second(3).burst_size(20)` (R16; `per_second(n)` is the replenish interval in seconds). `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (both routes, without a session).
  - `nami-engine/src/core/error.rs`: `AppError::Gone(String)` → `410 {"error": msg}`. There is no 410 variant on `main`.
- Test: `nami-engine/tests/incidents/incident_actions.rs` (+ unit tests in `action/token.rs`), `action.test.ts`

**Interfaces:**
- Consumes: `core::crypto::Cipher::derive_subkey`, Task 1 `lifecycle::{acknowledge, resolve}`, `rbac` role ranks.
- Produces:

```rust
// incidents::action::token
pub const TOKEN_TTL: chrono::Duration = chrono::Duration::hours(24);
pub const SUBKEY_INFO: &str = "nami/incident-action/v1";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action { Acknowledge, Resolve }   // "ack" | "resolve" in the payload
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims { pub incident_id: Uuid, pub action: Action, pub user_id: Uuid, pub expires_at: DateTime<Utc> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid { Malformed, BadSignature, Expired }
/// Adds a fresh 9-byte random nonce to the payload (R16); `Claims` does not carry it.
pub fn sign(cipher: &Cipher, claims: &Claims) -> String;
pub fn verify(cipher: &Cipher, token: &str, now: DateTime<Utc>) -> Result<Claims, Invalid>;
pub fn hash(token: &str) -> [u8; 32];   // SHA-256, the single-use key
pub fn links(cipher: &Cipher, ui_base_url: &str, incident_id: Uuid, user_id: Uuid, now: DateTime<Utc>) -> ActionLinks;
// links(): "{base}/incidents/action?token={sign(..)}" for each action, expiring now + TOKEN_TTL.
```

- Routes (private API, no session, behind the governor's per-IP limit):
  - `POST /incident-actions/inspect` `{token}` → `200 {"action": "acknowledge"|"resolve", "incident": {id, project_id, project_name, title, status, severity, started_at}, "user_email"}`. Never mutates.
  - `POST /incident-actions/confirm` `{token}` → `200 {"status": "<new status>"}`.
  - Errors (both):
    - `400 {"error": "this link is invalid or has expired"}` for malformed, bad signature, expired, unknown incident, or a token issued before the user's last password change (`exp - TOKEN_TTL < date_trunc('second', users.password_changed_at)`, compared at whole seconds because `exp` is a unix second: a link signed in the same second as an account's creation must still work).
    - `403 {"error": "you no longer have access to this incident"}` when the user is not a project `member`/`admin` and not a global admin.
    - `410 {"error": "this link was already used"}`.
    - `409 {"error": "the incident is already <status>"}` when the action no longer applies. This does **not** consume the token.

`confirm` runs in one transaction, in this order (R16):
1. Verify the token (no database work for a bad one).
2. Load and lock the incident (`select ... for update`). Unknown → 400 as above. The user's `password_changed_at` check also happens here.
3. Check the role. A user who is no longer a `member`/`admin` and not a global admin → 403.
4. `select exists(select 1 from incident_action_token_uses where token_hash = $1)` → 410 `this link was already used`. A used link always says "used", even though its action no longer applies either.
5. Run `lifecycle::acknowledge`/`resolve` with the token's user. `false` → roll back, 409 (a fresh token is not consumed).
6. `insert into incident_action_token_uses (token_hash, incident_id, expires_at) values (...) on conflict do nothing returning token_hash`. No row → roll back, 410. The state change in step 5 is undone with it.
7. Commit and `kick` the worker (a resolve queues notices).

Concurrent confirms of one token serialise on the row lock of step 2. The second one sees the stored use (410) and never acts twice. The primary key of step 6 backs this up should a future change drop the lock. `inspect` runs steps 1-3 and never writes. The key is `Zeroizing::new(cipher.derive_subkey(SUBKEY_INFO))`, derived per call and wiped on drop.

- [ ] **Step 1: Migration**

Append to `nami-engine/migrations/NNNN_incidents_part1.sql`:

```sql
-- Action links are single-use (NAMI-INC-11): the SHA-256 of each confirmed
-- token is kept until well after it would have expired anyway.
create table incident_action_token_uses (
    token_hash bytea primary key check (octet_length(token_hash) = 32),
    incident_id uuid not null references incidents(id) on delete cascade,
    expires_at timestamptz not null,
    used_at timestamptz not null default now()
);
create index incident_action_token_uses_expires_idx on incident_action_token_uses (expires_at);
```

- [ ] **Step 2: Failing unit tests** in `action/token.rs` `mod tests`:

```rust
use super::*;
use crate::core::crypto::{Cipher, TEST_KEY};

fn claims(action: Action, exp_minutes: i64) -> Claims {
    Claims { incident_id: Uuid::from_u128(1), action, user_id: Uuid::from_u128(2),
        expires_at: DateTime::from_timestamp(1_792_000_000 + exp_minutes * 60, 0).unwrap() }
}
fn now() -> DateTime<Utc> { DateTime::from_timestamp(1_792_000_000, 0).unwrap() }

#[test]
fn a_signed_token_verifies_to_its_claims() {
    let cipher = Cipher::new(&TEST_KEY);
    let token = sign(&cipher, &claims(Action::Acknowledge, 60));
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)), "URL-safe: {token}");
    assert_eq!(verify(&cipher, &token, now()), Ok(claims(Action::Acknowledge, 60)));
}

#[test]
fn tampered_expired_and_foreign_key_tokens_are_rejected() {
    let cipher = Cipher::new(&TEST_KEY);
    let token = sign(&cipher, &claims(Action::Acknowledge, 60));
    assert_eq!(verify(&cipher, &token, now() + chrono::Duration::minutes(61)), Err(Invalid::Expired));
    let other = Cipher::new(b"another-encryption-key-32bytes!!");
    assert_eq!(verify(&other, &token, now()), Err(Invalid::BadSignature));
    // Swap the action inside the payload, keep the MAC.
    let (payload, mac) = token.split_once('.').unwrap();
    let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    let forged = URL_SAFE_NO_PAD.encode(decoded.replace("|ack|", "|resolve|"));
    assert_eq!(verify(&cipher, &format!("{forged}.{mac}"), now()), Err(Invalid::BadSignature));
    for junk in ["", ".", "a.b", "x", &"A".repeat(5000), &format!("{token}.extra")] {
        assert!(matches!(verify(&cipher, junk, now()), Err(Invalid::Malformed | Invalid::BadSignature)), "{junk}");
    }
}

#[test]
fn the_action_key_is_its_own_subkey() {
    let cipher = Cipher::new(&TEST_KEY);
    assert_ne!(cipher.derive_subkey(SUBKEY_INFO), cipher.derive_subkey("nami/hmac/v1"));
}
```

(`URL_SAFE_NO_PAD` is `base64::engine::general_purpose::URL_SAFE_NO_PAD`.) The MAC is checked with `Mac::verify_slice` (constant time) **before** the payload is parsed. A payload over 256 bytes is `Malformed` before any HMAC work.

- [ ] **Step 3: Failing API tests**

`nami-engine/tests/incidents/incident_actions.rs`:

```rust
// Acknowledge/resolve links. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::StatusCode;
use chrono::Utc;
use common::{call, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app, user_with_role};
use nami_engine::{incidents::action::token::{self as action_token, Action, Claims}, core::state::AppState};
use serde_json::json;
use uuid::Uuid;

fn token(state: &AppState, incident: Uuid, user: Uuid, action: Action) -> String {
    action_token::sign(&state.cipher, &Claims { incident_id: incident, action, user_id: user,
        expires_at: Utc::now() + action_token::TOKEN_TTL })
}

#[sqlx::test]
async fn emails_carry_links_that_acknowledge_without_a_session_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 1).await;
    let ack_line = mail[0].1.lines().find(|l| l.starts_with("Acknowledge: ")).expect("ack link").to_string();
    let link = ack_line.trim_start_matches("Acknowledge: ");
    assert!(link.starts_with(&format!("{}/incidents/action?token=", state.config.ui_base_url.trim_end_matches('/'))));
    let token = link.split("token=").nth(1).unwrap();

    let (status, inspected) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!((inspected["action"].as_str(), inspected["incident"]["status"].as_str()), (Some("acknowledge"), Some("open")));
    let status_after_inspect: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(status_after_inspect, "open", "inspect never mutates");

    let (status, done) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!((status, done["status"].as_str()), (StatusCode::OK, Some("acknowledged")), "{done}");
    let by: Option<Uuid> = sqlx::query_scalar("select acknowledged_by from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(by, Some(alice));
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::GONE);
}

#[sqlx::test]
async fn viewers_get_no_links_and_their_tokens_do_nothing(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (viewer, viewer_email, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open_test_incident(&pool, project, member).await;
    run_incident_ticks(&state).await;
    assert!(!settled_mail(&viewer_email, 1).await[0].1.contains("Acknowledge:"));
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None,
        Some(json!({"token": token(&state, id, viewer, Action::Resolve)}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_token_of_a_user_no_longer_in_the_project_is_refused(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Resolve);
    sqlx::query("delete from project_members where user_id = $1").bind(alice).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": t}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_password_change_revokes_outstanding_links(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Acknowledge);
    sqlx::query("update users set password_changed_at = now() + interval '1 second' where id = $1").bind(alice).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": t}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn a_token_is_bound_to_its_incident_and_action(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let other = open_test_incident(&pool, project, alice).await;
    let ack = token(&state, id, alice, Action::Acknowledge);
    call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": ack}))).await;
    let statuses: Vec<String> = sqlx::query_scalar("select status from incidents where id = any($1) order by id = $2 desc")
        .bind(vec![id, other]).bind(id).fetch_all(&pool).await.unwrap();
    assert_eq!(statuses, ["acknowledged", "open"], "only its own incident changed");
    // A second, distinct link for the same action (the nonce makes every token distinct, R16).
    let late_ack = token(&state, id, alice, Action::Acknowledge);
    assert_ne!(late_ack, ack);
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": late_ack}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "already acknowledged");
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": late_ack}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "a 409 does not consume the token");
    for bad in [json!({"token": "nope"}), json!({}), json!({"token": 5})] {
        let (status, _) = call(&app, "POST", "/incident-actions/inspect", None, Some(bad)).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[sqlx::test]
async fn a_get_never_acknowledges(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Acknowledge);
    let (status, _) = call(&app, "GET", &format!("/incident-actions/confirm?token={t}"), None, None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let still: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(still, "open");
}
```

- [ ] **Step 4: The nami-ui page** (outside `(app)`, no login)

`nami-ui/src/routes/incidents/action/+page.server.ts`:
- `load`:
  - Read `token` from `url.searchParams` (missing or > 1,024 chars → `error(400, 'This link is invalid.')`).
  - Set `{'cache-control': 'private, no-store', 'referrer-policy': 'no-referrer', 'x-robots-tag': 'noindex'}` once.
  - POST `/incident-actions/inspect` with `{token}` through `callEngine`.
  - Return `{ inspected, token }` on 200, or `{ problem: <engine message> }` on 400/403/410. Other statuses → `error(500)`.
- `actions.confirm`: read `token` from the form (never from the URL) and POST `/incident-actions/confirm`. Return `{ done: true, status }`, or `fail(status, { message })` via `engineFailure` semantics (but without a login redirect: this page has no session).

`+page.svelte` shows:
- the project, incident title, severity and status;
- one button ("Acknowledge incident" / "Resolve incident") in a `<form method="POST" action="?/confirm">` with a hidden `token` input;
- the result or problem message;
- a link to the incident page (`/projects/{project_id}/incidents/{id}`), which needs a login. It is rendered only when both ids pass `isUuid` (`#lib/core/uuid`). The page takes no redirect or return-to parameter.

The page renders text only through Svelte expressions (escaped). The `confirm` action relies on SvelteKit's origin check for form actions (`kit.csrf.checkOrigin`, default on; the review checks `svelte.config.js` does not disable it). The page lives outside `(app)`, so `(app)/+layout.server.ts` never redirects it to the login page, and no `engineFailure` login redirect applies: a 401 cannot happen on these sessionless routes.

`action.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const inspected = { action: 'acknowledge', incident: { id: 'i', project_id: 'p', project_name: 'Shop', title: 'db down',
	status: 'open', severity: 'high', started_at: '2026-10-19T00:00:00Z' }, user_email: 'a@b.test' };

describe('incident action page', () => {
	it('inspects without mutating and sets no-store and no-referrer once', async () => {
		const { event, calls, setHeaders } = fakeEvent({ url: 'http://ui.test/incidents/action?token=abc', body: JSON.stringify(inspected) });
		expect(await load(event)).toEqual({ inspected, token: 'abc' });
		expect(calls()).toEqual([{ method: 'POST', path: '/incident-actions/inspect', search: '', body: '{"token":"abc"}' }]);
		expect(setHeaders).toHaveBeenCalledTimes(1);
		expect(setHeaders.mock.calls[0][0]).toMatchObject({ 'cache-control': 'private, no-store', 'referrer-policy': 'no-referrer' });
	});

	it('shows the engine problem for used, expired or revoked links', async () => {
		const { event } = fakeEvent({ url: 'http://ui.test/incidents/action?token=abc', status: 410, body: '{"error":"this link was already used"}' });
		expect(await load(event)).toEqual({ problem: 'this link was already used' });
	});

	it('refuses a missing token', async () => {
		await expect(load(fakeEvent({ url: 'http://ui.test/incidents/action' }).event)).rejects.toMatchObject({ status: 400 });
	});

	it('confirms with the token from the form body only', async () => {
		const { event, calls } = fakeEvent({ url: 'http://ui.test/incidents/action?token=fromurl', form: { token: 'fromform' }, body: '{"status":"acknowledged"}' });
		expect(await actions.confirm(event)).toEqual({ done: true, status: 'acknowledged' });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: '/incident-actions/confirm', body: '{"token":"fromform"}' });
	});
});
```

- [ ] **Step 5: Run** engine command `--test incidents --test core`, then engine command `--lib incidents::action::token`, the full engine suite and clippy, then the UI command with `src/routes/incidents/action`. Expected: green.
- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/NNNN_incidents_part1.sql nami-engine/src nami-engine/tests \
  nami-ui/src/routes/incidents/action
git commit -m "feat(engine,ui): signed single-use acknowledge/resolve links with a confirmation page (NAMI-INC-11)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12 (NAMI-INC-12): UI — incident list, detail with timeline, actions, comments, postmortem, manual incidents

**Files:**
- Create: `nami-ui/src/lib/incidents/markdown.ts` (+ `markdown.test.ts`), `nami-ui/src/lib/components/SeverityBadge.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/[incidentId=uuid]/+page.server.ts`, `+page.svelte`, `incident-detail.test.ts`
- Modify: `nami-ui/src/lib/incidents/incidents.ts` (+ `incidents.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/+page.server.ts`, `+page.svelte`, and its two tests. Keep Logs Part 2's log-alert link on list rows (NAMI-LOG-18) and NAMI-UP-29's `<details><summary>Job output</summary><pre>…</pre></details>` block (R26), with their existing test cases.
- Test: the files above

**Interfaces:**
- Consumes: Task 2 routes; Task 8 policies (for the "New incident" policy select: `GET /projects/{id}/escalation-policies`).
- Produces:

```ts
// lib/incidents/incidents.ts
export const STATUSES = ['open', 'acknowledged', 'resolved'] as const;
export const SOURCES = ['monitor', 'heartbeat', 'log_alert', 'metric_alert', 'manual', 'integration', 'report_form'] as const;
export type ListTab = 'unresolved' | 'acknowledged' | 'resolved' | 'all';
export function parseListQuery(url: URL): { tab: ListTab; source: string | null; cursor: string | null };
export function engineListQuery(q: ReturnType<typeof parseListQuery>): string;   // "?status=..&source=..&cursor=.."
export function eventLabel(kind: string): string;          // "opened" → "Incident opened", unknown → kind
export function duration(startedAt: string, endedAt: string | null, now: Date): string;  // "3 h 12 min", "45 s"
export function sourceLink(projectId: string, i: Incident): string | null;  // monitor/heartbeat/log alert page
// lib/incidents/markdown.ts
export function renderMarkdown(source: string): string;   // R22: escaped HTML of a fixed subset
```

Page behaviour:
- **List** (`/projects/{id}/incidents`):
  - Tabs Unresolved (default), Acknowledged, Resolved, All. A source `<select>`. "Load more" carries the cursor in the query string.
  - Columns: severity badge, title (link to detail), source with link, started, duration, acknowledged by, status.
  - A "New incident" form, shown when `data.role` is `member` or `admin` (R28; viewers do not see it, and an engine 403 still surfaces as the form error): title, description, severity, escalation policy. The `create` action redirects to the new incident.
  - Query values are only forwarded after `parseListQuery` keeps them (known tab/source, cursor `^[A-Za-z0-9_-]{1,200}$`).
- **Detail** (`/projects/{id}/incidents/{incidentId}`):
  - Header with title, severity badge, status, started/duration, source link, acknowledged/resolved by.
  - Buttons: Acknowledge (open), Resolve (open/acknowledged), Reopen (resolved). Each is a named action posting `{}`. They, the severity select, the comment form and the postmortem editor render only when `data.role` is `member` or `admin` (R28). An engine 403 still surfaces, and `incident-detail.test.ts` renders the page for `viewer` (no action forms) and `member`.
  - Severity `<select>` (action `severity`).
  - Timeline, oldest first: event label, actor email, time (UTC), and message. Messages render as text (Svelte escapes); comments keep line breaks with `white-space: pre-wrap`.
  - Comment form (action `comment`).
  - Postmortem: rendered with `{@html renderMarkdown(postmortem)}`, the only `{@html}` in the feature, plus an edit form (textarea with `defaultValue`, action `postmortem`).
  - Cause shown in a `<pre>` (escaped). When `details` is set, the same `Job output` `<details>` block as the list (R26), text only.
  - The incident list's "New incident" policy select loads `GET /projects/{id}/escalation-policies`. Policy and severity ids from forms pass `isUuid` before `enginePath`.
  - Load sets `cache-control: private, no-store`.

- [ ] **Step 1: Failing tests**

`markdown.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
	it('renders the supported subset', () => {
		expect(renderMarkdown('# Cause\n\nThe **db** was *slow*, see `pg_stat`.\n\n- one\n- two'))
			.toBe('<h1>Cause</h1><p>The <strong>db</strong> was <em>slow</em>, see <code>pg_stat</code>.</p><ul><li>one</li><li>two</li></ul>');
		expect(renderMarkdown('```\nselect 1;\n```')).toBe('<pre><code>select 1;</code></pre>');
		expect(renderMarkdown('[runbook](https://wiki.test/a?b=1&c=2)'))
			.toBe('<p><a href="https://wiki.test/a?b=1&amp;c=2" rel="noopener noreferrer nofollow">runbook</a></p>');
	});

	it.each([
		['<script>alert(1)</script>', '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>'],
		['<img src=x onerror=alert(1)>', '<p>&lt;img src=x onerror=alert(1)&gt;</p>'],
		['[x](javascript:alert(1))', '<p>[x](javascript:alert(1))</p>'],
		['[x](data:text/html,<b>)', '<p>[x](data:text/html,&lt;b&gt;)</p>'],
		['[x](https://a.test/"onmouseover="alert(1))', '<p><a href="https://a.test/&quot;onmouseover=&quot;alert(1)" rel="noopener noreferrer nofollow">x</a></p>'],
		['**<b>bold</b>**', '<p><strong>&lt;b&gt;bold&lt;/b&gt;</strong></p>'],
		['```\n<script>x</script>\n```', '<pre><code>&lt;script&gt;x&lt;/script&gt;</code></pre>']
	])('never passes HTML or script: %s', (input, expected) => {
		expect(renderMarkdown(input)).toBe(expected);
	});

	it('copes with huge and unbalanced input', () => {
		expect(renderMarkdown('*'.repeat(100_000)).length).toBeGreaterThan(0);
		expect(renderMarkdown('**open')).toBe('<p>**open</p>');
	});
});
```

`incidents.test.ts` (added cases): `parseListQuery` drops an unknown tab/source and a malformed cursor; `engineListQuery` maps `unresolved` → `status=unresolved`, `all` → no status; `duration` formats `45 s`, `12 min`, `3 h 12 min`, `2 d 4 h`; `eventLabel('escalation_fallback')` is `"Moved to fallback policy"`; an unknown kind is returned as is.

`incident-detail.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent, PROJECT_ID } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';

const INCIDENT_ID = '3f0c8a52-7d1e-4b6a-9c2f-5e8d7a6b4c31';
const params = { incidentId: INCIDENT_ID };
const base = `/projects/${PROJECT_ID}/incidents/${INCIDENT_ID}`;

describe('incident detail', () => {
	it('loads the incident and severities, no-store', async () => {
		const { event, calls, setHeaders } = fakeEvent({ params, respond: (url) =>
			new Response(url.pathname.endsWith('/severities') ? '[]' : JSON.stringify({ id: INCIDENT_ID, events: [] })) });
		expect(await load(event)).toMatchObject({ incident: { id: INCIDENT_ID }, severities: [] });
		expect(calls().map((c) => c.path).sort()).toEqual([base, `/projects/${PROJECT_ID}/severities`].sort());
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store' });
	});

	it('404s for an unknown incident', async () => {
		await expect(load(fakeEvent({ params, status: 404, body: '{"error":"not found"}' }).event)).rejects.toMatchObject({ status: 404 });
	});

	it.each(['acknowledge', 'resolve', 'reopen'] as const)('%s posts {} to its engine action', async (name) => {
		const { event, calls } = fakeEvent({ params, form: {} });
		await actions[name](event);
		expect(calls()).toEqual([{ method: 'POST', path: `${base}/${name}`, search: '', body: '{}' }]);
	});

	it('shows a 409 from the engine as the form message', async () => {
		const { event } = fakeEvent({ params, form: {}, status: 409, body: '{"error":"incident is not open"}' });
		expect(await actions.acknowledge(event)).toMatchObject({ status: 409, data: { message: 'incident is not open' } });
	});

	it('posts comments and postmortems as JSON and refuses an empty comment locally', async () => {
		const { event, calls } = fakeEvent({ params, form: { body: ' rolled back ' }, status: 201 });
		await actions.comment(event);
		expect(calls()[0]).toMatchObject({ method: 'POST', path: `${base}/comments`, body: '{"body":"rolled back"}' });
		const empty = fakeEvent({ params, form: { body: '  ' } });
		expect(await actions.comment(empty.event)).toMatchObject({ status: 400 });
		expect(empty.calls()).toEqual([]);
		const pm = fakeEvent({ params, form: { postmortem: '' } });
		await actions.postmortem(pm.event);
		expect(pm.calls()[0]).toMatchObject({ method: 'PUT', path: `${base}/postmortem`, body: '{"postmortem":null}' });
	});

	it('refuses a severity id that is not a UUID before calling the engine', async () => {
		const { event, calls } = fakeEvent({ params, form: { severity_id: '../x' } });
		expect(await actions.severity(event)).toMatchObject({ status: 400 });
		expect(calls()).toEqual([]);
	});
});
```

The list tests cover:
- the load forwards only parsed query values (`?tab=bogus&source=x&cursor=%%` → engine `?status=unresolved`);
- the `create` action posts `{title, description?, severity_id?, escalation_policy_id?}` (empty fields omitted) and redirects 303 to the new incident's page;
- a non-UUID policy id is refused locally.

Run: UI command with `src/lib/incidents/markdown.test.ts src/lib/incidents/incidents.test.ts "src/routes/(app)/projects/[id=uuid]/incidents"`. Expected: FAIL.

- [ ] **Step 2: Implement** the modules and pages.

`renderMarkdown`:
1. Normalise `\r\n` and cap the input at 102,400 chars.
2. Split into blocks: fenced code (```` ``` ````), headings `#{1,6} `, list runs (`- `/`* `/`1. `), paragraphs. Block text is HTML-escaped (`& < > " '`) **before** any inline rule runs.
3. Inline rules in order: code spans, links `[text](http(s)://…)` (the URL must match `^https?://[^\s<>]+$` after escaping; anything else stays literal), `**strong**`, `*em*`/`_em_`. Unbalanced markers stay literal.
4. No other HTML is produced.

- [ ] **Step 3: Run** the UI command with the files above, then `npm run check`. Expected: PASS, 0 errors / 0 warnings.
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): incident list and detail with timeline, actions, comments and postmortems (NAMI-INC-12)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13 (NAMI-INC-13): UI — notification channels, personal and project, with write-only secrets and "send test"

**Files:**
- Create: `nami-ui/src/lib/server/incidents/channel-form.ts` (+ `channel-form.test.ts`), `nami-ui/src/lib/components/ChannelList.svelte`, `nami-ui/src/routes/(app)/account/notifications/+page.server.ts`, `+page.svelte`, `notifications.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/notifications/+page.server.ts`, `+page.svelte`, `project-channels.test.ts`
- Modify: `nami-ui/src/routes/(app)/+layout.svelte` (a "Notifications" link next to the existing Security link, `(app)/security`), `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/+page.svelte` (link to the notifications sub-page)
- Test: the files above

**Interfaces:**
- Consumes: Task 3 and Task 5 routes.
- Produces:

```ts
// lib/server/incidents/channel-form.ts
export const CHANNEL_KINDS = ['email', 'webhook', 'slack', 'teams', 'telegram', 'discord'] as const;
export type ChannelPayload = { name?: string; kind?: string; config?: Record<string, string>; enabled?: boolean };
/** mode 'create': every config field of the kind is required. mode 'edit': config only when every
 *  secret field of the kind is filled (all-or-nothing replace); blank = keep. */
export function channelPayloadFromForm(form: FormData, mode: 'create' | 'edit', scope: 'personal' | 'project'):
	{ ok: true; payload: ChannelPayload } | { ok: false; message: string };
```

The config fields per kind:
- `email`: `address` (project scope only; personal → "Email goes to your account address").
- `webhook`: `url`.
- `slack` / `teams` / `discord`: `webhook_url`.
- `telegram`: `bot_token` and `chat_id`.

Values are trimmed. Secret inputs are `type="password"` with `autocomplete="off"` and are never rendered with a value. On edit they show the placeholder "Leave blank to keep the current value".

Pages:
- Both list channels (name, kind, summary, enabled toggle), with forms to create, edit, "Send test" (shows the engine's `ok` or error), and two-step delete (`confirm=yes`; the first submit returns `{ confirmDelete: id }`).
- After a webhook create, the action result carries `signingSecret` once. It is shown in a copy box with "Shown once. Store it now." and never placed in a URL or a redirect. Under it, the receiver guidance of R29: "Each request has `X-Nami-Timestamp` and `X-Nami-Signature: sha256=<hex>`, the HMAC-SHA256 of `{X-Nami-Timestamp}.{raw body}` with this secret. Reject requests whose timestamp is more than 5 minutes old, and drop repeats of `X-Nami-Delivery`."
- Both loads set `cache-control: private, no-store`.
- Channel ids from forms pass `isUuid` before reaching `enginePath`.
- The project page shows the create/edit/test/delete forms only when `data.role === 'admin'` (R28, controller ruling). Others see the list and the line "Only project admins can change project channels." The actions still surface an engine 403 as the form message (the engine stays the authority). `project-channels.test.ts` renders the page with `role: 'viewer'` (no `<form>`) and `role: 'admin'` (forms), and keeps the case where the engine answers 403 to a create.

- [ ] **Step 1: Failing tests**

`channel-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { channelPayloadFromForm } from './channel-form';

function form(fields: Record<string, string>) {
	const f = new FormData();
	for (const [k, v] of Object.entries(fields)) f.set(k, v);
	return f;
}

describe('channelPayloadFromForm', () => {
	it('builds a create payload per kind', () => {
		expect(channelPayloadFromForm(form({ name: ' Ops ', kind: 'slack', webhook_url: ' https://hooks.slack.com/services/x ' }), 'create', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', kind: 'slack', config: { webhook_url: 'https://hooks.slack.com/services/x' }, enabled: true } });
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '1:abc', chat_id: '42' }), 'create', 'personal'))
			.toMatchObject({ ok: true, payload: { config: { bot_token: '1:abc', chat_id: '42' } } });
	});

	it('requires every config field on create and refuses unknown or personal email kinds', () => {
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '1:abc' }), 'create', 'personal')).toMatchObject({ ok: false });
		expect(channelPayloadFromForm(form({ name: 'x', kind: 'sms' }), 'create', 'project')).toMatchObject({ ok: false });
		expect(channelPayloadFromForm(form({ name: 'x', kind: 'email', address: 'a@b.test' }), 'create', 'personal')).toMatchObject({ ok: false });
	});

	it('keeps secrets on edit unless all of them are replaced', () => {
		expect(channelPayloadFromForm(form({ name: 'Ops', kind: 'slack', webhook_url: '', enabled: 'on' }), 'edit', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', enabled: true } });
		expect(channelPayloadFromForm(form({ name: 'tg', kind: 'telegram', bot_token: '', chat_id: '43' }), 'edit', 'personal'))
			.toMatchObject({ ok: false, message: expect.stringContaining('both') });
		expect(channelPayloadFromForm(form({ name: 'Ops', kind: 'slack', webhook_url: 'https://hooks.slack.com/services/y' }), 'edit', 'project'))
			.toEqual({ ok: true, payload: { name: 'Ops', enabled: false, kind: 'slack', config: { webhook_url: 'https://hooks.slack.com/services/y' } } });
	});
});
```

`notifications.test.ts` (personal page):
- the load is `GET /account/notification-channels` with `no-store`;
- `create` posts the payload and returns `{ created: true, signingSecret }` only for webhooks;
- `test` posts `{}` to `/account/notification-channels/<id>/test` and returns `{ tested: id, ok: true }` or the engine error;
- `delete` without `confirm=yes` returns `{ confirmDelete: id }` and calls nothing;
- a non-UUID channel id → 400 with no engine call;
- the HTML never contains a secret: render `+page.svelte` with a fake `data`/`form` the way `monitor-form-render.test.ts` does, and assert that the `webhook_url` input has no `value` attribute.

`project-channels.test.ts`: the same cases against `/projects/<id>/notification-channels`, plus a 403 from the engine on create surfaces as the form message.

Run: UI command with these files. Expected: FAIL.

- [ ] **Step 2: Implement.** Use the same component patterns as the log sources pages (reveal-once token box, `CopyButton`).
- [ ] **Step 3: Run** the UI command with the files above and `npm run check`. Expected: PASS, 0/0.
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): personal and project notification channels with send test (NAMI-INC-13)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14 (NAMI-INC-14): UI — escalation policy editor, and policy/severity selects on monitors, heartbeats and log alerts

**Files:**
- Create: `nami-ui/src/lib/server/incidents/policy-form.ts` (+ `policy-form.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/escalation/+page.server.ts`, `+page.svelte`, `escalation.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/escalation/[policyId=uuid]/+page.server.ts`, `+page.svelte`, `policy-editor.test.ts`
- Modify:
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte`: an "Escalation" tab after "Incidents".
  - `nami-ui/src/lib/components/MonitorForm.svelte` and `nami-ui/src/lib/server/uptime/monitor-form.ts` (+ test).
  - `nami-ui/src/lib/server/uptime/heartbeat-form.ts` (+ test) and the heartbeat pages.
  - `nami-ui/src/lib/server/logs/log-alert-form.ts` (+ test) and `nami-ui/src/lib/components/logs/LogAlertForm.svelte` (planned in NAMI-LOG-18).
  - The loads of the monitor new/edit, heartbeat and log-alert pages, which fetch `/escalation-policies` and `/severities` for the selects.
- Test: the files above

**Interfaces:**
- Consumes: Task 8 routes, Task 2 severities, Task 7 schedules (target choices), Task 3 project channels (target choices).
- Produces:

```ts
// lib/server/incidents/policy-form.ts
export const MAX_STEPS = 20;
export type TargetChoice = { value: string; label: string };   // "all_members" | "user:<uuid>" | "schedule:<uuid>" | "channel:<uuid>"
export function policyPayloadFromForm(form: FormData):
	{ ok: true; payload: { name: string; repeat_count: number; fallback_policy_id: string | null; steps: unknown[] } }
	| { ok: false; message: string };
export function policyToFormState(policy: PolicyView): EditorState;   // for initial editor values
```

Form field names:
- `name`, `repeat_count` (0-10), `fallback_policy_id` ('' = none), `step_count`.
- Per step `i` in `0..step_count`:
  - `step_<i>_delay` (0-1440);
  - `step_<i>_target` (repeated; each value parsed as above, ids checked with `isUuid`);
  - `step_<i>_condition` ('on' enables it), `step_<i>_weekday` (repeated 1-7), `step_<i>_from`, `step_<i>_to` (`HH:MM`), `step_<i>_timezone`.
- A step with no target → "Step <i+1> needs at least one target".

The editor page keeps steps in `$state` (add, remove, move up/down) and renders the inputs with `defaultValue`/`defaultChecked`. Its select options come from the load:
- members: `GET /projects/{id}/members` (Task 7, R28: `[{user_id, email, project_role}]`);
- schedules: `GET /projects/{id}/schedules`;
- project channels: `GET /projects/{id}/notification-channels`;
- policies: for the fallback, excluding itself.

The list page shows policies, the default badge, a "Make default" action and two-step delete (a 409 shows the engine message). Admin-only forms render only when `data.role === 'admin'` (R28); others read. Tests render both roles, and an engine 403 still surfaces. The policy/severity selects on monitor, heartbeat and log-alert forms follow those forms' existing (member) visibility.

Source forms: `escalation_policy_id` and `severity_id` selects ("Project default" = empty). Create omits empty values. Edit sends `null` for empty (clear) and the id otherwise. Both values must pass `isUuid` when non-empty.

- [ ] **Step 1: Failing tests**

`policy-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { policyPayloadFromForm } from './policy-form';

const U = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const S = '7d1e2f3a-4b5c-4d6e-8f70-8192a3b4c5d6';

function form(entries: [string, string][]) {
	const f = new FormData();
	for (const [k, v] of entries) f.append(k, v);
	return f;
}

describe('policyPayloadFromForm', () => {
	it('builds steps with targets and an optional condition', () => {
		const result = policyPayloadFromForm(form([
			['name', ' Business hours '], ['repeat_count', '2'], ['fallback_policy_id', ''], ['step_count', '2'],
			['step_0_delay', '0'], ['step_0_target', `user:${U}`], ['step_0_target', 'all_members'],
			['step_0_condition', 'on'], ['step_0_weekday', '1'], ['step_0_weekday', '5'],
			['step_0_from', '09:00'], ['step_0_to', '17:00'], ['step_0_timezone', 'Asia/Jakarta'],
			['step_1_delay', '15'], ['step_1_target', `schedule:${S}`]
		]));
		expect(result).toEqual({ ok: true, payload: { name: 'Business hours', repeat_count: 2, fallback_policy_id: null, steps: [
			{ delay_minutes: 0, targets: [{ type: 'user', id: U }, { type: 'all_members' }],
			  condition: { weekdays: [1, 5], from: '09:00', to: '17:00', timezone: 'Asia/Jakarta' } },
			{ delay_minutes: 15, targets: [{ type: 'schedule', id: S }], condition: null }
		] } });
	});

	it.each([
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0']], 'Step 1 needs at least one target'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '1441'], ['step_0_target', 'all_members']], 'delay'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'user:../../auth/me']], 'target'],
		[[['name', 'x'], ['step_count', '21']], '20'],
		[[['name', 'x'], ['repeat_count', '11'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members']], 'repeat'],
		[[['name', 'x'], ['step_count', '1'], ['step_0_delay', '0'], ['step_0_target', 'all_members'], ['step_0_condition', 'on'], ['step_0_from', '9'], ['step_0_to', '17:00'], ['step_0_timezone', 'UTC']], 'HH:MM']
	] as [string, string][][])('refuses %j', (entries, message) => {
		expect(policyPayloadFromForm(form(entries as unknown as [string, string][]))).toMatchObject({ ok: false, message: expect.stringContaining(message as unknown as string) });
	});
});
```

`policy-editor.test.ts`:
- the load fetches the policy, schedules, project channels, members (`/projects/<id>/members`) and policies (5 calls, ids via `enginePath`);
- `save` PUTs the payload to `/projects/<id>/escalation-policies/<policyId>`;
- a 400 from the engine (for example "fallback policy not found") is shown;
- `default` posts `{}` to `.../default`;
- `delete` needs `confirm=yes`, and a 409 shows the engine message.

`escalation.test.ts`: the list load, and `create` posting the payload then redirecting to the editor.

The monitor/heartbeat/log-alert form tests gain:
- create omits empty `escalation_policy_id`/`severity_id`;
- edit sends `null` for empty;
- a non-UUID value → 400 without an engine call.

Run: UI command with these files. Expected: FAIL.

- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** the UI command (all touched test files) and `npm run check`. Expected: PASS, 0/0. The existing `form-inputs.test.ts` rule (no `value=`/`checked=` on inputs) must still pass for the new forms.
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): escalation policy editor and per-source policy and severity (NAMI-INC-14)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15 (NAMI-INC-15): UI — on-call schedules, overrides, next 14 days, and "on call now" on the project dashboard

**Files:**
- Create: `nami-ui/src/lib/server/incidents/schedule-form.ts` (+ `schedule-form.test.ts`), `nami-ui/src/lib/incidents/oncall.ts` (+ `oncall.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/oncall/+page.server.ts`, `+page.svelte`, `oncall.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/oncall/[scheduleId=uuid]/+page.server.ts`, `+page.svelte`, `schedule-detail.test.ts`
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` ("On-call" tab after "Escalation"), `nami-ui/src/routes/(app)/projects/[id=uuid]/+page.server.ts` and `+page.svelte` ("On call now" card from `GET /projects/{id}/on-call`; a failure there hides the card and never fails the page)
- Test: the files above

**Interfaces:**
- Consumes: Task 7 routes.
- Produces:

```ts
// lib/server/incidents/schedule-form.ts
export function schedulePayloadFromForm(form: FormData):
	{ ok: true; payload: { name: string; timezone: string; rotation: 'daily' | 'weekly' | 'custom'; rotation_hours?: number;
	  handoff_time: string; start_date: string; participants: string[]; restrictions: { weekdays: number[]; from: string; to: string }[] } }
	| { ok: false; message: string };
export function overridePayloadFromForm(form: FormData, timezone: string):
	{ ok: true; payload: { user_id: string; starts_at: string; ends_at: string } } | { ok: false; message: string };
// lib/incidents/oncall.ts
export function shiftRows(shifts: Shift[], timezone: string): { who: string; from: string; to: string; isOverride: boolean }[];
export function localInputToUtc(local: string, timezone: string): string | null;   // "2026-10-19T09:00" in tz → ISO UTC
```

Form fields:
- `name`, `timezone`, `rotation`, `rotation_hours` (custom only, 1-8760), `handoff_time` (`HH:MM`), `start_date` (`YYYY-MM-DD`);
- `participant` (repeated, ordered as submitted, each `isUuid`, 1-50, no duplicates);
- `restriction_count` plus `restriction_<i>_weekday` (repeated), `restriction_<i>_from`, `restriction_<i>_to` (at most 14).

Overrides take `user_id`, `starts_at`/`ends_at` as `datetime-local` values in the **schedule's** timezone. They are converted by `localInputToUtc`, which picks the earlier instant for an ambiguous time and refuses a nonexistent one with "That time does not exist in <tz> (daylight saving change)". The page states the zone next to the inputs.

Pages:
- The list shows schedules with "on call now" and "until" in the schedule's zone (also UTC in a tooltip), and creation (admin).
- The detail page holds the editor (admin), participants order (move up/down), restrictions, the next 14 days of shifts (`shiftRows`; overrides marked), overrides (list, add, two-step delete), and a schedule delete (two-step; a 409 "used by escalation policy" is shown).
- Non-member participants are flagged "not a project member: never paged" (R7).
- Participant and override-user choices come from `GET /projects/{id}/members` (Task 7, R28). Schedule, participant, restriction and override forms render only when `data.role === 'admin'`. The actions still surface an engine 403. Route tests render both roles (R28).

- [ ] **Step 1: Failing tests**

`oncall.test.ts` (lib):

```ts
import { describe, expect, it } from 'vitest';
import { localInputToUtc, shiftRows } from './oncall';

describe('localInputToUtc', () => {
	it('converts in the schedule zone, across DST', () => {
		expect(localInputToUtc('2026-03-30T09:00', 'Europe/Berlin')).toBe('2026-03-30T07:00:00.000Z');
		expect(localInputToUtc('2026-03-27T09:00', 'Europe/Berlin')).toBe('2026-03-27T08:00:00.000Z');
	});
	it('takes the earlier instant of a repeated hour and refuses a skipped one', () => {
		expect(localInputToUtc('2026-10-25T02:30', 'Europe/Berlin')).toBe('2026-10-25T00:30:00.000Z');
		expect(localInputToUtc('2026-03-29T02:30', 'Europe/Berlin')).toBeNull();
	});
	it('refuses junk', () => {
		expect(localInputToUtc('yesterday', 'UTC')).toBeNull();
		expect(localInputToUtc('2026-10-19T09:00', 'Mars/Olympus')).toBeNull();
	});
});

describe('shiftRows', () => {
	it('formats times in the schedule zone and marks overrides', () => {
		const rows = shiftRows([{ user_id: 'u', email: 'a@b.test', starts_at: '2026-10-19T07:00:00Z', ends_at: '2026-10-20T07:00:00Z', override_id: 'o' }], 'Europe/Berlin');
		expect(rows).toEqual([{ who: 'a@b.test', from: 'Mon 19 Oct 09:00', to: 'Tue 20 Oct 09:00', isOverride: true }]);
	});
});
```

(`localInputToUtc` uses `Intl.DateTimeFormat` with the zone to find the offset: try the offset at the guessed instant and at ±1 day, then keep the candidates that format back to the same local wall time. None → `null`; two → the earlier one.)

`schedule-form.test.ts`:
- a full weekly schedule form becomes the payload, with participants in submitted order;
- `custom` without hours → "Enter the rotation length in hours (1-8760)";
- duplicate participants → refused;
- a non-UUID participant → refused;
- `rotation_hours` is dropped for daily/weekly;
- a restriction with no weekday → refused;
- an override that ends before it starts → refused;
- an override in a skipped hour → the DST message.

`schedule-detail.test.ts`:
- the load makes 3 calls (schedule, shifts, overrides) plus members (`/projects/<id>/members`), `no-store` is not required (no secrets), and a 404 → 404;
- `save` PUTs;
- `addOverride` posts the UTC payload;
- `deleteOverride` and `delete` need `confirm=yes`;
- a 409 on delete shows the engine message.

`oncall.test.ts` (route): the list load, and `create` redirecting to the new schedule. The dashboard test: the card data comes from `/on-call`, and a 500 there yields `onCall: []` with the page still loading.

Run: UI command with these files. Expected: FAIL.

- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** the UI command (all touched test files) and `npm run check`. Expected: PASS, 0/0.
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): on-call schedules, overrides, shifts and the on-call-now card (NAMI-INC-15)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Phase close (after Task 15)

The phase's security review and fix wave close the same branch, `feat/incidents-part1`; they get no task number of their own, so Part 2 can start at NAMI-INC-16. Commits in this stage say `(NAMI-INC-15 phase close)`. In order:

1. **Whole-phase security review** of the branch against `origin/main` (CLAUDE.md: every phase ends with one), with the checklist below. Each finding gets a severity, a fix and a test.
2. **Fix wave:** one or more `fix(...)` commits on `feat/incidents-part1`, then a re-review until it is clean.
3. **Decisions doc:** commit `docs/superpowers/decisions/2026-10-13-incident-management-part1.md` with the rulings above (R1-R30, including the controller rulings in R8, R9, R16-R18, R23, R25, R28 and R29; R17 supersedes Logs Part 2's R-P2-27 silent disable), the flap-control ruling of Task 10, the limits, the deferred items (everything under "Part 2 follows") and any execution-time changes.
4. **Roadmap ticks** in `docs/superpowers/roadmap.md` §4, in the same branch. Tick the items Part 1 delivers fully: R4.01, R4.02, R4.03, R4.04, R4.05, R4.07, R4.08, R4.09 and R4.11. Leave the partial ones unticked with a status note saying what is on `main` and that Part 2 finishes it: R4.06 (only the working-hours restriction), R4.14 (severities are seeded and read-only, no metadata editing), R4.15 (status and source filters only), R4.18 (the UI waits for Part 2).
5. **Push and open the phase PR**: purpose and scope, the task list with commit ranges, an implementation summary, the tests run and their results, breaking changes (the incident list response shape, the alert email subjects, the flap-control change), risks and follow-ups. It is auto-merged when the review is clean (pre-authorized, CLAUDE.md), then the branch and the worktree are deleted.

The review must check, with a test for every issue it finds:

1. **SSRF.**
   - Every send path (webhook, Slack, Discord, Teams, Telegram, project email, "send test") goes through `outbound::client()` or `outbound::public_client()` as R9 assigns them, and re-checks the destination before each request.
   - Personal channels (any logged-in user, a global viewer included) can reach only public addresses, by IP literal and by name (`PublicOnlyResolver`). Test it with a name that resolves to `127.0.0.1` (for example `localhost`) as a personal webhook under `OutboundPolicy::Strict`.
   - No redirect is followed. No proxy is used. The idle pool is off, so every request is resolved and judged again (DNS rebinding).
   - A name resolving to `169.254.169.254`, `fd00:ec2::254` or `0.0.0.0` is refused at DNS time.
   - Mapped and NAT64 forms are refused.
   - `TELEGRAM_API_BASE_URL` is config, not user input.
   - `OutboundPolicy::Relaxed` is reachable only through `Config::from_env_for_test`: grep for every construction of `Relaxed`.
2. **Secrets at rest and in transit.**
   - Channel configs and signing secrets are sealed under per-row contexts and never appear in responses, logs (INFO and below), the timeline, `Debug` output or error strings.
   - Grep the engine for `webhook_url`, `bot_token` and `signing_secret` uses outside `src/incidents/channel/` and `src/incidents/send.rs`.
   - No type holding a secret derives `Debug` (Global Constraints), and no `tracing` call or `#[instrument]` records one.
   - Webhook signatures are `sha256=` + hex HMAC-SHA256(secret, `{X-Nami-Timestamp}.{exact body bytes}`) (R29), verifiable by receivers, with a fresh timestamp per attempt. `X-Nami-Delivery` lets them drop at-least-once duplicates and replays inside the 5-minute window.
   - `describe_error` never includes a URL.
3. **Escaping.** Re-run the injection inputs (mentions, markup, HTML, links, CR/LF, bidi controls, oversized text) through all six formatters and the nami-ui pages (titles, comments, causes, timeline messages, postmortem markdown).
4. **Escalation safety.**
   - No incident can produce more than 500 deliveries.
   - No channel receives more than 30 per minute.
   - No policy graph loops: fallback is one hop, verified with A→B→A.
   - Repeats are spaced at least 5 minutes apart.
   - A tick can never spin (at most 20 steps per incident per tick).
5. **DST and time zones.** The rotation, restriction and condition tests pass under `TZ=Pacific/Chatham` and `TZ=America/Santiago` for the test process (the code must not depend on the host zone).
6. **Tenant isolation.** Every new route takes a foreign project id in the path (403), a foreign resource id inside an accessible project (404), and a personal channel of another user (404). Policy targets, fallback, schedule participants and overrides never cross projects.
7. **Action links.**
   - Tampering, expiry, wrong action, wrong incident, reuse (410), a revoked membership (403) and a password change are all refused.
   - The confirm endpoint is POST-only and outside the session.
   - Tokens never appear in engine logs.
   - The nami-ui page sends `no-store`, `no-referrer` and `noindex`.
   - Rate limiting applies per IP (the actions' own governor, R16).
   - Two concurrent confirms of one token act once (the row lock), and a used token always answers 410.
   - The nami-ui page keeps SvelteKit's form-action origin check, and its only link is built from UUID-checked ids (no open redirect).
8. **RBAC.** A table of every route × role (outsider, viewer, member, admin, global admin) matches the spec's RBAC section. `tests/core/json_bodies.rs` lists every new state-changing route.
9. **Failure behaviour.** A down SMTP server, ClickHouse or chat provider never blocks incident state changes, the uptime worker or the API. Deliveries retry per R12 and are visible in the timeline.
10. **Queue robustness.** A row whose lease expired after its last attempt is failed, not reclaimed (poison rows, R12). Two drains in parallel (and two engine instances) never exceed the storm caps (the claim's advisory lock, R13). With `INCIDENTS_WORKER_ENABLED=false`, no tick or drain runs on that instance, `kick` included.

No CI is configured on this repo yet. The phase PR states that, and lists the engine and UI commands run locally with their results.
