# Incident Management + On-call

> Parity additions (2026-10-13): incoming integrations (generic webhook, Alertmanager, Grafana), Microsoft Teams channel, severities + incident metadata, reporting (MTTA/MTTR, counts, SLA, CSV), public incident report form, escalation time-window conditions + fallback policy, on-call working hours, shift reminders, ICS feed, take over shift, incident list filters, Slack Acknowledge/Resolve buttons, GitHub/Linear/Jira tickets.

## Context

Fourth sub-project (roadmap §4), mirroring BetterStack Incident management
and On-call. Builds on the `incidents` table and `alerts::notify_transition`
entry point from Uptime Phase 1 (extended by Logs for `log_alert_id`), the
worker loop, `crypto.rs` (AES-GCM) from Uptime Phase 2, Uptime Phase 3's
availability function (SLA reports), and RBAC.

## Goals

### Incident lifecycle

- Status: `open` → `acknowledged` → `resolved` (resolve allowed from either
  open state; `reopen` from resolved returns to `open` and restarts
  escalation). Columns added: `acknowledged_at`, `acknowledged_by`,
  `resolved_by` (null = automatic), `source` (`monitor` | `heartbeat` |
  `log_alert` | `metric_alert` | `manual` | `integration` |
  `report_form`; later sub-projects add their own), `escalation_policy_id`,
  `escalation_step`, `escalation_repeat`, `next_escalation_at`,
  `postmortem` (markdown, max 100 KiB), `severity_id`, `metadata` — full
  list in "Data model additions".
- Manual incidents: members create with title, description, optional
  policy (default = project default policy), optional severity (default =
  project default) and metadata.
- Timeline `incident_events`: `opened`, `acknowledged`, `resolved`,
  `reopened`, `escalated`, `notification_sent`, `notification_failed`,
  `comment`, `postmortem_updated`, `integration_event`, `step_skipped`,
  `escalation_fallback`, `severity_changed`, `metadata_updated`,
  `ticket_created` — actor (nullable), message, metadata JSON, timestamp.
  Comments: members post, max 10 KiB.
- List `GET /projects/{id}/incidents` filters: `status`, `source`,
  `severity_id` (each repeatable), `from` / `to` on `started_at`,
  `metadata.<key>=<value>` (repeatable, exact), `q` (title substring);
  keyset pagination, newest first.

### Severities and metadata

- `incident_severities` per project: `name` (`^[a-z0-9_-]{1,32}$`, unique
  per project), `rank` (1 = most severe, unique), exactly one
  `is_default`. New projects (and a backfill migration) get `critical`
  (1), `high` (2, default), `low` (3). Admins manage; deleting a
  severity still referenced → 409.
- `incidents.severity_id` is set at open from, in order: the payload
  (integrations, by name), the source's `severity_id` (monitors,
  heartbeats, log alerts, metric alerts, integrations and the report
  form each gain one), else the project default. Members change it later
  (`severity_changed`). Severity appears in every notification.
- `incidents.metadata` (jsonb object): keys `^[A-Za-z0-9_.-]{1,64}$`,
  string values <= 1 KiB, <= 50 keys, <= 16 KiB total. Filled from
  integration payloads (Alertmanager/Grafana labels), editable by members
  (`metadata_updated`), shown on the detail page, filterable (GIN index).

### Incoming integrations

- `incident_integrations` per project: `name`, `kind` (`webhook` |
  `alertmanager` | `grafana`), optional `escalation_policy_id` (else the
  project default) and `severity_id`, `enabled`. Each has a secret URL
  `POST /api/v1/integrations/{token}` (no session); token = 32 random
  bytes base64url, stored SHA-256 + AES-GCM ciphertext (members can view
  it again, like source tokens); rotate = the old URL stops working at
  once. Unknown, rotated or disabled token → 404.
- Payloads (JSON, body <= 1 MiB):
  - `webhook`: fields `title` (required, <= 255), `cause` (<= 10 KiB),
    `dedup_key` (<= 255), `status` (`open` default | `resolved`),
    `severity` (a project severity name), `metadata` (object of string
    values). `resolved` without `dedup_key` → 400.
  - `alertmanager` (webhook payload `version: "4"`) and `grafana`
    (unified-alerting webhook contact point): each `alerts[]` entry (max
    100 per request; the rest is ignored and counted) is one incident
    keyed by its `fingerprint`; title = `annotations.summary` else
    `labels.alertname`; cause = `annotations.description`; metadata =
    labels plus `generatorURL`; per-alert `status` `firing` | `resolved`.
