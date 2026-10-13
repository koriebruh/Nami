# Status Pages

## Context

Fifth sub-project (roadmap §5), mirroring BetterStack Status pages. Reads
monitors, heartbeats, incidents and maintenance windows (Uptime Phases 1-2,
Incident Management) and reuses `crypto.rs`, argon2, Mailer and the
delivery queue.

## Goals

### Pages, sections, components

- `status_pages`: project, `slug` (globally unique, `^[a-z0-9-]{3,63}$`),
  title, description, `logo_url` (https only), `accent_color` (`#rrggbb`),
  `visibility` (`public` | `private`), `password_hash` (argon2, private
  only), optional `custom_domain` (unique hostname; nami-ui routes by Host
  header — DNS/TLS for it are the operator's job), `timezone`.
- `status_page_sections` (name, position) and `status_page_components`
  (section, display name, description, exactly one of monitor/heartbeat,
  position, `show_uptime_bars`).
- Component status: `operational`, `degraded`, `partial_outage`,
  `major_outage`, `maintenance`, `no_data` — derived from the bound
  monitor/heartbeat (`up` → operational, `down` → major outage, active
  maintenance window → maintenance, `pending` → no data), overridable by
  the impact set in an active status report.
- Overall banner = worst component status.

### 90-day history

Per component, daily uptime % for the last 90 days computed from incident
durations overlapping each day (in the page's timezone), so monitors and
heartbeats are treated identically; each bar's tooltip shows the date,
uptime %, and that day's incidents. Days before the component existed are
`no_data`.

### Status reports

- `status_reports`: page, title, `kind` (`incident` | `maintenance`),
  current status (`investigating` | `identified` | `monitoring` |
  `resolved`; maintenance: `scheduled` | `in_progress` | `completed`),
  affected components with impact level, `starts_at`/`ends_at` for
  maintenance, `published_at`.
- `status_report_updates`: report, status, markdown message (rendered
  safely, no raw HTML), timestamp. Posting an update moves the report's
  status.
- Members create/edit/post updates; admins delete.

### Subscribers

- Email subscription with double opt-in: subscribe → confirmation email →
  confirmation page with a button that POSTs (never confirm on GET).
- Every new report and update emails confirmed subscribers through the
  durable delivery queue; emails carry `List-Unsubscribe` +
  `List-Unsubscribe-Post: List-Unsubscribe=One-Click` and an unsubscribe
  page. Subscribe endpoint rate-limited per IP (5/hour) and per page.
- Admin view lists subscribers (email, confirmed, created) with remove.

### Private pages

Password form → argon2 verify → HMAC-signed, HttpOnly cookie scoped to the
page (30 days). Rate limited (10 attempts / 15 min / IP / page). Private
page content, feed and badge all require it.

### Feed and badge

- `/status/{slug}/feed.atom` — Atom feed of reports and updates (XML-escaped).
- `/status/{slug}/badge.svg` — shields-style badge (overall status), text
  escaped; `Cache-Control: max-age=60`.

### Public API (engine, no session)

`GET /public/status-pages/{slug}` (and by custom domain) returns a
purpose-built DTO: page branding, sections, component names/status/bars,
active and recent reports. It never includes monitor URLs/hosts/ports,
keywords, project or monitor ids, member emails, or subscriber data.
Cacheable for 30 s. Unknown or private-without-cookie → 404 (no
existence oracle for private slugs).

### UI

- Admin (project section "Status pages"): list, editor (settings,
  branding, visibility/password, custom domain, sections and components
  with up/down ordering), reports composer + update timeline, subscribers.
- Public routes outside the `(app)` group: `/status/[slug]`,
  confirmation/unsubscribe pages, password form, feed/badge proxies;
  `hooks.server.ts` maps a custom-domain Host to its page. BetterStack-like
  look: banner, component list with 90 bars, incident history, subscribe
  form, "Powered by Nami" footer.

## Security requirements (tested)

- Public DTO leak test: response contains no target URL/host/keyword/id of
  any bound monitor even when they are set.
- Private page: wrong password, missing/tampered cookie, other page's
  cookie → no content; brute-force limit → 429.
- Subscription: confirm/unsubscribe tokens single-purpose and hashed at
  rest; GET never mutates; subscribe rate limit.
- Markdown/XML/SVG output escaping (script tags, `]]>`, quotes).
- Slug/custom-domain uniqueness and validation; cross-project ids → 404.

## Non-goals

SMS/Slack subscribers, multi-language pages, custom CSS/JS injection,
automatic TLS for custom domains.
