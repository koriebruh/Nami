# Nami Roadmap — BetterStack Feature Parity

Definition of done for the "rebuild BetterStack" goal. Each sub-project gets
its own spec → plan → per-task PRs (see `CLAUDE.md`). Every sub-project ends
with a dedicated security review + fix wave and a whole-sub-project review;
the final sub-project is a production-hardening pass.

Out of scope on purpose (needs paid third-party telephony or global
infrastructure): SMS and phone-call alerts, multi-region checking,
Playwright browser checks, Terraform provider.

Also out of scope (ruled 2026-10-13):
- Inbound email incidents — needs an SMTP listener or IMAP poller; the
  webhook integration and the public report form cover the need.
- Grafana dashboard import and PromQL — a second query language to
  emulate; Nami's query API and dashboard templates cover the use.
- SAML SSO and SCIM provisioning — generic OIDC covers Keycloak,
  Authentik and Entra ID at single-org scale.
- Request phase timing (DNS/connect/TLS/first byte) — reqwest/hyper
  expose no per-phase timings; would need a custom connector stack.
- Traceroute/MTR — needs raw-socket privileges and adds little from a
  single vantage point.
- Autonomous AI remediation and AI-created pull requests — AI SRE is
  read-only; every action needs a human click.
- Google Analytics and custom JS on status pages — third-party scripts
  break the CSP and no-cookie posture.

## 1. Foundation — done (PRs #1-#16)

- [x] Email/password registration with verification, login, logout
- [x] Server-side sessions (Postgres), session fixation protection
- [x] TOTP 2FA, password reset with session invalidation
- [x] Google/GitHub OAuth (BFF passthrough)
- [x] Two-level RBAC (global role + per-project role), projects, members
- [x] Rate limiting on auth endpoints (per client IP)
- [x] Full UI for all of the above

## 2. Uptime monitoring — in progress

### Phase 1 — done (PRs #17-#27, security fix wave #30; decisions: `docs/superpowers/decisions/2026-10-13-uptime-phase1.md`)

- [x] Monitor types: HTTP status, keyword / keyword absence, TCP port
- [x] Interval (>= 30 s), timeout, confirmation threshold, pause/resume
- [x] Heartbeats (cron monitoring) with period + grace
- [x] Automatic incidents (open on down, resolve on recovery)
- [x] Email alerts on transitions
- [x] Uptime % (24 h / 7 d / 30 d), response-time history chart, check log
- [x] UI for all of the above, auto-refresh

### Phase 2 — done (PRs #28, #33-#35, #37, #40, #41, #44, #46-#48, security fix wave #58; decisions: `docs/superpowers/decisions/2026-10-13-uptime-phase2.md`)

- [x] Monitor types: ping (ICMP), DNS record, SSL certificate expiry, domain expiry
- [x] Monitor types: SMTP, POP3, IMAP, UDP
- [x] Request config: method, custom headers, body, basic auth, follow
      redirects toggle, SSL-verification toggle, expected status range
- [x] Recovery threshold
- [x] Maintenance windows (suppress incidents/alerts during a window)
- [x] Recurring maintenance windows
- [x] Monitor groups
- [x] UI for all of the above

### Phase 3

- [x] Heartbeat `/fail` endpoint and exit-code path segment (0 = success)
- [x] Heartbeat output capture (<= 10 KiB, sanitized, attached to the incident)
- [x] Heartbeat groups (shared with monitor groups)
- [ ] Monitor bulk import: URL list (<= 500, per-line results), Uptime
      Kuma JSON, UptimeRobot CSV
- [x] Advanced request options: IP version (any/IPv4/IPv6), remember
      cookies within a check, HTTP(S) proxy with encrypted credentials
- [x] Availability (SLA) report for a monitor over an arbitrary range;
      check-history CSV export
- [x] UI for all of the above

## 3. Log management

### Part 1 — done (PRs #29-#45, security fix wave #50; decisions: `docs/superpowers/decisions/2026-10-13-log-management-part1.md`)

- [x] Sources per project with ingest tokens (create, rotate, delete)
- [x] HTTP ingestion: JSON object, JSON array, NDJSON; size limits;
      per-source rate limiting