- Dedup: at most one unresolved incident per `(integration_id,
  dedup_key)` (partial unique index, so concurrent duplicates open one
  incident). A repeat `open`/`firing` appends an `integration_event` to
  that incident instead of opening another; a `resolved` payload resolves
  it automatically (`resolved_by` null) and notifies; a payload after
  resolution opens a new incident. No `dedup_key` → every `open` payload
  is a new incident.
- Response `202 {"opened": n, "deduplicated": n, "resolved": n,
  "ignored": n}`. Per-integration rate limit 60 requests/min, burst 120
  → 429. Each integration shows its last-received time and has a "send
  test payload" action.

### Public incident report form

- One optional form per project (`incident_report_forms`): `enabled`,
  secret token (handled like integration tokens), optional
  `escalation_policy_id` and `severity_id`, optional intro text (plain
  text <= 2 KiB). Admins enable, rotate, disable.
- Public nami-ui page `/report/[token]` (outside the `(app)` group,
  `noindex`): title (required, <= 200), description (<= 10 KiB), reporter
  name (<= 100) and contact (<= 200), all plain text, plus a hidden
  honeypot field. Posts through the BFF to
  `POST /public/report-forms/{token}/incidents`.
- Creates an incident (`source = 'report_form'`; reporter name and
  contact stored on it and shown in the timeline) routed through the
  form's policy. A filled honeypot gets the same success response and
  creates nothing. Rate limits per client IP: 5/hour per form, plus
  30/hour per form overall → 429. Unknown or disabled token → 404.

### Escalation policies

- Per project; ordered steps, each: `delay_minutes` (0-1440, relative to
  the previous step; step 1 relative to incident open) and targets — any
  mix of `user`, `schedule` (whoever is on call at execution time),
  `all_members`, `project_channel`.
- `repeat_count` (0-10): rerun all steps while still unacknowledged.
- Each project gets a default policy on creation (and a migration backfills
  existing projects): one step, delay 0, target `all_members` — so behavior
  matches Uptime's email-everyone default until edited.
- Acknowledge stops escalation; resolve notifies everyone already notified.
- Optional per-step time-window condition `{weekdays: [1-7], from:
  "HH:MM", to: "HH:MM", timezone: IANA}` (`from > to` spans midnight).
  A step whose condition is false at execution time is skipped
  (`step_skipped`) and the next step is scheduled as usual. Evaluation is
  a pure function, tested across DST transitions.
- Optional `fallback_policy_id` (same project, not itself). When a full
  pass over the steps notifies nobody (every step skipped or resolved to
  zero recipients, e.g. nobody on call) or the last repeat ends
  unacknowledged, escalation continues on the fallback policy
  (`escalation_fallback`). A fallback policy's own fallback is not
  followed (one hop, so no loops).

### On-call schedules

- Per project: name, IANA timezone, ordered participants, rotation length
  (`daily`, `weekly`, or custom hours), handoff time (local `HH:MM`), start
  date. Overrides: user + `starts_at`/`ends_at`, newest wins on overlap.
- `on_call_at(schedule, instant)` is a pure function (tested across DST
  transitions); API exposes "on call now" and the next 14 days of shifts.
- Working-hours restriction on a schedule's rotation: `restrictions` =
  list of `{weekdays, from, to}` windows in the schedule's timezone
  (empty = always). Outside them the rotation has nobody on call;
  overrides still apply. Follow-the-sun = several restricted schedules as
  targets of one escalation step.
- Shift reminders: the worker emails a user once when one of their
  shifts (rotation or override) starts within 24 h
  (`on_call_shift_reminders`, unique per schedule, user and shift start;
  a shift created less than 24 h ahead gets its reminder at once).
  Personal opt-out in account settings.
- ICS feed per user: `GET /public/oncall/{token}.ics` lists the user's
  shifts from 7 days back to 60 days ahead across projects they still
  belong to, computed per request. Token: 32 random bytes, stored
  SHA-256, shown once; regenerating kills the old URL. Stable `UID` per
  shift, text escaped per RFC 5545, read-only; unknown token → 404.
