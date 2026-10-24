# Incident Management + On-call, Part 2 Implementation Plan

> **Status:** outline. Nothing of Part 2 is merged. It starts once Incidents Part 1 (NAMI-INC-1 .. NAMI-INC-15 and its security fix wave) is on `main`; the step-level code is written then, against that `main`.
> **Branch:** `feat/incidents-part2` in `.worktrees/incidents-part2/`.
> **Depends on:** Incidents Part 1 (`docs/superpowers/plans/2026-10-13-incident-management-part1.md`), which itself follows Logs Part 2 and Uptime Phase 3.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking once the step-level code is written at phase start.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R4.06 | On-call working hours, reminders, ICS, take over. Part 2 half: shift reminder emails (24 h ahead) with opt-out, per-user ICS feed, "take over shift" (Part 1 ships the working-hours restriction) | NAMI-INC-25, NAMI-INC-26, NAMI-INC-27, NAMI-INC-32 |
| R4.10 | Slack Acknowledge/Resolve buttons (signed requests, optional per project) | NAMI-INC-21, NAMI-INC-29 |
| R4.12 | Incoming integrations: generic JSON webhook, Alertmanager, Grafana (secret URL, dedup, auto-resolve) | NAMI-INC-18, NAMI-INC-19, NAMI-INC-29 |
| R4.13 | Public incident report form (secret URL, per-IP rate limit, honeypot) | NAMI-INC-20, NAMI-INC-29, NAMI-INC-30 |
| R4.14 | Severities and incident metadata. Part 2 half: severity management (create, rename, reorder, default, delete) and metadata editing (Part 1 seeds severities read-only and stores `metadata`) | NAMI-INC-16, NAMI-INC-17, NAMI-INC-28 |
| R4.15 | Incident list filters. Part 2 half: severity, date range, `metadata.<key>`, `q` (Part 1 R21 ships status and source) | NAMI-INC-17, NAMI-INC-28 |
| R4.16 | Reporting: MTTA, MTTR, counts by source/severity/responder, SLA report (compound + average), CSV export | NAMI-INC-23, NAMI-INC-24, NAMI-INC-31 |
| R4.17 | Create GitHub / Linear / Jira tickets from an incident | NAMI-INC-22, NAMI-INC-28, NAMI-INC-29 |
| R4.18 | UI for all of the above. Part 2 half: the screens for every item in this table (Part 1 Tasks 12-15 ship the rest) | NAMI-INC-28, NAMI-INC-29, NAMI-INC-30, NAMI-INC-31, NAMI-INC-32 |
| (none) | Part 2 security review + fix wave over the whole sub-project | NAMI-INC-33 |

Not in this plan: the Microsoft Teams channel (R4.08) is Part 1's.
Part 1 has the `teams` channel kind and the Adaptive Card formatter `format::teams`, and ticks that item. Every other §4 item not in the table above is Part 1's.

**Goal:** Finish incident management and on-call. Severities become manageable and incidents get editable metadata and the full list filters. Alerts arrive from outside through secret-URL integrations (generic JSON webhook, Prometheus Alertmanager, Grafana), with dedup and auto-resolve, and from people through a public report form. Slack messages gain signed Acknowledge/Resolve buttons. Responders create GitHub, Linear and Jira tickets from an incident. Teams get MTTA/MTTR reporting and an SLA report with compound and average uptime, both exportable as CSV. On-call users get reminder emails 24 h before a shift, a personal ICS feed, and "take over shift". nami-ui gets the screens for all of it.

**Architecture:** Every incident still opens and resolves only through Part 1's `incidents::lifecycle` (planned in NAMI-INC-1). Part 2 adds the `IncidentSource::Integration { integration_id, dedup_key }` and `IncidentSource::ReportForm { form_project_id }` variants that Part 1 left room for, each with its own conflict target. Integrations, the Slack interactivity endpoint and the ICS feed are called by machines outside, so they sit on the public listener (`core::router::build_public_router`, `PUBLIC_PORT`) next to heartbeat pings and log ingest. The report form is posted by people through nami-ui, so its engine endpoints sit on the private API, where the per-IP limit can trust the `X-Forwarded-For` that nami-ui's `callEngine` sets. Every outbound request (Slack `users.info` and `response_url`, GitHub, Linear, Jira) goes through Part 1's guarded clients `incidents::outbound::client()` / `incidents::outbound::public_client()` (planned in NAMI-INC-3). Every secret (integration and report-form tokens, the Slack signing secret and bot token, ticket API tokens) is sealed with `core::crypto::Cipher` under a per-row context, as Part 1 R10 seals channel configs. Lookup tokens are stored as SHA-256. Payload parsing, ICS rendering, report aggregation and the compound SLA are pure functions with unit tests.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13 through Part 1's outbound clients, chrono-tz 0.10.4, `hmac 0.13.0` added by Part 1 NAMI-INC-3, `serde_urlencoded` through axum's `Form`), Postgres 17, Mailpit, wiremock 0.6.5 (dev), SvelteKit + Svelte 5 runes + Tailwind 4, vitest. No new crate.

**Spec:** `docs/superpowers/specs/2026-10-13-incident-management-design.md`, the whole file including "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §4.

**When to start:** after Incidents Part 1 is merged to `main`, including its security review and fix wave. At phase start, read Part 1's decisions doc (`docs/superpowers/decisions/2026-10-13-incident-management-part1.md`) and its merged code, then write each task's steps against them. This plan names Part 1 pieces by module and type name. Where Part 1's merged code places a module at a different path than this plan assumes, Part 1's path wins and the task adapts with the same behaviour and tests.

Part 1 pieces this plan consumes (all planned in Part 1, none merged on 2026-10-24):
- `incidents::lifecycle::{IncidentSource, OpenIncident, open, resolve, resolve_source, acknowledge, record_event, EventKind}` (NAMI-INC-1). `EventKind` already has `IntegrationEvent`, `MetadataUpdated` and `TicketCreated`. The `incidents.source` check already allows `integration` and `report_form`. `incidents.metadata` (jsonb object) exists.
- `incident_severities` with the three seeded rows, `incidents::severity::model::{Severity, list_for_project, belongs_to}` and `GET /projects/{id}/severities` (NAMI-INC-1, NAMI-INC-2).
- `incidents::model::{ListFilter, list, detail, IncidentDetail, IncidentListItem}` with the keyset cursor and the `status`/`source` filters (NAMI-INC-2, Part 1 R21).
- `incidents::outbound::{client, public_client, check_url, check_url_for_send, describe_error, OutboundPolicy, Reach, UrlRule}` and `Config.outbound_policy` (NAMI-INC-3, Part 1 R9).
- `incidents::format::{Message, slack, escape_slack, hmac_hex}` (NAMI-INC-4) and `incidents::send` / `incidents::deliveries` (NAMI-INC-5).
- `incidents::worker::{run_forever, kick}` (NAMI-INC-5, NAMI-INC-9).
- `incidents::oncall::rotation::{Rotation, Override, Shift, shifts, on_call_at, MAX_SHIFT_RANGE}` (NAMI-INC-6) and `incidents::oncall::model::{load_rotation, add_override, list_overrides, OverrideView, MAX_OVERRIDE_DAYS}` with `on_call_overrides.created_by` (NAMI-INC-7).
- `projects::model::{list_members, effective_role}` and `role` on `GET /projects/{id}` (NAMI-INC-7, Part 1 R28).
- `escalation_policies` and `incidents::escalation` (NAMI-INC-8, NAMI-INC-9). `monitors.severity_id`, `heartbeats.severity_id`, `log_alerts.severity_id` (NAMI-INC-10).
- `incidents::action::token` and its own governor `rate_limited_actions` (NAMI-INC-11), and `AppError::Gone`.
- nami-ui: `SeverityBadge.svelte`, the incident list and detail pages, `renderMarkdown`, the schedule pages and the account notifications page (NAMI-INC-12 .. NAMI-INC-15).

