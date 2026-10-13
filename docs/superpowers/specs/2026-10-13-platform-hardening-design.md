# Platform + Production Hardening

> Parity additions (2026-10-13): moved to last (roadmap §11, after Error tracking, RUM and AI SRE); REST API coverage for every configurable resource, MCP server, audit log forwarding, generic OIDC login, org-wide 2FA enforcement, invite by email, usage page; one outbound (SSRF) policy for all outbound requests.

## Context

Eleventh and final sub-project (roadmap §11, after Error tracking, RUM and
AI SRE): makes Nami deployable and operable in production, completes the
API surface (REST coverage, MCP), adds org-level identity controls,
closes every parked security item from earlier sub-projects, and ends
with a full-system security review + penetration-style test pass.

## Goals

### API tokens + REST API

- Personal access tokens (`nami_pat_<random>`): name, optional expiry,
  scope = list of projects + max role (`viewer`/`member`/`admin`, capped by
  the user's real role at request time), SHA-256 stored, shown once,
  `last_used_at`, revoke. `Authorization: Bearer` accepted by every
  project-scoped endpoint alongside sessions (session-only for auth/account
  endpoints and token management itself).
- REST coverage: CRUD (plus the actions the UI offers) for every
  configurable resource — monitors, heartbeats, groups, maintenance
  windows, sources and pipelines, log/metric/error/RUM alerts, saved
  views, dashboards and share links, scrape targets, escalation policies,
  schedules and overrides, channels, severities, integrations, status
  pages with sections, components, reports and subscribers, error and RUM
  applications, AI settings — through the same handlers and RBAC as the
  UI, one error shape, keyset pagination. This is the infrastructure-as-
  code path (no Terraform provider).
- OpenAPI 3.1 document (`/api/openapi.json`, generated with `utoipa`)
  covering every route above; a test fails when a route is missing from
  it; rendered docs page in nami-ui.

### MCP server

- Streamable HTTP endpoint `/mcp` on the engine (the current MCP spec
  revision at implementation time). Auth: `Authorization: Bearer
  nami_pat_…` only; the token's project scope and max role apply to every
  tool call.
- Tools wrap the REST layer (same handlers, same RBAC). Read tools:
  monitors and heartbeats with stats, incidents with timelines, who is on
  call, logs search and patterns, metrics query and discovery, traces
  search and detail, error issues and events, status pages. Safe actions:
  acknowledge, resolve and comment on an incident (annotated
  `readOnlyHint: false`).
- `X-MCP-Tools-Only: a,b` / `X-MCP-Tools-Except: c` headers narrow the
  tool list per connection (unknown tool names → 400).
- Results are capped (rows and bytes, as the AI SRE tools); every tool
  call is audited (tool, token, project, result size); per-token rate
  limit; a request whose `Origin` header is outside
  `MCP_ALLOWED_ORIGINS` → 403.

### Audit log

`audit_events`: actor (user/token/system), action, target type/id,
project, IP, user agent, metadata, timestamp. Written for logins
(success/failure), logout, password change/reset, 2FA changes, token
create/revoke, member/role changes, invites, MCP tool calls, and
create/update/delete of every configurable resource. Admin UI with
filters; retention 365 days.

- Forwarding (global admin): to a log source (each record becomes a log
  line with `fields` = the record) and/or a webhook (signed like webhook
  channels, durable delivery queue, retries). At-least-once; AI SRE's
  `ai_calls` records are forwarded too; failures are visible on the
  settings page; changing the forwarding config is itself audited.
  Records never contain secret values.

### Identity and access

- Generic OIDC login, one configured provider (`OIDC_ISSUER_URL`,
  `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET`, `OIDC_DISPLAY_NAME`): issuer
  discovery, authorization code + PKCE, `state` and `nonce`; the ID token
  is checked for `iss`, `aud`, `exp` and a JWKS signature (RS256/ES256
  only; `none` and HS* rejected). Identity = (`oidc`, `sub`) in
  `oauth_identities`. An existing account is linked only when
  `email_verified` is true and the email matches its verified email.
  `OIDC_AUTO_PROVISION` (default false) creates new users with
  `OIDC_DEFAULT_ROLE` (default `viewer`), optionally limited to
  `OIDC_ALLOWED_DOMAINS`. BFF passthrough like Google/GitHub. Tested
  against a mock provider; checked manually with Keycloak, Authentik and
  Entra ID.
- Org-wide 2FA enforcement (global admin setting): a user without TOTP
  gets 403 `2fa_enrollment_required` on every endpoint except TOTP
  enrollment, logout and session info — PAT and MCP requests included —
  until enrolled; the UI redirects to enrollment. Optional exemption for
  OIDC logins (the IdP enforces MFA).
- Invite by email: project admins (global admins also without a
  project) invite an address with a project role. Token: random, stored
  SHA-256, single use, 7 days, revocable, resendable. Accepting either
  registers the account (the email counts as verified) or, for a
  logged-in user with that verified email, adds the membership.
  `REGISTRATION_MODE=open|invite_only` (`invite_only` in the production
  compose) closes public sign-up. Invites are rate-limited per inviter.

### Usage page

Global admins see ingest volume per source and per error/RUM application
(events and bytes per day, from each ingest path's daily usage counters —
Logs' `source_usage_daily`, extended here to metric samples and spans),
ClickHouse storage per table from `system.parts` (rows, compressed and
uncompressed bytes, bytes on disk) and Postgres database and largest-table
sizes. Project admins see their own project's volume.

### Data model additions

```sql
create table project_invites (
    id uuid primary key default gen_random_uuid(),
    project_id uuid references projects(id) on delete cascade, -- null = no project
    email text not null,                -- lowercased
    project_role text check (project_role in ('admin', 'member', 'viewer')),
    token_hash bytea not null unique,
    invited_by uuid not null references users(id) on delete cascade,
    expires_at timestamptz not null,
    accepted_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz not null default now()
);

create table instance_settings (        -- single row
    id boolean primary key default true check (id),
    require_2fa boolean not null default false,
    exempt_oidc_from_2fa boolean not null default false,
    audit_forward_source_id uuid,
    audit_forward_webhook_url text,
    audit_forward_secret_enc bytea
);
```

### Deployment

- Multi-stage `nami-engine/Dockerfile` (cargo build --release with cached
  deps, runtime on distroless/debian-slim, non-root user) and
  `nami-ui/Dockerfile` (adapter-node build, non-root).
- `docker-compose.prod.yml`: engine, ui, postgres, clickhouse, mailpit
  replaced by real SMTP env, Caddy reverse proxy with automatic TLS, health
  checks, restart policies, named volumes, no published DB ports. Caddy
  sends the engine's public surface to the engine — ingest endpoints
  (logs, metrics, OTLP, Sentry envelope/store, RUM, heartbeats,
  integrations, Slack interactivity), the REST API under a prefix that
  cannot collide with nami-ui pages, and `/mcp` — and everything else to
  nami-ui.
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
- One outbound (SSRF) policy for every server-side request — monitors
  and their proxies, webhooks and channels, scrape targets, ticket
  integrations, the LLM provider, OIDC discovery, audit forwarding:
  `OUTBOUND_ALLOW_PRIVATE_NETWORKS` (default `true`, since Nami watches
  the operator's own infrastructure; `false` restricts to public
  addresses) plus optional `OUTBOUND_DENY_CIDRS`, enforced after DNS
  resolution by connecting to the vetted IP (DNS-rebinding safe).
  Regardless of the setting, link-local and cloud-metadata destinations
  (`169.254.0.0/16`, `fe80::/10`, `fd00:ec2::254/128`) and unspecified/
  broadcast addresses are always refused.
  Destinations supplied by unauthenticated visitors (status-page webhook
  subscribers) always use strict mode (public unicast only).
- Request logs never contain path tokens (heartbeat, integration, report
  form, ICS, share links) or ingest keys (`sentry_key`, RUM client key).
- Reapers: expired sessions, consumed/expired tokens, old deliveries.
- Security headers in nami-ui (CSP via SvelteKit `kit.csp` with nonces,
  HSTS, `frame-ancestors 'none'`, Referrer-Policy, Permissions-Policy,
  `X-Content-Type-Options`); global request body limit on the engine;
  CORS closed except where a spec opens it on purpose (Sentry and RUM
  ingest with per-app allowed origins, public status JSON, RUM SDK
  files).
- Explicit `SameSite`, `Secure` (prod), `HttpOnly` on every cookie; session
  ID rotation already in place.

### Full-system security pass

An automated authorization-matrix test that enumerates every route and
MCP tool × {anonymous, viewer, member, admin, other-project admin, token
with lower scope, user pending 2FA enrollment} and asserts the expected
status; fuzz tests on every ingestion endpoint (logs JSON/NDJSON/
MessagePack, syslog, OTLP, metrics, Sentry envelope/store, RUM beacons and
replay chunks, incident integrations); `cargo audit` / `npm audit` clean
or documented; a final opus security review with a fix wave until no
Critical/Important findings remain.

Feature tests for the additions: OIDC (wrong `iss`/`aud`, expired token,
`alg: none`, HS256 with the public key, replayed `state`/`nonce`,
unverified email never links an account); 2FA enforcement on session,
PAT and MCP requests; invites (expired, reused, revoked, other email);
MCP tool filters and RBAC; audit forwarding payloads free of secrets;
outbound policy (private targets blocked when restricted, DNS rebinding,
strict mode for subscriber webhooks regardless of the setting).

## Non-goals

Kubernetes manifests/Helm, SAML/SCIM (generic OIDC covers Keycloak,
Authentik and Entra ID; SCIM provisioning has no self-host payoff at
single-org scale), multiple OIDC providers, OIDC group/role sync, OAuth
for MCP clients (PAT only), multi-tenant billing.
