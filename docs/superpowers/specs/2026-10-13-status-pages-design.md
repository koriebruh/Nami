# Status Pages

> Parity additions (2026-10-13): component-level email subscriptions, webhook subscribers, public JSON status API, per-update "notify subscribers" toggle, automatic reports, theme + navigation links + dark-mode logo, embeddable announcement widget, `noindex` toggle, IP allowlist for private pages.

## Context

Fifth sub-project (roadmap §5), mirroring BetterStack Status pages. Reads
monitors, heartbeats, incidents and maintenance windows (Uptime Phases 1-2,
Incident Management) and reuses `crypto.rs`, argon2, Mailer and the
delivery queue.

## Goals

### Pages, sections, components

- `status_pages`: project, `slug` (globally unique, `^[a-z0-9-]{3,63}$`),
  title, description, `logo_url` (https only), `logo_dark_url` (https
  only, used in dark mode), `accent_color` (`#rrggbb`), `theme` (`light` |
  `dark` | `system`), `nav_links` (<= 4 `{label <= 40 chars, url}`, url
  `https:` or `mailto:` only), `visibility` (`public` | `private`),
  `password_hash` (argon2, private only), `ip_allowlist` (private only,
  <= 50 CIDRs), optional `custom_domain` (unique hostname; nami-ui routes
  by Host header — DNS/TLS for it are the operator's job), `timezone`,
  `noindex`, `automatic_reports` + `automatic_reports_notify`,
  `allow_webhook_subscribers`.
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
  safely, no raw HTML), timestamp, `notify_subscribers` (default true;
  the same toggle is offered when a report is created). Posting an update
  moves the report's status; with the toggle off no email or webhook
  goes out for it.
- Members create/edit/post updates; admins delete.

### Automatic reports

With `automatic_reports` on, an incident opened for a monitor/heartbeat
bound to any of the page's components creates one status report (unique
per page and incident via `status_reports.incident_id`): kind `incident`,
status `investigating`, impact `major_outage` on every component bound to
that monitor/heartbeat, title "`<component display names>` — service
disruption", body from a fixed template. Recovery posts a `resolved`
update. The text uses component display names only — never the monitor's
name, target, cause or ids. Notifications follow
`automatic_reports_notify` (default on). Members can edit the report and
post further updates; resolving it by hand ends automatic handling for
that report.

### Subscribers

- Email subscription with double opt-in: subscribe → confirmation email →
  confirmation page with a button that POSTs (never confirm on GET).
- Component-level: a subscriber picks all components or a subset, at
  subscribe time and later on a "manage subscription" page reached
  through a single-purpose token in every email (changes POST). A report
  reaches a subset subscriber only if it affects one of their
  components; a report without affected components reaches everyone.
- Every new report and update (unless its notify toggle is off) emails
  confirmed subscribers through the durable delivery queue; emails carry
  `List-Unsubscribe` + `List-Unsubscribe-Post: List-Unsubscribe=One-Click`
  and an unsubscribe page. Subscribe endpoint rate-limited per IP
  (5/hour) and per page.
- Webhook subscribers (public form when `allow_webhook_subscribers` is
  on; admins can always add): https URLs only, delivered in strict
  outbound mode — public unicast addresses only whatever the operator's
  outbound setting, checked after DNS resolution, redirects not
  followed. Activation needs a handshake: Nami POSTs `{"type":
  "verification", "challenge": …}` and the endpoint must echo
  `challenge` within 10 s; nothing else is delivered before that.
  Events `{event_id, event: report.created | report.updated |
  component.status_changed, page: {slug, title, url}, report?, update?,
  component?, sent_at, unsubscribe_url}` are signed `X-Nami-Signature:
  sha256=<HMAC(secret, body)>` with a per-subscriber secret shown once,
  and go through the same delivery queue and retries as emails; 20
  consecutive failed deliveries disable the subscriber. Same subscribe
  rate limits as email.
- Admin view lists subscribers (email or webhook host, components,
  confirmed, created) with remove.

### Private pages

Access is granted by a valid password cookie or by a client IP inside
`ip_allowlist` (either suffices; a private page needs at least one of
the two configured). Password form → argon2 verify → HMAC-signed,
HttpOnly cookie scoped to the page (30 days). Rate limited (10 attempts /
15 min / IP / page). The client IP is SvelteKit's `getClientAddress()`
(adapter-node `ADDRESS_HEADER` / `XFF_DEPTH` set for the deployment's
reverse proxy), passed to the engine by the BFF; a client-supplied
`X-Forwarded-For` never widens access. Private page content, feed, badge
and JSON all require it; private pages are always `noindex` and have no
widget or cross-origin JSON.

### Feed and badge

- `/status/{slug}/feed.atom` — Atom feed of reports and updates (XML-escaped).
- `/status/{slug}/badge.svg` — shields-style badge (overall status), text
  escaped; `Cache-Control: max-age=60`.

### Public JSON API and announcement widget