- [x] MessagePack ingestion (`application/msgpack`)
- [x] Syslog ingestion (RFC 5424 over TCP/UDP)
- [ ] Per-source ingest pipeline: drop, remove fields, redact, rename,
      sample (1 in N)
- [x] ClickHouse storage with per-source retention (TTL)
- [x] Search: time range, level, full-text, structured field filters,
      pagination
- [x] Field filter operators: regex, not_regex, gt/gte/lt/lte
- [ ] Message pattern grouping + explorer Patterns tab
- [x] `trace_id` / `span_id` columns (trace linking)
- [x] Shareable explorer URLs, column picker
- [ ] Streamed CSV/NDJSON export (capped at 100k rows)
- [x] Per-source ingestion stats (events/bytes per day) and optional
      daily byte quota (429 `quota_exceeded`)
- [x] Live tail (SSE through the BFF)
- [ ] Saved views
- [ ] Log alerts (threshold over a window → incident)
- [ ] UI: sources, explorer, live tail, saved views, alerts

## 4. Incident management + on-call

- [ ] Incident lifecycle: open → acknowledged → resolved; manual incidents
- [ ] Incident timeline (events, comments), postmortem notes
- [ ] Escalation policies (multi-step, delays, repeat) per project
- [ ] Escalation step time-window conditions and fallback policy
- [ ] On-call schedules: rotations, handoff, overrides, "who's on call"
- [ ] On-call working-hours restriction, shift reminder emails (24 h
      ahead), per-user ICS feed, "take over shift"
- [ ] Notification channels: email, generic webhook, Slack, Telegram,
      Discord; per-user and per-policy targets
- [ ] Microsoft Teams channel (Adaptive Cards)
- [ ] Acknowledge/resolve from email links (signed, expiring tokens)
- [ ] Slack Acknowledge/Resolve buttons (signed requests, optional per project)
- [ ] Monitors, heartbeats and log alerts route through escalation policies
- [ ] Incoming integrations: generic JSON webhook, Prometheus
      Alertmanager, Grafana alerting (secret URL, dedup, auto-resolve)
- [ ] Public incident report form (secret URL, per-IP rate limit, honeypot)
- [ ] Severities (per-project list with default) and incident metadata
- [ ] Incident list filters (status, source, severity, date range, metadata)
- [ ] Reporting: MTTA, MTTR, counts by source/severity/responder, SLA
      report (compound + average), CSV export
- [ ] Create GitHub / Linear / Jira tickets from an incident
- [ ] UI for all of the above

## 5. Status pages

- [ ] Public status page per slug (no auth), custom branding (name, logo
      URL, accent color)
- [ ] Theme (light/dark/system), up to 4 navigation links, dark-mode logo
- [ ] Components and sections bound to monitors/heartbeats
- [ ] 90-day uptime history bars, overall status banner
- [ ] Status updates (investigating / identified / monitoring / resolved)
      and scheduled maintenance announcements
- [ ] Per-update "notify subscribers" toggle
- [ ] Automatic reports from bound monitor/heartbeat incidents (per page)
- [ ] Email subscribers (double opt-in, unsubscribe)
- [ ] Component-level email subscriptions
- [ ] Webhook subscribers (signed payloads, retries)
- [ ] Password-protected (private) pages
- [ ] IP allowlist for private pages; `noindex` toggle
- [ ] RSS/Atom feed, embeddable status badge (SVG)
- [ ] Public JSON status API
- [ ] Embeddable announcement widget (CSP-safe JS, no cookies)
- [ ] UI: editor + public page

## 6. Metrics + dashboards

- [ ] Metric ingestion (JSON, Prometheus text format) with source tokens
- [ ] Prometheus scrape targets (pull) with target health
- [ ] ClickHouse storage, retention
- [ ] Query API: aggregations (avg/min/max/sum/count/p50/p95/p99) over
      time buckets, group-by tags
- [ ] Dashboards with charts (line, bar, stat), layout, time range
- [ ] Dashboard variables (tag-value dropdowns)
- [ ] Markdown text panels
- [ ] Dashboard template library: host (node_exporter), Postgres, Redis,
      Nginx, Docker/cAdvisor