- "Take over shift": a member (not a viewer) creates an override for
  themselves only, covering a chosen shift or part of it; the displaced
  user gets an email; the override records its creator.

### Notification channels

- Kinds: `email`, `webhook`, `slack`, `teams`, `telegram`, `discord`.
- Personal channels (per user: their own email is implicit; plus any other
  kinds) and project channels (team rooms, admin-managed).
- Config (webhook URLs, Slack/Discord webhook URLs, Teams Workflows or
  incoming-webhook URL, Telegram bot token + chat id) encrypted at rest
  with AES-GCM; API never returns secrets (write-only, like Uptime Phase
  2 headers). "Send test" action per channel.
- Formatting with escaping (tested): Slack — escape `&`, `<`, `>` so
  `<!channel>`/`<@U…>` cannot be injected; Discord — `allowed_mentions:
  {"parse": []}` and escape markdown; Telegram — HTML parse mode with
  `&`/`<`/`>` escaped; Teams — an Adaptive Card (v1.4) posted to the
  https URL, user-controlled text only in `TextBlock`s with markdown
  control characters (`*`, `_`, `[`, `]`, `(`, `)`, `` ` ``, `#`, `>`,
  `-`, `!`) and `&`, `<`, `>` escaped so links, `<at>` mentions and
  formatting cannot be injected, and the only link an `Action.OpenUrl` to
  the incident; webhook — JSON body
  `{event, incident:{id,title,cause,status,source,severity,started_at,url}, project:{id,name}}`
  plus `X-Nami-Signature: sha256=<HMAC of body>` with a per-channel secret
  shown once at creation.
- Delivery queue `notification_deliveries` (durable): pending → sent |
  failed, up to 5 attempts with exponential backoff (30 s … 1 h), 10 s
  timeout, per-attempt result recorded in the incident timeline. A delivery
  that succeeded is never resent.

### Escalation engine

Worker tick claims incidents with `status = 'open' AND next_escalation_at
<= now()` atomically (`FOR UPDATE SKIP LOCKED`), resolves the current
step's targets to users and channels (schedule → current on-call user,
deduplicated), enqueues deliveries, records events, and schedules the next
step / repeat / fallback / end. A separate tick drains due deliveries.

### Acknowledge / resolve from notifications

Notifications carry a link to a nami-ui confirmation page
(`/incidents/action?token=…`) — never a state-changing GET, so email
scanners and link previews cannot acknowledge anything. The page shows the
incident and a button that POSTs the token. Tokens: HMAC-SHA256 over
`(incident_id, action, user_id, exp)` with a key derived from
`NAMI_ENCRYPTION_KEY`, expiring after 24 h; the POST works without a
session (the token is the credential) and records the token's user as the
actor.

### Slack interactivity (optional, per project)

- Admins store a Slack app's signing secret and bot token (encrypted,
  write-only). That project's Slack messages then carry Block Kit
  Acknowledge/Resolve buttons; the app's interactivity Request URL is
  `POST /api/v1/slack/interactivity/{project_id}`.
- Verified before parsing: `X-Slack-Signature` must equal `v0=` + hex
  HMAC-SHA256(signing secret, `v0:{X-Slack-Request-Timestamp}:{raw
  body}`) (constant-time compare) and the timestamp must be within 5
  minutes of now, else 401.
- The clicking Slack user is resolved through `users.info` (bot token,
  `users:read.email`) to a Nami user with the same verified email; only
  a project member or admin can act. Anyone else gets an ephemeral "not
  authorized" reply and nothing changes. The action is recorded with
  that user as actor, then the message is updated through the payload's
  `response_url` (accepted only under `https://hooks.slack.com/`).
  Repeated clicks are idempotent.

### Tickets (GitHub Issues, Linear, Jira)

- `ticket_integrations` per project (admin): `github` (repository
  `owner/name` + token), `linear` (team id + API key), `jira` (https base
  URL, project key, issue type, account email + API token). Tokens are
  encrypted with `NAMI_ENCRYPTION_KEY` and write-only.
