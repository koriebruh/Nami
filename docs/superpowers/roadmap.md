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

## Plans and execution order

Every unticked item below is claimed by a task in one of these plans
(`python3 scripts/check-plans.py` proves it; see
`docs/superpowers/plan-conventions.md`). Phases run in this order, one
phase PR each.

| # | Phase | Plan | Branch | Status |
|---|---|---|---|---|
| 1 | Logs Part 2 | `plans/2026-10-13-log-management-part2.md` | `feat/logs-part2` | done |
| 2 | Incidents Part 1 | `plans/2026-10-13-incident-management-part1.md` | `feat/incidents-part1` | ready |
| 3 | Incidents Part 2 | `plans/2026-10-24-incident-management-part2.md` | `feat/incidents-part2` | outline |
| 4 | Status Pages Part 1 | `plans/2026-10-13-status-pages-part1.md` | `feat/statuspages-part1` | ready |
| 5 | Status Pages Part 2 | `plans/2026-10-24-status-pages-part2.md` | `feat/statuspages-part2` | outline |
| 6 | Metrics Part 1 | `plans/2026-10-13-metrics-dashboards-part1.md` | `feat/metrics-part1` | ready |
| 7 | Metrics Part 2 | `plans/2026-10-24-metrics-dashboards-part2.md` | `feat/metrics-part2` | outline |
| 8 | Traces | `plans/2026-10-24-traces-part1.md` | `feat/traces-part1` | outline |
| 9 | Error tracking Part 1 | `plans/2026-10-24-error-tracking-part1.md` | `feat/errors-part1` | outline |
| 10 | Error tracking Part 2 | `plans/2026-10-24-error-tracking-part2.md` | `feat/errors-part2` | outline |
| 11 | RUM Part 1 | `plans/2026-10-24-rum-part1.md` | `feat/rum-part1` | outline |
| 12 | RUM Part 2 | `plans/2026-10-24-rum-part2.md` | `feat/rum-part2` | outline |
| 13 | AI SRE | `plans/2026-10-24-ai-sre-part1.md` | `feat/ai-part1` | outline |
| 14 | Platform Part 1 | `plans/2026-10-24-platform-hardening-part1.md` | `feat/platform-part1` | outline |
| 15 | Platform Part 2 | `plans/2026-10-24-platform-hardening-part2.md` | `feat/platform-part2` | outline |

`outline` plans fix tasks, data shapes, endpoints, tests and acceptance;
their step-level code is written at phase start against the then-current
`main`.

## 1. Foundation — done (PRs #1-#16)

- [x] **R1.01** Email/password registration with verification, login, logout
- [x] **R1.02** Server-side sessions (Postgres), session fixation protection
- [x] **R1.03** TOTP 2FA, password reset with session invalidation
- [x] **R1.04** Google/GitHub OAuth (BFF passthrough)
- [x] **R1.05** Two-level RBAC (global role + per-project role), projects, members
- [x] **R1.06** Rate limiting on auth endpoints (per client IP)
- [x] **R1.07** Full UI for all of the above

## 2. Uptime monitoring — done (Phases 1-3)

### Phase 1 — done (PRs #17-#27, security fix wave #30; decisions: `docs/superpowers/decisions/2026-10-13-uptime-phase1.md`)

- [x] **R2.01** Monitor types: HTTP status, keyword / keyword absence, TCP port
- [x] **R2.02** Interval (>= 30 s), timeout, confirmation threshold, pause/resume
- [x] **R2.03** Heartbeats (cron monitoring) with period + grace
- [x] **R2.04** Automatic incidents (open on down, resolve on recovery)
- [x] **R2.05** Email alerts on transitions
- [x] **R2.06** Uptime % (24 h / 7 d / 30 d), response-time history chart, check log
- [x] **R2.07** UI for all of the above, auto-refresh

### Phase 2 — done (PRs #28, #33-#35, #37, #40, #41, #44, #46-#48, security fix wave #58; decisions: `docs/superpowers/decisions/2026-10-13-uptime-phase2.md`)

