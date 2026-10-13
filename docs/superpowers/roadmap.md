# Nami Roadmap — BetterStack Feature Parity

Definition of done for the "rebuild BetterStack" goal. Each sub-project gets
its own spec → plan → per-task PRs (see `CLAUDE.md`). Every sub-project ends
with a dedicated security review + fix wave and a whole-sub-project review;
the final sub-project is a production-hardening pass.

Out of scope on purpose (needs paid third-party telephony or global
infrastructure): SMS and phone-call alerts, multi-region checking,
Playwright browser checks, Terraform provider.

## 1. Foundation — done (PRs #1-#16)

- [x] Email/password registration with verification, login, logout
- [x] Server-side sessions (Postgres), session fixation protection
- [x] TOTP 2FA, password reset with session invalidation
- [x] Google/GitHub OAuth (BFF passthrough)
- [x] Two-level RBAC (global role + per-project role), projects, members
- [x] Rate limiting on auth endpoints (per client IP)
- [x] Full UI for all of the above

## 2. Uptime monitoring — in progress

- [ ] Monitor types: HTTP status, keyword / keyword absence, TCP port
- [ ] Monitor types: ping (ICMP), DNS record, SSL certificate expiry, domain expiry
- [ ] Request config: method, custom headers, body, basic auth, follow
      redirects toggle, SSL-verification toggle, expected status range
- [ ] Interval (>= 30 s), timeout, confirmation threshold, recovery,
      pause/resume
- [ ] Heartbeats (cron monitoring) with period + grace
- [ ] Automatic incidents (open on down, resolve on recovery)
- [ ] Email alerts on transitions
- [ ] Uptime % (24 h / 7 d / 30 d), response-time history chart, check log
- [ ] Maintenance windows (suppress incidents/alerts during a window)
- [ ] Monitor groups
- [ ] UI for all of the above, auto-refresh

## 3. Log management

- [ ] Sources per project with ingest tokens (create, rotate, delete)
- [ ] HTTP ingestion: JSON object, JSON array, NDJSON; size limits;
      per-source rate limiting
- [ ] Syslog ingestion (RFC 5424 over TCP/UDP)
- [ ] ClickHouse storage with per-source retention (TTL)
- [ ] Search: time range, level, full-text, structured field filters,
      pagination
- [ ] Live tail (SSE through the BFF)
- [ ] Saved views
- [ ] Log alerts (threshold over a window → incident)
- [ ] UI: sources, explorer, live tail, saved views, alerts

## 4. Incident management + on-call

- [ ] Incident lifecycle: open → acknowledged → resolved; manual incidents
- [ ] Incident timeline (events, comments), postmortem notes
- [ ] Escalation policies (multi-step, delays, repeat) per project
- [ ] On-call schedules: rotations, handoff, overrides, "who's on call"
- [ ] Notification channels: email, generic webhook, Slack, Telegram,
      Discord; per-user and per-policy targets
- [ ] Acknowledge/resolve from email links (signed, expiring tokens)
- [ ] Monitors, heartbeats and log alerts route through escalation policies
- [ ] UI for all of the above

## 5. Status pages

- [ ] Public status page per slug (no auth), custom branding (name, logo
      URL, accent color)
- [ ] Components and sections bound to monitors/heartbeats
- [ ] 90-day uptime history bars, overall status banner
- [ ] Status updates (investigating / identified / monitoring / resolved)
      and scheduled maintenance announcements
- [ ] Email subscribers (double opt-in, unsubscribe)
- [ ] Password-protected (private) pages
- [ ] RSS/Atom feed, embeddable status badge (SVG)
- [ ] UI: editor + public page

## 6. Metrics + dashboards

- [ ] Metric ingestion (JSON, Prometheus text format) with source tokens
- [ ] ClickHouse storage, retention
- [ ] Query API: aggregations (avg/min/max/sum/count/p50/p95/p99) over
      time buckets, group-by tags
- [ ] Dashboards with charts (line, bar, stat), layout, time range
- [ ] Built-in metrics from uptime checks and log volume
- [ ] Metric alerts (threshold → incident)
- [ ] UI for all of the above

## 7. Traces (OpenTelemetry)

- [ ] OTLP/HTTP (JSON + protobuf) trace ingestion with source tokens
- [ ] ClickHouse span storage, retention
- [ ] Trace search (service, operation, duration, status, time range)
- [ ] Trace waterfall view

## 8. Platform + production hardening

- [ ] API tokens + documented REST API for monitors/heartbeats/incidents
- [ ] Audit log of security-relevant and configuration changes
- [ ] Dockerfiles (engine + UI) and a production docker-compose
- [ ] CI (GitHub Actions): engine tests with service containers, UI check/test
- [ ] Security headers (CSP, HSTS, frame-ancestors), request body limits
- [ ] Close parked Foundation items: 2FA-change re-auth, timing-safe auth
      responses, trusted-proxy check for X-Forwarded-For, expired
      session/token reapers, per-user TOTP attempt limiting
- [ ] Dedicated full-system security review + penetration-style test pass
