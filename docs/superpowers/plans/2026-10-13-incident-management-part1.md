# Incident Management + On-call — Part 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Nami the core of BetterStack incident management and on-call. That means the incident lifecycle (open → acknowledged → resolved, reopen, manual incidents, comments, timeline, postmortem), severities, escalation policies (steps, delays, repeat, time-window conditions, fallback policy), on-call schedules (rotations, handoff, working-hours restrictions, overrides, "who is on call"), and notification channels (email, generic webhook, Slack, Microsoft Teams, Telegram, Discord), both personal and project-wide, with a durable delivery queue. It also adds signed acknowledge/resolve links in emails, and routes every monitor, heartbeat and log-alert incident through one internal open/resolve API, escalation and channels. nami-ui gets the screens for all of it.

**Architecture:** Every incident is opened, acknowledged, resolved and reopened only through `incidents::lifecycle`. That module writes the state change and its timeline event in the caller's transaction. It also arms escalation (`next_escalation_at`) and queues "resolved" notifications. Monitors, heartbeats, log alerts (Logs Part 2), manual incidents and Part 2's integrations and report form all call it with an `IncidentSource`. One engine loop (`incidents::worker`) runs two ticks. The escalation tick claims due incidents with `FOR UPDATE SKIP LOCKED`, resolves the current step's targets to users and channels, and inserts deduplicated rows into `notification_deliveries`. The delivery tick claims due deliveries and sends them through `incidents::send`. Chat and webhook requests go through one guarded outbound client (`outbound.rs`: no proxy, link-local and metadata refusal at DNS time, no redirects, 10 s timeout). Formatting is pure (`incidents::format`) and escapes per channel. On-call math is pure (`oncall::rotation`) and is tested across DST transitions. Channel secrets are sealed with the existing `crypto::Cipher` under per-row contexts and are never returned. Action links carry an HMAC token over `(incident, action, user, exp)`, are single-use, and are confirmed by a POST from a nami-ui page.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, chrono-tz 0.10.4), new direct crate `hmac 0.13.0` (already in the lock file through `hkdf`), Postgres 17, Mailpit, wiremock 0.6.5 (dev, already present), SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-incident-management-design.md`, including its "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §4 (the definition of done).