- "Create ticket" on an incident (member): title = incident title; body
  = cause, severity, metadata and a link to the incident, as plain text
  (GitHub/Linear markdown and `@` mentions escaped; Jira gets an ADF
  document of text nodes). Stored in `incident_tickets`, one per incident
  and integration (a second create → 409 with the existing link), plus a
  `ticket_created` event. The returned URL is stored only if it is https
  on the expected host (github.com, linear.app, the Jira base URL host).
  10 s timeout, no automatic retry; errors are shown to the user.
- Requests use the shared outbound HTTP client, which Platform +
  hardening later puts under its outbound policy.

### Reporting

- `GET /projects/{id}/reports/incidents?from&to` (viewer; range <= 3
  years) over incidents started in `[from, to)`: total; MTTA = mean of
  `acknowledged_at - started_at` over acknowledged ones; MTTR = mean of
  `resolved_at - started_at` over resolved ones; counts with MTTA/MTTR
  per source, per severity and per responder (the acknowledging user,
  else the resolving user; automatic resolutions count as "automatic").
- `GET /projects/{id}/reports/sla?from&to&monitor_id=…&heartbeat_id=…`
  (repeatable, max 100 resources): per resource the uptime % and
  downtime from Uptime Phase 3's availability function; `average` = mean
  of the resources' uptime %; `compound` = share of the range in which
  all selected resources were up at once (1 - union of their downtime
  intervals / range, maintenance excluded the same way).
- Both accept `format=csv` (RFC 4180, formula-injection guard as in
  Logs export). Ids from another project → 404.

### Routing

`alerts::notify_transition` becomes: down/alerting transition → open
incident with the source's project default policy (or the monitor's
assigned policy — monitors, heartbeats and log alerts gain an optional
`escalation_policy_id` and `severity_id`) and start escalation; recovery →
resolve incident + resolved notifications. Direct emailing from the uptime
worker goes away. Integrations and the report form open incidents with
their own policy and severity (else the project defaults).

### RBAC

Viewers read incidents, policies, schedules, reports. Members create/ack/
resolve/reopen/comment/edit postmortems, change severity and metadata,
create tickets, and take over shifts for themselves. Admins manage
policies, schedules, overrides, project channels, severities,
integrations, the report form, Slack interactivity and ticket
integrations. Every user manages their own personal channels, reminder
opt-out and ICS token.

### UI

- Incidents (project): status tabs, source, started, duration,
  acknowledged by; detail page with timeline, comments, ack/resolve/reopen,
  postmortem editor (markdown rendered safely — no raw HTML).
- Incident list filters (status, source, severity, date range, metadata),
  severity badges, metadata panel with edit, reporter details on form
  incidents, heartbeat output (`details`, Uptime Phase 3) shown escaped,
  "Create ticket" with existing ticket links.
- Escalation policies list + step editor (time-window condition,
  fallback policy).
- On-call schedules: list, editor (working-hours restriction),
  next-14-days shift list with "take over", overrides, "on call now" card
  on the dashboard. Account settings: reminder opt-out, ICS link with
  regenerate.
- Notification channels: personal (account settings) and project
  (project settings), each with "send test".
- Project settings: severities, integrations (create, copy URL, rotate,
  send test, last received), report form, Slack interactivity, ticket
  integrations.
- Reports: date range, MTTA/MTTR tiles, breakdown tables, SLA table with
  compound and average rows, CSV export.
- Public-ish `/incidents/action` confirmation page and public
  `/report/[token]` form.

### Data model additions