- [x] **R2.08** Monitor types: ping (ICMP), DNS record, SSL certificate expiry, domain expiry
- [x] **R2.09** Monitor types: SMTP, POP3, IMAP, UDP
- [x] **R2.10** Request config: method, custom headers, body, basic auth, follow
      redirects toggle, SSL-verification toggle, expected status range
- [x] **R2.11** Recovery threshold
- [x] **R2.12** Maintenance windows (suppress incidents/alerts during a window)
- [x] **R2.13** Recurring maintenance windows
- [x] **R2.14** Monitor groups
- [x] **R2.15** UI for all of the above

### Phase 3 — done (PRs #53, #55, #57, #60, #61, #64, #65, #68-#70, #72-#74, security fix wave #77; decisions: `docs/superpowers/decisions/2026-10-13-uptime-phase3.md`)

- [x] **R2.16** Heartbeat `/fail` endpoint and exit-code path segment (0 = success)
- [x] **R2.17** Heartbeat output capture (<= 10 KiB, sanitized, attached to the incident)
- [x] **R2.18** Heartbeat groups (shared with monitor groups)
- [x] **R2.19** Monitor bulk import: URL list (<= 500, per-line results), Uptime
      Kuma JSON, UptimeRobot CSV
- [x] **R2.20** Advanced request options: IP version (any/IPv4/IPv6), remember
      cookies within a check, HTTP(S) proxy with encrypted credentials
- [x] **R2.21** Availability (SLA) report for a monitor over an arbitrary range;
      check-history CSV export
- [x] **R2.22** UI for all of the above

## 3. Log management

### Part 1 — done (PRs #29-#45, security fix wave #50; decisions: `docs/superpowers/decisions/2026-10-13-log-management-part1.md`)

- [x] **R3.01** Sources per project with ingest tokens (create, rotate, delete)
- [x] **R3.02** HTTP ingestion: JSON object, JSON array, NDJSON; size limits;
      per-source rate limiting
- [x] **R3.03** MessagePack ingestion (`application/msgpack`)
- [x] **R3.04** Syslog ingestion (RFC 5424 over TCP/UDP)
- [x] **R3.05** ClickHouse storage with per-source retention (TTL)
- [x] **R3.06** Search: time range, level, full-text, structured field filters,
      pagination
- [x] **R3.07** Field filter operators: regex, not_regex, gt/gte/lt/lte
- [x] **R3.08** `trace_id` / `span_id` columns (trace linking)
- [x] **R3.09** Shareable explorer URLs, column picker
- [x] **R3.10** Per-source ingestion stats (events/bytes per day) and optional
      daily byte quota (429 `quota_exceeded`)
- [x] **R3.11** Live tail (SSE through the BFF)
- [x] **R3.12** UI: sources, explorer, live tail

### Part 2 — done (PRs #56, #63, #67, #75, #76 and the phase PR for NAMI-LOG-14..19; decisions: `docs/superpowers/decisions/2026-10-25-log-management-part2.md`)

- [x] **R3.13** Per-source ingest pipeline: drop, remove fields, redact, rename,
      sample (1 in N).
- [x] **R3.14** Message pattern grouping + explorer Patterns tab.
- [x] **R3.15** Streamed CSV/NDJSON export (capped at 100k rows).
- [x] **R3.16** Saved views.
- [x] **R3.17** Log alerts (threshold over a window → incident).
- [x] **R3.18** Live-tail resume across reconnects and the explorer fixes Part 1
      deferred.
- [x] **R3.19** 30-day usage chart per source.
- [x] **R3.20** UI: pipeline editor, Patterns tab, saved views, alerts.
- [x] **R3.21** Part 2 security review + fix wave.

## 4. Incident management + on-call

- [ ] **R4.01** Incident lifecycle: open → acknowledged → resolved; manual incidents
- [ ] **R4.02** Incident timeline (events, comments), postmortem notes
- [ ] **R4.03** Escalation policies (multi-step, delays, repeat) per project
- [ ] **R4.04** Escalation step time-window conditions and fallback policy
- [ ] **R4.05** On-call schedules: rotations, handoff, overrides, "who's on call"
- [ ] **R4.06** On-call working-hours restriction, shift reminder emails (24 h
      ahead), per-user ICS feed, "take over shift"