Already on `main` and reused by name:
- `core::router::{build_router, build_public_router}`, `core::serve::ServeLimits::PUBLIC`, `require_json_body` (private API only).
- `core::crypto::Cipher` (`encrypt`, `decrypt`, `derive_subkey`), `core::crypto::SecretString`.
- `core::download::{csv_record, push_csv_field, neutralize}` (the Logs export formula-injection guard).
- `core::email::Mailer::send`.
- `core::validate::{validate_name, nullable, has_invisible_or_bidi}` and `uptime::checker::is_unsafe_in_text`.
- `logs::ingest::rate_limit::TokenBuckets` (per-key token buckets in memory, as heartbeat pings use them).
- `uptime::availability::{for_monitor, for_heartbeat, check_range, compute, union, subtract, seconds, Availability, MAX_RANGE_DAYS}` (`MAX_RANGE_DAYS = 1096`, the spec's 3 years).
- `logs::source::model` token pattern (32 random bytes base64url, SHA-256 lookup, sealed copy, `reveal`, `rotate`).
- nami-ui `callEngine`, `enginePath`, `engineFailure`, `publicEngineUrl`, `isUuid` (`#lib/core/uuid`), `CopyButton.svelte`, the design system in `src/lib/components/ui/`.

**Execution:** per `CLAUDE.md` and `docs/superpowers/plan-conventions.md`. One branch `feat/incidents-part2` in `.worktrees/incidents-part2/`, from the latest `origin/main`. Each task is one or more Conventional Commits with its task ID, followed by a task review and fix commits on the same branch. Tasks run in order. Engine tasks 16-27 and UI tasks 28-32 may run in a parallel worktree on a short-lived local branch once the engine task a UI task consumes has passed its review (the API shapes below are the contract). After NAMI-INC-33: the decisions doc `docs/superpowers/decisions/<date>-incident-management-part2.md`, the roadmap ticks, one phase PR, auto-merged when the review is clean.

Engine test command (one domain, one file):

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test incidents <file>::
```

Unit tests: `cd nami-engine && cargo test --lib <module path>`. Never combine a `--test` target and a `--lib` filter in one command (Part 1, "Test filters").

UI command: `cd nami-ui && npm run check && npx vitest run <files>`.

Engine tests need `docker compose up -d postgres mailpit` (and `clickhouse` for the SLA tests that seed check results, if any). Never restart running containers.

## Global Constraints

- Everything in Part 1's "Global Constraints" still holds (RBAC shape, JSON bodies on the private API, text caps, outbound rules, secrets in memory, nami-ui rules). The items below add to it.
- Layout (`docs/STANDARDS.md` §1): every new area is `nami-engine/src/incidents/<area>/{mod,model,routes}.rs` (a small area may be one file). Routes hold no SQL. Pure logic (payload parsers, ICS rendering, report aggregation, the compound SLA, Slack signature checks) lives in functions with no I/O and has unit tests in the same file. Integration tests are files of the `tests/incidents/main.rs` target (`mod <file>;`), except the SLA report changes to `uptime::availability`, whose unit tests stay in `uptime/availability.rs`.
- RBAC (spec "RBAC"):
  - Viewers read incidents, severities, reports, the integration list (no tokens), the report-form settings (no token), the ticket integration list (no secrets) and the Slack interactivity status (configured or not).
  - Members change severity and metadata, create tickets, reveal integration and report-form tokens (as source tokens are revealed today) and take over shifts for themselves.
  - Admins manage severities, integrations (create, rename, enable, rotate, delete, send test), the report form, Slack interactivity and ticket integrations.
  - Every logged-in user manages their own reminder opt-out and ICS token.
  - First line of each project-scoped handler: `require_project_role(&state.db, &user, project_id, Role::<X>)`. A foreign project is 403. A foreign resource id in an accessible project path is 404. A foreign id stored in a row (an escalation policy or severity on an integration or the report form) is 400.
- Status codes: 400 validation, 422 for a private-API JSON body that does not deserialize (a wrong type or an unknown field under `deny_unknown_fields`, axum's `Json` rejection, as existing tests assert today), 401 (session, or a bad Slack signature), 403 authorization, 404 unknown or foreign id and every unknown, rotated or disabled token, 409 name clash, quota or "already exists", 413 body over its cap, 429 every rate, slot or concurrency limit, 502 an upstream (Slack, GitHub, Linear, Jira) that failed.
- Bodies of third-party formats (Alertmanager, Grafana, Slack) are parsed from raw `Bytes` with `serde_json::from_slice` (or `serde_urlencoded::from_bytes` for Slack's form), never with axum's `Json` extractor, whose shape errors answer 422. A third-party sender gets 400 for every bad body (R-I2-6).
- Every new state-changing route of the private API is added to `tests/core/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json` in the task that adds it. Public-listener routes are not (that listener has no JSON guard, R-I2-4). Every new table is added to `tests/core/migrations.rs::all_tables_exist`.
- Every new limit is a named constant next to the type it bounds, with a test at the bound and one past it (`docs/STANDARDS.md` §7, §11).
- Secrets: integration tokens, the report-form token, the Slack signing secret and bot token, ticket API tokens and the ICS token never appear in a response other than the one that shows them, in `Debug` output, a `tracing` field, the timeline or an error string. Types that hold one get a manual `Debug` that prints ids and kinds only, and hold it in `zeroize::Zeroizing` or `core::crypto::SecretString`.
- nami-ui: every engine call goes through `callEngine` and `enginePath`; route params use the `[...=uuid]` matchers; deletes take two steps (`confirm=yes`); pages that show or accept secrets set `cache-control: private, no-store`; times are shown in UTC unless the page states another zone; only the design-system tokens and components.
- Commits: Conventional Commits with the task ID, ending with the attribution line the session gives.

## Review Focus

- **A retrying alert source must never open a second incident for one alert, or flood the timeline.** Example: Alertmanager re-sends a firing group every 5 minutes, and two engine instances receive the same payload at once. One incident opens (the partial unique index is the conflict target), and the repeat appends at most one `integration_event` per incident per minute → NAMI-INC-19 (`concurrent_duplicate_payloads_open_exactly_one_incident`, `repeats_append_at_most_one_event_per_minute`).
- **A resolved alert must close what it opened, and only that.** Example: a Grafana `resolved` for fingerprint `abc` resolves the unresolved incident of that integration and key, and nothing of another integration that happens to use the same key → NAMI-INC-19 (`a_resolved_payload_resolves_only_its_own_integrations_incident`).
- **A leaked or guessed URL must not act on another project.** Example: a Slack click whose button names an incident of project B, sent to project A's interactivity URL with A's valid signature, changes nothing. A rotated integration URL answers 404 at once → NAMI-INC-21 (`an_incident_of_another_project_is_rejected`), NAMI-INC-18 (`a_rotated_token_stops_working_at_once`).
- **A Slack user who is not a responder must not acknowledge anything.** Example: a viewer of the project, or a Slack user whose email maps to no Nami user, clicks Resolve. They get an ephemeral "not authorized" and the incident is unchanged → NAMI-INC-21 (`a_viewer_click_changes_nothing`, `an_unmapped_slack_user_changes_nothing`).
- **The public report form must not be a spam cannon or a phishing relay.** Example: a bot fills every field, the honeypot included, 100 times from one IP. No incident opens, the responses look like success until the per-IP limit answers 429, and reporter text with `<!channel>`, `<at>`, `[x](javascript:...)` or `<script>` is inert in every channel and page → NAMI-INC-20 (`a_honeypot_submission_creates_nothing_and_looks_identical`, `the_sixth_submission_from_one_ip_in_an_hour_is_429`), NAMI-INC-33.
- **Ticket bodies must not mention or link people on the tracker.** Example: an incident titled `@org/oncall fix #123` creates a GitHub issue that pings nobody and links no other issue, and a stored ticket URL is always https on the expected host → NAMI-INC-22 (`a_github_body_mentions_nobody`, `a_returned_url_on_another_host_is_not_stored`).
- **An ICS feed must show only its owner's shifts, and schedule names must not inject calendar data.** Example: a schedule named `x\r\nBEGIN:VEVENT` in a project the owner left yesterday is absent, and one in a current project renders as one escaped `SUMMARY` → NAMI-INC-26 (`a_feed_lists_only_current_projects`, `crlf_and_separators_in_names_cannot_add_properties`).
- **The compound SLA must not double count, and maintenance must not hide another resource's downtime.** Example: two monitors down over the same 10 minutes count 10 minutes, not 20; one monitor in maintenance while the other is down still counts the other's downtime → NAMI-INC-24 (`overlapping_downtime_counts_once`, `one_resources_maintenance_does_not_hide_anothers_downtime`).

## Decisions the spec leaves open (rulings)

Part 1's rulings (R1-R30 in its plan, "Part 1 R<n>" below) stay in force. These add to them.

- **R-I2-1 Task range.** Part 2 is NAMI-INC-16 .. NAMI-INC-33: twelve engine tasks, five UI tasks, then the phase security review and fix wave. Part 1's "Part 2 follows" and "Part 1 leaves room for these" lists define the scope, plus every §4 item Part 1 leaves partly done (the Roadmap coverage table).
- **R-I2-2 Migrations.** Two files, numbered when the phase starts:
  - `NNNN_incidents_part2.sql`. NAMI-INC-16 creates it. NAMI-INC-18, NAMI-INC-20, NAMI-INC-21, NAMI-INC-22 and NAMI-INC-25 append to it (allowed while it is unmerged).
  - `NNNN_auth_oncall_preferences.sql`, created by NAMI-INC-25: `users.shift_reminders` and `users.ics_token_hash`.
  - Neither file references a table of the other, so their order does not matter. Both come after Part 1's files (`NNNN_incidents_part1.sql`, `NNNN_uptime_escalation.sql`, `NNNN_logs_escalation.sql`), whose tables they reference.
  - Every constraint and index has a name, and every foreign key states `on delete`. The spec's "Data model additions" is the starting point. Deviations are named in the task that makes them (for example `incident_tickets.url` is nullable, R-I2-13).
- **R-I2-3 New incident sources.** `incidents::lifecycle::IncidentSource` gains two variants (Part 1 left room for them):
  - `Integration { integration_id: Uuid, dedup_key: Option<String> }`. With a key, `open` uses the conflict target `incidents_one_unresolved_per_integration_key` (the spec's partial unique index on `(integration_id, dedup_key) where status <> 'resolved' and dedup_key is not null`), and returns `None` when that pair already has an unresolved incident. Without a key, it is a plain insert and always opens.
  - `ReportForm { reporter_name: Option<String>, reporter_contact: Option<String> }`. No conflict target: every accepted submission opens an incident.
  - `as_str` gives `integration` and `report_form`. The escalation policy and severity of both come from their row (the integration or the form), else the project defaults, through the policy and severity fields Part 1 gives `OpenIncident`.
- **R-I2-4 Which listener serves what.**
  - Public listener (`build_public_router`, `PUBLIC_PORT`), because machines outside call them without a session: `POST /api/v1/integrations/{token}`, `POST /api/v1/slack/interactivity/{project_id}` and `GET /public/oncall/{file}` (the ICS feed; calendar apps fetch it directly).
  - Private API, through nami-ui: `GET /public/report-forms/{token}` and `POST /public/report-forms/{token}/incidents`. People post the form through the BFF, and the per-IP limit keys on the `X-Forwarded-For` that `callEngine` sets. On the public port anyone could forge that header.
  - The public listener has no JSON guard. Slack posts `application/x-www-form-urlencoded`, which `require_json_body` would refuse. Each public route has its own in-flight cap, so none of them can take all of `MAX_PUBLIC_CONNECTIONS`.
- **R-I2-5 Secret URL tokens.**
  - Integration, report-form and ICS tokens are 32 random bytes, base64url without padding (43 chars), like source tokens.
  - Integration and report-form tokens are stored as SHA-256 (`token_hash bytea`, unique) and sealed (`token_enc`) under `incident_integrations.token:{id}` / `incident_report_forms.token:{project_id}`, so members can reveal them again. The ICS token is stored as its SHA-256 only and shown once (spec).
  - A token that is not 43 base64url characters answers 404 without a database query. Lookup is an indexed equality on the hash.
  - Rotate replaces both columns in one `update`. The old URL answers 404 from that commit on.
  - **No request log carries a token.** The engine has no request access log today (there is no `TraceLayer` on `main`). New handlers never put a token, a token-bearing path or a raw body into a `tracing` field. NAMI-INC-33 greps for it. When Platform adds an access log, it must redact these path segments to `[token]`; the decisions doc records that follow-up.
- **R-I2-6 Third-party bodies.** Integration and Slack bodies are read as raw bytes up to their cap (`axum::body::to_bytes(body, MAX)`, over the cap → 413) after the token or project checks. They are parsed with `serde_json::from_slice` (Slack: `serde_urlencoded` for the outer form, then `serde_json` for `payload`), so a shape error is a 400 with a short message, never axum's 422. The `Content-Type` of an integration request is not checked (some senders say `text/plain`). Alertmanager, Grafana and Slack payload structs do **not** use `deny_unknown_fields`: these formats gain fields across versions, and refusing them would drop alerts. This is a deliberate exception to `docs/STANDARDS.md` §2. Nami's own `webhook` format does use `deny_unknown_fields`.
- **R-I2-7 Metadata from labels.** For `webhook` payloads, metadata that breaks a spec cap (key `^[A-Za-z0-9_.-]{1,64}$`, string value <= 1 KiB, <= 50 keys, <= 16 KiB total) is a 400 naming the rule. For Alertmanager and Grafana, labels are sanitised instead, because one odd label must not lose an alert: a key that fails the pattern is dropped; a value is cut to 1 KiB at a char boundary; then keys are kept in sorted order while the count and total stay within the caps; `generatorURL` is added as a key of its own, replacing a label of that name. The number dropped goes into the `opened` event's metadata as `metadata_dropped`.
- **R-I2-8 Severity from a payload.** `webhook`: `severity` must name a project severity, else 400 `unknown severity "<name>"`. Alertmanager and Grafana: `labels.severity`, lowercased, is used when it names a project severity, and ignored otherwise (Prometheus rules often say `warning` or `page`). Then the integration's `severity_id`, then the project default (spec order).
- **R-I2-9 Dedup, repeats and resolution.**
  - A repeat `open`/`firing` for a key with an unresolved incident counts as `deduplicated` and appends an `integration_event` ("alert fired again", metadata `{"status": "firing"}`), at most one per incident per `INTEGRATION_EVENT_GAP` (60 s), so a sender that re-sends every few seconds cannot grow the timeline without bound. A repeat changes no title, cause or metadata: the first payload wins, and members edit by hand.
  - A `resolved` for a key with an unresolved incident (open or acknowledged) resolves it through `lifecycle::resolve` with `actor: None`, which queues the resolved notifications (Part 1). The `resolved` event's message is `resolved by integration "<name>"`. A `resolved` that matches nothing counts as `ignored`.
  - `ignored` also counts alerts past the first `MAX_ALERTS_PER_REQUEST` (100) and opens refused by the cap below.
  - At most `MAX_UNRESOLVED_PER_INTEGRATION` (1,000) unresolved incidents per integration. An alert that would open one more counts as `ignored`, and the handler logs one `warn` per request with the integration id. The 1,000 existing incidents have already paged someone.
  - The request is validated as a whole first (a shape error anywhere → 400, nothing applied). Then each alert runs in its own short transaction, so concurrent requests interleave per alert, and the unique index decides each race. The worker is kicked once at the end.
- **R-I2-10 Rate limits held in memory.** All of these are per engine instance, like the ping limits, and `TokenBuckets` (keyed by `Uuid`) holds them. They live, with the in-flight semaphores of R-I2-12, R-I2-13 and R-I2-16, in one new state field, `AppState.incidents: Arc<incidents::IncidentsState>` (the `AppState.logs: Arc<LogsState>` pattern), never in process-wide statics: the tests of one `tests/incidents` binary run in parallel and would share a static's budget. `AppState::for_test` builds the defaults, and a test that needs a drained or lifted limit passes its own (`AppState { incidents: Arc::new(IncidentsState::with_limits(..)), ..AppState::for_test(pool) }`), as heartbeat tests inject `ping_limits`. Burst tests use a zero refill rate, so they never depend on timing. The incidents worker calls `prune_full` on every bucket set once a minute.
  - Integrations: `TokenBuckets::new(1.0, 120.0)` per integration (spec: 60 per minute, burst 120), checked after the token lookup and before the body is read. At most `MAX_INTEGRATION_REQUESTS_IN_FLIGHT` (8) requests at once per instance, waiting up to 2 s for a place, then 429.
  - Report form: per client address and form, 5 per hour, burst 5. Per form overall, 30 per hour, burst 30. Both are checked before the honeypot, so a bot meets the same 429 as a person. The address key is `Uuid::new_v5(form_project_id, <address bytes>)`. An IPv6 address counts by its /64, because one host controls a whole /64. The per-address bucket is taken first, so one address cannot drain the form's overall budget past its own 5.
  - Slack: per project, `TokenBuckets::new(5.0, 20.0)`, taken only after the signature is verified, so forged requests cannot use up a project's budget.
  - ICS: per token owner, 30 per hour, burst 10, plus `MAX_ICS_IN_FLIGHT` (2) feeds being computed at once per instance.
- **R-I2-11 Report form details.**
  - The honeypot is a body field `website` (the page renders it visually hidden, `tabindex="-1"`, `autocomplete="off"`, labelled "Leave this field empty"). A non-empty `website` gets the same `202 {"ok": true}` and writes nothing. Its log line carries the form's project id only.
  - `GET /public/report-forms/{token}` returns `{"project_name", "intro"}` so the page can show the intro. The page needs it, and the spec's POST alone cannot render one. The holder of the secret URL learns the project name, which the form shows anyway.
  - Disabled or unknown token → 404 for both routes. The settings row is created by the first `PUT` and gets a fresh token then. A newly created form is disabled until an admin enables it (spec default `enabled = false`).
  - The incident's title is the report's title, its cause is the description, and the `opened` event's message is `Reported by <name, or "anonymous"> (<contact>)`, in text, escaped wherever it is shown.
- **R-I2-12 Slack interactivity.**
  - **Which messages get buttons.** Only `triggered` messages to **project** Slack channels of a project that has a Slack app configured. A personal Slack webhook may belong to another Slack app or workspace, and that app's Request URL is not Nami's (the same caution as Part 1 R16). Resolved and test messages carry no buttons. `format::Message` gains `slack_buttons: bool`, which the delivery drain sets. The buttons are an `actions` block with `action_id` `nami_ack` / `nami_resolve` and `value` = the incident id. The value is never trusted: the handler checks that the incident belongs to the project in the path.
  - **Request handling.** The steps, in order:
    1. Read the body (at most `MAX_SLACK_BODY_BYTES` = 256 KiB, else 413).
    2. Parse `{project_id}`. Load the config. A malformed id, an unknown project or one with no Slack app → 401, the same answer as a bad signature, so the endpoint does not reveal which projects use Slack.
    3. Verify `X-Slack-Request-Timestamp` (an integer within `SLACK_MAX_SKEW` = 300 s of now, either direction) and `X-Slack-Signature` = `v0=` + hex HMAC-SHA256(signing secret, `v0:{timestamp}:{raw body}`), compared in constant time (`Mac::verify_slice` on the decoded hex). Failure → 401.
    4. Take the project's Slack token bucket (429).
    5. Decode `payload`. Only `type: "block_actions"` with one of the two action ids, a user id `^[UW][A-Z0-9]{2,20}$`, and a `response_url` that passes the send-time check below are acted on. Anything else gets an empty 200 and is ignored.
    6. Take one of `MAX_SLACK_ACTIONS_IN_FLIGHT` (8) places with `try_acquire_owned` (none free → 429, which Slack shows to the clicker), answer an empty `200` at once (Slack wants an answer within 3 s), and run the rest in a spawned task that holds the place.
  - **Who acts.** The spawned task calls `users.info` (`GET {SLACK_API_BASE_URL}/users.info?user=<id>`, `Authorization: Bearer <bot token>`, 10 s). It needs `ok: true`, `user.deleted: false` and a `user.profile.email`. `user.is_email_confirmed: false` counts as unmapped. The email maps, case-insensitively, to a Nami user with a verified email. That user must be a project `member` or `admin`, or a global admin, as for action links (Part 1 R16). Anyone else gets an ephemeral reply through `response_url` (`response_type: "ephemeral"`, `replace_original: false`, "You are not allowed to act on this incident in Nami.") and nothing changes.
  - **The action.** In one transaction: lock the incident row and check its project, then call `lifecycle::acknowledge` or `lifecycle::resolve` with the mapped user as the actor, event metadata `{"via": "slack"}`. `false` (already in that state or past it) changes nothing, so repeated clicks are idempotent. After the commit, kick the worker. The message is then replaced through `response_url` (`replace_original: true`) with `format::slack_status(..)`: the current status line, who acted, and the buttons that still apply (open: both; acknowledged: Resolve; resolved: none). Every user text is escaped with `escape_slack`.
  - **Outbound.** `response_url` comes from the payload: it must start with `https://hooks.slack.com/` (spec) under `OutboundPolicy::Strict` and is sent with `incidents::outbound::public_client()`. `users.info` goes to `SLACK_API_BASE_URL` (default `https://slack.com/api`), operator config read like Part 1's `TELEGRAM_API_BASE_URL`, so tests point it at wiremock, and is sent with `incidents::outbound::client()` (R-I2-22). Under `Relaxed` (tests only) the `hooks.slack.com` prefix rule is skipped, as Part 1 R9 skips host allowlists.
  - **Config.** `slack_app_configs` per spec. The signing secret matches `^[0-9a-f]{32}$`, the bot token `^xoxb-[A-Za-z0-9-]{20,250}$`. They are sealed under `slack_app_configs.signing_secret:{project_id}` and `slack_app_configs.bot_token:{project_id}`, write-only.
- **R-I2-13 Tickets.**
  - **Destinations.** GitHub: `POST {GITHUB_API_BASE_URL}/repos/{owner}/{repo}/issues` (default `https://api.github.com`). Linear: `POST {LINEAR_API_URL}` (default `https://api.linear.app/graphql`), mutation `issueCreate`. Both are operator config read like `TELEGRAM_API_BASE_URL` and are sent with `incidents::outbound::client()`, as Part 1 R9 sends the Telegram base. Jira: `POST {base_url}/rest/api/3/issue`. The base URL is admin input that must be `https` with a host and no userinfo, path, query or fragment. It is sent with `incidents::outbound::client()` (the monitors' policy: private hosts allowed for a self-hosted Jira, link-local and metadata refused), the analogue of Part 1 R9's project webhook.
  - **Body.** Title = incident title, one line, at most 255 chars. Body = cause, severity, each metadata pair as `key: value`, and `Incident: <incident url>`, as plain text. For GitHub and Linear, every ASCII punctuation character in user text is backslash-escaped (CommonMark), so `@team` mentions nobody, `#12` links nothing and `[x](y)` is no link. Jira gets an ADF `doc` of `paragraph`s holding only `text` nodes, without marks.
  - **Stored URL.** It must be `https` on `github.com`, `linear.app`, or the Jira base URL's host. For Jira it is built as `{base_url}/browse/{key}` from a key matching `^[A-Z][A-Z0-9_]{0,9}-[0-9]{1,10}$`. A URL that fails the check is not stored: the ticket row keeps `url = null` and its `external_id`, and the UI shows the id without a link. **This makes `incident_tickets.url` nullable**, unlike the spec's sketch, which has no place for such a ticket.
  - **One per incident and integration.** The create runs under a transaction-level advisory lock on `(incident, integration)`, held across the call (at most 10 s). It checks for an existing row inside the lock. An existing ticket → `409 {"error": "this incident already has a ticket there", "ticket": {...}}`. At most `MAX_TICKET_CREATES_IN_FLIGHT` (4) creates at once per instance (429).
  - **Errors.** 10 s timeout, no automatic retry (spec). A failure answers 502 with a fixed vocabulary: `GitHub answered HTTP 401`, `Jira could not be reached (timeout)`, never a response body. The response body is read up to `MAX_TICKET_RESPONSE_BYTES` (64 KiB).
  - **Config.** `ticket_integrations.config` holds the non-secret fields: GitHub `{repository}` (`^[A-Za-z0-9-]{1,39}/[A-Za-z0-9._-]{1,100}$`), Linear `{team_id}` (uuid), Jira `{base_url, project_key, issue_type, email}`. `secret_enc` holds the API token, sealed under `ticket_integrations.secret:{id}`. At most `MAX_TICKET_INTEGRATIONS_PER_PROJECT` (10). The list shows a label built from the config (`owner/repo`, `Linear team <id>`, `Jira <KEY> on <host>`). The spec has no name column, and a label covers the need.
- **R-I2-14 Reports.**
  - The range is `[from, to)` on `started_at`, checked with `uptime::availability::check_range` (`MAX_RANGE_DAYS` = 1096, the spec's 3 years).
  - MTTA and MTTR are integer seconds (rounded) or `null` when nothing qualifies. They use the columns as stored: a reopened incident keeps whatever `acknowledged_at`/`resolved_at` Part 1's reopen leaves.
  - **Responder keys.** The acknowledging user, else the resolving user. A resolution with no actor is `automatic`. An unresolved, unacknowledged incident is `none`. An acknowledgement or resolution whose user was deleted (the time is set, the user id is null) is `deleted`.
  - A severity that is null groups as `none`.
  - The incident report is one SQL literal with `group by grouping sets ((), (source), (severity), (responder))`.
  - **SLA compound.** Each resource's downtime is its incident spans minus its own maintenance, from its creation on (as `for_monitor`/`for_heartbeat` compute it today). The compound's range is `[from, min(to, now))`. Its downtime is the union of the resources' downtime, so an overlap counts once. Its maintenance is the **intersection** of the resources' maintenance: time is taken out only while every selected resource is in maintenance, so one resource's window never hides another's outage. Time before a resource existed is neither downtime nor maintenance for it. `uptime_pct` = 100 × (1 − downtime / (range − maintenance)), `null` when nothing is left.
  - **SLA average.** The mean of the resources' `uptime_pct` that are not `null`, or `null`.
  - **CSV.** `format=csv` answers `text/csv; charset=utf-8` with `content-disposition: attachment`, built with `core::download::csv_record` (RFC 4180, every cell quoted, formula guard). Both reports are bounded (a few hundred rows at most), so they are built in memory, not streamed. Any other `format` value → 400.
- **R-I2-15 Shift reminders.**
  - A pass runs in the incidents worker every `REMINDER_PASS_INTERVAL` (5 min). It holds no lock and no transaction across sends: exactly-once comes from the per-row insert below, which commits on its own before its email goes out. Two instances may both compute a pass; only one insert per key returns a row.
  - For each schedule, the pass takes `shifts(now, now + 24 h)` and keeps the shifts with `starts_at > now`. So a shift created less than 24 h ahead is reminded on the next pass (spec), and a shift already running is not.
  - It reminds the shift's user only when they are a project member, have a verified email and `users.shift_reminders` is true. The row `(schedule_id, user_id, shift_start)` is inserted `on conflict do nothing returning` and committed. Only the instance whose insert returned a row sends.
  - A failed send deletes the row, so a later pass retries while the shift is still ahead.
  - Caps: `MAX_REMINDERS_PER_PASS` (200) and `MAX_REMINDERS_PER_USER_PER_DAY` (10), the latter counted on `sent_at`, so a schedule edited back and forth cannot flood a user.
  - Reminder rows whose `shift_start` is more than 7 days old are pruned hourly. The rotation math runs in `spawn_blocking` (1,440 samples per schedule per pass).
  - Email: subject `[Nami] On call from <start, UTC>: <schedule>`. The body gives the schedule, project, start and end in UTC and in the schedule's zone, a link to the schedule and a link to the account page where reminders are turned off.
- **R-I2-16 ICS feed.**
  - The route is `GET /public/oncall/{file}`, with `{file}` = `<token>.ics`. The handler strips the suffix and answers 404 without it, because matchit 0.8.4 (axum 0.8.9) captures whole segments only.
  - The feed lists the owner's shifts from now − 7 days to now + 60 days (67 days, within `MAX_SHIFT_RANGE`), over the schedules of projects where the owner has a `project_members` row (a global admin who is not a member gets none), at most `MAX_ICS_SCHEDULES` (100) schedules, by project name then schedule name. It is computed in `spawn_blocking`.
  - Headers: `content-type: text/calendar; charset=utf-8`, `cache-control: private, max-age=300`.
  - Format, RFC 5545: CRLF line ends; lines folded at 75 octets on a UTF-8 boundary; TEXT values escape `\`, `;`, `,` and a newline as `\n`; CR and every other control character are removed.
  - Events: `UID:<schedule_id>-<starts_at unix>@nami` (stable per shift), `DTSTAMP`, `DTSTART`/`DTEND` in UTC (`YYYYMMDDTHHMMSSZ`), `SUMMARY:On call: <schedule> (<project>)`, `URL:<UI_BASE_URL>/projects/<id>/oncall/<schedule_id>` (ids only), and `DESCRIPTION:Override` for override shifts.
- **R-I2-17 Take over shift.**
  - `POST /projects/{id}/schedules/{schedule_id}/take-over` with `{starts_at, ends_at}` needs the `member` role. The caller must also have a `project_members` row: a global admin who is not a member gets 400 `only project members can be on call` (Part 1 R7 never pages non-members).
  - A `starts_at` in the past moves to now. The range must end after now, start at most 60 days ahead and last at most `MAX_OVERRIDE_DAYS`.
  - `shifts(starts_at, ends_at)` must return exactly one shift, of another user, covering the whole range. Otherwise → 400 `choose a time inside one shift of someone else`. A gap with nobody on call is for admins' overrides.
  - It creates the override through `add_override` with `created_by` = the caller.
  - The displaced user gets an email (if their email is verified and they are still a member), whatever their reminder setting: `[Nami] <caller email> takes over your on-call shift (<schedule>)`.
- **R-I2-18 Severity management.**
  - Create (admin) `{name, is_default?}` appends at `rank = max + 1`, up to `MAX_SEVERITIES_PER_PROJECT` (20). A name clash or the cap → 409.
  - Order changes only through `PUT /projects/{id}/severities/order` `{ids}` (every id of the project exactly once), which rewrites ranks to 1..n. Part 1's `unique (project_id, rank)` is replaced in the Part 2 migration by the named constraint `incident_severities_rank_unique ... deferrable initially immediate` (Postgres allows deferrable unique constraints, not deferrable unique indexes), and the reorder transaction runs `set constraints incident_severities_rank_unique deferred`.
  - `is_default: true` moves the default in one transaction. `is_default: false` → 400 `choose another default instead`.
  - Delete: the default → 409 `cannot delete the default severity`. A severity an incident references → 409 `this severity is used by incidents`, recognised with `is_foreign_key_violation()` (Part 1's `incidents.severity_id` has no `on delete` action). Monitors, heartbeats and log alerts (Part 1 NAMI-INC-10 declares `on delete set null`), integrations and the report form that point at it fall back to the project default.
- **R-I2-19 Metadata and list filters.**
  - `PUT /projects/{id}/incidents/{incident_id}/metadata` `{metadata}` replaces the whole object (member). Caps are `MAX_METADATA_KEYS` (50), `MAX_METADATA_VALUE_BYTES` (1,024) and `MAX_METADATA_BYTES` (16,384, keys plus values). Values are one line, without `is_unsafe_in_text` characters. The `metadata_updated` event carries `{"added": [keys], "removed": [keys], "changed": [keys]}`: key names, no values. A request that changes nothing records no event.
  - New list query keys: `severity_id` (repeatable; a malformed uuid → 400; an unknown or foreign id matches nothing), `from`/`to` (RFC 3339, `[from, to)` on `started_at`; `from >= to` → 400), `metadata.<key>=<value>` (at most `MAX_METADATA_FILTERS` = 10 pairs; one key given twice → 400; all pairs form one object matched with `metadata @> $n::jsonb`, which the GIN `jsonb_path_ops` index serves), and `q` (1-200 chars, case-insensitive title substring, `ilike` with `\`, `%` and `_` escaped in Rust).
  - There is no trigram index for `q`. The scan is project-scoped and keyset-limited. An index waits for a measurement (STANDARDS §7).
  - Repeated keys are parsed the way Part 1 NAMI-INC-2 parses `status` and `source`.
- **R-I2-20 Integration "send test payload".** `POST /projects/{id}/integrations/{integration_id}/test` (admin, `{}`) runs a synthetic payload of the integration's kind through the same pipeline as a real request: status open, dedup key `nami-test`, title `Test alert from integration "<name>"`. It opens a real incident routed through the integration's policy, because that is what proves the routing and the notifications. A second test while that incident is unresolved dedups into it. At most one test per integration per 10 s (an atomic `last_tested_at` update, as Part 1's channel test does) → 429. A test does not touch `last_received_at`. The UI says the test notifies people.
- **R-I2-21 Settings navigation in nami-ui.** Project settings get a `SegmentedTabs` link row: General (today's page), Notifications (Part 1), Severities, Integrations, Report form, Slack, Tickets. The project layout gets a "Reports" tab after "On-call". Reminder and ICS controls go on Part 1's account notifications page as an "On-call" card. They are personal notification settings, and one account page is enough.
- **R-I2-22 Which outbound client.** One rule for every Part 2 request, extending Part 1 R9. An API base that is operator config (`SLACK_API_BASE_URL`, `GITHUB_API_BASE_URL`, `LINEAR_API_URL`, like Part 1's Telegram base) is sent with `incidents::outbound::client()`. An admin-entered Jira base URL is sent with `client()` too (a self-hosted Jira may be private; link-local and metadata stay refused), the analogue of Part 1's project webhook. A URL that arrives in a third-party payload (Slack's `response_url`) is sent with `public_client()` after its host check.

## File Structure

Part 1 files are named as Part 1 plans them. If Part 1's merged layout differs, its paths win (see "When to start").

```
nami-engine/
  .env.example                              T19 none; T21 SLACK_API_BASE_URL; T22 GITHUB_API_BASE_URL, LINEAR_API_URL
  migrations/
    NNNN_incidents_part2.sql             T16 severity rank constraint, incidents columns + indexes;
                                            T18 incident_integrations; T20 incident_report_forms;
                                            T21 slack_app_configs; T22 ticket_integrations, incident_tickets;
                                            T25 on_call_shift_reminders
    NNNN_auth_oncall_preferences.sql        T25 users.shift_reminders, users.ics_token_hash
  src/
    core/config.rs                          T21 slack_api_base_url; T22 github_api_base_url, linear_api_url
    core/state.rs                           T19 AppState.incidents: Arc<IncidentsState> (R-I2-10)
    core/router.rs                          T16-T27 merge the new private routers; T19, T21, T26 the public ones
    incidents/
      mod.rs                                module list; T19 IncidentsState (buckets, semaphores);
                                            T20, T21, T22, T26 add their limits to it
      lifecycle.rs (Part 1)                 T18 IncidentSource::Integration; T20 IncidentSource::ReportForm
      metadata.rs                           T17 pure metadata validation and label sanitising (T19 uses it)
      model.rs (Part 1)                     T17 ListFilter gains severity_ids, from, to, metadata, q;
                                            T17 set_metadata; T22 IncidentDetail.tickets; T20 reporter fields
      routes.rs (Part 1)                    T17 list query keys, PUT metadata
      severity/model.rs, routes.rs (Part 1) T16 create, update, reorder, delete
      integration/
        mod.rs                              T19 apply (dedup, resolve, counts)
        model.rs                            T18 CRUD, tokens, reveal, rotate
        payload.rs                          T19 pure parsers: webhook, alertmanager, grafana
        routes.rs                           T18 admin API; T19 public endpoint and test
      report_form/
        model.rs, routes.rs                 T20 settings API and the two /public routes
      slack/
        mod.rs                              T21 signature check (pure), click handling
        model.rs                            T21 slack_app_configs seal/open
        routes.rs                           T21 config API and the public interactivity endpoint
      format.rs (Part 1)                    T21 Message.slack_buttons, slack actions block, slack_status
      deliveries.rs (Part 1)                T21 sets slack_buttons for project Slack channels
      ticket/
        mod.rs                              T22 create flow, advisory lock, URL check
        model.rs                            T22 ticket_integrations, incident_tickets
        render.rs                           T22 pure: markdown escaping, Jira ADF
        send.rs                             T22 GitHub, Linear, Jira requests
        routes.rs                           T22 ticket integration API, create ticket
      report/
        mod.rs                              T23 pure aggregation and CSV rows; T24 SLA assembly
        model.rs                            T23 grouping-sets query
        routes.rs                           T23 incident report; T24 SLA report
      oncall/ (Part 1: model.rs, routes.rs) T25 reminders.rs; T26 ics.rs (pure render), feed route;
                                            T27 take-over route; T25, T26 account routes (account.rs)
      worker.rs (Part 1)                    T19, T20 prune TokenBuckets; T25 reminder pass, reminder prune
    uptime/availability.rs                  T24 ResourceDowntime, downtime_for_monitor/heartbeat, intersect, compound
  tests/
    core/migrations.rs                      T16, T18, T20, T21, T22, T25 new tables and constraints
    core/json_bodies.rs                     every new private POST/PUT/PATCH route
    incidents/main.rs                       one `mod` per new file below
    incidents/severities_api.rs             T16
    incidents/incident_filters.rs           T17
    incidents/integrations_api.rs           T18
    incidents/integration_ingest.rs         T19
    incidents/report_form.rs                T20
    incidents/slack_interactivity.rs        T21
    incidents/tickets.rs                    T22
    incidents/incident_reports.rs           T23
    incidents/sla_report.rs                 T24
    incidents/shift_reminders.rs            T25
    incidents/ics_feed.rs                   T26
    incidents/take_over.rs                  T27

nami-ui/src/
  lib/incidents/filters.ts (+ .test.ts)     T28 list query <-> engine query (severity, dates, metadata, q)
  lib/incidents/reports.ts (+ .test.ts)     T31 durations, percentages, range defaults
  lib/server/incidents/metadata-form.ts     T28 (+ .test.ts)
  lib/server/incidents/integration-form.ts  T29 (+ .test.ts)
  lib/server/incidents/ticket-form.ts       T29 (+ .test.ts)
  lib/server/incidents/report-form.ts       T29 settings form, T30 public submission (+ .test.ts)
  lib/components/MetadataPanel.svelte       T28
  routes/(app)/projects/[id=uuid]/
    +layout.svelte                          T31 "Reports" tab
    incidents/+page.*                       T28 filters
    incidents/[incidentId=uuid]/+page.*     T28 metadata panel, reporter, tickets
    settings/+layout.svelte                 T28 settings tabs (R-I2-21)
    settings/severities/+page.*             T28
    settings/integrations/+page.*           T29
    settings/report-form/+page.*            T29
    settings/slack/+page.*                  T29
    settings/tickets/+page.*                T29
    reports/+page.*                         T31
    reports/incidents/export/+server.ts     T31 CSV relay
    reports/sla/export/+server.ts           T31 CSV relay
    oncall/[scheduleId=uuid]/+page.*        T32 "Take over" on shifts
  routes/(app)/account/notifications/+page.*  T32 "On-call" card: reminders, ICS link
  routes/report/[token]/+page.*             T30 public form (outside (app), no login)
```

---

### Task 16 (NAMI-INC-16): Severity management API and the Part 2 incidents migration

**Covers:** R4.14 (severity management half)  **Depends on:** Incidents Part 1 merged.

**Files:**
- Create: `nami-engine/migrations/NNNN_incidents_part2.sql`, `nami-engine/tests/incidents/severities_api.rs`
- Modify: `nami-engine/src/incidents/severity/model.rs`, `nami-engine/src/incidents/severity/routes.rs` (both planned in NAMI-INC-2), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:** the migration starts with:

```sql
-- Incident management, part 2. NAMI-INC-16 starts this file; later Part 2
-- tasks append their tables to it before the phase merges.

-- Ranks change only through the reorder endpoint, which rewrites 1..n in one
-- transaction: the unique constraint must be checkable at commit.
-- Part 1 leaves this constraint unnamed, so Postgres named it; use Part 1's
-- name instead if its merged file names it.
alter table incident_severities drop constraint incident_severities_project_id_rank_key;
alter table incident_severities add constraint incident_severities_rank_unique
    unique (project_id, rank) deferrable initially immediate;

alter table incidents
    add column dedup_key text constraint incidents_dedup_key_len check (char_length(dedup_key) between 1 and 255),
    add column reporter_name text constraint incidents_reporter_name_len check (char_length(reporter_name) <= 100),
    add column reporter_contact text constraint incidents_reporter_contact_len check (char_length(reporter_contact) <= 200);
create index incidents_metadata_gin on incidents using gin (metadata jsonb_path_ops);
```

`incidents.integration_id` and its dedup index are added by NAMI-INC-18 (they need `incident_integrations`).

```rust
// incidents::severity::model
pub const MAX_SEVERITIES_PER_PROJECT: i64 = 20;
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
pub struct NewSeverity { pub name: String, #[serde(default)] pub is_default: bool }
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
pub struct SeverityPatch { pub name: Option<String>, pub is_default: Option<bool> }
#[derive(Debug, Deserialize)] #[serde(deny_unknown_fields)]
pub struct SeverityOrder { pub ids: Vec<Uuid> }
pub fn validate_name(raw: &str) -> Result<String, String>;   // ^[a-z0-9_-]{1,32}$ after trim
pub enum Created { Created(Severity), NameTaken, Full }
pub async fn create(pool: &PgPool, project_id: Uuid, name: &str, is_default: bool) -> Result<Created, sqlx::Error>;
pub enum Updated { Updated(Severity), NotFound, NameTaken }
pub async fn update(pool: &PgPool, project_id: Uuid, id: Uuid, name: Option<&str>, make_default: bool) -> Result<Updated, sqlx::Error>;
pub enum Reordered { Reordered(Vec<Severity>), Mismatch }
pub async fn reorder(pool: &PgPool, project_id: Uuid, ids: &[Uuid]) -> Result<Reordered, sqlx::Error>;
pub enum Deleted { Deleted, NotFound, IsDefault, InUse }
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Deleted, sqlx::Error>;
```

**API:**

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `POST /projects/{id}/severities` | admin | `{name, is_default?}` | `201 Severity` | 400 name, 409 name taken / `severity limit reached (20 per project)` |
| `PATCH /projects/{id}/severities/{severity_id}` | admin | `{name?, is_default?}` | `200 Severity` | 400 `is_default: false`, 404, 409 name taken |
| `PUT /projects/{id}/severities/order` | admin | `{ids}` | `200 [Severity]` by rank | 400 `list every severity of the project exactly once` |
| `DELETE /projects/{id}/severities/{severity_id}` | admin | none | `204` | 404, 409 `cannot delete the default severity` / `this severity is used by incidents` |

`GET /projects/{id}/severities` stays as Part 1 made it (viewer).

**Behaviour:**
1. Create appends at `coalesce(max(rank), 0) + 1` inside one transaction with the count check (R-I2-18). `is_default: true` clears the old default in the same transaction.
2. Update with `is_default: true` swaps the default in one transaction. `is_default: false` → 400 `choose another default instead`. A rename takes effect in later notifications only.
3. Reorder: `ids` must be a permutation of the project's severity ids (no duplicates, none missing, no foreign ids). One transaction: `set constraints incident_severities_rank_unique deferred`, then `update ... set rank = t.ord from unnest($2::uuid[]) with ordinality t(id, ord)`.
4. Postgres refuses a deferrable constraint as an `on conflict` arbiter. Before switching the constraint, grep Part 1's merged code for `on conflict (project_id, rank)` or `on conflict on constraint` on `incident_severities`; the seeding trigger and backfill use plain inserts, so none is expected. If one exists, the task reorders through a different mechanism and records why.
5. Delete: the default → 409. Otherwise delete, and an FK violation from `incidents.severity_id` → 409 `this severity is used by incidents` (`is_foreign_key_violation()`). Monitors, heartbeats, log alerts, integrations and the report form that reference it fall back to the default (their FKs are `on delete set null`).

**Tests** (`tests/incidents/severities_api.rs`):
- `an_admin_creates_a_severity_at_the_end_of_the_order`
- `a_member_cannot_create_rename_reorder_or_delete_a_severity` (403 each)
- `a_viewer_still_lists_severities`
- `a_duplicate_name_is_409` and `an_invalid_name_is_400` (`High`, `a b`, 33 chars)
- `the_twentieth_severity_is_created_and_the_twenty_first_is_409` (names `MAX_SEVERITIES_PER_PROJECT`)
- `making_a_severity_the_default_clears_the_old_default`
- `unsetting_the_default_is_400`
- `reordering_rewrites_ranks_in_one_transaction` (reverse three ranks; no 500 from the unique constraint)
- `a_reorder_missing_or_repeating_an_id_is_400`, `a_reorder_naming_another_projects_severity_is_400`
- `the_default_severity_cannot_be_deleted`
- `a_severity_used_by_an_incident_cannot_be_deleted`
- `deleting_a_severity_a_monitor_uses_falls_back_to_the_default` (the monitor's next incident gets the default)
- `a_foreign_project_is_403_and_a_foreign_severity_id_is_404`
- `tests/core/migrations.rs`: `the_rank_constraint_is_deferrable` (swap two ranks inside one transaction with `set constraints ... deferred` and commit), and `the_metadata_gin_index_exists`.

**Acceptance:** the routes answer as tabled; `cargo test --test incidents severities_api::` and `cargo test --test core migrations::` pass; the four new write routes are in `json_bodies.rs`.

---

### Task 17 (NAMI-INC-17): Incident metadata editing and the full list filters

**Covers:** R4.14 (metadata editing), R4.15 (severity, date range, `metadata.<key>`, `q`)  **Depends on:** NAMI-INC-16

**Files:**
- Create: `nami-engine/src/incidents/metadata.rs`, `nami-engine/tests/incidents/incident_filters.rs`
- Modify: `nami-engine/src/incidents/mod.rs`, `nami-engine/src/incidents/model.rs`, `nami-engine/src/incidents/routes.rs`, `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:**

```rust
// incidents::metadata (pure)
pub const MAX_METADATA_KEYS: usize = 50;
pub const MAX_METADATA_VALUE_BYTES: usize = 1024;
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_METADATA_FILTERS: usize = 10;
pub type Metadata = serde_json::Map<String, serde_json::Value>;   // values are always strings
/// Strict: any broken rule is an Err with a 400 message (manual edits, webhook payloads).
pub fn validate(raw: &serde_json::Value) -> Result<Metadata, String>;
/// Lenient (R-I2-7): drops bad keys, cuts values, keeps sorted keys within the caps.
/// Returns the metadata and how many keys were dropped.
pub fn sanitize_labels(labels: &BTreeMap<String, String>, extra: &[(&str, &str)]) -> (Metadata, usize);
pub fn is_valid_key(key: &str) -> bool;                            // ^[A-Za-z0-9_.-]{1,64}$
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataDiff { pub added: Vec<String>, pub removed: Vec<String>, pub changed: Vec<String> }
pub fn diff(before: &Metadata, after: &Metadata) -> MetadataDiff;

// incidents::model (Part 1's ListFilter gains)
pub struct ListFilter { /* Part 1 fields */ pub severity_ids: Vec<Uuid>, pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>, pub metadata: Option<Metadata>, pub q: Option<String> }
pub const MAX_Q_CHARS: usize = 200;
pub async fn set_metadata(pool: &PgPool, project_id: Uuid, incident_id: Uuid, actor: Uuid, metadata: &Metadata)
    -> Result<Option<IncidentDetail>, sqlx::Error>;
```

**API:**

| Method + path | Role | Body / query | Success | Errors |
|---|---|---|---|---|
| `GET /projects/{id}/incidents?...&severity_id=&from=&to=&metadata.<key>=<value>&q=` | viewer | query | `200 {"incidents", "next_cursor"}` | 400 per rule below |
| `PUT /projects/{id}/incidents/{incident_id}/metadata` | member | `{metadata: {key: "value"}}` | `200 IncidentDetail` | 400 caps, 404 |

**Behaviour:**
1. Filter rules as R-I2-19. The SQL adds, as literals with bound values: `(cardinality($n::uuid[]) = 0 or severity_id = any($n))`, `($m::timestamptz is null or started_at >= $m)`, `($k::timestamptz is null or started_at < $k)`, `($j::jsonb is null or metadata @> $j)`, `($q::text is null or title ilike $q escape '\')`. `q` is bound as `%<escaped>%`.
2. Every filter combines with Part 1's `status`, `source`, source-id filters and the keyset cursor. A cursor from an unfiltered page used with a filter simply continues by `(started_at, id)`.
3. `metadata.` with an empty key, a key failing `is_valid_key`, a value over 1 KiB, a key given twice or more than 10 pairs → 400 naming the rule.
4. `PUT .../metadata` validates with `metadata::validate` (values must be strings, one line, no `is_unsafe_in_text` characters), then in one transaction locks the incident row, writes `metadata`, and records `metadata_updated` with the `diff` (key names only) when the diff is not empty.

**Tests:**
- Unit (`metadata.rs`): `validate_accepts_the_caps_exactly_and_refuses_one_past_each` (50 keys / 51; 1,024 / 1,025 bytes; 16,384 / 16,385 total; 64 / 65 char key), `non_string_values_are_refused`, `sanitize_drops_bad_keys_cuts_values_and_counts_the_drops`, `sanitize_keeps_sorted_keys_within_the_total_cap`, `diff_lists_added_removed_and_changed_keys`.
- Integration (`tests/incidents/incident_filters.rs`):
  - `the_list_filters_by_severity` (two ids repeat; an unknown id matches nothing)
  - `the_list_filters_by_date_range_half_open` (an incident at exactly `to` is excluded)
  - `from_not_before_to_is_400`
  - `the_list_filters_by_metadata_pairs` (two pairs AND; a value that differs excludes)
  - `a_metadata_key_given_twice_is_400`, `eleven_metadata_filters_is_400`
  - `q_matches_a_case_insensitive_title_substring_and_treats_percent_literally`
  - `filters_combine_with_status_source_and_the_cursor`
  - `a_member_replaces_metadata_and_the_timeline_records_key_names_only`
  - `an_unchanged_metadata_put_records_no_event`
  - `a_viewer_cannot_edit_metadata` (403), `metadata_over_a_cap_is_400`, `another_projects_incident_is_404`

**Acceptance:** `cargo test --lib incidents::metadata` and `cargo test --test incidents incident_filters::` pass; the GIN index is used for a metadata filter (`explain` in a test is not required; the review checks the plan output once by hand and notes it in the PR).

---

### Task 18 (NAMI-INC-18): Incoming integrations, model and admin API (tokens, reveal, rotate)

**Covers:** R4.12 (configuration half)  **Depends on:** NAMI-INC-17

**Files:**
- Create: `nami-engine/src/incidents/integration/mod.rs`, `nami-engine/src/incidents/integration/model.rs`, `nami-engine/src/incidents/integration/routes.rs`, `nami-engine/tests/incidents/integrations_api.rs`
- Modify: `nami-engine/migrations/NNNN_incidents_part2.sql` (append), `nami-engine/src/incidents/mod.rs`, `nami-engine/src/incidents/lifecycle.rs` (`IncidentSource::Integration`, R-I2-3), `nami-engine/src/core/router.rs`, `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:** appended to the incidents migration (the spec's table, with names and `on delete` actions):

```sql
create table incident_integrations (
    id uuid primary key,
    project_id uuid not null references projects(id) on delete cascade,
    name text not null constraint incident_integrations_name_len check (char_length(name) between 1 and 100),
    kind text not null constraint incident_integrations_kind check (kind in ('webhook', 'alertmanager', 'grafana')),
    token_hash bytea not null constraint incident_integrations_token_hash_len check (octet_length(token_hash) = 32),
    token_enc bytea not null,
    escalation_policy_id uuid references escalation_policies(id) on delete set null,
    severity_id uuid references incident_severities(id) on delete set null,
    enabled boolean not null default true,
    last_received_at timestamptz,
    last_tested_at timestamptz,
    created_at timestamptz not null default now(),
    constraint incident_integrations_name_unique unique (project_id, name),
    constraint incident_integrations_token_hash_unique unique (token_hash)
);
alter table incidents
    add column integration_id uuid references incident_integrations(id) on delete set null;
create unique index incidents_one_unresolved_per_integration_key on incidents (integration_id, dedup_key)
    where status <> 'resolved' and dedup_key is not null;
create index incidents_integration_unresolved_idx on incidents (integration_id) where status <> 'resolved';
```

The `incidents_derive_source` trigger (Part 1) gains `when new.integration_id is not null then 'integration'` in this file (a `create or replace function`).

```rust
// incidents::integration::model
pub const MAX_INTEGRATIONS_PER_PROJECT: i64 = 50;
pub const TOKEN_CONTEXT: &str = "incident_integrations.token";
pub fn token_context(id: Uuid) -> String;                      // "incident_integrations.token:{id}"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Kind { Webhook, Alertmanager, Grafana }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Integration { pub id: Uuid, pub project_id: Uuid, pub name: String, pub kind: Kind,
    pub escalation_policy_id: Option<Uuid>, pub severity_id: Option<Uuid>, pub enabled: bool,
    pub last_received_at: Option<DateTime<Utc>>, pub created_at: DateTime<Utc>, pub unresolved_incidents: i64 }
/// Create and rotate replies only; manual Debug prints the id.
pub struct IntegrationWithToken { pub integration: Integration, pub token: SecretString }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct NewIntegration { pub name: String, pub kind: Kind, pub escalation_policy_id: Option<Uuid>,
    pub severity_id: Option<Uuid>, #[serde(default = "yes")] pub enabled: bool }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct IntegrationPatch { pub name: Option<String>,
    #[serde(default, deserialize_with = "core::validate::nullable")] pub escalation_policy_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "core::validate::nullable")] pub severity_id: Option<Option<Uuid>>,
    pub enabled: Option<bool> }
pub async fn create(pool: &PgPool, cipher: &Cipher, project_id: Uuid, v: &ValidIntegration) -> Result<Created, sqlx::Error>;
pub async fn list(pool: &PgPool, project_id: Uuid) -> Result<Vec<Integration>, sqlx::Error>;
pub async fn find(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Option<Integration>, sqlx::Error>;
pub async fn update(pool: &PgPool, project_id: Uuid, id: Uuid, patch: &ValidPatch) -> Result<Updated, sqlx::Error>;
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>;
pub async fn reveal(pool: &PgPool, cipher: &Cipher, project_id: Uuid, id: Uuid) -> Result<Option<SecretString>, AppError>;
pub async fn rotate(pool: &PgPool, cipher: &Cipher, project_id: Uuid, id: Uuid) -> Result<Option<SecretString>, sqlx::Error>;
/// Public lookup: enabled integrations only; `None` for unknown, rotated or disabled.
pub async fn find_by_token(pool: &PgPool, token: &str) -> Result<Option<Integration>, sqlx::Error>;
```

Responses: create `201 {...Integration, "token": "<43 chars>", "url_path": "/api/v1/integrations/<token>"}`. Reveal and rotate `200 {"token", "url_path"}`.

**API:**

| Method + path | Role | Notes |
|---|---|---|
| `GET /projects/{id}/integrations` | viewer | no tokens |
| `POST /projects/{id}/integrations` | admin | 201 with token; 409 name / `integration limit reached (50 per project)`; 400 foreign policy or severity |
| `GET /projects/{id}/integrations/{integration_id}` | viewer | |
| `PATCH /projects/{id}/integrations/{integration_id}` | admin | `kind` in the body → 422 (`deny_unknown_fields`, axum's rejection) |
| `DELETE /projects/{id}/integrations/{integration_id}` | admin | 204; its incidents stay with `integration_id = null` |
| `GET /projects/{id}/integrations/{integration_id}/token` | member | like source tokens |
| `POST /projects/{id}/integrations/{integration_id}/rotate` | admin | `{}` |

**Behaviour:**
1. The id is generated in Rust before the insert so the sealing context names it (Part 1 R10 pattern). Token generation and hashing reuse the source-token helpers' shape (`logs::source::model::new_token`; hash SHA-256 raw bytes).
2. Name via `core::validate::validate_name`, unique per project.
3. `escalation_policy_id` / `severity_id` must belong to the project (400 `escalation policy is not in this project` / `severity is not in this project`).
4. Count check and insert in one transaction (`docs/STANDARDS.md` §5).
5. `find_by_token` returns `None` for a token that is not 43 base64url chars before any query (R-I2-5).

**Tests** (`tests/incidents/integrations_api.rs`):
- `an_admin_creates_an_integration_and_sees_its_token_once_in_the_reply`
- `the_list_and_detail_never_carry_a_token` (asserts the token string is absent from the JSON text)
- `a_member_reveals_the_token_and_a_viewer_cannot` (200 / 403)
- `a_member_cannot_create_patch_rotate_or_delete` (403 each)
- `a_rotated_token_stops_working_at_once` (`find_by_token(old)` is None, new one found)
- `a_disabled_integration_is_not_found_by_token`
- `the_fiftieth_integration_is_created_and_the_fifty_first_is_409`
- `a_policy_or_severity_of_another_project_is_400`
- `changing_kind_is_422` (unknown field under `deny_unknown_fields`)
- `the_token_is_stored_hashed_and_sealed` (row bytes differ from the token; `Cipher::decrypt` with the row context gives it back; the wrong context fails)
- `deleting_an_integration_keeps_its_incidents`
- `a_foreign_project_is_403_and_a_foreign_integration_id_is_404`
- `tests/core/migrations.rs`: `one_unresolved_incident_per_integration_and_key` (a second unresolved row with the same pair fails `23505`; a resolved one plus an open one is fine; two rows with a null key are fine).

**Acceptance:** `cargo test --test incidents integrations_api::` and `cargo test --test core migrations::` pass; no `Debug` output of `IntegrationWithToken` contains the token (unit test).

---

### Task 19 (NAMI-INC-19): The public integration endpoint: payloads, dedup, auto-resolve, limits, test

**Covers:** R4.12 (ingest half)  **Depends on:** NAMI-INC-18

**Files:**
- Create: `nami-engine/src/incidents/integration/payload.rs`, `nami-engine/tests/incidents/integration_ingest.rs`
- Modify: `nami-engine/src/incidents/integration/mod.rs`, `nami-engine/src/incidents/integration/routes.rs` (the public router and the test route), `nami-engine/src/core/router.rs` (`build_public_router_with_limits` merges `incidents::integration` public router), `nami-engine/src/incidents/mod.rs` (`IncidentsState` with the integration buckets and slots, `Default` and `with_limits`), `nami-engine/src/core/state.rs` (`AppState.incidents`, built in `for_test` and in `main.rs`), `nami-engine/src/incidents/worker.rs` (prune the buckets once a minute), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/json_bodies.rs` (the test route only)

**Data shape:**

```rust
// incidents::integration::payload (pure)
pub const MAX_INTEGRATION_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_ALERTS_PER_REQUEST: usize = 100;
pub const MAX_TITLE_CHARS: usize = 255;
pub const MAX_CAUSE_BYTES: usize = 10 * 1024;
pub const MAX_DEDUP_KEY_CHARS: usize = 255;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertStatus { Open, Resolved }
#[derive(Debug, Clone, PartialEq)]
pub struct Alert { pub status: AlertStatus, pub title: String, pub cause: Option<String>, pub dedup_key: Option<String>,
    pub severity: Option<String>, pub metadata: Metadata, pub metadata_dropped: usize }
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed { pub alerts: Vec<Alert>, pub truncated: usize }
/// Err is a 400 message. Never panics on any input (fuzz-style unit test).
pub fn parse(kind: Kind, body: &[u8]) -> Result<Parsed, String>;

// webhook (Nami's own format, deny_unknown_fields)
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct WebhookBody { title: String, cause: Option<String>, dedup_key: Option<String>,
    #[serde(default)] status: WebhookStatus, severity: Option<String>, metadata: Option<serde_json::Value> }
// alertmanager (version "4") and grafana: no deny_unknown_fields (R-I2-6)
#[derive(Deserialize)] struct AmBody { version: Option<String>, alerts: Vec<AmAlert> }
#[derive(Deserialize)] struct AmAlert { status: String, #[serde(default)] labels: BTreeMap<String, String>,
    #[serde(default)] annotations: BTreeMap<String, String>, #[serde(rename = "generatorURL")] generator_url: Option<String>,
    fingerprint: String }

// incidents::integration (I/O)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Outcome { pub opened: u32, pub deduplicated: u32, pub resolved: u32, pub ignored: u32 }
pub const MAX_UNRESOLVED_PER_INTEGRATION: i64 = 1000;
pub const INTEGRATION_EVENT_GAP: Duration = Duration::from_secs(60);
pub const MAX_INTEGRATION_REQUESTS_IN_FLIGHT: usize = 8;
pub const INTEGRATION_SLOT_WAIT: Duration = Duration::from_secs(2);
pub const REQUESTS_PER_SECOND: f64 = 1.0;
pub const REQUEST_BURST: f64 = 120.0;
pub const TEST_DEDUP_KEY: &str = "nami-test";
pub async fn apply(state: &AppState, integration: &Integration, parsed: Parsed) -> Result<Outcome, AppError>;
```

**API:**
- `POST /api/v1/integrations/{token}` (public listener, no session): any body up to 1 MiB → `202 {"opened", "deduplicated", "resolved", "ignored"}`. Errors: 404 unknown/rotated/disabled/malformed token, 429 rate or slot, 413 body over 1 MiB, 400 shape.
- `POST /projects/{id}/integrations/{integration_id}/test` (admin, `{}`): `202` with the same counts; 429 within 10 s of the last test (R-I2-20).

**Behaviour:**
1. Order: token format, lookup (404), rate bucket (429), in-flight slot (wait up to 2 s, then 429), read the body (413), parse (400), apply, set `last_received_at = now()`, kick the worker once.
2. Parsing per kind (spec, R-I2-7, R-I2-8):
   - `webhook`: `title` required, trimmed, 1-255 chars, one line (unsafe chars → 400). `cause` <= 10 KiB. `dedup_key` 1-255 chars. `status` `open` (default) or `resolved`; `resolved` without `dedup_key` → 400. `metadata` through `metadata::validate` (strict).
   - `alertmanager`: `version` must be `"4"` (else 400 `unsupported Alertmanager payload version`). `grafana`: any version, `alerts` required.
   - Each alert: `status` `firing` → Open, `resolved` → Resolved, anything else → 400. `fingerprint` 1-255 chars is the dedup key. Title = `annotations.summary`, else `labels.alertname`, else `alert <fingerprint>`, made one line (unsafe chars → space) and cut to 255 chars. Cause = `annotations.description`, cut to 10 KiB at a char boundary. Metadata = `sanitize_labels(labels, [("generatorURL", url)])`. Severity = `labels.severity`.
   - At most 100 alerts are used; the rest count as `truncated`, reported as `ignored`.
3. Applying (R-I2-9), one short transaction per alert:
   - Open with key: `lifecycle::open` with `IncidentSource::Integration { integration_id, dedup_key: Some(k) }`, title, cause, metadata, the severity (payload name, else the integration's, else default) and the integration's policy. `None` (an unresolved one exists) → `deduplicated`, plus an `integration_event` unless one was recorded in the last `INTEGRATION_EVENT_GAP`.
   - Open without key: always opens.
   - Before opening, when the integration has `MAX_UNRESOLVED_PER_INTEGRATION` unresolved incidents → `ignored`.
   - Resolved: lock the unresolved incident of `(integration_id, key)`, `lifecycle::resolve(.., None, now)` with the `resolved by integration "<name>"` message → `resolved`; none → `ignored`.
4. Concurrency: two requests with the same firing key at once open exactly one incident (the partial unique index is `open`'s conflict target).
5. The `opened` event's metadata carries `{"integration": "<name>", "metadata_dropped": n}`.

**Tests:**
- Unit (`payload.rs`): `a_webhook_needs_a_title`, `a_resolved_webhook_without_a_dedup_key_is_400`, `an_unknown_webhook_field_is_400`, `alertmanager_v4_maps_summary_description_labels_and_fingerprint`, `alertmanager_without_a_summary_uses_alertname`, `an_unknown_alertmanager_version_is_400`, `grafana_payloads_with_extra_fields_parse` (a real Grafana sample with `values`, `dashboardURL`, `silenceURL`), `alerts_past_one_hundred_are_counted_not_parsed`, `an_alert_status_other_than_firing_or_resolved_is_400`, `titles_are_one_line_and_capped`, `garbage_never_panics` (bytes 0..=255 prefixes, deep nesting, wrong types).
- Integration (`tests/incidents/integration_ingest.rs`, through `build_public_router`):
  - `a_webhook_opens_an_incident_with_the_integrations_policy_and_severity`
  - `a_payload_severity_name_wins_and_an_unknown_one_is_400`
  - `an_alertmanager_label_severity_is_used_when_it_names_a_project_severity`
  - `a_repeat_with_the_same_key_dedups_into_the_unresolved_incident`
  - `repeats_append_at_most_one_event_per_minute`
  - `a_resolved_payload_resolves_and_notifies` (one `resolved` delivery queued per earlier recipient)
  - `a_resolved_payload_resolves_only_its_own_integrations_incident` (same key on two integrations)
  - `an_acknowledged_incident_is_resolved_by_a_resolved_payload_too`
  - `a_payload_after_resolution_opens_a_new_incident`
  - `without_a_dedup_key_every_open_payload_opens_an_incident`
  - `concurrent_duplicate_payloads_open_exactly_one_incident` (20 parallel requests, one key)
  - `a_mixed_alertmanager_batch_reports_each_count`
  - `an_unknown_rotated_disabled_or_malformed_token_is_404`
  - `a_body_of_one_mebibyte_is_accepted_and_one_byte_more_is_413`
  - `malformed_json_is_400_never_422_or_500`
  - `the_121st_request_in_a_burst_is_429` (names `REQUEST_BURST`)
  - `the_unresolved_cap_ignores_further_opens` (seed `MAX_UNRESOLVED_PER_INTEGRATION` rows)
  - `last_received_at_is_set`
  - `the_test_action_opens_one_test_incident_and_dedups_a_second_test` and `a_second_test_within_ten_seconds_is_429`
  - `a_member_cannot_send_a_test` (403)
  - `the_endpoint_is_not_on_the_private_api` (404 from `build_router`)

**Acceptance:** `cargo test --lib incidents::integration` and `cargo test --test incidents integration_ingest::` pass; a curl of a real Alertmanager sample to a dev engine opens and resolves one incident (noted in the PR).

---

### Task 20 (NAMI-INC-20): Public incident report form (engine)

**Covers:** R4.13 (engine half)  **Depends on:** NAMI-INC-19 (shares the bucket pruning and the secret-token helpers)

**Files:**
- Create: `nami-engine/src/incidents/report_form/model.rs`, `nami-engine/src/incidents/report_form/routes.rs`, `nami-engine/tests/incidents/report_form.rs`
- Modify: `nami-engine/migrations/NNNN_incidents_part2.sql` (append), `nami-engine/src/incidents/mod.rs`, `nami-engine/src/incidents/lifecycle.rs` (`IncidentSource::ReportForm`, R-I2-3; the reporter columns are written by `open` for that variant), `nami-engine/src/incidents/model.rs` (`IncidentListItem`/`IncidentDetail` gain `reporter_name`, `reporter_contact`), `nami-engine/src/core/router.rs`, `nami-engine/src/incidents/mod.rs` (`IncidentsState` gains the report-form buckets), `nami-engine/src/incidents/worker.rs` (prune), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:**

```sql
create table incident_report_forms (
    project_id uuid primary key references projects(id) on delete cascade,
    token_hash bytea not null constraint incident_report_forms_token_hash_len check (octet_length(token_hash) = 32),
    token_enc bytea not null,
    enabled boolean not null default false,
    intro text constraint incident_report_forms_intro_len check (octet_length(intro) <= 2048),
    escalation_policy_id uuid references escalation_policies(id) on delete set null,
    severity_id uuid references incident_severities(id) on delete set null,
    updated_at timestamptz not null default now(),
    constraint incident_report_forms_token_hash_unique unique (token_hash)
);
```

```rust
// incidents::report_form::model
pub const TOKEN_CONTEXT: &str = "incident_report_forms.token";   // ":{project_id}"
pub const MAX_INTRO_BYTES: usize = 2048;
pub const MAX_REPORT_TITLE_CHARS: usize = 200;
pub const MAX_REPORT_DESCRIPTION_BYTES: usize = 10 * 1024;
pub const MAX_REPORTER_NAME_CHARS: usize = 100;
pub const MAX_REPORTER_CONTACT_CHARS: usize = 200;
pub const REPORTS_PER_ADDRESS_PER_HOUR: f64 = 5.0;
pub const REPORTS_PER_FORM_PER_HOUR: f64 = 30.0;
#[derive(Debug, Clone, Serialize)]
pub struct FormSettings { pub configured: bool, pub enabled: bool, pub intro: Option<String>,
    pub escalation_policy_id: Option<Uuid>, pub severity_id: Option<Uuid>, pub path: Option<String> /* "/report/<token>", members only */ }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct FormInput { pub enabled: bool, pub intro: Option<String>, pub escalation_policy_id: Option<Uuid>, pub severity_id: Option<Uuid> }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct Submission { pub title: String, pub description: Option<String>, pub reporter_name: Option<String>,
    pub reporter_contact: Option<String>, #[serde(default)] pub website: String /* honeypot */ }
pub fn address_key(project_id: Uuid, ip: IpAddr) -> Uuid;    // v5; IPv6 by its /64 (R-I2-10)
```

**API:**

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `GET /projects/{id}/report-form` | viewer | none | `200 FormSettings` (`path` for members and admins only) | |
| `PUT /projects/{id}/report-form` | admin | `FormInput` | `200 FormSettings` (creates the row and token on first use) | 400 intro, foreign policy/severity |
| `POST /projects/{id}/report-form/rotate` | admin | `{}` | `200 FormSettings` | 404 none yet |
| `GET /public/report-forms/{token}` | none (private API, via nami-ui) | none | `200 {"project_name", "intro"}` | 404 |
| `POST /public/report-forms/{token}/incidents` | none (private API, via nami-ui) | `Submission` | `202 {"ok": true}` | 400, 404, 429 |

**Behaviour:**
1. The two `/public` routes have no session and sit on the private API (R-I2-4); the client address is `SmartIpKeyExtractor`'s (the first `X-Forwarded-For` hop, set by nami-ui).
2. POST order: token format and lookup (404 unless the form exists and is enabled), per-address bucket then per-form bucket (429), body validation (400), honeypot (non-empty `website` → `202 {"ok": true}`, nothing written), then open.
3. Validation: title trimmed, 1-200 chars, one line; description <= 10 KiB, only `\n` and `\t` as control characters; name <= 100 and contact <= 200 chars, one line; every field refuses `is_unsafe_in_text` characters (bidi and invisible ones included).
4. Open through `lifecycle::open` with `IncidentSource::ReportForm { reporter_name, reporter_contact }`, the form's severity else the default, the form's policy else the default. `opened` message: `Reported by <name or "anonymous"> (<contact>)`. Kick the worker.
5. Reporter text reaches notifications only through Part 1's formatters, which escape per channel; this task adds formatter cases for reporter-style text (R-I2-11, spec "Security requirements").

**Tests** (`tests/incidents/report_form.rs`):
- `an_admin_enables_the_form_and_a_member_sees_its_path_but_a_viewer_does_not`
- `a_member_cannot_change_or_rotate_the_form` (403)
- `a_submission_opens_a_report_form_incident_with_reporter_details`
- `the_form_policy_and_severity_route_the_incident`
- `a_honeypot_submission_creates_nothing_and_looks_identical` (byte-identical response body and status)
- `the_sixth_submission_from_one_ip_in_an_hour_is_429` (names `REPORTS_PER_ADDRESS_PER_HOUR`)
- `two_addresses_of_one_ipv6_slash_64_share_a_bucket`
- `the_thirty_first_submission_to_a_form_in_an_hour_is_429` (31 addresses)
- `honeypot_hits_count_toward_the_limits`
- `a_disabled_unknown_rotated_or_malformed_token_is_404` (GET and POST)
- `caps_are_enforced_at_the_bound_and_one_past` (title 200/201, description 10,240/10,241 bytes, name 100/101, contact 200/201, intro 2,048/2,049)
- `bidi_and_control_characters_are_400`
- `reporter_text_is_inert_in_every_formatter` (unit tests in `format.rs`: `<!channel>`, `<@U1>`, `<at>x</at>`, `[x](javascript:alert(1))`, `<script>`, `**b**` through Slack, Teams, Discord, Telegram, email)
- `the_public_routes_need_no_session_but_the_settings_routes_do`

**Acceptance:** `cargo test --test incidents report_form::` and `cargo test --lib incidents::format` pass.

---

### Task 21 (NAMI-INC-21): Slack interactivity (signed Acknowledge/Resolve buttons)

**Covers:** R4.10 (engine half)  **Depends on:** NAMI-INC-20

**Files:**
- Create: `nami-engine/src/incidents/slack/mod.rs`, `nami-engine/src/incidents/slack/model.rs`, `nami-engine/src/incidents/slack/routes.rs`, `nami-engine/tests/incidents/slack_interactivity.rs`
- Modify: `nami-engine/migrations/NNNN_incidents_part2.sql` (append), `nami-engine/src/incidents/mod.rs` (module list; `IncidentsState` gains the Slack buckets and action places), `nami-engine/src/incidents/format.rs` (`Message.slack_buttons`, the actions block, `slack_status`), `nami-engine/src/incidents/deliveries.rs` (sets `slack_buttons`), `nami-engine/src/core/config.rs` (`slack_api_base_url`, env `SLACK_API_BASE_URL`, default `https://slack.com/api`, validated like `TELEGRAM_API_BASE_URL`), `nami-engine/.env.example`, `nami-engine/src/core/router.rs` (config routes on the API, interactivity on the public listener), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs` (the config `PUT`)

**Data shape:**

```sql
create table slack_app_configs (
    project_id uuid primary key references projects(id) on delete cascade,
    signing_secret_enc bytea not null,
    bot_token_enc bytea not null,
    updated_at timestamptz not null default now()
);
```

```rust
// incidents::slack (pure parts)
pub const MAX_SLACK_BODY_BYTES: usize = 256 * 1024;
pub const SLACK_MAX_SKEW: Duration = Duration::from_secs(300);
pub const MAX_SLACK_ACTIONS_IN_FLIGHT: usize = 8;
pub const ACK_ACTION_ID: &str = "nami_ack";
pub const RESOLVE_ACTION_ID: &str = "nami_resolve";
pub const RESPONSE_URL_PREFIX: &str = "https://hooks.slack.com/";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadRequest { MissingHeaders, Stale, BadSignature }
/// Timestamp within SLACK_MAX_SKEW of `now` and `v0=` + hex HMAC-SHA256 over "v0:{ts}:{body}", constant time.
pub fn verify(signing_secret: &[u8], timestamp: &str, signature: &str, body: &[u8], now: DateTime<Utc>) -> Result<(), BadRequest>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click { pub slack_user_id: String, pub action: incidents::action::token::Action, pub incident_id: Uuid, pub response_url: String }
/// `None` for anything that is not one of our two buttons (ignored with an empty 200).
pub fn parse_click(form_body: &[u8]) -> Result<Option<Click>, String>;

// incidents::slack::model
pub const SIGNING_SECRET_CONTEXT: &str = "slack_app_configs.signing_secret";   // ":{project_id}"
pub const BOT_TOKEN_CONTEXT: &str = "slack_app_configs.bot_token";            // ":{project_id}"
/// No Debug derive: a manual impl prints the project id only.
pub struct SlackApp { pub project_id: Uuid, pub signing_secret: Zeroizing<Vec<u8>>, pub bot_token: SecretString }
pub async fn load(pool: &PgPool, cipher: &Cipher, project_id: Uuid) -> Result<Option<SlackApp>, AppError>;
pub async fn save(pool: &PgPool, cipher: &Cipher, project_id: Uuid, signing_secret: &str, bot_token: &str) -> Result<(), sqlx::Error>;
pub async fn delete(pool: &PgPool, project_id: Uuid) -> Result<bool, sqlx::Error>;

// incidents::format
pub struct Message { /* Part 1 fields */ pub slack_buttons: bool }
/// The replacement message after a click: status, actor, the buttons that still apply.
pub fn slack_status(m: &Message, actor_email: Option<&str>) -> serde_json::Value;
```

**API:**

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `GET /projects/{id}/slack-app` | viewer | none | `200 {"configured": bool, "request_url_path": "/api/v1/slack/interactivity/<project_id>"}` | |
| `PUT /projects/{id}/slack-app` | admin | `{signing_secret, bot_token}` | `200 {"configured": true, ...}` | 400 formats |
| `DELETE /projects/{id}/slack-app` | admin | none | `204` | 404 |
| `POST /api/v1/slack/interactivity/{project_id}` | none (public listener, Slack-signed) | Slack form body | `200` empty | 401 unsigned/stale/unknown, 413, 429 |

**Behaviour:** R-I2-12, in its order. In addition:
1. Without a Slack app on the project, Slack messages are exactly Part 1's (no actions block).
2. `PUT` replaces both secrets; neither is ever returned (write-only, like channel configs).
3. The spawned work never panics the process and logs failures at `warn` with the project id and a fixed kind (`users.info failed`, `response_url failed`), never the token, the email or the response body.

**Tests:**
- Unit (`slack/mod.rs`): `a_signature_slack_documents_verifies` (Slack's published example secret, timestamp and body), `a_body_altered_after_signing_fails`, `a_timestamp_older_or_newer_than_five_minutes_fails`, `a_missing_or_non_hex_signature_fails`, `parse_click_ignores_other_payload_types_and_action_ids`, `parse_click_refuses_a_response_url_off_hooks_slack_com` (under Strict).
- Unit (`format.rs`): `project_slack_messages_carry_two_buttons_when_enabled`, `resolved_and_test_messages_carry_no_buttons`, `slack_status_offers_only_the_buttons_that_apply`, `slack_status_escapes_titles_and_emails`.
- Integration (`tests/incidents/slack_interactivity.rs`, wiremock as `users.info` and `response_url`, `OutboundPolicy::Relaxed`):
  - `a_member_click_acknowledges_and_updates_the_message` (event actor is the mapped user, metadata `via: slack`)
  - `a_resolve_click_resolves_and_queues_resolved_notices`
  - `repeated_clicks_are_idempotent`
  - `a_viewer_click_changes_nothing` (ephemeral reply posted, incident unchanged)
  - `an_unmapped_slack_user_changes_nothing` (no Nami user; an unverified Nami email; `is_email_confirmed: false`)
  - `an_incident_of_another_project_is_rejected`
  - `a_missing_or_wrong_signature_is_401`, `a_replayed_request_older_than_five_minutes_is_401`, `a_body_altered_after_signing_is_401`
  - `an_unknown_project_or_one_without_slack_is_401` (same body as a bad signature)
  - `a_body_over_256_kib_is_413`
  - `the_secrets_are_write_only_and_sealed`
  - `a_member_cannot_configure_slack` (403)
  - `personal_slack_channels_never_get_buttons`
  - `the_response_is_sent_before_users_info_answers` (wiremock delays `users.info` by 5 s; the endpoint answers within 1 s)

**Acceptance:** `cargo test --lib incidents::slack`, `cargo test --lib incidents::format` and `cargo test --test incidents slack_interactivity::` pass.

---

### Task 22 (NAMI-INC-22): GitHub, Linear and Jira tickets

**Covers:** R4.17 (engine half)  **Depends on:** NAMI-INC-21

**Files:**
- Create: `nami-engine/src/incidents/ticket/mod.rs`, `nami-engine/src/incidents/ticket/model.rs`, `nami-engine/src/incidents/ticket/render.rs`, `nami-engine/src/incidents/ticket/send.rs`, `nami-engine/src/incidents/ticket/routes.rs`, `nami-engine/tests/incidents/tickets.rs`
- Modify: `nami-engine/migrations/NNNN_incidents_part2.sql` (append), `nami-engine/src/incidents/mod.rs` (module list; `IncidentsState` gains the ticket-create places), `nami-engine/src/incidents/model.rs` (`IncidentDetail.tickets`), `nami-engine/src/core/config.rs` (`github_api_base_url`, env `GITHUB_API_BASE_URL`, default `https://api.github.com`; `linear_api_url`, env `LINEAR_API_URL`, default `https://api.linear.app/graphql`), `nami-engine/.env.example`, `nami-engine/src/core/router.rs`, `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:**

```sql
create table ticket_integrations (
    id uuid primary key,
    project_id uuid not null references projects(id) on delete cascade,
    kind text not null constraint ticket_integrations_kind check (kind in ('github', 'linear', 'jira')),
    config jsonb not null constraint ticket_integrations_config_object check (jsonb_typeof(config) = 'object'),
    secret_enc bytea not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
create index ticket_integrations_project_idx on ticket_integrations (project_id);

create table incident_tickets (
    incident_id uuid not null references incidents(id) on delete cascade,
    ticket_integration_id uuid not null references ticket_integrations(id) on delete cascade,
    external_id text not null constraint incident_tickets_external_id_len check (char_length(external_id) between 1 and 100),
    url text constraint incident_tickets_url_https check (url is null or url like 'https://%'),   -- nullable: R-I2-13
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    constraint incident_tickets_pkey primary key (incident_id, ticket_integration_id)
);
```

```rust
// incidents::ticket::model
pub const MAX_TICKET_INTEGRATIONS_PER_PROJECT: i64 = 10;
pub const SECRET_CONTEXT: &str = "ticket_integrations.secret";   // ":{id}"
#[derive(Clone, Deserialize, Serialize)] #[serde(tag = "kind", content = "config", rename_all = "snake_case")]
pub enum TicketConfig {
    Github { repository: String },
    Linear { team_id: Uuid },
    Jira { base_url: String, project_key: String, issue_type: String, email: String },
}
impl TicketConfig { pub fn validate(&self) -> Result<(), String>; pub fn label(&self) -> String; pub fn expected_host(&self) -> String }
#[derive(Debug, Clone, Serialize)]
pub struct TicketIntegration { pub id: Uuid, pub kind: String, pub label: String, pub config: TicketConfig, pub created_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Ticket { pub ticket_integration_id: Uuid, pub kind: String, pub external_id: String, pub url: Option<String>,
    pub created_by_email: Option<String>, pub created_at: DateTime<Utc> }

// incidents::ticket::render (pure)
pub fn escape_markdown(s: &str) -> String;      // backslash before every ASCII punctuation char
pub fn markdown_body(t: &TicketContent) -> String;
pub fn jira_adf(t: &TicketContent) -> serde_json::Value;   // doc > paragraph > text nodes only
pub struct TicketContent { pub title: String, pub cause: Option<String>, pub severity: String,
    pub metadata: Vec<(String, String)>, pub incident_url: String }

// incidents::ticket::send
pub const MAX_TICKET_RESPONSE_BYTES: usize = 64 * 1024;
pub const MAX_TICKET_CREATES_IN_FLIGHT: usize = 4;
pub struct CreatedTicket { pub external_id: String, pub url: Option<String> }   // url already host-checked
pub async fn create_issue(config: &Config, t: &TicketConfig, token: &SecretString, content: &TicketContent)
    -> Result<CreatedTicket, String>;   // Err: the fixed 502 message
```

**API:**

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `GET /projects/{id}/ticket-integrations` | viewer | none | `200 [TicketIntegration]` (no secrets) | |
| `POST /projects/{id}/ticket-integrations` | admin | `{kind, config, token}` | `201 TicketIntegration` | 400, 409 `ticket integration limit reached (10 per project)` |
| `PATCH /projects/{id}/ticket-integrations/{tid}` | admin | `{kind?+config?, token?}` (kind must stay) | `200` | 400, 404 |
| `DELETE /projects/{id}/ticket-integrations/{tid}` | admin | none | `204` (its ticket links go too, spec cascade) | 404 |
| `POST /projects/{id}/incidents/{incident_id}/tickets` | member | `{ticket_integration_id}` | `201 Ticket` | 400 foreign integration, 404 incident, 409 `{"error", "ticket"}`, 429, 502 |

**Behaviour:** R-I2-13. In addition:
1. Token formats: GitHub `^(ghp_|github_pat_)[A-Za-z0-9_]{20,255}$`, Linear `^lin_api_[A-Za-z0-9]{20,100}$`, Jira 1-500 chars without whitespace. Jira `email` 3-254 chars with one `@`, `project_key` `^[A-Z][A-Z0-9_]{1,9}$`, `issue_type` 1-50 chars one line.
2. GitHub request: `Authorization: Bearer <token>`, `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, body `{title, body}`. Success is 201 with `number` and `html_url`. Linear: `Authorization: <api key>`, GraphQL variables `{input: {teamId, title, description}}`, success needs `data.issueCreate.success == true` and `issue.{identifier, url}`. Jira: Basic `email:token`, success is 201 with `key`.
3. A `ticket_created` event records `{"kind", "external_id", "url"}` with the member as actor. Detail lists tickets oldest first.
4. The token is opened only inside `create_issue` and never logged, returned or put into an error.

**Tests:**
- Unit (`render.rs`): `a_github_body_mentions_nobody` (`@team`, `@org/oncall` → `\@`), `issue_refs_and_links_are_inert` (`#12`, `[x](https://e.test)`, `<https://e.test>`), `jira_adf_holds_text_nodes_only`, `the_body_carries_cause_severity_metadata_and_link`.
- Unit (`model.rs`): `config_validation_per_kind` (bad repo, non-https Jira, Jira URL with a path or userinfo, bad project key).
- Integration (`tests/incidents/tickets.rs`, wiremock for all three, `Relaxed`):
  - `a_member_creates_a_github_issue_and_the_link_is_stored`
  - `linear_and_jira_tickets_are_created_with_their_shapes`
  - `a_second_create_for_the_same_integration_is_409_with_the_existing_ticket`
  - `concurrent_creates_call_the_tracker_once` (two parallel requests, wiremock expects one call)
  - `a_returned_url_on_another_host_is_not_stored` (url null, external id kept)
  - `an_upstream_error_is_502_without_its_body` (wiremock returns 401 with a body containing a marker; the marker is absent from the reply)
  - `a_timeout_is_502` (wiremock delays 11 s)
  - `the_token_is_write_only_and_sealed`
  - `a_viewer_cannot_create_a_ticket_and_a_member_cannot_manage_integrations` (403)
  - `an_integration_of_another_project_is_400`
  - `the_eleventh_ticket_integration_is_409`
  - `a_jira_base_url_on_a_link_local_address_is_refused_at_send_time` (under `Strict`)

**Acceptance:** `cargo test --lib incidents::ticket` and `cargo test --test incidents tickets::` pass.

---

### Task 23 (NAMI-INC-23): Incident report (MTTA, MTTR, breakdowns, CSV)

**Covers:** R4.16 (incident report half)  **Depends on:** NAMI-INC-22

**Files:**
- Create: `nami-engine/src/incidents/report/mod.rs`, `nami-engine/src/incidents/report/model.rs`, `nami-engine/src/incidents/report/routes.rs`, `nami-engine/tests/incidents/incident_reports.rs`
- Modify: `nami-engine/src/incidents/mod.rs`, `nami-engine/src/core/router.rs`, `nami-engine/tests/incidents/main.rs`

**Data shape:**

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats { pub count: i64, pub acknowledged: i64, pub resolved: i64,
    pub mtta_seconds: Option<i64>, pub mttr_seconds: Option<i64> }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Group { pub key: String, pub label: String, #[serde(flatten)] pub stats: Stats }
// key: source name | severity name or "none" | user id, "automatic", "none" or "deleted"; label: the same or the email
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncidentReport { pub from: DateTime<Utc>, pub to: DateTime<Utc>, pub total: Stats,
    pub by_source: Vec<Group>, pub by_severity: Vec<Group>, pub by_responder: Vec<Group> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)] #[serde(rename_all = "snake_case")]
pub enum Format { #[default] Json, Csv }
pub fn csv(report: &IncidentReport) -> String;   // header + rows via core::download::csv_record
```

**API:** `GET /projects/{id}/reports/incidents?from=&to=&format=json|csv` (viewer). `200` JSON or `text/csv` (`content-disposition: attachment; filename="incidents-<from date>-<to date>.csv"`). 400 missing or bad range (`check_range`), unknown `format`.

**Behaviour:**
1. One SQL literal with `group by grouping sets ((), (i.source), (s.name), (responder))`, where `responder` is the `case` of R-I2-14 (ack user, else resolver, else `automatic` when resolved with no actor, else `none`; `deleted` when the time is set and the user id is null). MTTA = `round(avg(extract(epoch from acknowledged_at - started_at)))` over acknowledged rows; MTTR the same over resolved rows.
2. Groups are sorted by count descending, then key. Severity groups carry the current name. Responder labels are emails (joined), `automatic`, `none` or `deleted user`.
3. CSV header `group,key,label,count,acknowledged,resolved,mtta_seconds,mttr_seconds`; a `total` row first; empty cells for `null`.

**Tests** (`tests/incidents/incident_reports.rs`, seeding incidents with fixed times):
- `mtta_and_mttr_are_means_over_acknowledged_and_resolved_incidents`
- `the_range_is_half_open_on_started_at`
- `counts_break_down_by_source_severity_and_responder`
- `an_automatic_resolution_counts_as_automatic_and_an_untouched_incident_as_none`
- `a_deleted_responder_counts_as_deleted`
- `a_range_over_1096_days_is_400_and_1096_is_accepted` (names `MAX_RANGE_DAYS`)
- `csv_is_rfc4180_and_formula_guarded` (a responder email or source label starting with `=` is prefixed `'`; every cell quoted; CRLF)
- `an_unknown_format_is_400`
- `a_viewer_reads_reports_and_an_outsider_is_403`
- Unit (`report/mod.rs`): `csv_rows_follow_the_json_groups`, `null_means_render_as_empty_cells`.

**Acceptance:** `cargo test --test incidents incident_reports::` and `cargo test --lib incidents::report` pass.

---

### Task 24 (NAMI-INC-24): SLA report (per resource, average, compound, CSV)

**Covers:** R4.16 (SLA half)  **Depends on:** NAMI-INC-23

**Files:**
- Create: `nami-engine/tests/incidents/sla_report.rs`
- Modify: `nami-engine/src/uptime/availability.rs`, `nami-engine/src/incidents/report/mod.rs`, `nami-engine/src/incidents/report/routes.rs`, `nami-engine/tests/incidents/main.rs`

**Data shape:**

```rust
// uptime::availability (additions; for_monitor/for_heartbeat keep their signatures and results)
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDowntime { pub availability: Availability, /// incident spans minus own maintenance, clipped
    pub down: Vec<Span>, /// own maintenance, clipped to the resource's range
    pub maintenance: Vec<Span> }
pub async fn downtime_for_monitor(pool: &PgPool, project_id: Uuid, monitor_id: Uuid, windows: &[MaintenanceWindow],
    from: DateTime<Utc>, to: DateTime<Utc>, now: DateTime<Utc>) -> Result<Option<ResourceDowntime>, sqlx::Error>;
pub async fn downtime_for_heartbeat(/* same */) -> Result<Option<ResourceDowntime>, sqlx::Error>;
pub fn intersect(a: &[Span], b: &[Span]) -> Vec<Span>;   // both sorted, disjoint
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Compound { pub from: DateTime<Utc>, pub to: DateTime<Utc>, pub total_seconds: i64,
    pub maintenance_seconds: i64, pub downtime_seconds: i64, pub uptime_pct: Option<f64> }
/// R-I2-14: union of downtime, intersection of maintenance, over `range`.
pub fn compound(range: Span, resources: &[ResourceDowntime]) -> Compound;

// incidents::report
pub const MAX_SLA_RESOURCES: usize = 100;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)] #[serde(rename_all = "snake_case")]
pub enum ResourceKind { Monitor, Heartbeat }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlaRow { pub kind: ResourceKind, pub id: Uuid, pub name: String, #[serde(flatten)] pub availability: Availability }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlaReport { pub from: DateTime<Utc>, pub to: DateTime<Utc>, pub resources: Vec<SlaRow>,
    pub average: Option<f64>, pub compound: Compound }
pub fn sla_csv(report: &SlaReport) -> String;
```

**API:** `GET /projects/{id}/reports/sla?from=&to=&monitor_id=..&heartbeat_id=..&format=json|csv` (viewer). 400: no resource, more than 100 (after removing duplicates), bad range or format. 404: a monitor or heartbeat id that is not in the project (spec).

**Behaviour:**
1. `for_monitor`/`for_heartbeat` become wrappers over the `downtime_for_*` functions (load the windows, take `.availability`), so every existing availability test stays green unchanged.
2. The SLA route loads the project's maintenance windows once and calls `downtime_for_*` per resource.
3. `average` and `compound` per R-I2-14. Percentages are not rounded in JSON. The CSV writes them with three decimals.
4. CSV header `kind,id,name,uptime_pct,downtime_seconds,maintenance_seconds,total_seconds,incidents`, one row per resource, then `average` and `compound` rows.

**Tests:**
- Unit (`uptime/availability.rs`): `overlapping_downtime_counts_once`, `one_resources_maintenance_does_not_hide_anothers_downtime`, `common_maintenance_is_taken_out_of_the_range`, `time_before_a_resource_existed_is_neither_down_nor_maintenance`, `compound_of_one_resource_equals_its_own_uptime`, `all_maintenance_gives_null`, `intersect_of_disjoint_lists_is_empty`.
- Integration (`tests/incidents/sla_report.rs`):
  - `the_report_lists_each_resource_with_its_availability`
  - `average_is_the_mean_of_non_null_uptimes`
  - `compound_reflects_simultaneous_downtime`
  - `one_hundred_resources_are_accepted_and_one_hundred_and_one_is_400` (names `MAX_SLA_RESOURCES`)
  - `a_resource_of_another_project_is_404`
  - `no_resource_is_400`
  - `sla_csv_is_formula_guarded` (a monitor named `=HYPERLINK(...)`)
  - `the_existing_availability_endpoints_answer_as_before` (one call to each existing availability route, compared with a pinned JSON)

**Acceptance:** `cargo test --lib uptime::availability`, `cargo test --test incidents sla_report::` and `cargo test --test uptime availability::` pass.

---

### Task 25 (NAMI-INC-25): Shift reminder emails and the reminder opt-out

**Covers:** R4.06 (shift reminders)  **Depends on:** NAMI-INC-24

**Files:**
- Create: `nami-engine/migrations/NNNN_auth_oncall_preferences.sql`, `nami-engine/src/incidents/oncall/reminders.rs`, `nami-engine/src/incidents/oncall/account.rs`, `nami-engine/tests/incidents/shift_reminders.rs`
- Modify: `nami-engine/migrations/NNNN_incidents_part2.sql` (append), `nami-engine/src/incidents/oncall/mod.rs`, `nami-engine/src/incidents/worker.rs` (the reminder pass and the prune), `nami-engine/src/core/router.rs`, `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:**

```sql
-- NNNN_auth_oncall_preferences.sql (Incidents Part 2, NAMI-INC-25 and NAMI-INC-26)
alter table users
    add column shift_reminders boolean not null default true,
    add column ics_token_hash bytea constraint users_ics_token_hash_len check (octet_length(ics_token_hash) = 32),
    add constraint users_ics_token_hash_unique unique (ics_token_hash);

-- appended to NNNN_incidents_part2.sql
create table on_call_shift_reminders (
    schedule_id uuid not null references on_call_schedules(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    shift_start timestamptz not null,
    sent_at timestamptz not null default now(),
    constraint on_call_shift_reminders_pkey primary key (schedule_id, user_id, shift_start)
);
create index on_call_shift_reminders_user_sent_idx on on_call_shift_reminders (user_id, sent_at);
```

```rust
// incidents::oncall::reminders
pub const REMINDER_LEAD: chrono::Duration = chrono::Duration::hours(24);
pub const REMINDER_PASS_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const MAX_REMINDERS_PER_PASS: usize = 200;
pub const MAX_REMINDERS_PER_USER_PER_DAY: i64 = 10;
pub const REMINDER_RETENTION: chrono::Duration = chrono::Duration::days(7);
/// Pure: the shifts of `shifts(now, now + REMINDER_LEAD)` that start after `now`.
pub fn due_shifts(all: &[Shift], now: DateTime<Utc>) -> Vec<&Shift>;
/// One pass (R-I2-15). Returns how many emails were sent. Each reminder row commits before its email.
pub async fn run_pass(state: &AppState, now: DateTime<Utc>) -> Result<usize, AppError>;
pub fn email(schedule: &str, project: &str, tz: Tz, shift: &Shift, ui_base_url: &str, schedule_url: &str) -> (String, String);

// incidents::oncall::account
#[derive(Debug, Serialize)]
pub struct OnCallPreferences { pub shift_reminders: bool, pub ics_feed: bool }
```

**API:**

| Method + path | Role | Body | Success |
|---|---|---|---|
| `GET /account/oncall` | any logged-in user | none | `200 OnCallPreferences` |
| `PUT /account/oncall` | any logged-in user | `{shift_reminders: bool}` | `200 OnCallPreferences` |

**Behaviour:** R-I2-15. In addition:
1. The pass runs from `incidents::worker::run_forever` every `REMINDER_PASS_INTERVAL`, not from `kick`, and only when `incidents_worker_enabled` (Part 1 R23).
2. Email text is plain (`SinglePart::plain` through `core::email::Mailer::send`), and every user-chosen name goes through `is_unsafe_in_text` → space, so a schedule name cannot forge lines.
3. Rows are pruned once an hour (`shift_start < now - REMINDER_RETENTION`).

**Tests:**
- Unit (`reminders.rs`): `only_shifts_starting_after_now_are_due`, `the_email_states_utc_and_the_schedule_zone`, `names_cannot_forge_lines`.
- Integration (`tests/incidents/shift_reminders.rs`, `run_pass` with a fixed `now`, Mailpit filtered by recipient):
  - `a_user_is_reminded_once_of_a_shift_starting_within_24_hours`
  - `a_shift_more_than_24_hours_ahead_is_not_reminded_yet`
  - `a_shift_created_less_than_24_hours_ahead_is_reminded_on_the_next_pass`
  - `an_override_shift_is_reminded_too`
  - `a_running_shift_is_not_reminded`
  - `an_opted_out_user_gets_no_reminder` and `an_unverified_email_gets_no_reminder`
  - `a_non_member_participant_gets_no_reminder`
  - `two_instances_running_a_pass_send_one_email` (two concurrent `run_pass` calls)
  - `a_failed_send_is_retried_on_the_next_pass` (Mailer pointed at a closed port, then at Mailpit)
  - `the_eleventh_reminder_in_a_day_is_not_sent` (names `MAX_REMINDERS_PER_USER_PER_DAY`)
  - `a_user_reads_and_changes_only_their_own_preference`
- `tests/core/migrations.rs`: `one_reminder_per_schedule_user_and_shift_start`.

**Acceptance:** `cargo test --lib incidents::oncall::reminders` and `cargo test --test incidents shift_reminders::` pass.

---

### Task 26 (NAMI-INC-26): Per-user ICS feed

**Covers:** R4.06 (ICS feed)  **Depends on:** NAMI-INC-25

**Files:**
- Create: `nami-engine/src/incidents/oncall/ics.rs`, `nami-engine/tests/incidents/ics_feed.rs`
- Modify: `nami-engine/src/incidents/oncall/account.rs` (token routes), `nami-engine/src/incidents/oncall/mod.rs`, `nami-engine/src/incidents/mod.rs` (`IncidentsState` gains the feed buckets and places), `nami-engine/src/core/router.rs` (the feed on the public listener), `nami-engine/src/incidents/worker.rs` (prune the feed buckets), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/json_bodies.rs` (the token `POST`)

**Data shape:**

```rust
// incidents::oncall::ics (pure)
pub const ICS_PAST: chrono::Duration = chrono::Duration::days(7);
pub const ICS_AHEAD: chrono::Duration = chrono::Duration::days(60);
pub const MAX_ICS_SCHEDULES: usize = 100;
pub const MAX_ICS_IN_FLIGHT: usize = 2;
pub const ICS_PER_HOUR: f64 = 30.0;
pub const ICS_BURST: f64 = 10.0;
pub const MAX_ICS_LINE_OCTETS: usize = 75;
pub struct FeedEvent { pub schedule_id: Uuid, pub project_id: Uuid, pub schedule: String, pub project: String,
    pub starts_at: DateTime<Utc>, pub ends_at: DateTime<Utc>, pub is_override: bool }
pub fn escape_text(s: &str) -> String;           // RFC 5545 TEXT: \\ \; \, \n; CR and controls removed
pub fn fold(line: &str) -> String;               // 75 octets, UTF-8 safe, CRLF + space continuation
pub fn render(events: &[FeedEvent], now: DateTime<Utc>, ui_base_url: &str) -> String;
```

**API:**

| Method + path | Role | Body | Success | Errors |
|---|---|---|---|---|
| `POST /account/oncall/ics-token` | any logged-in user | `{}` | `201 {"token", "path": "/public/oncall/<token>.ics"}`, shown once; replaces any old token | |
| `DELETE /account/oncall/ics-token` | any logged-in user | none | `204` | 404 none |
| `GET /public/oncall/{file}` | none (public listener) | none | `200 text/calendar` | 404 unknown token or no `.ics`, 429 |

**Behaviour:** R-I2-16. In addition:
1. The token's owner is looked up by `sha256(token)`. The feed's schedules are those of projects with a `project_members` row for the owner, so leaving a project removes its shifts at once.
2. The shifts are `incidents::oncall::rotation::shifts(now - ICS_PAST, now + ICS_AHEAD)` filtered to the owner, computed in `spawn_blocking`.
3. An empty feed is a valid `VCALENDAR` with no events.

**Tests:**
- Unit (`ics.rs`): `crlf_and_separators_in_names_cannot_add_properties` (a schedule named `x\r\nBEGIN:VEVENT;y,z`), `lines_fold_at_75_octets_without_splitting_a_char` (multi-byte names), `uids_are_stable_per_shift`, `the_calendar_has_the_required_properties`, `an_empty_feed_is_valid`.
- Integration (`tests/incidents/ics_feed.rs`, through `build_public_router`):
  - `a_feed_lists_only_its_owners_shifts`
  - `a_feed_lists_only_current_projects` (remove the owner from one project; its shifts disappear)
  - `the_window_is_seven_days_back_to_sixty_ahead`
  - `regenerating_the_token_kills_the_old_url` and `deleting_the_token_makes_the_feed_404`
  - `the_token_is_stored_hashed_and_shown_once` (no route returns it again)
  - `an_unknown_token_or_a_path_without_ics_is_404`
  - `the_eleventh_fetch_in_a_burst_is_429` (names `ICS_BURST`)
  - `the_feed_is_not_on_the_private_api` (404 from `build_router`)

**Acceptance:** `cargo test --lib incidents::oncall::ics` and `cargo test --test incidents ics_feed::` pass; the feed imports into a calendar client without errors (manual check noted in the PR).

---

### Task 27 (NAMI-INC-27): "Take over shift"

**Covers:** R4.06 (take over)  **Depends on:** NAMI-INC-26

**Files:**
- Create: `nami-engine/tests/incidents/take_over.rs`
- Modify: `nami-engine/src/incidents/oncall/routes.rs`, `nami-engine/src/incidents/oncall/model.rs` (planned in NAMI-INC-7: a `take_over` function using `add_override`), `nami-engine/tests/incidents/main.rs`, `nami-engine/tests/core/json_bodies.rs`

**Data shape:**

```rust
pub const MAX_TAKE_OVER_AHEAD: chrono::Duration = chrono::Duration::days(60);
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct TakeOver { pub starts_at: DateTime<Utc>, pub ends_at: DateTime<Utc> }
pub enum TookOver { Created { override_view: OverrideView, displaced_user: Uuid }, NotInOneShift, NotAMember, NotFound }
pub async fn take_over(pool: &PgPool, project_id: Uuid, schedule_id: Uuid, caller: Uuid, req: &TakeOver, now: DateTime<Utc>)
    -> Result<TookOver, AppError>;
```

**API:** `POST /projects/{id}/schedules/{schedule_id}/take-over` (member) `{starts_at, ends_at}` → `201 OverrideView`. 400 `choose a time inside one shift of someone else`, `only project members can be on call`, `ends_at must be after starts_at`, range over `MAX_OVERRIDE_DAYS` or starting more than 60 days ahead; 403 viewer; 404 foreign schedule.

**Behaviour:** R-I2-17. The override is created for the caller only: the body has no user field, so a member can never create one for someone else. The displaced user's email is sent after the commit and its failure is logged, never surfaced.

**Tests** (`tests/incidents/take_over.rs`):
- `a_member_takes_over_part_of_someone_elses_shift`
- `the_displaced_user_gets_an_email` (Mailpit)
- `the_override_records_its_creator`
- `a_range_spanning_two_shifts_is_400` and `taking_over_ones_own_shift_is_400` and `a_gap_with_nobody_on_call_is_400`
- `a_past_start_moves_to_now`
- `a_viewer_is_403`
- `a_user_field_in_the_body_is_422` (`deny_unknown_fields`: a member cannot target someone else)
- `a_global_admin_who_is_not_a_member_is_400`
- `a_range_starting_more_than_60_days_ahead_is_400` and `a_range_longer_than_max_override_days_is_400`
- `after_a_take_over_the_caller_is_on_call` (`on_call_at` through `GET /projects/{id}/schedules/{sid}/shifts`)

**Acceptance:** `cargo test --test incidents take_over::` passes.

---

### Task 28 (NAMI-INC-28): UI: list filters, metadata panel, reporter, tickets on the detail page, severity settings

**Covers:** R4.14, R4.15, R4.17 (create ticket and ticket links), R4.18  **Depends on:** NAMI-INC-17, NAMI-INC-22 (and NAMI-INC-16 for the settings page)

**Files:**
- Create: `nami-ui/src/lib/incidents/filters.ts` (+ `filters.test.ts`), `nami-ui/src/lib/server/incidents/metadata-form.ts` (+ `metadata-form.test.ts`), `nami-ui/src/lib/components/MetadataPanel.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/+layout.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/severities/+page.server.ts`, `+page.svelte`, `severities.test.ts`
- Modify: the incident list and detail pages and their tests (planned in NAMI-INC-12), Part 1's incident list query helpers (`parseListQuery` / `engineListQuery`, planned in NAMI-INC-12), which `filters.ts` extends

**Data shape:**

```ts
// lib/incidents/filters.ts
export type ListFilters = { severityIds: string[]; from: string | null; to: string | null;
	metadata: [string, string][]; q: string | null };
export const MAX_METADATA_FILTERS = 10;
export const MAX_Q_CHARS = 200;
/** Keeps only valid values: uuids, `YYYY-MM-DD` dates (read as UTC midnight), keys ^[A-Za-z0-9_.-]{1,64}$. */
export function parseFilters(url: URL): ListFilters;
/** Engine query string: severity_id=..&from=<ISO>&to=<ISO, next day>&metadata.<k>=<v>&q=.. */
export function engineFilterQuery(f: ListFilters): string;
// lib/server/incidents/metadata-form.ts
export function metadataPayloadFromForm(form: FormData):
	{ ok: true; payload: { metadata: Record<string, string> } } | { ok: false; message: string };
```

**Behaviour:**
1. List: a filter bar above the tabs: severity checkboxes (from `GET /projects/{id}/severities`), `<input type="date">` from/to (UTC, the `to` day included, so the engine gets the next midnight), metadata key/value rows (add/remove, up to 10), a search box for `q`. Filters live in the URL query, so a filtered view can be shared, and "Load more" keeps them. Clearing resets to the default tab.
2. Detail: `MetadataPanel` lists the pairs (text only). Members and admins (`data.role`, Part 1 R28) get an edit form (rows of key/value inputs, action `metadata`, `PUT .../metadata`), validated by `metadataPayloadFromForm` with the engine's caps (50 keys, 1 KiB values) before the call, and the engine's 400 shown otherwise.
3. Detail, report-form incidents: a "Reported by" block with name and contact as text (Svelte escapes).
4. Detail: a "Tickets" card lists existing tickets (kind, external id, link only when `url` is set, `rel="noopener noreferrer"`, `target="_blank"`). Members see "Create ticket" with a select of the project's ticket integrations that have no ticket yet. A 409 shows the existing link; a 502 shows the engine's message.
5. Severity settings (`settings/severities`): list by rank with default badge, create (name, default), rename, make default, move up/down (posts the whole order to `PUT .../order`), delete (two-step; 409 messages shown). Forms render for admins only; everyone else sees the list.
6. `settings/+layout.svelte` renders the settings tabs of R-I2-21 with `SegmentedTabs` (link mode).

**Tests:**
- `filters.test.ts`: `drops_invalid_values`, `turns_the_to_day_into_the_next_utc_midnight`, `caps_metadata_pairs_at_ten`, `round_trips_through_the_url`.
- `metadata-form.test.ts`: `builds_the_object_from_rows`, `refuses_a_bad_key_a_duplicate_key_and_an_oversized_value`.
- Route tests: the list forwards only parsed filters; the detail renders the metadata panel without an edit form for `role: 'viewer'` and with it for `role: 'member'`; reporter text with `<script>` renders as text; a ticket without `url` renders no link; the severities page renders forms for `admin` only and its actions post the right engine bodies.

**Acceptance:** `npm run check` has 0 errors and 0 warnings; `npx vitest run` passes for the files above.

---

### Task 29 (NAMI-INC-29): UI: integrations, report form, Slack and ticket settings

**Covers:** R4.10, R4.12, R4.13 (settings), R4.17 (ticket integrations), R4.18  **Depends on:** NAMI-INC-18 .. NAMI-INC-22, NAMI-INC-28 (settings tabs)

**Files:**
- Create: `nami-ui/src/lib/server/incidents/integration-form.ts` (+ test), `nami-ui/src/lib/server/incidents/ticket-form.ts` (+ test), `nami-ui/src/lib/server/incidents/report-form.ts` (+ test), and for each of `settings/integrations`, `settings/report-form`, `settings/slack`, `settings/tickets` under `nami-ui/src/routes/(app)/projects/[id=uuid]/`: `+page.server.ts`, `+page.svelte`, `<name>.test.ts`

**Behaviour:**
1. **Integrations.** List with kind, enabled, last received (UTC, "never"), unresolved count. Create (admin): name, kind, policy and severity selects. Right after creation, and on "Show URL" (member, `GET .../token`), the full URL `publicEngineUrl() + url_path` with `CopyButton`, plus a kind-specific snippet: an Alertmanager `webhook_configs` block, the Grafana contact point steps, or a `curl` for the generic webhook. Rotate (admin, two-step, says the old URL stops at once), enable/disable, "Send test" (admin; says it opens a real incident and notifies), delete (two-step).
2. **Report form.** Enable/disable, intro (textarea, 2 KiB), policy and severity selects (admin). Members see the public URL (`<UI origin>/report/<token>` from `path`) with `CopyButton`. Rotate (admin, two-step).
3. **Slack.** Status (configured or not). Admins paste the signing secret and the bot token (both `type="password"`, `autocomplete="off"`, never prefilled), or remove the app (two-step). The page shows the interactivity Request URL (`publicEngineUrl() + request_url_path`) and the setup steps: enable interactivity, scopes `users:read` and `users:read.email`, and that only project Slack channels get buttons.
4. **Tickets.** List by label. Admins create a GitHub (repository, token), Linear (team id, API key) or Jira (base URL, project key, issue type, email, API token) integration, replace a token (write-only), delete (two-step, warns that ticket links on incidents go too).
5. Every page that shows or accepts a secret sets `cache-control: private, no-store`. Form helpers check formats before the engine call (the same patterns as the engine) and the engine stays the authority.

**Tests:** helper tests per form (valid payloads, every refusal); route tests per page: forms for `admin` only, the token or URL never rendered for `viewer`, secrets never echoed back into inputs after a failed submit, `cache-control` set, delete needs `confirm=yes`.

**Acceptance:** `npm run check` clean; `npx vitest run` passes for these files.

---

### Task 30 (NAMI-INC-30): UI: the public report form page

**Covers:** R4.13 (public page), R4.18  **Depends on:** NAMI-INC-20

**Files:**
- Create: `nami-ui/src/routes/report/[token]/+page.server.ts`, `+page.svelte`, `report-page.test.ts`
- Modify: `nami-ui/src/lib/server/incidents/report-form.ts` (+ test: `submissionFromForm`)

**Behaviour:**
1. Outside the `(app)` group, so no login (like Part 1's `/incidents/action`). The token param must match `^[A-Za-z0-9_-]{43}$` before it reaches `enginePath`; anything else is a 404 page without an engine call.
2. `load` calls `GET /public/report-forms/{token}`; 404 renders "This form does not exist or is closed." Headers: `x-robots-tag: noindex, nofollow`, `referrer-policy: no-referrer`, `cache-control: no-store`; the page also has `<meta name="robots" content="noindex, nofollow">`.
3. The form: title (required, 200), description (textarea, 10 KiB), name (100), contact (200), and the honeypot `website` (visually hidden, `tabindex="-1"`, `autocomplete="off"`, label "Leave this field empty"). The default action posts through `callEngine` (which sets `X-Forwarded-For` from `getClientAddress()`). SvelteKit's origin check stays on.
4. Success (including a honeypot hit, which the engine answers identically) shows "Thanks, your report was sent." 429 shows "Too many reports from your network. Try again later." 400 shows the engine message next to the form, keeping the typed values.
5. The intro renders as text with line breaks kept (`white-space: pre-wrap`), never as HTML.

**Tests:** `report-page.test.ts`: a malformed token makes no engine call; headers are set; the intro with `<script>` renders as text; a honeypot-filled post and a normal post render the same success; 429 and 400 messages; the posted body carries exactly the five fields.

**Acceptance:** `npm run check` clean; `npx vitest run src/routes/report` passes.

---

### Task 31 (NAMI-INC-31): UI: reports page (incident report, SLA report, CSV)

**Covers:** R4.16, R4.18  **Depends on:** NAMI-INC-23, NAMI-INC-24

**Files:**
- Create: `nami-ui/src/lib/incidents/reports.ts` (+ `reports.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/reports/+page.server.ts`, `+page.svelte`, `reports.test.ts`, `reports/incidents/export/+server.ts`, `reports/sla/export/+server.ts`
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` ("Reports" tab after "On-call")

**Data shape:**

```ts
export function defaultRange(now: Date): { from: string; to: string };   // last 30 days, UTC dates
export function rangeQuery(from: string, to: string): string | null;     // null when invalid or over 1096 days
export function formatSeconds(s: number | null): string;                 // "n/a", "45 s", "3 h 12 min"
export function formatPct(p: number | null): string;                     // "99.952 %", "n/a"
```

**Behaviour:**
1. Date range (`<input type="date">`, UTC) in the query string; invalid or too long → an inline message and no engine call.
2. Incident report: `StatStrip` with `StatCard`s for incidents, MTTA, MTTR, acknowledged share; then three tables (by source, severity, responder) with count, MTTA, MTTR.
3. SLA: a multi-select of the project's monitors and heartbeats (up to 100, checkboxes), a table per resource (uptime, downtime, maintenance, incidents) with `average` and `compound` rows at the bottom, explained in one line each ("mean of the rows", "share of time all were up at once").
4. "Download CSV" links go to the two `+server.ts` relays, which call the engine with `format=csv` and stream the body through `#lib/server/core/relay-stream` with the engine's `content-type` and `content-disposition`.
5. Viewers see everything (spec: viewers read reports).

**Tests:** `reports.test.ts` (lib): range defaults and limits, formatting. Route tests: the loader forwards only valid ranges and uuids; the relays pass `format=csv` and stream without buffering; the tab appears in the layout.

**Acceptance:** `npm run check` clean; `npx vitest run` passes for these files.

---

### Task 32 (NAMI-INC-32): UI: reminders, ICS link and "take over shift"

**Covers:** R4.06, R4.18  **Depends on:** NAMI-INC-25, NAMI-INC-26, NAMI-INC-27

**Files:**
- Modify: `nami-ui/src/routes/(app)/account/notifications/+page.server.ts`, `+page.svelte` and its test (planned in NAMI-INC-13), `nami-ui/src/routes/(app)/projects/[id=uuid]/oncall/[scheduleId=uuid]/+page.server.ts`, `+page.svelte` and `schedule-detail.test.ts` (planned in NAMI-INC-15)

**Behaviour:**
1. Account page, "On-call" card: a reminder toggle (action `reminders`, `PUT /account/oncall`), and the ICS feed: "Create link" / "Regenerate link" (two-step when a link exists: the old URL stops working) and "Remove link" (two-step). After create or regenerate, the full URL (`publicEngineUrl() + path`) shows once with `CopyButton` and the note that it cannot be shown again. The page sets `cache-control: private, no-store`.
2. Schedule page: each upcoming shift of someone else gets "Take over" for `member`/`admin` (`data.role`), opening a small form with `datetime-local` start and end prefilled with the shift's bounds in the schedule's zone, converted with Part 1's `localInputToUtc` (`#lib/incidents/oncall`, planned in NAMI-INC-15). The action posts `{starts_at, ends_at}` to `.../take-over`; 400 messages show inline; success reloads the shift list.

**Tests:** the account page renders the URL only in the create/regenerate result, never on a plain load; regenerate and remove need `confirm=yes`; the toggle posts a boolean. The schedule page shows "Take over" for `member` and not for `viewer`, never on the caller's own shifts, and converts the inputs in the schedule's zone (a DST-day case).

**Acceptance:** `npm run check` clean; `npx vitest run` passes for these files.

---

### Task 33 (NAMI-INC-33): Part 2 security review and fix wave

**Covers:** the phase's security review over all of Part 2, and the sub-project's final review  **Depends on:** NAMI-INC-16 .. NAMI-INC-32

**Files:** fix commits on `feat/incidents-part2`, each with a test for the issue it fixes; `docs/superpowers/decisions/<date>-incident-management-part2.md`.

**Behaviour:** a dedicated review of the whole branch (and, as the last phase of the sub-project, a pass over Part 1's surface where Part 2 touches it), checking with a test for every issue found:
1. **Tokens and secret URLs.** Integration, report-form and ICS tokens: unknown, rotated or disabled → 404; hashed at rest; never in a response after the one that shows them, a `Debug` print, a `tracing` field, the timeline or an error string. Grep the engine for `tracing::` calls in every new module and for any field carrying a path, token or body (R-I2-5).
2. **Third-party input.** Integration payloads: > 1 MiB → 413; malformed JSON and wrong shapes → 400, never 422 or a panic; metadata caps; > 100 alerts truncated and counted; 20 concurrent duplicates open one incident.
3. **Slack.** Missing or wrong signature, replay outside 5 minutes, altered body → 401; a viewer or unmapped Slack user changes nothing; an incident of another project is rejected; `response_url` off `hooks.slack.com` is never called under `Strict`; `OutboundPolicy::Relaxed` is reachable only through `Config::from_env_for_test` (grep).
4. **Report form.** Honeypot submissions create nothing and look identical; per-address and per-form limits; reporter text with mention, markup, link and HTML payloads is inert in the UI, in every channel formatter (Slack, Teams, Discord, Telegram, email) and in ticket bodies.
5. **Tickets.** Tokens sealed and absent from responses, logs and the timeline; stored URLs are https on the expected host; `@team` in a title or cause mentions nobody on GitHub; Jira's base URL cannot reach link-local or metadata addresses.
6. **ICS.** Only the owner's shifts, only in current projects; CR/LF, `;` and `,` in names cannot inject properties or events; the feed's cost is bounded (schedules, in-flight, rate).
7. **Take over.** Viewers → 403; a member cannot create an override for someone else.
8. **Reports.** Ids of another project → 404; CSV formula guard on every text cell; ranges capped.
9. **RBAC table.** Every new route × role (outsider, viewer, member, admin, global admin) matches the spec's RBAC section and the Global Constraints above. `tests/core/json_bodies.rs` lists every new private state-changing route.
10. **Bounds.** Every limit constant has a test at the bound and one past it. No lock is held across `.await`; ICS and reminder computations run in `spawn_blocking`.
11. **nami-ui.** No `{@html}` added by Part 2; secrets never prefilled; `no-store` on secret pages; the public report page sends `noindex`, `no-referrer`, `no-store` and keeps the origin check.

**Acceptance:** every finding has a fix commit with its test, or a recorded ruling in the decisions doc; the engine (`cargo test`) and UI (`npm run check`, `npx vitest run`) suites pass; the decisions doc records R-I2-1 .. R-I2-21, every execution-time change, and the follow-ups below.

## After NAMI-INC-33: roadmap ticks and the phase PR

Tick these §4 items in `docs/superpowers/roadmap.md` in the phase branch, once the review is clean (each line is the item, the tasks are in the Roadmap coverage table):
- On-call working-hours restriction, shift reminder emails, per-user ICS feed, "take over shift" (Part 1 shipped the restriction).
- Slack Acknowledge/Resolve buttons.
- Incoming integrations.
- Public incident report form.
- Severities and incident metadata.
- Incident list filters.
- Reporting.
- GitHub / Linear / Jira tickets.
- UI for all of the above.

The phase PR lists purpose and scope, each task with its commit range, the implementation summary, the engine and UI commands run with their results, breaking changes (none expected; the list endpoint only gains query keys and fields), risks and follow-ups. No CI is configured yet; the PR says so.

Follow-ups for the decisions doc (not in this plan): redaction of token path segments in a future request access log (Platform); a trigram index for `q` if a measurement asks for it; Slack interactivity for personal Slack channels; GitHub Enterprise hosts.
