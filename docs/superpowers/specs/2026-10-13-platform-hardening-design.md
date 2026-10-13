# Platform + Production Hardening

## Context

Eighth and final sub-project (roadmap §8): makes Nami deployable and
operable in production, closes every parked security item from earlier
sub-projects, and ends with a full-system security review + penetration-
style test pass.

## Goals

### API tokens + REST API

- Personal access tokens (`nami_pat_<random>`): name, optional expiry,
  scope = list of projects + max role (`viewer`/`member`/`admin`, capped by
  the user's real role at request time), SHA-256 stored, shown once,
  `last_used_at`, revoke. `Authorization: Bearer` accepted by every
  project-scoped endpoint alongside sessions (session-only for auth/account
  endpoints and token management itself).
- OpenAPI 3.1 document (`/api/openapi.json`, generated with `utoipa`)
  covering monitors, heartbeats, incidents, status reports, logs search,
  metrics query; rendered docs page in nami-ui.

### Audit log

`audit_events`: actor (user/token/system), action, target type/id,
project, IP, user agent, metadata, timestamp. Written for logins
(success/failure), logout, password change/reset, 2FA changes, token
create/revoke, member/role changes, and create/update/delete of every
configurable resource. Admin UI with filters; retention 365 days.

### Deployment

- Multi-stage `nami-engine/Dockerfile` (cargo build --release with cached
  deps, runtime on distroless/debian-slim, non-root user) and
  `nami-ui/Dockerfile` (adapter-node build, non-root).
- `docker-compose.prod.yml`: engine, ui, postgres, clickhouse, mailpit
  replaced by real SMTP env, Caddy reverse proxy with automatic TLS, health
  checks, restart policies, named volumes, no published DB ports.
- Graceful shutdown on SIGTERM (stop accepting, drain requests, stop
  workers after their current tick), `/health` (liveness) and `/ready`
  (Postgres + ClickHouse reachable), structured JSON logs, Prometheus
  `/metrics` for Nami itself (behind a token).

### CI

GitHub Actions: `cargo fmt --check`, `cargo clippy -D warnings`,
`cargo test` with Postgres/ClickHouse/Mailpit service containers,
`cargo audit`; UI `npm ci`, `npm run check`, `npm test`, `npm audit
--audit-level=high`; Docker image builds. Branch protection documented.

### Security hardening (closes parked items)

- Re-authentication (current password or TOTP code) to set up, change or
  disable 2FA, change password, or create API tokens.
- Timing-safe auth responses: register and forgot-password do equal work
  on both paths (enqueue email instead of sending inline; dummy argon2 on
  unknown users at login).
- Per-user TOTP attempt limiting (5 failures → 15 min lockout) on top of
  per-IP limits.
- Trusted proxies: honor `X-Forwarded-For` only when the TCP peer is in
  `TRUSTED_PROXIES` (CIDR list); otherwise key rate limits on the peer IP.
- `OUTBOUND_ALLOW_PRIVATE_NETWORKS` (default `false` in the production
  compose, `true` in dev) for monitors and webhooks, enforced after DNS
  resolution by connecting to the vetted IP (DNS-rebinding safe).
- Reapers: expired sessions, consumed/expired tokens, old deliveries.
- Security headers in nami-ui (CSP via SvelteKit `kit.csp` with nonces,
  HSTS, `frame-ancestors 'none'`, Referrer-Policy, Permissions-Policy,
  `X-Content-Type-Options`); global request body limit on the engine;
  CORS closed.
- Explicit `SameSite`, `Secure` (prod), `HttpOnly` on every cookie; session
  ID rotation already in place.

### Full-system security pass

An automated authorization-matrix test that enumerates every route ×
{anonymous, viewer, member, admin, other-project admin, token with lower
scope} and asserts the expected status; fuzz tests on every ingestion
endpoint; `cargo audit` / `npm audit` clean or documented; a final opus
security review with a fix wave until no Critical/Important findings
remain.

## Non-goals

Kubernetes manifests/Helm, SAML/SCIM, multi-tenant billing.