**When to start:** after every Logs Part 2 task (NAMI-LOG-9 .. NAMI-LOG-18, plan `docs/superpowers/plans/2026-10-13-log-management-part2.md`, PR #52) is merged to `main`. This plan assumes that end state. In particular, NAMI-LOG-14's migration `0044_create_log_alerts.sql` exists: `log_alerts`, `incidents.log_alert_id` (FK, cascade) and the index `incidents_one_open_per_log_alert ... where status = 'open'`. The plan also assumes `models::incident::{open_for_log_alert, resolve_for_log_alert}`, `uptime::alerts::Subject::LogAlert`, `logs::alerts::apply_evaluation` returning `Option<Transition>`, and log-alert emails sent through `alerts::notify_transition`. Review fixes that land in the meantime may shift a line, a name or a test count. Implementers adapt such drift minimally (same behaviour, same tests) and note it in the PR. They never redesign around it.

**Part 2 follows** (a separate plan, NAMI-INC-16 onward). It covers:
- severity management (CRUD + settings UI);
- incident metadata editing and the full list filters (severity, date range, `metadata.<key>`, `q`);
- incoming integrations (generic webhook, Alertmanager, Grafana) on the public listener, with dedup and auto-resolve;
- the public incident report form (`/report/[token]`, honeypot, per-IP limits);
- Slack interactivity (signed Acknowledge/Resolve buttons);
- GitHub/Linear/Jira tickets;
- reporting (MTTA/MTTR, breakdowns, CSV) and the SLA report (needs Uptime Phase 3's availability function);
- shift reminder emails, the per-user ICS feed and "take over shift";
- heartbeat output (`details`, Uptime Phase 3) on the incident page.

Part 1 leaves room for these:
- `IncidentSource` gets `Integration { integration_id, dedup_key }` and `ReportForm` variants, each with its own conflict target.
- `incidents.metadata` (jsonb object) and the `metadata_updated`, `integration_event` and `ticket_created` event kinds already exist.
- `incident_severities` is seeded and read-only in Part 1.
- `oncall::rotation::shifts` is the function the ICS feed and the reminders call.
- `outbound::client()` is the shared client that the ticket integrations use.

Postgres migrations 0060-0099 are reserved for Incidents (ruling in `docs/superpowers/decisions/2026-10-13-uptime-phase1.md`). None of these numbers was in use on 2026-10-19 (`main` ends at 0041; Logs Part 2 adds 0042-0044). Part 1 uses 0060-0066. Part 2 continues at 0067.

**Verification status (2026-10-19):** written against `main` at `95940ee` plus the Logs Part 2 plan's stated end state. Unlike the Logs Part 1 and Uptime Phase 2 plans, the code here has **not** been applied to a scratch copy. Tests are given in full. Code that decides something is given in full: migrations, SQL, the escalation planner, rotation math, escaping, token signing and URL rules. Elsewhere the plan gives exact signatures, names and values, and the implementer writes the body (the Logs Part 2 convention). Run every step. A test that fails for a reason the plan did not predict is a plan bug: fix it minimally and say so in the PR.

**Execution:** per CLAUDE.md. Use one worktree per task under `.worktrees/NAMI-INC-<n>/`, branched from the latest `origin/main`, with the branch named under each task heading. Commit with Conventional Commits, then run the task review, open the PR, and auto-merge on a clean review (pre-authorized for sub-projects 2-11). Tasks are sequential (each consumes the interfaces of the ones before it). Two exceptions: T4 depends only on T1, and T6 depends on nothing in this plan. Engine tests need `docker compose up -d postgres clickhouse mailpit` (never restart running containers).

The **engine command** is used throughout:

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_TARGET_DIR=/Users/mitengineer/development/nami/.worktrees/.cargo-target cargo test <args>
```

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
  - webhook bodies are `{event, incident:{id,title,cause,status,source,severity,started_at,url}, project:{id,name}}` with `X-Nami-Signature: sha256=<hex HMAC-SHA256 of body>` under a per-channel secret shown once at creation.
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
- Outbound requests: every chat/webhook send goes through `outbound::client()`. That client uses `.no_proxy()`, a `GuardedResolver` (refuses link-local `169.254.0.0/16`/`fe80::/10`, `fd00:ec2::254`, `0.0.0.0/8`, `::` and their mapped/NAT64 forms, the same policy as monitors) and no redirects. IP literals are checked before the request. Error text is cleaned (`clean_error`) and stripped of the URL (`reqwest::Error::without_url`).
- nami-ui:
  - every engine call goes through `callEngine`, and every path through `enginePath`;
  - route params use `[...=uuid]` matchers;
  - form actions return `engineFailure(...)` on engine errors;
  - deletes take two steps (`confirm=yes`);
  - pages that show or accept secrets set `cache-control: private, no-store`;
  - follow SvelteKit 3 conventions (`#lib` alias, `process.env` in server code, runes, `page` from `$app/state`), use `defaultValue`/`defaultChecked` for initial input values, and copy the dark theme classes from the existing pages.
- Tests: test binaries run one after another (`cargo test`). Mailpit assertions filter by their own recipient and never delete messages.
- Commits: Conventional Commits whose message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **A DST change must not move a handoff or a working-hours window by an hour.** Example: a weekly rotation in `Europe/Berlin` handing off Monday 09:00 must hand off at 09:00 local both before and after the spring-forward Sunday. A 02:30 handoff on that day (a local time that does not exist) happens at 03:00, never at 01:30 and never a day late → Task 6 (`weekly_handoff_keeps_local_time_across_spring_forward`, `a_handoff_inside_the_spring_forward_gap_happens_at_the_first_valid_instant`, `fall_back_repeats_no_shift`).
- **A flapping source, or many incidents opening at once, must not flood a channel.** Example: a monitor that goes down and up every minute, or 50 monitors behind one dead switch. Deliveries to one channel are capped per minute (the excess waits, it is not dropped), each incident is capped in total, and a repeat pass never starts sooner than 5 minutes after the previous one → Task 5 (`a_channel_never_receives_more_than_its_per_minute_cap`) and Task 9 (`the_per_incident_delivery_cap_stops_a_storm`, `repeats_wait_at_least_the_minimum_gap`).
- **An acknowledgement that races an escalation tick must win.** Example: a step-2 notification that is already queued when someone acknowledges. That notification must not go out, and no later step may run → Task 9 (`an_acknowledge_cancels_queued_step_notifications`).
- **Nobody on call must never mean nobody notified, silently.** Example: a schedule target outside its working hours, or a schedule with no participants. That pass falls through to the fallback policy. Without a fallback, the timeline records `escalation_exhausted` with the reason, so the gap is visible on the incident → Task 9 (`a_pass_that_notifies_nobody_moves_to_the_fallback_once`, `without_a_fallback_an_empty_pass_is_recorded_as_exhausted`).
- **Someone removed from a project must stop getting its alerts, and their links must stop working.** This applies to schedule participants, user targets, personal channels and outstanding action links → Task 9 (`a_removed_member_is_not_notified_even_when_on_call`) and Task 11 (`a_token_of_a_user_no_longer_in_the_project_is_refused`).

## Decisions the spec leaves open (rulings)

- **R1 Split.** The spec has 19 roadmap bullets. They do not fit one plan of at most 16 tasks, so this is Part 1, the core named in the header. Part 2's scope is listed there.
- **R2 Migrations.**
  - `0060_incident_lifecycle.sql`: severities, incident columns, `incident_events`, generalised unique indexes.
  - `0061_create_notification_channels.sql`.
  - `0062_create_notification_deliveries.sql`.
  - `0063_create_on_call_schedules.sql`.
  - `0064_create_escalation_policies.sql`: policies, steps, targets, incident escalation columns, default policy.
  - `0065_source_escalation_routing.sql`: `escalation_policy_id` and `severity_id` on monitors, heartbeats and log alerts.
  - `0066_create_incident_action_token_uses.sql`.
- **R3 Table names** (the spec leaves these to the plan): `escalation_policies`, `escalation_steps`, `escalation_step_targets`, `on_call_schedules`, `on_call_participants`, `on_call_overrides`, `notification_channels`, `notification_deliveries`, `incident_events`, `incident_severities`, `incident_action_token_uses`.
- **R4 One unresolved incident per source.** Migration 0060 drops `incidents_one_open_per_monitor`, `incidents_one_open_per_heartbeat` and `incidents_one_open_per_log_alert` (all `where status = 'open'`). It recreates them as `..._one_unresolved_per_...` with `where status <> 'resolved'`, so an acknowledged incident still blocks a second one. `lifecycle::open` names the matching conflict target per source. Each target is a separate SQL literal, because a `match` on the variant picks the statement.
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
- **R8 Personal channel kinds.** `webhook`, `slack`, `teams`, `telegram` and `discord` are allowed. A personal `email` channel is not: the implicit verified address covers it, and an unverified extra address would let anyone mail third parties through Nami. Project `email` channels (admin) take any address, for example a team list.
- **R9 URL rules per kind.** These are checked at create/update and again before each send.
  - webhook: `http` or `https`, no userinfo, host required. Private and loopback hosts are allowed (self-hosted receivers), with the same DNS guard as monitors.
  - slack: `https://hooks.slack.com/` exactly.
  - discord: `https://discord.com/api/webhooks/…`, also `discordapp.com`, `ptb.discord.com`, `canary.discord.com`.
  - teams: any `https` host (Workflows URLs are tenant-specific).
  - telegram: no URL. A bot token `^\d{1,20}:[A-Za-z0-9_-]{30,64}$` and a chat id `^-?\d{1,20}$` or `^@[A-Za-z0-9_]{5,32}$`, sent to `TELEGRAM_API_BASE_URL` (default `https://api.telegram.org`, like `RDAP_BASE_URL`).
  - Integration tests insert channels through the model, which does not apply the host allowlist, so Slack/Discord/Teams payloads can be sent to wiremock.
- **R10 Secrets at rest.** `notification_channels.config_enc` is sealed under `notification_channels.config:{id}` and `signing_secret_enc` under `notification_channels.signing_secret:{id}` (ids generated in Rust before the insert). A non-secret `summary` column holds what the UI shows: the webhook URL's scheme and host, `hooks.slack.com`, `discord.com`, the Teams host, `Telegram chat <chat_id>`, or the email address. A channel whose secret no longer decrypts (key changed) fails each delivery with `channel configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)` and is never sent anywhere.
- **R11 Errors recorded in the timeline.** Viewers can read the timeline, so it records only kinds and status codes: `HTTP 404`, `timeout`, `connection failed`, `destination not allowed`, `email not delivered`. It never records a response body, URL, address or token. The Telegram bot token is part of the request path, so `describe_error` always calls `without_url()`.
- **R12 Retry policy.**
  - Retryable: timeouts, connection errors, HTTP 5xx and 429 (`Retry-After` honoured up to 1 h), and SMTP failures.
  - Any other 4xx fails at once (the URL or token is wrong; retrying cannot help).
  - Backoff after failed attempt n is 30 s × 4^(n−1), capped at 1 h: 30 s, 2 min, 8 min, 32 min. After attempt 5 the delivery is `failed`.
  - A claimed delivery holds a 60 s lease. A crash between "sent" and recording it can resend once (at-least-once at the transport level). Webhooks carry `X-Nami-Delivery: <delivery id>` so receivers can drop that duplicate.
- **R13 Storm caps.**
  - At most 30 deliveries per channel per rolling minute, and at most 20 emails per user per minute. Excess deliveries are rescheduled 60 s later, not counted as an attempt and not dropped.
  - At most 500 deliveries per incident, after which one `notification_failed` event says the cap was reached.
  - At most 100 recipients per step.
  - A repeat pass starts no sooner than 5 minutes after the previous pass ended (`MIN_REPEAT_GAP`).
  - One tick runs at most 20 consecutive zero-delay steps of one incident.
- **R14 Escalation cursor.**
  - Columns: `escalation_policy_id`, `escalation_step` (0-based, the next step to run), `escalation_repeat`, `escalation_on_fallback`, `escalation_notified` (recipients notified in the current pass) and `next_escalation_at`.
  - A step's run time is the previous step's *scheduled* time plus its delay, not "now", so a slow tick does not drift the policy.
  - The decision after each step is the pure `escalation::plan::after_step` (Task 8).
  - Deleting the policy of a running incident ends its escalation with `escalation_exhausted` ("policy deleted").
- **R15 Ack/resolve cancels queued step notifications.** A `triggered` delivery whose incident is no longer `open` when it is claimed becomes `cancelled`. `resolved` deliveries always go out.
- **R16 Action links only in email.** Each email carries the recipient's own acknowledge and resolve links. Chat messages carry only the incident link, because a "personal" Slack webhook may post into a shared room. Tokens are `base64url(payload) "." base64url(mac)` with payload `v1|<incident>|<ack|resolve>|<user>|<exp unix>`. The MAC is HMAC-SHA256 under `cipher.derive_subkey("nami/incident-action/v1")`. Tokens are **single-use**: the first successful confirm stores SHA-256(token) in `incident_action_token_uses`, and a second confirm gets 410. The user must still be a project `member` or `admin` (or a global admin) when the link is used. The token never creates a session and grants nothing else. The engine endpoints take the token in a JSON body (never a URL), so it stays out of the engine's request logs. The nami-ui page sends `referrer-policy: no-referrer` and `cache-control: no-store`.
- **R17 Routing.**
  - `alerts::notify_transition` keeps its signature, so the uptime worker, the heartbeat endpoint and Logs Part 2's alert worker do not change.
  - Its body no longer emails anyone. It calls `incidents::worker::kick(state)`, which runs one escalation tick and one delivery drain at once. The `Notifications` set from the uptime worker stays the bounded vehicle for that.
  - The worker loop (every 2 s) is the durable backstop.
  - The source wrappers (`models::incident::open_for_monitor` and the others) keep their signatures and call `lifecycle::open` with the source's `escalation_policy_id`/`severity_id`.
- **R18 Email subjects change** with routing (Task 10). A step notification is `[Nami] [<severity>] <title>`. A resolution is `[Nami] Resolved: <title>`. The existing worker, heartbeat, maintenance and log-alert tests that assert the old subjects (`… is DOWN`, `… is back UP`, `Log alert … triggered/resolved`) are updated in Task 10. Their counts stay the same (one email per member per transition).
- **R19 Time windows** reuse `models::maintenance::Schedule::Weekly::covers` and `weekday_from_iso` (Phase 2, already DST-tested). `from == to` means a full 24 h, as for maintenance windows. Wall-clock semantics apply: a window inside a skipped hour does not happen that day, and one across a repeated hour lasts an hour longer.
- **R20 Rotation math.**
  - `daily`/`weekly` shifts are calendar arithmetic in the schedule's zone. Shift k starts at local `(start_date + k days|weeks) handoff` and is converted with `local_to_utc`: a nonexistent time moves to the first valid minute after it, and an ambiguous one takes the earlier instant.
  - Custom rotations (`rotation_hours` 1-8760) are absolute: `epoch + k × hours`.
  - Before the epoch nobody is on call.
  - Restrictions blank the rotation outside their windows. Overrides apply regardless (spec).
  - `shifts(from, to)` evaluates `on_call_at` at every UTC minute boundary and at every override edge in the range, then merges runs of the same user. The range is at most 70 days, which covers the 14-day view and Part 2's ICS window.
- **R21 Incident list.** `GET /projects/{id}/incidents` returns `{"incidents": [...], "next_cursor": string|null}`, newest first, `limit` 1-100 (default 50). It takes `status` (`open`, `acknowledged`, `resolved`, or `unresolved`; repeatable) and `source` (repeatable). The response shape changes from a bare array, and Task 2 updates the existing nami-ui loader in the same PR. Part 2 adds the remaining filters.
- **R22 Postmortem rendering.** nami-ui renders postmortems with a small in-repo renderer (`#lib/markdown.ts`). It HTML-escapes everything first, then applies a fixed subset: headings, emphasis, inline code, fenced code, lists, paragraphs, and `http(s)` links with `rel="noopener noreferrer nofollow"`. No raw HTML passes, and there is no new dependency.
- **R23 Worker switch.** The incidents worker runs where the uptime worker runs (`UPTIME_WORKER_ENABLED`). One flag keeps single-instance deployments simple. Every claim uses `skip locked`, so several instances are safe.
- **R24 Not in Part 1** (Part 2 or later): everything listed under "Part 2 follows". SMS, phone and push are spec non-goals (they would need a paid provider), so this plan has none.

## File Structure

```
nami-engine/
  Cargo.toml (+ Cargo.lock)            T3 hmac 0.13.0 (direct)
  .env.example                         T5 TELEGRAM_API_BASE_URL
  migrations/
    0060_incident_lifecycle.sql        T1
    0061_create_notification_channels.sql T3
    0062_create_notification_deliveries.sql T5
    0063_create_on_call_schedules.sql  T7
    0064_create_escalation_policies.sql T8
    0065_source_escalation_routing.sql T10
    0066_create_incident_action_token_uses.sql T11
  src/
    lib.rs                             T1 mod incidents; T3 mod outbound; T6 mod oncall, mod timewindow
    config.rs                          T5 telegram_api_base_url
    main.rs                            T5 spawn incidents::worker::run_forever
    router.rs, routes/mod.rs           T2, T3, T7, T8, T11 merge routers
    outbound.rs                        T3 guarded client, URL rules, describe_error
    timewindow.rs                      T6 TimeWindow (wraps maintenance::Schedule::Weekly)
    incidents/
      mod.rs                           T1 module list
      lifecycle.rs                     T1 IncidentSource, OpenIncident, open/acknowledge/resolve/reopen/comment/record_event;
                                       T8 arms escalation; T9 queues resolved notifications
      format.rs                        T4 per-channel rendering + escaping, webhook signature
      send.rs                          T5 senders per kind, SendError, retry classification
      deliveries.rs                    T5 queue claim/drain/backoff/caps
      escalation/mod.rs, plan.rs, run.rs T8 plan.rs (pure planner); T9 run.rs (tick)
      worker.rs                        T5 loop (drain); T9 + escalation tick, kick
      action_token.rs                  T11 sign/verify, single use
    oncall/
      mod.rs, rotation.rs              T6 pure rotation math
    models/
      incident.rs                      T1 wrappers over lifecycle; T2 list/detail/events
      severity.rs                      T2 list/find
      channel.rs                       T3 Channel, ChannelConfig, seal/open, CRUD
      schedule.rs                      T7 schedules, participants, overrides
      escalation_policy.rs             T8 policies, steps, targets, validation
      monitor.rs, heartbeat.rs, log_alert.rs  T10 escalation_policy_id, severity_id
    routes/
      incidents.rs                     T2 list/create/detail/ack/resolve/reopen/comments/postmortem/severity
      severities.rs                    T2
      channels.rs                      T3 project + personal channels; T5 test send
      schedules.rs                     T7
      escalation_policies.rs           T8
      incident_actions.rs              T11 inspect/confirm (no session)
    uptime/
      checker.rs                       T3 GuardedResolver/clean_error moved to outbound.rs (re-used)
      alerts.rs                        T10 notify_transition → incidents::worker::kick
      worker.rs, heartbeat.rs          T10 pass policy/severity (through models::incident wrappers)
    logs/alerts.rs                     T10 pass policy/severity
  tests/
    common/mod.rs                      T1 open_test_incident; T5 run_incident_ticks; T8 seed_policy helpers
    incident_lifecycle.rs              T1
    incidents_api.rs                   T2
    notification_channels.rs           T3
    notification_deliveries.rs         T5
    schedules_api.rs                   T7
    escalation_policies_api.rs         T8
    escalation_engine.rs               T9
    incident_routing.rs                T10
    incident_actions.rs                T11
    migrations.rs                      T1, T3, T5, T7, T8, T10, T11
    uptime_worker.rs, heartbeats.rs, maintenance_windows.rs, recovery_threshold.rs,
    uptime_worker_timing.rs, checker_http_config.rs, log_alerts.rs  T10 subject assertions (R18)

nami-ui/src/
  lib/incidents.ts (+ .test.ts)        T2 types; T12 status/source/severity helpers, timeline labels
  lib/markdown.ts (+ .test.ts)         T12 safe postmortem renderer
  lib/server/channel-form.ts (+ .test) T13 channel form → engine body (write-only secrets)
  lib/server/policy-form.ts (+ .test)  T14 policy editor form → engine body
  lib/server/schedule-form.ts (+ .test) T15 schedule/override form → engine body
  lib/components/SeverityBadge.svelte  T12
  lib/components/ChannelList.svelte    T13 shared by personal + project pages
  routes/(app)/projects/[id=uuid]/
    +layout.svelte                     T14 "Escalation", T15 "On-call" tabs
    +page.svelte, +page.server.ts      T15 "On call now" card
    incidents/+page.*                  T2 loader shape; T12 list, tabs, new incident
    incidents/[incidentId=uuid]/+page.* T12 detail, timeline, actions, comments, postmortem
    settings/notifications/+page.*     T13 project channels
    escalation/+page.*, escalation/[policyId=uuid]/+page.*  T14
    oncall/+page.*, oncall/[scheduleId=uuid]/+page.*        T15
    monitors/new, monitors/[monitorId=uuid]/edit, heartbeats/..., logs/alerts/...  T14 policy/severity selects
  routes/(app)/account/notifications/+page.*  T13 personal channels
  routes/incidents/action/+page.*      T11 confirmation page (outside (app), no login)
```

---
### Task 1 (NAMI-INC-1): Incident lifecycle schema and the unified open/resolve API

Branch `feat/NAMI-INC-1-incident-lifecycle`, worktree `.worktrees/NAMI-INC-1/`.

**Files:**
- Create: `nami-engine/migrations/0060_incident_lifecycle.sql`, `nami-engine/src/incidents/mod.rs`, `nami-engine/src/incidents/lifecycle.rs`
- Modify: `nami-engine/src/lib.rs` (`pub mod incidents;`), `nami-engine/src/models/incident.rs` (the six source wrappers delegate to `lifecycle`), `nami-engine/tests/common/mod.rs` (`open_test_incident`), `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/incident_lifecycle.rs`. The existing `tests/incidents.rs` must stay green unchanged, except `status_is_limited_to_open_and_resolved`, which is renamed `status_is_limited_to_the_lifecycle_states` and also accepts `acknowledged`.

**Interfaces:**
- Consumes: `incidents` (0010/0011/0019/0044), `uptime::checker::is_unsafe_in_text`.
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
pub async fn comment(conn: &mut PgConnection, incident_id: Uuid, actor: Uuid, body: &str) -> Result<i64, sqlx::Error>;
pub async fn record_event(conn: &mut PgConnection, incident_id: Uuid, kind: EventKind, actor: Option<Uuid>,
    message: Option<&str>, metadata: serde_json::Value) -> Result<i64, sqlx::Error>;
```

`models::incident::{open_for_monitor, resolve_for_monitor, open_for_heartbeat, resolve_for_heartbeat, open_for_log_alert, resolve_for_log_alert}` keep their exact signatures and become one-line calls into `lifecycle`. Later sub-projects add `IncidentSource` variants (Part 2: `Integration`, `ReportForm`; Metrics: `MetricAlert`) with their own conflict target.

- [ ] **Step 1: Write the migration**

`nami-engine/migrations/0060_incident_lifecycle.sql`:

```sql
-- Incident management, part 1 (NAMI-INC-1): severities, the incident
-- lifecycle (open -> acknowledged -> resolved, reopen), the timeline.
-- Migrations 0060-0099 are reserved for incident management.

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
update incidents i set severity_id = s.id
from incident_severities s where s.project_id = i.project_id and s.is_default;
-- Rows resolved before 0010 recorded a time cannot exist (the worker always
-- set resolved_at), but a hand-written row could: give it one.
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

Add to `nami-engine/tests/migrations.rs` (in the style of its existing table tests):

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
```

Run: engine command `--test migrations`. Expected: FAIL (no table `incident_severities`), then PASS once the migration file exists.

- [ ] **Step 3: Write the failing lifecycle tests**

`nami-engine/tests/incident_lifecycle.rs`:

```rust
// The incident lifecycle (incidents::lifecycle). Requires `docker compose up -d postgres`.
mod common;

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

Run: engine command `--test incident_lifecycle`. Expected: FAIL to compile (`incidents::lifecycle` missing).

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

Then rewrite the six wrappers in `models/incident.rs` as calls, for example:

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

`list_for_project`'s `status` filter keeps working; Task 2 replaces it.

Add to `tests/common/mod.rs`:

```rust
/// Opens a manual incident in `project` (title "test incident") and returns its id.
pub async fn open_test_incident(pool: &sqlx::PgPool, project: Uuid, created_by: Uuid) -> Uuid
```

- [ ] **Step 5: Run the tests**

Run: engine command `--test incident_lifecycle --test incidents --test migrations --test uptime_worker --test heartbeats --test log_alerts`. Expected: PASS (the email-based tests are untouched by this task).

- [ ] **Step 6: Run the full engine suite and clippy**

Run: engine command (no args), then `cargo clippy --all-targets -- -D warnings` with the same env. Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/0060_incident_lifecycle.sql nami-engine/src/incidents nami-engine/src/lib.rs \
  nami-engine/src/models/incident.rs nami-engine/tests/incident_lifecycle.rs nami-engine/tests/incidents.rs \
  nami-engine/tests/migrations.rs nami-engine/tests/common/mod.rs
git commit -m "feat(engine): incident lifecycle, severities and timeline (NAMI-INC-1)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2 (NAMI-INC-2): Incident API — list, manual create, detail with timeline, ack/resolve/reopen, comments, postmortem, severity

Branch `feat/NAMI-INC-2-incident-api`, worktree `.worktrees/NAMI-INC-2/`.

**Files:**
- Create: `nami-engine/src/models/severity.rs`, `nami-engine/src/routes/severities.rs`
- Modify: `nami-engine/src/models/incident.rs` (list with cursor, `IncidentDetail`, `IncidentEvent`), `nami-engine/src/routes/incidents.rs`, `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-ui/src/lib/incidents.ts` (new: types), `nami-ui/src/lib/uptime.ts` (re-export `Incident` from `incidents.ts` for existing imports), `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/+page.server.ts` (+ its tests: read `.incidents` from the new shape)
- Test: `nami-engine/tests/incidents_api.rs`, the existing `incidents-page.test.ts` / `incidents.test.ts` updated

**Interfaces:**
- Consumes: Task 1 `lifecycle::*`.
- Produces:

```rust
// models::severity
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Severity { pub id: Uuid, pub name: String, pub rank: i32, pub is_default: bool }
pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Severity>, sqlx::Error>; // by rank
pub async fn belongs_to(conn: &mut PgConnection, project_id: Uuid, severity_id: Uuid) -> Result<bool, sqlx::Error>;

// models::incident
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct IncidentListItem { pub id: Uuid, pub project_id: Uuid, pub monitor_id: Option<Uuid>,
    pub heartbeat_id: Option<Uuid>, pub log_alert_id: Option<Uuid>, pub source: String,
    pub source_name: Option<String>, pub title: String, pub cause: Option<String>, pub status: String,
    pub severity: Option<String>, pub severity_rank: Option<i32>, pub started_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>, pub acknowledged_by_email: Option<String>,
    pub resolved_at: Option<DateTime<Utc>> }
pub struct ListFilter { pub statuses: Vec<String>, pub sources: Vec<String>, pub before: Option<(DateTime<Utc>, Uuid)>, pub limit: i64 }
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
| `GET /projects/{id}/incidents?status=&source=&limit=&cursor=` | viewer | — | `200 {"incidents", "next_cursor"}` | 400 bad status/source/limit/cursor |
| `POST /projects/{id}/incidents` | member | `{title, description?, severity_id?}` | `201 IncidentDetail` | 400 caps, 404 foreign severity |
| `GET /projects/{id}/incidents/{incident_id}` | viewer | — | `200 IncidentDetail` | 404 |
| `POST .../acknowledge` | member | `{}` | `200 IncidentDetail` | 409 `incident is not open` |
| `POST .../resolve` | member | `{}` | `200 IncidentDetail` | 409 `incident is already resolved` |
| `POST .../reopen` | member | `{}` | `200 IncidentDetail` | 409 `incident is not resolved` / `this source already has an unresolved incident` |
| `POST .../comments` | member | `{body}` | `201 IncidentEvent` | 400 |
| `PUT .../postmortem` | member | `{postmortem: string or null}` | `200 IncidentDetail` | 400 > 102,400 bytes |
| `PATCH /projects/{id}/incidents/{incident_id}` | member | `{severity_id}` | `200 IncidentDetail` | 404 foreign severity |

The cursor is opaque: base64url of `<started_at RFC 3339 micros>|<id>`, and a malformed one is a 400. The list orders by `(started_at desc, id desc)` and fetches `limit + 1` rows to decide `next_cursor`. The SQL takes `status = any($2)` (empty array = all) and `source = any($3)`. `unresolved` expands to `open` + `acknowledged`. Every incident path checks `incidents.project_id = {id}` (404 otherwise). Request bodies are `#[serde(deny_unknown_fields)]`.

Validation rules:
- title: trimmed, 1-255 chars, no char with `is_unsafe_in_text`.
- description and comment body: ≤ 10,240 bytes; the only control characters allowed are `\n` and `\t`.
- postmortem: ≤ 102,400 bytes, same character rule.

A `severity_changed` event carries `{"from": "<name>", "to": "<name>"}`. `postmortem_updated` carries no text, so the timeline stays small. Detail returns at most the latest 1,000 events, oldest first.

- [ ] **Step 1: Write the failing API tests**

`nami-engine/tests/incidents_api.rs`:

```rust
// The incident API. Requires `docker compose up -d postgres`.
mod common;

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

Run: engine command `--test incidents_api`. Expected: FAIL (routes missing).

- [ ] **Step 2: Implement** the models and routes as specified in the Interfaces block. Every write handler runs in one transaction: lock the incident row (`select ... where id = $1 and project_id = $2 for update`), call `lifecycle`, then load the detail after commit.

- [ ] **Step 3: Update the nami-ui loader**

In `nami-ui/src/lib/incidents.ts`, define `Incident` (mirrors `IncidentListItem`), `IncidentDetail`, `IncidentEvent`, `Severity` and `IncidentPage = { incidents: Incident[]; next_cursor: string | null }`. The existing page's `+page.server.ts` keeps its open/resolved toggle and reads `(await response.json()).incidents`, with `status=unresolved` for the "open" tab. Update its two test files to the new response shape.

Run: UI command with `src/routes/(app)/projects/[id=uuid]/incidents`. Expected: PASS, 0 errors / 0 warnings.

- [ ] **Step 4: Run** engine command `--test incidents_api --test incidents --test incident_lifecycle`, then the full engine suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src nami-engine/tests/incidents_api.rs nami-ui/src/lib/incidents.ts nami-ui/src/lib/uptime.ts \
  "nami-ui/src/routes/(app)/projects/[id=uuid]/incidents"
git commit -m "feat(engine,ui): incident API with timeline, actions, comments and postmortems (NAMI-INC-2)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 3 (NAMI-INC-3): Guarded outbound client and notification channels (personal + project, sealed, write-only)

Branch `feat/NAMI-INC-3-notification-channels`, worktree `.worktrees/NAMI-INC-3/`.

**Files:**
- Create: `nami-engine/src/outbound.rs`, `nami-engine/migrations/0061_create_notification_channels.sql`, `nami-engine/src/models/channel.rs`, `nami-engine/src/routes/channels.rs`
- Modify: `nami-engine/src/uptime/checker.rs` (move `GuardedResolver`, `names_forbidden_address`, `clean_error` into `outbound.rs` as `pub(crate)` items and import them back; behaviour unchanged), `nami-engine/src/lib.rs`, `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/Cargo.toml` (`cargo add hmac@0.13.0`), `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/notification_channels.rs` (+ unit tests in `outbound.rs` and `models/channel.rs`)

**Interfaces:**
- Consumes: `crypto::Cipher`, `uptime::checker::{is_forbidden_destination, allowed_addresses}`, `models::validate_name`, `rbac::{require_project_role, CurrentUser}`.
- Produces:

```rust
// outbound
pub const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// No proxy, GuardedResolver, no redirects, no referer, 10 s total timeout, UA "Nami-Notify/<version>".
pub fn client() -> reqwest::Client;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRule { HttpOrHttps, Https, HttpsHosts(&'static [&'static str]), HttpsHostPrefix { host: &'static [&'static str], path_prefix: &'static str } }
/// Parses and checks a destination: scheme, host present, no userinfo, the rule, and no
/// forbidden IP literal (`names_forbidden_address`). The error is a user-facing message.
pub fn check_url(raw: &str, rule: UrlRule) -> Result<reqwest::Url, String>;
/// "timeout" | "connection failed" | "destination not allowed" | "HTTP <code>" | "request failed".
/// Never contains the URL (uses `without_url`) or a response body.
pub fn describe_error(error: &reqwest::Error) -> String;

// models::channel
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
impl ChannelConfig {
    pub fn kind(&self) -> &'static str;
    /// R9 rules; the error is a 400 message.
    pub fn validate(&self) -> Result<(), String>;
    /// R10 display string, never secret.
    pub fn summary(&self) -> String;
}
pub enum Owner { Project(Uuid), User(Uuid) }

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
pub struct OpenedChannel { pub id: Uuid, pub kind: String, pub name: String, pub config: ChannelConfig, pub signing_secret: Option<Vec<u8>> }
pub async fn open_for_send(pool: &PgPool, cipher: &Cipher, id: Uuid) -> Result<Option<Result<OpenedChannel, DecryptError>>, sqlx::Error>;
```

- Routes:
  - `GET /projects/{id}/notification-channels` (viewer) and `POST` (admin): `{name, kind, config, enabled?}`.
  - `PATCH` (admin): `{name?, enabled?, kind?+config?}`. A config replaces the old one as a whole and must keep its kind; a kind change is a 400.
  - `DELETE /projects/{id}/notification-channels/{channel_id}` (admin): 409 `channel is used by escalation policy "<name>"` once Task 8 exists (until then, always deletable).
  - `GET|POST /account/notification-channels` and `PATCH|DELETE /account/notification-channels/{channel_id}` (any logged-in user, own channels only; another user's id is 404; `email` kind → 400).
  - Create returns `201 CreatedChannel`. The signing secret (webhooks only) appears in that one response and never again. Over the limit → 409 `channel limit reached (50 per project)` / `(10 per user)`.

- [ ] **Step 1: Write the migration**

`nami-engine/migrations/0061_create_notification_channels.sql`:

```sql
-- Notification channels (NAMI-INC-3). A channel belongs to a project (a team
-- room, admin-managed) or to one user (personal). Secrets are sealed with
-- crypto::Cipher under per-row contexts; `summary` is what the UI may show.
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
#[test]
fn urls_follow_their_rule() {
    assert!(check_url("https://hooks.slack.com/services/T/B/x", SLACK).is_ok());
    for bad in ["http://hooks.slack.com/services/x", "https://hooks.slack.com.evil.test/x",
                "https://evil.test/hooks.slack.com/", "https://user:pw@hooks.slack.com/x"] {
        assert!(check_url(bad, SLACK).is_err(), "{bad}");
    }
    assert!(check_url("https://discord.com/api/webhooks/1/abc", DISCORD).is_ok());
    assert!(check_url("https://discordapp.com/api/webhooks/1/abc", DISCORD).is_ok());
    assert!(check_url("https://discord.com/other/1/abc", DISCORD).is_err());
    assert!(check_url("http://10.0.0.5:8080/hook", UrlRule::HttpOrHttps).is_ok(), "private receivers are allowed");
    for bad in ["ftp://x.test/", "http://169.254.169.254/latest", "http://[fe80::1]/", "http://0.0.0.0/",
                "http://[::ffff:169.254.169.254]/", "javascript:alert(1)", "http:///nohost"] {
        assert!(check_url(bad, UrlRule::HttpOrHttps).is_err(), "{bad}");
    }
    assert!(check_url("http://teams.example/x", UrlRule::Https).is_err());
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
    // `GuardedResolver` is the checker's, moved here unchanged; its address filter
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

In `models/channel.rs` `mod tests`:

```rust
#[test]
fn telegram_tokens_and_chat_ids_are_shaped() {
    let ok = ChannelConfig::Telegram { bot_token: format!("123456:{}", "A".repeat(35)), chat_id: "-1001234".into() };
    assert!(ok.validate().is_ok());
    for (token, chat) in [("123456:short", "1"), ("abc:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/../x", "1"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "@ab"),
                          ("123456:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "1; drop")] {
        let c = ChannelConfig::Telegram { bot_token: token.into(), chat_id: chat.into() };
        assert!(c.validate().is_err(), "{token} {chat}");
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

`nami-engine/tests/notification_channels.rs`:

```rust
// Notification channels: personal and project, sealed and write-only.
// Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_project, test_app, test_cipher, user_with_role};
use nami_engine::models::channel::{config_context, signing_secret_context, ChannelConfig};
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

Run: engine command `--test notification_channels --lib outbound models::channel`. Expected: FAIL.

- [ ] **Step 4: Implement** `outbound.rs`, `models/channel.rs` and `routes/channels.rs` as specified.
- `outbound::client()`: `reqwest::Client::builder().no_proxy().dns_resolver(GuardedResolver).redirect(Policy::none()).referer(false).timeout(OUTBOUND_TIMEOUT).connect_timeout(CONNECT_TIMEOUT).pool_max_idle_per_host(0).user_agent(concat!("Nami-Notify/", env!("CARGO_PKG_VERSION")))`.
- `describe_error` order: `is_timeout` → "timeout"; the error chain contains `DestinationNotAllowed` → "destination not allowed"; `is_connect` → "connection failed"; `status()` → `HTTP {code}`; else "request failed".
- `ChannelConfig::validate` uses `check_url` (`SLACK`, `DISCORD`, `UrlRule::Https` for Teams, `UrlRule::HttpOrHttps` for webhooks). It also uses the Telegram regexes from R9, an email check via `lettre::Address::from_str`, and lengths (URL ≤ 2,048 bytes, address ≤ 254).
- Routes refuse a `kind` not in the list with 400 `unknown channel kind`.
- Every config is serialised to JSON, then `cipher.encrypt(&config_context(id), ..)`.

- [ ] **Step 5: Run** engine command `--test notification_channels --test migrations --test uptime_checker --test uptime_checker_proxy --test checker_http_config`, then the full suite and clippy. Expected: green (the checker tests prove the move changed nothing).

- [ ] **Step 6: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/migrations/0061_create_notification_channels.sql \
  nami-engine/src nami-engine/tests/notification_channels.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): guarded outbound client and sealed notification channels (NAMI-INC-3)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4 (NAMI-INC-4): Notification formatting with per-channel escaping and webhook signatures

Branch `feat/NAMI-INC-4-notification-format`, worktree `.worktrees/NAMI-INC-4/`.

**Files:**
- Create: `nami-engine/src/incidents/format.rs`
- Modify: `nami-engine/src/incidents/mod.rs`
- Test: unit tests in `format.rs`

**Interfaces:**
- Consumes: `uptime::checker::is_unsafe_in_text`, `hmac 0.13`, `sha2`.
- Produces (`nami_engine::incidents::format`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent { Triggered, Resolved, Test }
impl NotifyEvent { pub fn webhook_name(self) -> &'static str }  // "incident.triggered" | "incident.resolved" | "test"

#[derive(Debug, Clone)]
pub struct IncidentSummary { pub id: Uuid, pub title: String, pub cause: Option<String>, pub status: String,
    pub source: String, pub severity: String, pub started_at: DateTime<Utc> }
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
pub fn signature(secret: &[u8], body: &[u8]) -> String;    // "sha256=<lowercase hex>"
pub fn escape_slack(s: &str) -> String;
pub fn escape_discord(s: &str) -> String;
pub fn escape_telegram(s: &str) -> String;
pub fn escape_teams(s: &str) -> String;
pub const MAX_CHAT_CAUSE_CHARS: usize = 1000;
```

Content rules:
- **Every user text is first made one line.** Title and project name go through `is_unsafe_in_text` → space. The cause keeps `\n` (shown as a block) but loses every other unsafe char, and is cut to 1,000 chars with `…` for chat (the email keeps 10 KiB).
- **Headline.** Triggered: `[<severity>] <title>`. Resolved: `Resolved: <title>`. Test: `Test notification from Nami`.
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
- **Slack.** `{"text": <escaped headline>, "blocks": [section mrkdwn "*<escaped headline>*\n<escaped project> · <escaped severity> · <status>", section mrkdwn "<escaped cause>" (when any), section mrkdwn "<url|View incident>"]}`. `escape_slack` replaces `&`→`&amp;`, `<`→`&lt;`, `>`→`&gt;` (so `<!channel>`, `<@U…>` and `<http://x|y>` become inert). The only `<url|…>` is built from `incident_url`, which is ids only.
- **Discord.** `{"content": "<escaped headline>\n<escaped project> · <severity> · <status>\n<escaped cause>\n<incident_url>", "allowed_mentions": {"parse": []}}`, cut to 2,000 chars. `escape_discord` puts a backslash before `\ * _ ~ ` ` | > # - [ ] ( ) @ :` and before `<`.
- **Telegram.** `{"chat_id": .., "text": "<b>headline</b>\n...\n<a href=\"<incident_url>\">View incident</a>", "parse_mode": "HTML", "disable_web_page_preview": true}`. `escape_telegram` handles `&`, `<`, `>` (and `"` for the attribute). The text is cut to 4,096 chars before the link line is appended.
- **Teams.** `{"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {"$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "type": "AdaptiveCard", "version": "1.4", "body": [TextBlock headline (weight bolder, wrap), TextBlock "<project> · <severity> · <status>", TextBlock cause (wrap)], "actions": [{"type": "Action.OpenUrl", "title": "View incident", "url": incident_url}]}}]}`. `escape_teams` puts a backslash before each of `\ * _ [ ] ( ) ` # > - !` and replaces `&`, `<`, `>` with entities.
- **Webhook.** `{"event": .., "incident": {"id", "title", "cause", "status", "source", "severity", "started_at", "url"}, "project": {"id", "name"}}`, serialised once. `signature` is computed over exactly those bytes.

- [ ] **Step 1: Write the failing tests** in `format.rs` `mod tests`:

```rust
use super::*;

fn message(title: &str, cause: Option<&str>) -> Message {
    Message {
        event: NotifyEvent::Triggered,
        incident: IncidentSummary { id: Uuid::nil(), title: title.into(), cause: cause.map(Into::into),
            status: "open".into(), source: "monitor".into(), severity: "critical".into(),
            started_at: DateTime::from_timestamp(1_792_000_000, 0).unwrap() },
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
    assert_eq!(signature(b"Jefe", b"what do ya want for nothing?"),
        "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}
```

Run: engine command `--lib incidents::format`. Expected: FAIL to compile.

- [ ] **Step 2: Implement** `format.rs` to the rules above. `signature` uses `Hmac::<Sha256>::new_from_slice(secret)`, `update(body)`, then `hex::encode(finalize().into_bytes())`.

- [ ] **Step 3: Run** engine command `--lib incidents::format`, then clippy. Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src/incidents
git commit -m "feat(engine): per-channel notification formatting with escaping and signatures (NAMI-INC-4)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 5 (NAMI-INC-5): Durable delivery queue, senders per channel, "send test", and the incidents worker loop

Branch `feat/NAMI-INC-5-notification-deliveries`, worktree `.worktrees/NAMI-INC-5/`.

**Files:**
- Create: `nami-engine/migrations/0062_create_notification_deliveries.sql`, `nami-engine/src/incidents/send.rs`, `nami-engine/src/incidents/deliveries.rs`, `nami-engine/src/incidents/worker.rs`
- Modify: `nami-engine/src/incidents/mod.rs`, `nami-engine/src/config.rs` (`telegram_api_base_url`, env `TELEGRAM_API_BASE_URL`, default `https://api.telegram.org`, must be an absolute http(s) URL with no path, query or userinfo), `nami-engine/.env.example`, `nami-engine/src/main.rs` (spawn `incidents::worker::run_forever` next to the uptime worker, under the same flag, R23), `nami-engine/src/routes/channels.rs` (the two `/test` routes), `nami-engine/tests/common/mod.rs` (`run_incident_ticks`, `insert_channel`), `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/notification_deliveries.rs`

**Interfaces:**
- Consumes: Task 3 `outbound::{client, describe_error}`, `models::channel::{open_for_send, OpenedChannel, ChannelConfig}`. Task 4 `format::*`. Task 1 `lifecycle::{record_event, EventKind}`. `email::Mailer::send`.
- Produces:

```rust
// incidents::send
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError { pub message: String, pub retry: Retry }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry { Never, After(Option<Duration>) }   // After(Some(d)) = Retry-After from a 429
pub struct Senders { pub http: reqwest::Client, pub telegram_base: String }
impl Senders { pub fn new(telegram_base: &str) -> Senders }  // http = outbound::client()
/// One request (or one email). Re-validates the destination (R9) before sending.
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
pub async fn run_forever(state: AppState);
/// One drain now (Task 9 adds one escalation tick before it). Used by notify_transition (Task 10) and tests.
pub async fn kick(state: &AppState);
```

- Routes:
  - `POST /projects/{id}/notification-channels/{channel_id}/test` (admin) and `POST /account/notification-channels/{channel_id}/test` (owner), body `{}`.
  - Each sends a `NotifyEvent::Test` message synchronously (not queued) and answers `200 {"ok": true}`, or `502 {"error": "<describe_error text>"}` on failure.
  - At most one test per channel per 10 s → 429 `wait a few seconds before testing again`. The limit is enforced atomically on `last_tested_at`: `update notification_channels set last_tested_at = now() where id = $1 and (last_tested_at is null or last_tested_at < now() - interval '10 seconds') returning id`.
  - A test sends to a disabled channel too (that is how it is checked before enabling).

- [ ] **Step 1: Write the migration**

`nami-engine/migrations/0062_create_notification_deliveries.sql`:

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

Migration test `a_target_gets_one_delivery_per_incident_round`: a second insert with the same `(incident_id, round, target_key)` fails with `23505`.

- [ ] **Step 2: Write the failing tests**

`deliveries.rs` `mod tests`:

```rust
#[test]
fn backoff_is_30s_times_4_to_the_attempt_capped_at_an_hour() {
    let secs: Vec<u64> = (1..=6).map(|a| backoff(a).as_secs()).collect();
    assert_eq!(secs, [30, 120, 480, 1920, 3600, 3600]);
}
```

`nami-engine/tests/notification_deliveries.rs`:

```rust
// The notification queue and its senders. Requires `docker compose up -d postgres mailpit`.
mod common;

use axum::http::StatusCode;
use common::{call, insert_channel, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app,
    user_with_role};
use nami_engine::{
    incidents::deliveries::{self, DeliveryEvent, NewDelivery, Target},
    models::channel::ChannelConfig,
    state::AppState,
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
async fn webhooks_are_signed_and_carry_the_delivery_id(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST")).and(header_exists("x-nami-signature")).and(header_exists("x-nami-delivery"))
        .respond_with(ResponseTemplate::new(204)).expect(1).mount(&hook).await;
    let channel = insert_channel(&pool, Some(project), None, "hook", ChannelConfig::Webhook { url: hook.uri() }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;

    let request = &hook.received_requests().await.unwrap()[0];
    let sealed: Vec<u8> = sqlx::query_scalar("select signing_secret_enc from notification_channels where id = $1")
        .bind(channel).fetch_one(&pool).await.unwrap();
    let secret = state.cipher.decrypt(&nami_engine::models::channel::signing_secret_context(channel), &sealed).unwrap();
    let expected = nami_engine::incidents::format::signature(&secret, &request.body);
    assert_eq!(request.headers.get("x-nami-signature").unwrap().to_str().unwrap(), expected);
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
```

`tests/common/mod.rs` gains:

```rust
/// Seals and inserts a channel without the host allowlist (R9), so tests can target wiremock.
pub async fn insert_channel(pool: &sqlx::PgPool, project: Option<Uuid>, user: Option<Uuid>, name: &str,
    config: nami_engine::models::channel::ChannelConfig) -> Uuid
/// One escalation tick (from Task 9 on) and one delivery drain, as the worker would run them.
pub async fn run_incident_ticks(state: &AppState) { nami_engine::incidents::worker::kick(state).await }
```

`insert_channel` needs a `created_by`. It uses `user`, or for project channels it seeds an admin through `seed_user`.

Run: engine command `--test notification_deliveries --lib incidents::deliveries`. Expected: FAIL.

- [ ] **Step 3: Implement**

`deliveries::drain`:

1. Claim (one statement):

```sql
update notification_deliveries d set status = 'sending', locked_until = now() + interval '60 seconds',
    attempts = d.attempts + 1
where d.id in (
    select id from notification_deliveries
    where (status = 'pending' and next_attempt_at <= now())
       or (status = 'sending' and locked_until < now())
    order by next_attempt_at
    for update skip locked
    limit $1)
returning d.id, d.incident_id, d.event, d.round, d.user_id, d.channel_id, d.attempts
```

2. Per claimed row, in a task bounded by a `Semaphore(MAX_SENDS_IN_FLIGHT)`:
   a. **Cancel (R15).** If `event = 'triggered'` and the incident's status is not `open`: `status = 'cancelled'`, no event.
   b. **Rate cap (R13).** Count the sends in the last minute for this channel (or for this user's email). At or above the cap: `status = 'pending', attempts = attempts - 1, next_attempt_at = now() + 60 s`, and stop.
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
- webhook: `webhook_body`, headers `content-type: application/json`, `x-nami-signature`, `x-nami-delivery: <id>`, `x-nami-event`.
- slack / discord / teams: their JSON.
- telegram: `{telegram_base}/bot{token}/sendMessage` with `format::telegram`.
- email (project channel): `send_email` to the configured address.

Before every request it re-runs the destination rules (`check_url` with the kind's rule). A refusal is `Retry::Never` with "destination not allowed". Responses: 2xx is success; 429 is `After(Retry-After capped at 1 h)`; 408 and 5xx are `After(None)`; any other 4xx is `Never`. Transport errors are `After(None)` with `describe_error`. Mailer errors are `After(None)` with "email not delivered". The response body is read only to drain it (at most 64 KiB) and is never stored.

`worker::run_forever`: every `TICK` (missed ticks delayed), spawn one iteration in its own task (a panic is logged, never fatal) that calls `kick`. `kick` builds `Senders::new(&state.config.telegram_api_base_url)` (cheap; read per call so tests can point it at wiremock), calls `deliveries::drain` once, and logs errors with `tracing::error!` (no secrets).

- [ ] **Step 4: Run** engine command `--test notification_deliveries --test notification_channels --test migrations --lib incidents`, then the full suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/0062_create_notification_deliveries.sql nami-engine/src nami-engine/.env.example \
  nami-engine/tests/notification_deliveries.rs nami-engine/tests/common/mod.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): durable notification deliveries with retries, caps and channel tests (NAMI-INC-5)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6 (NAMI-INC-6): On-call rotation math and time windows (pure, DST-tested)

Branch `feat/NAMI-INC-6-oncall-rotation`, worktree `.worktrees/NAMI-INC-6/`.

**Files:**
- Create: `nami-engine/src/timewindow.rs`, `nami-engine/src/oncall/mod.rs`, `nami-engine/src/oncall/rotation.rs`
- Modify: `nami-engine/src/lib.rs` (`pub mod timewindow; pub mod oncall;`)
- Test: unit tests in both files

**Interfaces:**
- Consumes: `models::maintenance::{Schedule, weekday_from_iso}` (R19), `chrono-tz`.
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

70 days are ~100k evaluations, fine for a request. The tests pin correctness, not speed.

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
use crate::timewindow::TimeWindow;

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

Run: engine command `--lib timewindow oncall`. Expected: FAIL to compile.

- [ ] **Step 2: Implement** both modules as specified.
- [ ] **Step 3: Run** engine command `--lib timewindow oncall --lib models::maintenance`, then clippy. Expected: PASS.
- [ ] **Step 4: Commit**

```bash
git add nami-engine/src/timewindow.rs nami-engine/src/oncall nami-engine/src/lib.rs
git commit -m "feat(engine): on-call rotation math and time windows, DST-safe (NAMI-INC-6)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7 (NAMI-INC-7): On-call schedules API — schedules, participants, restrictions, overrides, "on call now", shifts

Branch `feat/NAMI-INC-7-schedules-api`, worktree `.worktrees/NAMI-INC-7/`.

**Files:**
- Create: `nami-engine/migrations/0063_create_on_call_schedules.sql`, `nami-engine/src/models/schedule.rs`, `nami-engine/src/routes/schedules.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/schedules_api.rs`

**Interfaces:**
- Consumes: Task 6 `oncall::rotation::*`, `timewindow::{TimeWindow, parse_timezone}`.
- Produces:

```rust
// models::schedule
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
- `name` via `models::validate_name`, unique per project (409).
- `rotation` is `daily` | `weekly` | `custom`; `rotation_hours` is required for `custom` (1-8760) and must be absent otherwise.
- `handoff_time` is `HH:MM`.
- `participants` are 1-50 distinct users who are current members of the project (a non-member → 400 `user <id> is not a member of this project`).
- `restrictions` hold at most 14 windows, each passing `TimeWindow::validate`.
- `start_date` lies within ±5 years of today.
- Override: `ends_at > starts_at`, `ends_at > now()`, length ≤ 90 days, the user is a project member.

`until` in `on_call_now` is the end of the current merged shift (`shifts(now, now + 14 days)[0].ends_at`). Shifts show each user's email and `is_member` (R7: non-members are listed but flagged, and never paged).

- [ ] **Step 1: Write the migration**

`nami-engine/migrations/0063_create_on_call_schedules.sql`:

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

Migration test `a_custom_rotation_needs_hours_and_others_must_not_have_them` (two failing inserts, `23514`).

- [ ] **Step 2: Write the failing API tests**

`nami-engine/tests/schedules_api.rs`:

```rust
// On-call schedules. Requires `docker compose up -d postgres`.
mod common;

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

Run: engine command `--test schedules_api --test migrations`. Expected: FAIL.

- [ ] **Step 3: Implement** the model and routes. Writes run in one transaction. Participants are rewritten as positions 0..n-1. `updated_at = now()` on replace. The `InUse` check is added in Task 8 (until then `delete` never returns it).
- [ ] **Step 4: Run** engine command `--test schedules_api --test migrations`, then the full suite and clippy. Expected: green.
- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/0063_create_on_call_schedules.sql nami-engine/src nami-engine/tests/schedules_api.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): on-call schedules with restrictions, overrides and shifts (NAMI-INC-7)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 8 (NAMI-INC-8): Escalation policies — schema, default policy, pure planner, API

Branch `feat/NAMI-INC-8-escalation-policies`, worktree `.worktrees/NAMI-INC-8/`.

**Files:**
- Create: `nami-engine/migrations/0064_create_escalation_policies.sql`, `nami-engine/src/incidents/escalation/mod.rs`, `nami-engine/src/incidents/escalation/plan.rs`, `nami-engine/src/models/escalation_policy.rs`, `nami-engine/src/routes/escalation_policies.rs`
- Modify:
  - `nami-engine/src/incidents/lifecycle.rs`: `OpenIncident.escalation_policy_id: Option<Uuid>`. `open`, `reopen`, `acknowledge` and `resolve` arm or disarm escalation. Every construction site in Task 1/2 code and tests sets the new field to `None`.
  - `nami-engine/src/models/channel.rs` and `nami-engine/src/models/schedule.rs`: `delete` returns `InUse` when a step targets the row.
  - `nami-engine/src/routes/incidents.rs`: manual create accepts `escalation_policy_id` (a foreign id → 404).
  - `nami-engine/src/models/mod.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/tests/common/mod.rs` (`policy_body`), `nami-engine/tests/migrations.rs`.
- Test: `nami-engine/tests/escalation_policies_api.rs` (+ unit tests in `plan.rs`)

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

// models::escalation_policy
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
pub async fn belongs_to(conn: &mut PgConnection, project_id: Uuid, policy_id: Uuid) -> Result<bool, sqlx::Error>;
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

`nami-engine/migrations/0064_create_escalation_policies.sql`:

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
    schedule_id uuid references on_call_schedules(id) on delete restrict,
    channel_id uuid references notification_channels(id) on delete restrict,
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

`nami-engine/tests/escalation_policies_api.rs`:

```rust
// Escalation policies. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, insert_channel, seed_project, test_app, user_with_role};
use nami_engine::models::channel::ChannelConfig;
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

Run: engine command `--test escalation_policies_api --test migrations`. Expected: FAIL.

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

- [ ] **Step 5: Run** engine command `--test escalation_policies_api --test incident_lifecycle --test incidents_api --test notification_channels --test schedules_api --test migrations --lib incidents`, then the full suite and clippy. Expected: green.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/0064_create_escalation_policies.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): escalation policies, default policy and the escalation planner (NAMI-INC-8)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 9 (NAMI-INC-9): Escalation engine — targets to recipients, steps, repeats, fallback, resolve notifications

Branch `feat/NAMI-INC-9-escalation-engine`, worktree `.worktrees/NAMI-INC-9/`.

**Files:**
- Create: `nami-engine/src/incidents/escalation/run.rs`
- Modify: `nami-engine/src/incidents/escalation/mod.rs`, `nami-engine/src/incidents/worker.rs` (`kick` = escalation tick, then drain), `nami-engine/src/incidents/lifecycle.rs` (`resolve` queues "resolved" deliveries), `nami-engine/tests/common/mod.rs` (`make_escalation_due`, `set_policy`)
- Test: `nami-engine/tests/escalation_engine.rs`

**Interfaces:**
- Consumes: Task 5 `deliveries::{enqueue, NewDelivery, Target, DeliveryEvent}`. Task 6 `oncall::rotation::on_call_at`. Task 7 `models::schedule::load_rotation`. Task 8 `plan::{after_step, round, PolicyShape, Cursor, Next, Exhausted}`. `timewindow`.
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
// `sent` triggered delivery in the current generation (or, for `escalation_legacy` incidents,
// one email per project member), round "resolved:g{generation}".
```

`tests/common/mod.rs` gains:

```rust
/// Makes the incident's next escalation step due now.
pub async fn make_escalation_due(pool: &sqlx::PgPool, incident: Uuid)
/// Replaces the project's default policy (PUT) through the API as `admin`; returns its id.
pub async fn set_policy(app: &Router, project: Uuid, admin: &HeaderValue, body: serde_json::Value) -> Uuid
```

**The tick** runs one transaction per incident:
1. `select ... from incidents where status = 'open' and next_escalation_at <= now() order by next_escalation_at limit 1 for update skip locked`. Stop when none is left or after `ESCALATION_BATCH` incidents.
2. Load the incident's policy, its fallback and the `active` one (fallback when `escalation_on_fallback`).
   - A missing policy (deleted) → `next_escalation_at = null` + `escalation_exhausted` "escalation policy was deleted".
   - A cursor past the active policy's last step (the policy was shortened) runs as the end of the pass with 0 newly notified.
3. Run the step scheduled at `ran_at = next_escalation_at`:
   - **Condition false.** If the step has a condition that does not cover `ran_at`, record a `step_skipped` event (`"Step <n> skipped: outside <weekdays> <from>-<to> <timezone>"`) with 0 newly notified.
   - **Recipients.** Otherwise call `recipients(project, step, ran_at)`. Then cap at `MAX_DELIVERIES_PER_INCIDENT - existing` (when the cap is hit, add one `notification_failed` event "notification cap reached (500 per incident)", once per incident, detected by `metadata->>'cap' = 'true'`).
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

`nami-engine/tests/escalation_engine.rs`:

```rust
// The escalation engine. Requires `docker compose up -d postgres mailpit`.
mod common;

use axum::http::{HeaderValue, StatusCode};
use chrono::{Duration, Utc};
use common::{call, insert_channel, make_escalation_due, run_incident_ticks, seed_project, set_policy,
    settled_mail, test_app, user_with_role};
use nami_engine::{incidents::escalation::run, models::channel::ChannelConfig, state::AppState};
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
```

Run: engine command `--test escalation_engine`. Expected: FAIL.

- [ ] **Step 2: Implement** `run.rs`, the `resolve` hook and `kick` as specified above.
- [ ] **Step 3: Run** engine command `--test escalation_engine --test notification_deliveries --test incidents_api --test incident_lifecycle --lib incidents`, then the full suite and clippy. Expected: green.
- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests/escalation_engine.rs nami-engine/tests/common/mod.rs
git commit -m "feat(engine): escalation engine with schedules, repeats, fallback and resolve notices (NAMI-INC-9)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10 (NAMI-INC-10): Route monitors, heartbeats and log alerts through escalation policies and channels

Branch `feat/NAMI-INC-10-incident-routing`, worktree `.worktrees/NAMI-INC-10/`.

**Files:**
- Create: `nami-engine/migrations/0065_source_escalation_routing.sql`
- Modify:
  - `nami-engine/src/models/incident.rs`: the six wrappers read the source's `escalation_policy_id`/`severity_id` and pass them to `lifecycle::open`.
  - `nami-engine/src/uptime/alerts.rs`: `notify_transition` calls `incidents::worker::kick`; `render` and its tests are removed; `sanitize_cause` stays.
  - `nami-engine/src/models/{monitor,heartbeat,log_alert}.rs` and their routes: the two new optional fields in create/patch bodies and views.
  - The existing tests that assert alert subjects (R18): `tests/uptime_worker.rs`, `tests/heartbeats.rs`, `tests/maintenance_windows.rs`, `tests/recovery_threshold.rs`, `tests/uptime_worker_timing.rs`, `tests/checker_http_config.rs`, `tests/log_alerts.rs`.
  - `nami-engine/tests/migrations.rs`.
- Test: `nami-engine/tests/incident_routing.rs`

**Interfaces:**
- Consumes: Tasks 1-9. Logs Part 2: `log_alerts`, `logs::alerts::{claim_due_alerts, apply_evaluation}`.
- Produces:
  - `Monitor`/`MonitorView`, `Heartbeat`/`HeartbeatView` and `LogAlert` gain `escalation_policy_id: Option<Uuid>` and `severity_id: Option<Uuid>`.
  - Create bodies accept them as optional fields. Patch bodies accept them as `Option<Option<Uuid>>` (`null` clears, so the project default applies; deserialized with `models::nullable`, like `group_id`).
  - An id from another project is rejected exactly the way a foreign `group_id` is today (same status and message style, checked with `escalation_policy::belongs_to` / `severity::belongs_to` inside the write transaction).
  - `alerts::notify_transition(state, transition)` keeps its signature. Its body is now `crate::incidents::worker::kick(state).await`, because the incident and its deliveries were already written in the source's transaction.

- [ ] **Step 1: Write the migration**

`nami-engine/migrations/0065_source_escalation_routing.sql`:

```sql
-- Monitors, heartbeats and log alerts pick an escalation policy and a
-- severity for the incidents they open (NAMI-INC-10). Null = the project
-- default; deleting the policy or severity falls back to the default.
alter table monitors
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
alter table heartbeats
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
alter table log_alerts
    add column escalation_policy_id uuid references escalation_policies(id) on delete set null,
    add column severity_id uuid references incident_severities(id) on delete set null;
```

Migration test `deleting_a_policy_clears_it_on_monitors`.

- [ ] **Step 2: Write the failing routing tests**

`nami-engine/tests/incident_routing.rs`:

```rust
// Monitors, heartbeats and log alerts open incidents through escalation.
// Requires `docker compose up -d postgres clickhouse mailpit`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, set_policy, settled_mail, state_with_clickhouse, test_app,
    user_with_role};
use nami_engine::{logs::alerts, uptime::{alerts as uptime_alerts, worker}};
use serde_json::json;
use uuid::Uuid;
use wiremock::{matchers::any, Mock, MockServer, ResponseTemplate};

async fn severity(pool: &sqlx::PgPool, project: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar("select id from incident_severities where project_id = $1 and name = $2")
        .bind(project).bind(name).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn a_monitor_pages_through_its_own_policy_with_its_severity(pool: sqlx::PgPool) {
    let (state, _ch) = state_with_clickhouse(pool.clone()).await;
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
    let (state, _ch) = state_with_clickhouse(pool.clone()).await;
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
    let (state, _ch) = state_with_clickhouse(pool.clone()).await;
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
    let (state, _ch) = state_with_clickhouse(pool.clone()).await;
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
    let due = alerts::claim_due_alerts(&pool, 10).await.unwrap();
    let transition = alerts::apply_evaluation(&pool, &due[0], 12, chrono::Utc::now()).await.unwrap().unwrap();
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

`state_with_clickhouse`, `make_due` and the heartbeat create body follow the existing helpers and API in `tests/common/mod.rs` and `tests/heartbeats.rs`. Where a field name differs (for example `grace_seconds`), use the existing one.

Run: engine command `--test incident_routing --test migrations`. Expected: FAIL.

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

Delete `render` and its unit tests. The escaping they covered (CR/LF in names never reaching a subject) is covered by `incidents::format` (`email_subject_and_body_carry_severity_and_links_on_single_lines`). Fix the uses of `render` that the compiler reports. Logs Part 2's `log_alert_emails_say_triggered_and_resolved...` test goes with it.

- [ ] **Step 4: Update the existing subject assertions (R18)**

Run the full engine suite. The failures to expect are only subject strings and, where a test counted on one email per member per transition, nothing else. Rewrite each subject to the new form:
- `"[Nami] X is DOWN"` → `"[Nami] [high] X is down"`
- `"[Nami] X is back UP"` → `"[Nami] Resolved: X is down"`
- heartbeats: `"[Nami] [high] X missed its heartbeat"` / `"[Nami] Resolved: X missed its heartbeat"`
- log alerts: `"[Nami] [high] Log alert: X"` / `"[Nami] Resolved: Log alert: X"`

Body assertions that looked for `Cause: …` keep working (the format keeps that line). Any other failure is a behaviour change: stop and fix the code, not the test.

- [ ] **Step 5: Run** the full engine suite and clippy. Expected: green.
- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/0065_source_escalation_routing.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): monitors, heartbeats and log alerts page through escalation policies (NAMI-INC-10)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11 (NAMI-INC-11): Signed, single-use acknowledge/resolve links in emails, and the confirmation page

Branch `feat/NAMI-INC-11-action-links`, worktree `.worktrees/NAMI-INC-11/`.

**Files:**
- Create: `nami-engine/migrations/0066_create_incident_action_token_uses.sql`, `nami-engine/src/incidents/action_token.rs`, `nami-engine/src/routes/incident_actions.rs`, `nami-ui/src/routes/incidents/action/+page.server.ts`, `nami-ui/src/routes/incidents/action/+page.svelte`, `nami-ui/src/routes/incidents/action/action.test.ts`
- Modify:
  - `nami-engine/src/incidents/deliveries.rs`: implicit-email deliveries of `triggered` events carry `ActionLinks` for that user when they are a `member`/`admin` of the project or a global admin.
  - `nami-engine/src/incidents/worker.rs`: prune token uses older than 2 days, once an hour.
  - `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs` (merge behind `rate_limited_with_governor`), `nami-engine/tests/migrations.rs`.
- Test: `nami-engine/tests/incident_actions.rs` (+ unit tests in `action_token.rs`), `action.test.ts`

**Interfaces:**
- Consumes: `crypto::Cipher::derive_subkey`, Task 1 `lifecycle::{acknowledge, resolve}`, `rbac` role ranks.
- Produces:

```rust
// incidents::action_token
pub const TOKEN_TTL: chrono::Duration = chrono::Duration::hours(24);
pub const SUBKEY_INFO: &str = "nami/incident-action/v1";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action { Acknowledge, Resolve }   // "ack" | "resolve" in the payload
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims { pub incident_id: Uuid, pub action: Action, pub user_id: Uuid, pub expires_at: DateTime<Utc> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid { Malformed, BadSignature, Expired }
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

`confirm` runs in one transaction:
1. Verify the token.
2. Load and lock the incident.
3. Check the role.
4. `insert into incident_action_token_uses (token_hash, incident_id, expires_at) values (...) on conflict do nothing returning token_hash`. No row → 410.
5. Run `lifecycle::acknowledge`/`resolve` with the token's user. `false` → roll back (409).
6. Commit and `kick` the worker (a resolve queues notices).

- [ ] **Step 1: Migration**

`nami-engine/migrations/0066_create_incident_action_token_uses.sql`:

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

- [ ] **Step 2: Failing unit tests** in `action_token.rs` `mod tests`:

```rust
use super::*;
use crate::crypto::{Cipher, TEST_KEY};

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

`nami-engine/tests/incident_actions.rs`:

```rust
// Acknowledge/resolve links. Requires `docker compose up -d postgres mailpit`.
mod common;

use axum::http::StatusCode;
use chrono::Utc;
use common::{call, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app, user_with_role};
use nami_engine::{incidents::action_token::{self, Action, Claims}, state::AppState};
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
    let late_ack = token(&state, id, alice, Action::Acknowledge);
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
- a link to the incident page (`/projects/{project_id}/incidents/{id}`), which needs a login.

The page renders text only through Svelte expressions (escaped).

`action.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/fake-event';
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

- [ ] **Step 5: Run** engine command `--test incident_actions --test notification_deliveries --test migrations --lib incidents::action_token`, the full engine suite and clippy, then the UI command with `src/routes/incidents/action`. Expected: green.
- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/0066_create_incident_action_token_uses.sql nami-engine/src nami-engine/tests \
  nami-ui/src/routes/incidents/action
git commit -m "feat(engine,ui): signed single-use acknowledge/resolve links with a confirmation page (NAMI-INC-11)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12 (NAMI-INC-12): UI — incident list, detail with timeline, actions, comments, postmortem, manual incidents

Branch `feat/NAMI-INC-12-ui-incidents`, worktree `.worktrees/NAMI-INC-12/`.

**Files:**
- Create: `nami-ui/src/lib/markdown.ts` (+ `markdown.test.ts`), `nami-ui/src/lib/components/SeverityBadge.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/[incidentId=uuid]/+page.server.ts`, `+page.svelte`, `incident-detail.test.ts`
- Modify: `nami-ui/src/lib/incidents.ts` (+ `incidents.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/+page.server.ts`, `+page.svelte`, and its two tests. Keep Logs Part 2's log-alert link on list rows.
- Test: the files above

**Interfaces:**
- Consumes: Task 2 routes; Task 8 policies (for the "New incident" policy select: `GET /projects/{id}/escalation-policies`).
- Produces:

```ts
// lib/incidents.ts
export const STATUSES = ['open', 'acknowledged', 'resolved'] as const;
export const SOURCES = ['monitor', 'heartbeat', 'log_alert', 'metric_alert', 'manual', 'integration', 'report_form'] as const;
export type ListTab = 'unresolved' | 'acknowledged' | 'resolved' | 'all';
export function parseListQuery(url: URL): { tab: ListTab; source: string | null; cursor: string | null };
export function engineListQuery(q: ReturnType<typeof parseListQuery>): string;   // "?status=..&source=..&cursor=.."
export function eventLabel(kind: string): string;          // "opened" → "Incident opened", unknown → kind
export function duration(startedAt: string, endedAt: string | null, now: Date): string;  // "3 h 12 min", "45 s"
export function sourceLink(projectId: string, i: Incident): string | null;  // monitor/heartbeat/log alert page
// lib/markdown.ts
export function renderMarkdown(source: string): string;   // R22: escaped HTML of a fixed subset
```

Page behaviour:
- **List** (`/projects/{id}/incidents`):
  - Tabs Unresolved (default), Acknowledged, Resolved, All. A source `<select>`. "Load more" carries the cursor in the query string.
  - Columns: severity badge, title (link to detail), source with link, started, duration, acknowledged by, status.
  - A "New incident" form (members; shown to everyone, and the engine's 403 surfaces as the form error): title, description, severity, escalation policy. The `create` action redirects to the new incident.
  - Query values are only forwarded after `parseListQuery` keeps them (known tab/source, cursor `^[A-Za-z0-9_-]{1,200}$`).
- **Detail** (`/projects/{id}/incidents/{incidentId}`):
  - Header with title, severity badge, status, started/duration, source link, acknowledged/resolved by.
  - Buttons: Acknowledge (open), Resolve (open/acknowledged), Reopen (resolved). Each is a named action posting `{}`.
  - Severity `<select>` (action `severity`).
  - Timeline, oldest first: event label, actor email, time (UTC), and message. Messages render as text (Svelte escapes); comments keep line breaks with `white-space: pre-wrap`.
  - Comment form (action `comment`).
  - Postmortem: rendered with `{@html renderMarkdown(postmortem)}`, the only `{@html}` in the feature, plus an edit form (textarea with `defaultValue`, action `postmortem`).
  - Cause shown in a `<pre>` (escaped).
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
import { fakeEvent, PROJECT_ID } from '#lib/server/fake-event';
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

Run: UI command with `src/lib/markdown.test.ts src/lib/incidents.test.ts "src/routes/(app)/projects/[id=uuid]/incidents"`. Expected: FAIL.

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

Branch `feat/NAMI-INC-13-ui-channels`, worktree `.worktrees/NAMI-INC-13/`.

**Files:**
- Create: `nami-ui/src/lib/server/channel-form.ts` (+ `channel-form.test.ts`), `nami-ui/src/lib/components/ChannelList.svelte`, `nami-ui/src/routes/(app)/account/notifications/+page.server.ts`, `+page.svelte`, `notifications.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/notifications/+page.server.ts`, `+page.svelte`, `project-channels.test.ts`
- Modify: `nami-ui/src/routes/(app)/+layout.svelte` (an "Notifications" link next to the existing account/security link), `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/+page.svelte` (link to the notifications sub-page)
- Test: the files above

**Interfaces:**
- Consumes: Task 3 and Task 5 routes.
- Produces:

```ts
// lib/server/channel-form.ts
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
- After a webhook create, the action result carries `signingSecret` once. It is shown in a copy box with "Shown once. Store it now." and never placed in a URL or a redirect.
- Both loads set `cache-control: private, no-store`.
- Channel ids from forms pass `isUuid` before reaching `enginePath`.
- The project page shows the forms only to admins (`data.role` from the project layout). Viewers see the list.

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

Branch `feat/NAMI-INC-14-ui-escalation`, worktree `.worktrees/NAMI-INC-14/`.

**Files:**
- Create: `nami-ui/src/lib/server/policy-form.ts` (+ `policy-form.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/escalation/+page.server.ts`, `+page.svelte`, `escalation.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/escalation/[policyId=uuid]/+page.server.ts`, `+page.svelte`, `policy-editor.test.ts`
- Modify:
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte`: an "Escalation" tab after "Incidents".
  - `nami-ui/src/lib/components/MonitorForm.svelte` and `nami-ui/src/lib/server/monitor-form.ts` (+ test).
  - `nami-ui/src/lib/server/heartbeat-form.ts` (+ test) and the heartbeat pages.
  - `nami-ui/src/lib/server/log-alert-form.ts` (+ test) and `nami-ui/src/lib/components/logs/LogAlertForm.svelte` (Logs Part 2).
  - The loads of the monitor new/edit, heartbeat and log-alert pages, which fetch `/escalation-policies` and `/severities` for the selects.
- Test: the files above

**Interfaces:**
- Consumes: Task 8 routes, Task 2 severities, Task 7 schedules (target choices), Task 3 project channels (target choices).
- Produces:

```ts
// lib/server/policy-form.ts
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
- members: `GET /projects/{id}` members, or the members endpoint the settings page already uses;
- schedules: `GET /projects/{id}/schedules`;
- project channels: `GET /projects/{id}/notification-channels`;
- policies: for the fallback, excluding itself.

The list page shows policies, the default badge, a "Make default" action and two-step delete (a 409 shows the engine message). Admin-only forms; viewers read.

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
- the load fetches the policy, schedules, project channels, members and policies (5 calls, ids via `enginePath`);
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

Branch `feat/NAMI-INC-15-ui-oncall`, worktree `.worktrees/NAMI-INC-15/`.

**Files:**
- Create: `nami-ui/src/lib/server/schedule-form.ts` (+ `schedule-form.test.ts`), `nami-ui/src/lib/oncall.ts` (+ `oncall.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/oncall/+page.server.ts`, `+page.svelte`, `oncall.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/oncall/[scheduleId=uuid]/+page.server.ts`, `+page.svelte`, `schedule-detail.test.ts`
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` ("On-call" tab after "Escalation"), `nami-ui/src/routes/(app)/projects/[id=uuid]/+page.server.ts` and `+page.svelte` ("On call now" card from `GET /projects/{id}/on-call`; a failure there hides the card and never fails the page)
- Test: the files above

**Interfaces:**
- Consumes: Task 7 routes.
- Produces:

```ts
// lib/server/schedule-form.ts
export function schedulePayloadFromForm(form: FormData):
	{ ok: true; payload: { name: string; timezone: string; rotation: 'daily' | 'weekly' | 'custom'; rotation_hours?: number;
	  handoff_time: string; start_date: string; participants: string[]; restrictions: { weekdays: number[]; from: string; to: string }[] } }
	| { ok: false; message: string };
export function overridePayloadFromForm(form: FormData, timezone: string):
	{ ok: true; payload: { user_id: string; starts_at: string; ends_at: string } } | { ok: false; message: string };
// lib/oncall.ts
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
- the load makes 3 calls (schedule, shifts, overrides) plus members, `no-store` is not required (no secrets), and a 404 → 404;
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

## After Task 15: roadmap ticks

Once NAMI-INC-15 merges, tick these items in `docs/superpowers/roadmap.md` §4 (one `docs:` PR, as for Logs Part 1):
- Incident lifecycle and manual incidents.
- Incident timeline (events, comments), postmortem notes.
- Escalation policies (multi-step, delays, repeat) per project.
- Escalation step time-window conditions and fallback policy.
- On-call schedules: rotations, handoff, overrides, "who's on call".
- Notification channels: email, generic webhook, Slack, Telegram, Discord; per-user and per-policy targets.
- Microsoft Teams channel (Adaptive Cards).
- Acknowledge/resolve from email links (signed, expiring tokens).
- Monitors, heartbeats and log alerts route through escalation policies.

Do not tick:
- "On-call working-hours restriction, shift reminder emails, per-user ICS feed, take over shift": only the restriction ships in Part 1.
- "Severities … and incident metadata": management is Part 2.
- "UI for all of the above": that waits for Part 2.

Record the rulings above (R1-R24) and any execution-time changes in `docs/superpowers/decisions/2026-10-13-incident-management-part1.md`.

## Final review (whole Part 1)

When NAMI-INC-1 .. NAMI-INC-15 are merged, run the sub-project's dedicated **security review + fix wave**, then a whole-Part-1 review, before Part 2 starts (CLAUDE.md: every sub-project ends with one; Part 2 ends with its own over the whole sub-project). The fix wave is one branch, `fix/NAMI-INC-part1-security`, with one PR. That PR lists each finding with severity, its fix and its test. It must check, with a test for every issue it finds:

1. **SSRF.**
   - Every send path (webhook, Slack, Discord, Teams, Telegram, project email, "send test") goes through `outbound::client()` and re-checks the destination before each request.
   - No redirect is followed.
   - A name resolving to `169.254.169.254`, `fd00:ec2::254` or `0.0.0.0` is refused at DNS time.
   - Mapped and NAT64 forms are refused.
   - `TELEGRAM_API_BASE_URL` is config, not user input.
2. **Secrets at rest and in transit.**
   - Channel configs and signing secrets are sealed under per-row contexts and never appear in responses, logs (INFO and below), the timeline, `Debug` output or error strings.
   - Grep the engine for `webhook_url`, `bot_token` and `signing_secret` uses outside `models/channel.rs` and `incidents/send.rs`.
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
   - Rate limiting applies per IP.
8. **RBAC.** A table of every route × role (outsider, viewer, member, admin, global admin) matches the spec's RBAC section.
9. **Failure behaviour.** A down SMTP server, ClickHouse or chat provider never blocks incident state changes, the uptime worker or the API. Deliveries retry per R12 and are visible in the timeline.

No CI is configured on this repo yet. Every PR description states that, and lists the engine and UI commands run with their results.