- [ ] Read-only public share link per dashboard (revocable)
- [ ] Built-in metrics from uptime checks and log volume
- [ ] Metric alerts (threshold → incident)
- [ ] Alert improvements (metric + log): no-data handling,
      confirmation/recovery periods, per-series incidents
- [ ] Collector bundle: OpenTelemetry Collector + Vector configs and
      compose snippets, surfaced on UI setup pages
- [ ] UI for all of the above

## 7. Traces (OpenTelemetry)

- [ ] OTLP/HTTP (JSON + protobuf) trace ingestion with source tokens
- [ ] ClickHouse span storage, retention
- [ ] Trace search (service, operation, duration, status, time range)
- [ ] Trace waterfall view
- [ ] Trace-to-log linking both ways via `trace_id`
- [ ] Apdex per service (configurable threshold)

## 8. Error tracking (Sentry-compatible)

- [ ] Applications with DSNs; Sentry envelope + store endpoints
      (gzip/deflate/br, size limits, unknown items skipped)
- [ ] Event normalization (exception chain, frames, breadcrumbs, tags,
      contexts, user, release, environment, SDK) with data scrubbing
- [ ] Grouping (SDK fingerprint, else type + normalized message + top
      in-app frames)
- [ ] Issues: first/last seen, counts, affected users, resolve, ignore
      (until N occurrences / until date), regression on a new release
- [ ] Release tracking
- [ ] Spike protection per DSN (rate limit + daily cap)
- [ ] JS source map upload (upload token) + server-side symbolication
- [ ] Alert rules (new issue, regression, frequency) → incidents
- [ ] ClickHouse `error_events` with retention
- [ ] UI: applications, issue list, issue detail (stack trace,
      breadcrumbs), releases, alert rules

## 9. Real user monitoring

- [ ] RUM applications with public client keys; JS SDK served by Nami
- [ ] Beacon ingest (allowed origins, size and rate limits)
- [ ] Page views, web vitals (LCP/CLS/INP/TTFB/FCP), custom events,
      user identify
- [ ] JS errors forwarded to error tracking
- [ ] Session replay (rrweb: sampling, input masking, chunked upload,
      retention, player)
- [ ] Website analytics (visits, cookieless unique visitors, referrers,
      UTM, pages, countries)
- [ ] Web vitals per URL (p75) with alerts
- [ ] Funnels
- [ ] Privacy: no cookies, no raw IPs, DNT option, masking defaults,
      per-user erasure
- [ ] UI for all of the above

## 10. AI SRE (bring your own LLM)

- [ ] Provider config: Anthropic (default `claude-sonnet-5-5`) or an
      OpenAI-compatible endpoint; key encrypted at rest
- [ ] "Explain with AI" on incidents, log lines and error issues
- [ ] Automatic investigation when an incident opens (labeled summary +
      suspected cause on the timeline)
- [ ] AI post-mortem drafts
- [ ] Project chat with read-only tools, scoped to the user's projects
- [ ] Daily token/cost budgets
- [ ] Prompt-injection posture (untrusted telemetry, read-only tools,
      human click for every action, sanitized output)
- [ ] Audit of every AI call

## 11. Platform + production hardening (last)

- [ ] API tokens + documented REST API for monitors/heartbeats/incidents
- [ ] REST API coverage for every configurable resource (OpenAPI)
- [ ] MCP server (streamable HTTP, PAT auth, tool allow/deny headers)
- [ ] Audit log of security-relevant and configuration changes
- [ ] Audit log forwarding (log source or webhook)
- [ ] Generic OIDC login (Keycloak, Authentik, Entra ID)
- [ ] Org-wide 2FA enforcement
- [ ] Invite by email for unregistered users
- [ ] Usage page (ingest volume per source, storage per table)
- [ ] Dockerfiles (engine + UI) and a production docker-compose
- [ ] CI (GitHub Actions): engine tests with service containers, UI check/test
- [ ] Security headers (CSP, HSTS, frame-ancestors), request body limits
- [ ] Close parked Foundation items: 2FA-change re-auth, timing-safe auth
      responses, trusted-proxy check for X-Forwarded-For, expired
      session/token reapers, per-user TOTP attempt limiting
- [ ] Dedicated full-system security review + penetration-style test pass