```sql
create table incident_severities (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    rank int not null,
    is_default boolean not null default false,
    unique (project_id, name),
    unique (project_id, rank)
);
create unique index on incident_severities (project_id) where is_default;

create table incident_integrations (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    kind text not null check (kind in ('webhook', 'alertmanager', 'grafana')),
    token_hash bytea not null unique,
    token_enc bytea not null,
    escalation_policy_id uuid,          -- fk to the policies table
    severity_id uuid references incident_severities(id) on delete set null,
    enabled boolean not null default true,
    last_received_at timestamptz,
    created_at timestamptz not null default now()
);

create table incident_report_forms (
    project_id uuid primary key references projects(id) on delete cascade,
    token_hash bytea not null unique,
    token_enc bytea not null,
    enabled boolean not null default false,
    intro text,
    escalation_policy_id uuid,
    severity_id uuid references incident_severities(id) on delete set null
);

alter table incidents
    add column severity_id uuid references incident_severities(id),
    add column metadata jsonb not null default '{}',
    add column integration_id uuid
        references incident_integrations(id) on delete set null,
    add column dedup_key text,
    add column reporter_name text,
    add column reporter_contact text;
create unique index on incidents (integration_id, dedup_key)
    where status <> 'resolved' and dedup_key is not null;
create index on incidents using gin (metadata jsonb_path_ops);

create table ticket_integrations (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    kind text not null check (kind in ('github', 'linear', 'jira')),
    config jsonb not null,              -- non-secret fields
    secret_enc bytea not null,          -- API token
    created_at timestamptz not null default now()
);

create table incident_tickets (
    incident_id uuid not null references incidents(id) on delete cascade,
    ticket_integration_id uuid not null
        references ticket_integrations(id) on delete cascade,
    external_id text not null,
    url text not null,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    primary key (incident_id, ticket_integration_id)
);

create table slack_app_configs (
    project_id uuid primary key references projects(id) on delete cascade,
    signing_secret_enc bytea not null,
    bot_token_enc bytea not null
);

create table on_call_shift_reminders (
    schedule_id uuid not null,          -- fk to the schedules table
    user_id uuid not null references users(id) on delete cascade,
    shift_start timestamptz not null,
    sent_at timestamptz not null default now(),
    primary key (schedule_id, user_id, shift_start)
);

alter table users
    add column shift_reminders boolean not null default true,
    add column ics_token_hash bytea unique;
```

Also: policy steps gain `condition jsonb`, policies gain
`fallback_policy_id`, schedules gain `restrictions jsonb not null default
'[]'`, overrides gain `created_by`, and monitors, heartbeats, log alerts
and (later) metric alerts gain `severity_id`. Policy/schedule table names
are fixed by the plan.

## Security requirements (tested)

- RBAC on every endpoint; cross-project ids → 404.
- Action tokens: tampered, expired, wrong action, other incident → rejected;
  GET never mutates.
- Secrets encrypted at rest and absent from responses/logs/timeline.
- Formatter injection tests per channel (mentions, markup, HTML).
- Webhook signature verifiable by receivers; delivery idempotency.
- Postmortem markdown renders without executing HTML/JS.
- Integration, report-form and ICS tokens: unknown, rotated or disabled
  → 404; hashed at rest; redacted from request logs (path segment →
  `[token]`), like heartbeat tokens.
- Integration payloads: > 1 MiB → 413; malformed JSON or wrong shapes →
  400 without panics; metadata caps enforced; > 100 alerts truncated and
  counted; concurrent duplicate deliveries open exactly one incident.
- Report form: honeypot submissions create nothing and look identical;
  per-IP and per-form limits → 429; reporter text renders escaped in the
  UI, in every channel formatter (Slack, Teams, Discord, Telegram, email)
  and in ticket bodies — tested with mention, markup, link and HTML
  payloads.
- Slack: missing or wrong signature, timestamp older than 5 min (replay)
  or a body altered after signing → 401; a valid click from a Slack user
  who maps to no member (or only a viewer) changes nothing; an incident
  from another project is rejected.
- Tickets: tokens encrypted and absent from responses, logs and the
  timeline; stored ticket URLs are https on the expected host; `@team`
  in an incident title mentions nobody on GitHub.
- ICS: only the token owner's shifts, only in projects they still belong
  to; schedule names containing CR/LF, `;` or `,` cannot inject
  properties or events.
- Take over: viewers → 403; a member cannot create an override for
  someone else.
- Time-window conditions and fallback: DST-transition and
  midnight-spanning cases; a fallback chain never loops.

## Non-goals

SMS/phone/push, Google Calendar sync (ICS import), incident merging,
status-page publishing (Status Page sub-project), PagerDuty/Opsgenie
integrations, inbound email incidents (needs an SMTP listener or IMAP
poller; the webhook integration and the report form cover the need).
Also not included: comment attachments, runbook steps, alert screening
(silent/discard) steps, metadata-based step branching, the Slack
`/incident` command and incident channels, Teams interactivity,
primary/backup schedule pairs, personal per-severity notification
overrides.
