# Incident Management + On-call

## Context

Fourth sub-project (roadmap §4), mirroring BetterStack Incident management
and On-call. Builds on the `incidents` table and `alerts::notify_transition`
entry point from Uptime Phase 1 (extended by Logs for `log_alert_id`), the
worker loop, `crypto.rs` (AES-GCM) from Uptime Phase 2, and RBAC.

## Goals

### Incident lifecycle

- Status: `open` → `acknowledged` → `resolved` (resolve allowed from either
  open state; `reopen` from resolved returns to `open` and restarts
  escalation). Columns added: `acknowledged_at`, `acknowledged_by`,
  `resolved_by` (null = automatic), `source` (`monitor` | `heartbeat` |
  `log_alert` | `metric_alert` | `manual`), `escalation_policy_id`,
  `escalation_step`, `escalation_repeat`, `next_escalation_at`,
  `postmortem` (markdown, max 100 KiB).
- Manual incidents: members create with title, description, optional
  policy (default = project default policy).
- Timeline `incident_events`: `opened`, `acknowledged`, `resolved`,
  `reopened`, `escalated`, `notification_sent`, `notification_failed`,
  `comment`, `postmortem_updated` — actor (nullable), message, metadata
  JSON, timestamp. Comments: members post, max 10 KiB.

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

### On-call schedules

- Per project: name, IANA timezone, ordered participants, rotation length
  (`daily`, `weekly`, or custom hours), handoff time (local `HH:MM`), start
  date. Overrides: user + `starts_at`/`ends_at`, newest wins on overlap.
- `on_call_at(schedule, instant)` is a pure function (tested across DST
  transitions); API exposes "on call now" and the next 14 days of shifts.

### Notification channels

- Kinds: `email`, `webhook`, `slack`, `telegram`, `discord`.
- Personal channels (per user: their own email is implicit; plus any other
  kinds) and project channels (team rooms, admin-managed).
- Config (webhook URLs, Slack/Discord webhook URLs, Telegram bot token +
  chat id) encrypted at rest with AES-GCM; API never returns secrets
  (write-only, like Uptime Phase 2 headers). "Send test" action per channel.
- Formatting with escaping (tested): Slack — escape `&`, `<`, `>` so
  `<!channel>`/`<@U…>` cannot be injected; Discord — `allowed_mentions:
  {"parse": []}` and escape markdown; Telegram — HTML parse mode with
  `&`/`<`/`>` escaped; webhook — JSON body
  `{event, incident:{id,title,cause,status,source,started_at,url}, project:{id,name}}`
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
step / repeat / end. A separate tick drains due deliveries.

### Acknowledge / resolve from notifications

Notifications carry a link to a nami-ui confirmation page
(`/incidents/action?token=…`) — never a state-changing GET, so email
scanners and link previews cannot acknowledge anything. The page shows the
incident and a button that POSTs the token. Tokens: HMAC-SHA256 over
`(incident_id, action, user_id, exp)` with a key derived from
`NAMI_ENCRYPTION_KEY`, expiring after 24 h; the POST works without a
session (the token is the credential) and records the token's user as the
actor.

### Routing

`alerts::notify_transition` becomes: down/alerting transition → open
incident with the source's project default policy (or the monitor's
assigned policy — monitors, heartbeats and log alerts gain an optional
`escalation_policy_id`) and start escalation; recovery → resolve incident
+ resolved notifications. Direct emailing from the uptime worker goes away.

### RBAC

Viewers read incidents, policies, schedules. Members create/ack/resolve/
reopen/comment/edit postmortems. Admins manage policies, schedules,
overrides and project channels. Every user manages their own personal
channels.

### UI

- Incidents (project): status tabs, source, started, duration,
  acknowledged by; detail page with timeline, comments, ack/resolve/reopen,
  postmortem editor (markdown rendered safely — no raw HTML).
- Escalation policies list + step editor.
- On-call schedules: list, editor, next-14-days shift list, overrides,
  "on call now" card on the dashboard.
- Notification channels: personal (account settings) and project
  (project settings), each with "send test".
- Public-ish `/incidents/action` confirmation page.

## Security requirements (tested)

- RBAC on every endpoint; cross-project ids → 404.
- Action tokens: tampered, expired, wrong action, other incident → rejected;
  GET never mutates.
- Secrets encrypted at rest and absent from responses/logs/timeline.
- Formatter injection tests per channel (mentions, markup, HTML).
- Webhook signature verifiable by receivers; delivery idempotency.
- Postmortem markdown renders without executing HTML/JS.

## Non-goals

SMS/phone/push, Google Calendar sync, incident merging, status-page
publishing (Status Page sub-project), PagerDuty/Opsgenie integrations.