- [ ] **R4.07** Notification channels: email, generic webhook, Slack, Telegram,
      Discord; per-user and per-policy targets
- [ ] **R4.08** Microsoft Teams channel (Adaptive Cards)
- [ ] **R4.09** Acknowledge/resolve from email links (signed, expiring tokens)
- [ ] **R4.10** Slack Acknowledge/Resolve buttons (signed requests, optional per project)
- [ ] **R4.11** Monitors, heartbeats and log alerts route through escalation policies
- [ ] **R4.12** Incoming integrations: generic JSON webhook, Prometheus
      Alertmanager, Grafana alerting (secret URL, dedup, auto-resolve)
- [ ] **R4.13** Public incident report form (secret URL, per-IP rate limit, honeypot)
- [ ] **R4.14** Severities (per-project list with default) and incident metadata
- [ ] **R4.15** Incident list filters (status, source, severity, date range, metadata)
- [ ] **R4.16** Reporting: MTTA, MTTR, counts by source/severity/responder, SLA
      report (compound + average), CSV export
- [ ] **R4.17** Create GitHub / Linear / Jira tickets from an incident
- [ ] **R4.18** UI for all of the above

## 5. Status pages

- [ ] **R5.01** Public status page per slug (no auth), custom branding (name, logo
      URL, accent color)
- [ ] **R5.02** Theme (light/dark/system), up to 4 navigation links, dark-mode logo
- [ ] **R5.03** Components and sections bound to monitors/heartbeats
- [ ] **R5.04** 90-day uptime history bars, overall status banner
- [ ] **R5.05** Status updates (investigating / identified / monitoring / resolved)
      and scheduled maintenance announcements
- [ ] **R5.06** Per-update "notify subscribers" toggle
- [ ] **R5.07** Automatic reports from bound monitor/heartbeat incidents (per page)
- [ ] **R5.08** Email subscribers (double opt-in, unsubscribe)
- [ ] **R5.09** Component-level email subscriptions
- [ ] **R5.10** Webhook subscribers (signed payloads, retries)
- [ ] **R5.11** Password-protected (private) pages
- [ ] **R5.12** IP allowlist for private pages; `noindex` toggle
- [ ] **R5.13** RSS/Atom feed, embeddable status badge (SVG)
- [ ] **R5.14** Public JSON status API
- [ ] **R5.15** Embeddable announcement widget (CSP-safe JS, no cookies)
- [ ] **R5.16** UI: editor + public page

## 6. Metrics + dashboards

- [ ] **R6.01** Metric ingestion (JSON, Prometheus text format) with source tokens
- [ ] **R6.02** Prometheus scrape targets (pull) with target health
- [ ] **R6.03** ClickHouse storage, retention
- [ ] **R6.04** Query API: aggregations (avg/min/max/sum/count/p50/p95/p99) over
      time buckets, group-by tags
- [ ] **R6.05** Dashboards with charts (line, bar, stat), layout, time range
- [ ] **R6.06** Dashboard variables (tag-value dropdowns)
- [ ] **R6.07** Markdown text panels
- [ ] **R6.08** Dashboard template library: host (node_exporter), Postgres, Redis,
      Nginx, Docker/cAdvisor
- [ ] **R6.09** Read-only public share link per dashboard (revocable)
- [ ] **R6.10** Built-in metrics from uptime checks and log volume
- [ ] **R6.11** Metric alerts (threshold → incident)
- [ ] **R6.12** Alert improvements (metric + log): no-data handling,
      confirmation/recovery periods, per-series incidents
- [ ] **R6.13** Collector bundle: OpenTelemetry Collector + Vector configs and
      compose snippets, surfaced on UI setup pages
- [ ] **R6.14** UI for all of the above

## 7. Traces (OpenTelemetry)

- [ ] **R7.01** OTLP/HTTP (JSON + protobuf) trace ingestion with source tokens
- [ ] **R7.02** ClickHouse span storage, retention
- [ ] **R7.03** Trace search (service, operation, duration, status, time range)
- [ ] **R7.04** Trace waterfall view
- [ ] **R7.05** Trace-to-log linking both ways via `trace_id`
- [ ] **R7.06** Apdex per service (configurable threshold)