- `/status/{slug}/status.json` (nami-ui, built from the engine DTO):
  `{version: 1, page: {name, url, updated_at}, status, components:
  [{name, section, status}], active_reports: [{title, kind, status,
  impact, updated_at, latest_update}], upcoming_maintenance: [...]}` —
  only fields the public DTO already exposes.
  `Access-Control-Allow-Origin: *`, no credentials,
  `Cache-Control: max-age=30`. Public pages only.
- Widget: `<script src="https://<host>/status/{slug}/widget.js" async>
  </script>` on the customer's site shows a small banner while an
  incident or maintenance is active, linking to the page. It fetches
  `status.json` with `credentials: "omit"`, sets no cookies or storage,
  uses no `eval`, `new Function` or inline scripts, writes text with
  `textContent` only, and styles a Shadow DOM through constructable
  stylesheets, so it runs under a strict CSP that allows the status host
  in `script-src` and `connect-src`. Size budget 5 KiB gzip.
- `noindex` adds `<meta name="robots" content="noindex">` and
  `X-Robots-Tag: noindex` to the page, feed and JSON.

### Public API (engine, no session)

`GET /public/status-pages/{slug}` (and by custom domain) returns a
purpose-built DTO: page branding (incl. theme, dark logo, navigation
links), sections, component names/status/bars, active and recent
reports. It never includes monitor URLs/hosts/ports, keywords, project or
monitor ids, member emails, or subscriber data. Cacheable for 30 s.
Unknown, or private without a valid cookie or allowlisted IP → 404 (no
existence oracle for private slugs).

### UI

- Admin (project section "Status pages"): list, editor (settings,
  branding incl. theme, dark logo and navigation links, visibility with
  password and/or IP allowlist, `noindex`, custom domain, automatic
  reports, webhook subscribers toggle, sections and components with
  up/down ordering, widget snippet), reports composer with the "notify
  subscribers" toggle + update timeline, subscribers (email and webhook).
- Public routes outside the `(app)` group: `/status/[slug]`,
  confirmation/unsubscribe/manage-subscription pages, password form,
  feed/badge/JSON/widget routes; `hooks.server.ts` maps a custom-domain
  Host to its page. BetterStack-like look: banner, navigation links,
  component list with 90 bars, incident history, subscribe form (email
  with component picker, webhook when enabled), "Powered by Nami" footer,
  light/dark per the page theme.

### Data model additions

```sql
alter table status_pages
    add column logo_dark_url text,
    add column theme text not null default 'system'
        check (theme in ('light', 'dark', 'system')),
    add column nav_links jsonb not null default '[]',
    add column noindex boolean not null default false,
    add column ip_allowlist cidr[] not null default '{}',
    add column automatic_reports boolean not null default false,
    add column automatic_reports_notify boolean not null default true,
    add column allow_webhook_subscribers boolean not null default false;

alter table status_reports
    add column incident_id uuid references incidents(id) on delete set null;
create unique index on status_reports (status_page_id, incident_id)
    where incident_id is not null;

alter table status_report_updates
    add column notify_subscribers boolean not null default true;

-- no rows for a subscriber = all components
create table status_page_subscriber_components (
    subscriber_id uuid not null,        -- fk to the email subscriber table
    component_id uuid not null
        references status_page_components(id) on delete cascade,
    primary key (subscriber_id, component_id)
);

create table status_page_webhook_subscribers (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null references status_pages(id) on delete cascade,
    url text not null,
    secret_enc bytea not null,
    verified_at timestamptz,            -- challenge echoed
    consecutive_failures int not null default 0,
    disabled_at timestamptz,
    created_at timestamptz not null default now()
);
```

## Security requirements (tested)

- Public DTO leak test: response contains no target URL/host/keyword/id of
  any bound monitor even when they are set.
- Private page: wrong password, missing/tampered cookie, other page's
  cookie → no content; brute-force limit → 429.
- Subscription: confirm/unsubscribe tokens single-purpose and hashed at
  rest; GET never mutates; subscribe rate limit.
- Markdown/XML/SVG output escaping (script tags, `]]>`, quotes).
- Slug/custom-domain uniqueness and validation; cross-project ids → 404.
- Webhook subscribers: URLs resolving to loopback, private, link-local
  (incl. 169.254.169.254) or other non-public ranges are rejected at
  subscribe time and at every delivery (connect to the vetted IP — DNS
  rebinding safe); `http://` and redirects refused; no event before the
  challenge is echoed; signatures verify; secrets encrypted and never
  returned after creation; auto-disable after 20 failures.
- JSON API, widget data and automatic reports pass the DTO leak test (no
  monitor names, targets, causes or ids).
- Widget: report titles containing HTML/script render as text (DOM
  test); no cookie is set or sent.
- Navigation links: `javascript:`, `data:` and `http:` URLs → 400.
- IP allowlist: a spoofed `X-Forwarded-For` grants nothing; IPv4, IPv6
  and CIDR edge cases.
- Manage-subscription tokens single-purpose; GET never mutates.
- `noindex` meta and header present when on, and always on private
  pages.

## Non-goals

SMS/Slack subscribers, multi-language pages, custom CSS/JS injection,
Google Analytics or other third-party scripts, automatic TLS for custom
domains, SSO-gated pages, page groups, response-time chart widgets.
