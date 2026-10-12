# Foundation: Auth, Sessions, RBAC

## Context

Nami is a self-hosted reimplementation of BetterStack (log management,
uptime monitoring, incident management/on-call, status pages, metrics),
built to avoid a recurring BetterStack subscription.

Stack already scaffolded:
- `nami-engine` — Rust backend (currently empty `main.rs`, bare `Cargo.toml`)
- `nami-ui` — SvelteKit 5 + Tailwind 4 frontend (fresh `sv create` scaffold)
- `docker-compose.yaml` — lists Postgres, ClickHouse, Mailpit as intended
  services (not yet configured)

BetterStack is actually several independent products. Building all of them
as one spec is too large for one implementation plan, so the project is
decomposed into sequential sub-projects:

1. **Foundation: auth, org, RBAC, session** (this spec)
2. Uptime monitoring
3. Log management
4. Incident management + on-call alerting
5. Status page
6. Metrics/APM

This spec covers only #1. Later sub-projects depend on it but are out of
scope here.

## Goals

- A single user can register, verify their email, log in, and manage their
  password — with sessions that can be instantly revoked.
- Optional 2FA (TOTP) and OAuth (Google/GitHub) login.
- Role-based access control at two levels: a global role per user, and a
  per-project role that future resources (monitors, log sources, etc.)
  will attach to.
- `nami-ui` never talks to `nami-engine` directly from the browser — it
  acts as a backend-for-frontend (BFF) that proxies and owns the cookie.

## Non-goals

- Multi-tenancy (multiple separate organizations on one instance). This is
  single-org only.
- Any actual monitoring/log/incident/status-page functionality — those are
  later sub-projects and only need a `projects` table to attach to here.
- Production email delivery — Mailpit is a local SMTP catcher for
  development; swapping in a real provider later is out of scope.
- Audit logging of permission changes (can be added later without
  reshaping this schema).

## Architecture

```
Browser ──cookie──> nami-ui (SvelteKit, BFF)
                         │ server-to-server (internal network, no CORS)
                         ▼
                     nami-engine (Rust/Axum)
                         │
              ┌──────────┼──────────┐
              ▼          ▼          ▼
          Postgres   Mailpit    (ClickHouse — reserved for later slices)
```

- `nami-ui`'s `hooks.server.ts` and route handlers forward requests to
  `nami-engine` over the internal docker network and relay/set the
  session cookie on the `nami-ui` origin. The browser never calls
  `nami-engine` directly, so no CORS configuration is needed and the
  cookie stays same-site.
- `nami-engine` is the only service that talks to Postgres and Mailpit in
  this slice. ClickHouse is provisioned in `docker-compose.yaml` but
  unused until the uptime-monitoring sub-project.

## Rust stack (nami-engine)

- **axum** — HTTP framework; `tower` middleware used for session
  extraction and RBAC guards.
- **sqlx** (Postgres, async, compile-time checked queries) — no ORM.
- **tower-sessions** + **tower-sessions-sqlx-store** — server-side
  sessions; cookie holds only a random session ID, session data lives in
  Postgres, logout/revoke is an immediate row delete.
- **argon2** — password hashing.
- **oauth2** — Google/GitHub OAuth2 login.
- **totp-rs** — TOTP 2FA.
- **lettre** — SMTP client, sends to Mailpit in development.
- **tower_governor** (or equivalent) — rate limiting on login/register.

## Data model (Postgres)

```sql
users (
  id               uuid primary key,
  email            text unique not null,
  password_hash    text,              -- null for OAuth-only accounts
  email_verified_at timestamptz,
  global_role      text not null,     -- 'admin' | 'member' | 'viewer'
  totp_secret      text,
  totp_enabled     boolean not null default false,
  created_at       timestamptz not null default now()
)

oauth_identities (
  id                uuid primary key,
  user_id           uuid not null references users(id) on delete cascade,
  provider          text not null,    -- 'google' | 'github'
  provider_user_id  text not null,
  created_at        timestamptz not null default now(),
  unique (provider, provider_user_id)
)

projects (
  id         uuid primary key,
  name       text not null,
  slug       text unique not null,
  created_at timestamptz not null default now()
)

project_members (
  project_id   uuid not null references projects(id) on delete cascade,
  user_id      uuid not null references users(id) on delete cascade,
  project_role text not null,  -- 'admin' | 'member' | 'viewer'
  primary key (project_id, user_id)
)

email_verification_tokens (
  id          uuid primary key,
  user_id     uuid not null references users(id) on delete cascade,
  token_hash  text not null,
  expires_at  timestamptz not null,
  consumed_at timestamptz
)

password_reset_tokens (
  id          uuid primary key,
  user_id     uuid not null references users(id) on delete cascade,
  token_hash  text not null,
  expires_at  timestamptz not null,
  consumed_at timestamptz
)

-- sessions table: standard schema generated by tower-sessions-sqlx-store
```

`global_role` governs account-wide defaults (e.g. who can create projects,
manage any user). `project_role` in `project_members` overrides/narrows
access within a specific project. Future resources (monitors, log
sources, incidents, status pages) will carry a `project_id` and
authorization checks will consult `project_members` for that project.

Tokens are stored hashed (not raw) so a database read alone can't be used
to forge a valid verification/reset link.

## Auth flows

1. **Register** (email + password) → user row created with
   `email_verified_at = null` → verification email sent via lettre to
   Mailpit → login is blocked until the email is verified.
2. **Email verification** → user clicks link with token → token validated
   (unexpired, unconsumed, hash match) → `email_verified_at` set,
   token marked consumed.
3. **OAuth login** (Google/GitHub) → redirect to provider → callback →
   match existing `oauth_identities` row or create a new user
   (auto-verified, no password) → session created.
4. **Password login** → verify argon2 hash → if `totp_enabled`, require a
   second step with a valid TOTP code before a session is created →
   session row inserted in Postgres, cookie set.
5. **Password reset** → user requests reset → one-time token emailed →
   user sets new password → **all existing sessions for that user are
   invalidated** (deleted from the session store).
6. **Logout** → session row deleted, cookie cleared.

All auth error responses to the client are generic (e.g. "invalid email
or password") to avoid user-enumeration — registration success/failure
responses must not reveal whether an email is already registered via
timing or message differences beyond what's necessary.

## RBAC enforcement

- Axum middleware loads the session → resolves `user_id` → loads
  `global_role`.
- Routes scoped to a `project_id` run a second middleware step that loads
  the `project_members` row for `(user_id, project_id)` and resolves the
  effective project role (falls back to denying access if no membership
  row exists, even for global `member`/`viewer`; global `admin` bypasses
  project membership checks).
- Authorization failure → `403`, not `404` — this is an internal tool, not
  a public surface, so there's no need to hide resource existence from an
  already-authenticated user.

## Testing

- `nami-engine`: integration tests via `sqlx::test` (transactional
  rollback per test) against a Postgres testcontainer; email flows
  assert against Mailpit's SMTP capture.
- `nami-ui`: unit tests for the BFF proxy/cookie-relay logic in
  `hooks.server.ts`. End-to-end browser testing deferred until there's
  real UI to test in a later slice.

## Open questions for implementation time

- Exact OAuth provider set is Google + GitHub for now; adding more later
  is additive (new row in `oauth_identities`, no schema change).
- Rate-limit thresholds (requests/window) are an implementation detail,
  not fixed here.