## 8. Error tracking (Sentry-compatible)

- [ ] **R8.01** Applications with DSNs; Sentry envelope + store endpoints
      (gzip/deflate/br, size limits, unknown items skipped)
- [ ] **R8.02** Event normalization (exception chain, frames, breadcrumbs, tags,
      contexts, user, release, environment, SDK) with data scrubbing
- [ ] **R8.03** Grouping (SDK fingerprint, else type + normalized message + top
      in-app frames)
- [ ] **R8.04** Issues: first/last seen, counts, affected users, resolve, ignore
      (until N occurrences / until date), regression on a new release
- [ ] **R8.05** Release tracking
- [ ] **R8.06** Spike protection per DSN (rate limit + daily cap)
- [ ] **R8.07** JS source map upload (upload token) + server-side symbolication
- [ ] **R8.08** Alert rules (new issue, regression, frequency) → incidents
- [ ] **R8.09** ClickHouse `error_events` with retention
- [ ] **R8.10** UI: applications, issue list, issue detail (stack trace,
      breadcrumbs), releases, alert rules

## 9. Real user monitoring

- [ ] **R9.01** RUM applications with public client keys; JS SDK served by Nami
- [ ] **R9.02** Beacon ingest (allowed origins, size and rate limits)
- [ ] **R9.03** Page views, web vitals (LCP/CLS/INP/TTFB/FCP), custom events,
      user identify
- [ ] **R9.04** JS errors forwarded to error tracking
- [ ] **R9.05** Session replay (rrweb: sampling, input masking, chunked upload,
      retention, player)
- [ ] **R9.06** Website analytics (visits, cookieless unique visitors, referrers,
      UTM, pages, countries)
- [ ] **R9.07** Web vitals per URL (p75) with alerts
- [ ] **R9.08** Funnels
- [ ] **R9.09** Privacy: no cookies, no raw IPs, DNT option, masking defaults,
      per-user erasure
- [ ] **R9.10** UI for all of the above

## 10. AI SRE (bring your own LLM)

- [ ] **R10.01** Provider config: Anthropic (default `claude-sonnet-5-5`) or an
      OpenAI-compatible endpoint; key encrypted at rest
- [ ] **R10.02** "Explain with AI" on incidents, log lines and error issues
- [ ] **R10.03** Automatic investigation when an incident opens (labeled summary +
      suspected cause on the timeline)
- [ ] **R10.04** AI post-mortem drafts
- [ ] **R10.05** Project chat with read-only tools, scoped to the user's projects
- [ ] **R10.06** Daily token/cost budgets
- [ ] **R10.07** Prompt-injection posture (untrusted telemetry, read-only tools,
      human click for every action, sanitized output)
- [ ] **R10.08** Audit of every AI call

## 11. Platform + production hardening (last)

- [ ] **R11.01** API tokens + documented REST API for monitors/heartbeats/incidents
- [ ] **R11.02** REST API coverage for every configurable resource (OpenAPI)
- [ ] **R11.03** MCP server (streamable HTTP, PAT auth, tool allow/deny headers)
- [ ] **R11.04** Audit log of security-relevant and configuration changes
- [ ] **R11.05** Audit log forwarding (log source or webhook)
- [ ] **R11.06** Generic OIDC login (Keycloak, Authentik, Entra ID)
- [ ] **R11.07** Org-wide 2FA enforcement
- [ ] **R11.08** Invite by email for unregistered users
- [ ] **R11.09** Usage page (ingest volume per source, storage per table)
- [ ] **R11.10** Dockerfiles (engine + UI) and a production docker-compose
- [ ] **R11.11** CI (GitHub Actions): engine tests with service containers, UI check/test
- [ ] **R11.12** Security headers (CSP, HSTS, frame-ancestors), request body limits
- [ ] **R11.13** Close parked Foundation items: 2FA-change re-auth, timing-safe auth
      responses, trusted-proxy check for X-Forwarded-For, expired
      session/token reapers, per-user TOTP attempt limiting
- [ ] **R11.14** Dedicated full-system security review + penetration-style test pass
