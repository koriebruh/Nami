# Platform + Production Hardening, Part 2 (identity, production and the full-system pass) Implementation Plan

> **Status:** outline. Nothing merged. Next: NAMI-PLT-10 once Part 1 (`feat/platform-part1`, NAMI-PLT-1..9) is on `main`.
> **Branch:** `feat/platform-part2` in `.worktrees/platform-part2/`.
> **Depends on:** Platform Part 1 (`docs/superpowers/plans/2026-10-24-platform-hardening-part1.md`), and through it AI SRE and every earlier phase.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. This is an `outline` plan (`docs/superpowers/plan-conventions.md`): tasks, scope, data shapes, endpoints, files, tests and acceptance are fixed here, and the step-level code is written at phase start against the then-current `main`.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R11.04 | Audit log, Part 2 half: the events of the Part 2 code paths (password change, 2FA disable, OIDC link, invites, instance settings) | NAMI-PLT-12, NAMI-PLT-13, NAMI-PLT-14, NAMI-PLT-15 |
| R11.06 | Generic OIDC login (Keycloak, Authentik, Entra ID) | NAMI-PLT-13, NAMI-PLT-22 |
| R11.07 | Org-wide 2FA enforcement | NAMI-PLT-14, NAMI-PLT-22 |
| R11.08 | Invite by email for unregistered users | NAMI-PLT-15, NAMI-PLT-22 |
| R11.09 | Usage page (ingest volume per source, storage per table) | NAMI-PLT-16, NAMI-PLT-22 |
| R11.10 | Dockerfiles (engine + UI) and a production docker-compose (runtime half: shutdown, health, readiness, logs, metrics, config) | NAMI-PLT-18, NAMI-PLT-20 |
| R11.11 | CI, Part 2 half: the Docker image-build job (Part 1 did the rest, NAMI-PLT-1) | NAMI-PLT-20 |
| R11.12 | Security headers (CSP, HSTS, frame-ancestors), request body limits, closed CORS, cookie attributes | NAMI-PLT-17 |
| R11.13 | Parked Foundation items, Part 2 half: re-auth on password and 2FA changes, timing-safe auth responses, trusted-proxy check, reapers, per-user TOTP limiting; plus the spec's other parked hardening (one outbound policy, request-log redaction) and every item earlier decisions docs sent to Platform | NAMI-PLT-10, NAMI-PLT-11, NAMI-PLT-12, NAMI-PLT-17, NAMI-PLT-18, NAMI-PLT-19, NAMI-PLT-21 |
| R11.14 | Dedicated full-system security review + penetration-style test pass | NAMI-PLT-23, NAMI-PLT-24 |

R11.13 names five Foundation items. The spec's "Security hardening (closes parked items)" section adds the outbound policy, request-log redaction, reapers, headers and cookies, and the decisions docs add their own list ("Deferred items inventory" below). All of them are claimed under R11.13 here, except headers, body limits and cookies, which are R11.12.

## Split (see Part 1, "Why two parts")

Part 1 delivered CI, PATs, the audit log, REST/OpenAPI, MCP and forwarding. This part needs Part 1's token principal (2FA enforcement covers PAT and MCP requests), its audit recorder (invites, OIDC and settings write events) and its `auth::reauth` (password and 2FA changes). Its last two tasks are the full-system security pass the spec ends with.

## Goal

Make Nami safe to run on the internet and easy to deploy: one outbound (SSRF) policy for every server-side request, trusted-proxy aware rate limits and per-IP limits, timing-safe auth with re-authentication and per-user TOTP limits, generic OIDC login, org-wide 2FA enforcement, email invites with an invite-only registration mode, a usage page, security headers and body limits, graceful shutdown with health, readiness, JSON logs and Prometheus metrics, secrets handling and key rotation, production Docker images and compose behind Caddy, every item earlier phases parked for Platform, and a final authorization-matrix, fuzzing and penetration-style review.

## Architecture

- **`core::outbound` becomes the only way out.** Incident Management (planned in NAMI-INC-3) created `outbound::client()` / `public_client()` with a DNS-time guard. NAMI-PLT-10 turns it into one policy (`OutboundPolicy` built from config: allow private networks, deny CIDRs, and the always-refused set) with two modes, `Normal` and `Strict` (public unicast only), and moves every caller to it: monitors and their proxies, RDAP, channels and webhooks, ticket integrations, status-page webhook subscribers (strict), scrape targets, the LLM provider, OIDC discovery/JWKS/token calls, Google/GitHub OAuth, and audit forwarding. Non-HTTP checks (tcp, udp, smtp, pop3, imap, ping, dns server) call `outbound::check_ip` after their own resolution and connect to the vetted address.
- **`core::client_ip` decides who the client is.** One function maps `(peer, X-Forwarded-For, TRUSTED_PROXIES)` to the client address; the governor key extractor, the audit `ClientMeta`, the per-IP limits and the public listener's per-peer connection cap all use it.
- **Identity stays in `auth`:** `auth::oidc` (generic OIDC with `openidconnect`), `auth::invite` (invites, accept), `auth::jobs` (the background queue that makes register and forgot-password timing-safe), `auth::totp` lockout and replay checks, and `auth::enforcement` (the 2FA rule applied inside `CurrentUser`).
- **`platform` grows** `usage.rs` (usage endpoints), `metrics.rs` (Nami's own `/metrics`), `shutdown.rs` (the cancellation token every loop and listener watches), `reseal.rs` (key rotation), and the reaper gets the remaining retention jobs.
- **Deployment** is two multi-stage Dockerfiles (`nami-engine/Dockerfile`, `nami-ui/Dockerfile`), `docker-compose.prod.yml` and `Caddyfile` at the repository root, and a CI image-build job.

## Tech Stack

Rust 2024 (Axum 0.8, SQLx 0.9, tokio, reqwest 0.13, hickory-resolver 0.26), new direct crates: `openidconnect 4.0.1` (`default-features = false`; built on `oauth2 ^5.0.0`, which is already a dependency; brings `rsa`, `p256`, `p384`, `ed25519-dalek`), `ipnet 2.12.2` (already in `Cargo.lock` through reqwest/hickory; CIDR parsing for `TRUSTED_PROXIES` and `OUTBOUND_DENY_CIDRS`), `tokio-util 0.7.20` (already in the lock; `CancellationToken`). Feature additions on existing crates: `tower-http` `limit`, `tracing-subscriber` `json` and `env-filter`, `hyper-util` `server-graceful`. UI: `@sveltejs/adapter-node 6.0.0` (replaces `adapter-auto`; check its SvelteKit 3.0.x peer range at phase start). Caddy 2 (official image), distroless `cc-debian12:nonroot`, `node` LTS slim.

## Spec

`docs/superpowers/specs/2026-10-13-platform-hardening-design.md`, sections "Identity and access", "Usage page", "Data model additions", "Deployment", "CI" (image builds), "Security hardening (closes parked items)" and "Full-system security pass". Roadmap: `docs/superpowers/roadmap.md` §11.

## When to start

After Part 1's phase PR merges. Then:

1. Branch `feat/platform-part2` from the latest `origin/main` in `.worktrees/platform-part2/`.
2. Re-grep `docs/superpowers/decisions/` (now including `2026-10-24-platform-part1.md` and every doc from Incident Management to AI SRE) for `Platform`, `NAMI-PLT`, `hardening`, `deferred` and `parked`, and add new items to the inventory table below with a task.
3. Check "Consumed interfaces" against `main`; adapt names minimally, stop on behaviour differences.

## Consumed interfaces

| From | Interface | Used by |
|---|---|---|
| Platform Part 1 | `CurrentUser { auth: AuthKind }`, `TokenGrant`, `require_project_role` with scope and cap (NAMI-PLT-2); `auth::reauth::{verify, Reauth, REAUTH_WINDOW}` (NAMI-PLT-2); `platform::audit::{record, Actor, AuditEvent, ClientMeta, Recorded}` and `platform::reaper::run_forever` (NAMI-PLT-3); `api_routes()` per domain and `platform::rest::router` (NAMI-PLT-4); `platform::openapi::document()` (NAMI-PLT-5); `platform::mcp::TOOLS` (NAMI-PLT-6); `instance_settings` and `platform::settings` (NAMI-PLT-7); `AppError` coded variant. | all |
| Incidents | `outbound::{client, public_client, PublicOnlyResolver, OutboundPolicy, is_non_public, describe_error}` (planned in NAMI-INC-3); `notification_deliveries` and its final states (planned in NAMI-INC-5); incident action-token uses (planned in NAMI-INC-11); ticket integrations' HTTP calls (Incidents Part 2). | PLT-10, PLT-18 |
| Status pages | webhook subscribers with strict outbound (Status Pages Part 2); private-page password cookies; the custom-domain check endpoint `GET /public/status-domain-check/{host}` (if merged) for Caddy's on-demand TLS `ask`. | PLT-10, PLT-17, PLT-20 |
| Metrics, Traces, Errors, RUM, AI | scrape-target client, the LLM provider client, per-source / per-application daily usage counters (Logs' `source_usage_daily` pattern), ingest endpoints for the fuzz suite, CORS rules each spec opened on purpose. | PLT-10, PLT-16, PLT-17, PLT-23 |
| Foundation and Uptime | `auth/routes.rs` handlers (register, login, totp setup/enable/verify, forgot/reset, OAuth start/callback), `auth::oauth::Provider`, `uptime::checker::{is_forbidden_destination, allowed_addresses, HttpClients, CheckContext}`, `core::serve::{serve, ServeLimits, MAX_PUBLIC_CONNECTIONS, MAX_API_CONNECTIONS}`, `core::crypto::{Cipher, TEST_KEY, KEY_LEN}`, `core::config::Config`, `logs::syslog`, `logs::ingest::usage::run_forever`, `uptime::worker::run_forever`, `worker_state`. | PLT-10..21 |

## Global Constraints

- Everything in `CLAUDE.md`, `docs/STANDARDS.md`, `docs/superpowers/plan-conventions.md` and Part 1's Global Constraints (error shape with `code`, 403/404/400/409/429 rules, named limits with tests at and past the bound, no secrets in logs or audit rows, `deny_unknown_fields`).
- Engine tests: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test <domain> <file>::`, new files in `tests/auth/`, `tests/platform/`, `tests/core/`, or the domain the behaviour belongs to. Config parsing is tested as pure functions (unit tests), never by mutating the process environment. CI (Part 1) runs everything.
- Migrations for this phase, one file per domain, applied in this order: `NNNN_auth_identity.sql` (OIDC provider value, TOTP lockout and replay columns, pending TOTP secret, `project_invites`), then `NNNN_uptime_heartbeat_token_hash.sql` (NAMI-PLT-19), then `NNNN_logs_source_names.sql` (NAMI-PLT-19). `project_invites` references `projects` from the `0002_projects.sql` baseline, so the auth file has no dependency on the later files. No platform file is needed: `instance_settings` already has every spec column (Part 1).
- Every outbound request goes through `core::outbound` (NAMI-PLT-10 onward). A test that needs a loopback destination uses the incidents plan's test-only `Relaxed` seam, never a production switch.
- The phase decisions doc `docs/superpowers/decisions/2026-10-24-platform-part2.md` starts with NAMI-PLT-10's first commit and collects rulings as they are made.

## Review Focus

- No outbound request escapes the policy (grep for `reqwest::Client::new`, `Client::builder`, `TcpStream::connect`, `UdpSocket` and the oauth2 vendored client), DNS rebinding is closed by connecting to the vetted address, strict mode is used for every visitor-supplied destination.
- The client IP cannot be forged by a peer outside `TRUSTED_PROXIES`, on either listener.
- Auth endpoints are timing-safe and response-identical for known and unknown emails; argon2 never runs on the async runtime; TOTP is locked after 5 failures and a code is never accepted twice.
- OIDC: `iss`, `aud`, `exp`, `nonce`, signature (RS256/ES256 only), single-use `state`, PKCE, the `email_verified` linking rule, and no account takeover through an unverified email.
- 2FA enforcement covers sessions, PATs and MCP; only the documented exempt endpoints pass; an admin cannot lock the instance out.
- Invites are single-use, expire, are bound to their email, and do not reveal whether an email is registered.
- Headers, CSP and cookies on every response; no CORS except where a spec opened it; body limits on every listener.
- Shutdown never loses queued syslog frames, usage counters or a half-sent batch beyond the documented at-least-once behaviour.
- Containers run as non-root with no published database ports.

## Rulings

- **R-PLT-21 Outbound policy.** Config: `OUTBOUND_ALLOW_PRIVATE_NETWORKS` (default `true`) and `OUTBOUND_DENY_CIDRS` (comma-separated CIDRs, default empty; a malformed entry stops startup). Always refused, whatever the config: `0.0.0.0/8`, `169.254.0.0/16`, `fe80::/10`, `fd00:ec2::254/128`, `100.100.100.200` (Alibaba metadata, already in `uptime::checker`), `255.255.255.255`, `::`, multicast (`224.0.0.0/4`, `ff00::/8`), and each of these written as IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`, the Uptime Phase 1 `::a9fe:a9fe` deferral) or NAT64 (`64:ff9b::/96`). "Private" (refused when `OUTBOUND_ALLOW_PRIVATE_NETWORKS=false`): `10/8`, `172.16/12`, `192.168/16`, `127/8`, `100.64/10`, `198.18/15`, `fc00::/7`, `::1`. `Strict` mode refuses private and denied addresses regardless of the setting and is used for every destination an unauthenticated visitor or a non-admin personal setting supplies (status-page webhook subscribers, personal channels). Enforcement happens in the resolver: names resolve through hickory, the resolved addresses are filtered, and reqwest connects only to the vetted addresses, so a rebinding second answer is never used; every redirect hop goes through the same resolver. Monitor proxies: the proxy's own address is checked; a target name behind an HTTP CONNECT proxy is resolved by the proxy and cannot be checked (documented residual, the Uptime Phase 3 deferral closes as "accepted"); a literal IP target is checked locally. `MONITOR_DENY_CIDRS` and `MONITOR_PROXY_ALLOWLIST` (Uptime Phase 2/3 deferrals) are not added: `OUTBOUND_DENY_CIDRS` covers both.
- **R-PLT-22 Trusted proxies.** `TRUSTED_PROXIES` (comma-separated CIDRs) defaults to `127.0.0.1/32,::1/128`, so nami-ui on the same host in development keeps per-user login limits. The client address is the TCP peer, unless the peer is trusted and `X-Forwarded-For` is present, in which case it is the rightmost XFF entry that is not itself trusted (all trusted → the leftmost entry). `X-Real-IP` and `Forwarded` are ignored. The production compose sets the Docker network's CIDR (Caddy and nami-ui are the only peers there). nami-ui runs with `ADDRESS_HEADER=X-Forwarded-For` and `XFF_DEPTH=1` behind Caddy, so the address it forwards is the real client's.
- **R-PLT-23 Per-IP limits.** On the public listener: at most `MAX_CONNECTIONS_PER_PEER = 64` open connections per TCP peer that is not trusted (trusted peers, i.e. Caddy, are exempt; their clients are limited by the next rule), and a per-client-IP bucket of failed authentications on public endpoints (unknown ingest token, unknown heartbeat token, refused PAT on REST or MCP): `AUTH_FAILURES_PER_MINUTE_PER_IP = 60`, burst 30; once empty, requests from that IP that carry a credential get 429 before any database lookup for 60 s. The listener caps become config: `PUBLIC_MAX_CONNECTIONS`, `API_MAX_CONNECTIONS` (default 512 each, at most 65,536). The compose sets `nofile` to 65,536 for the engine.
- **R-PLT-24 Timing-safe auth.** `POST /auth/register` and `POST /auth/password/forgot` validate the request shape, enqueue a job on `auth::jobs` (a bounded `tokio::sync::mpsc` of `AUTH_JOB_QUEUE = 1000`) and answer the generic response at once; a worker does the lookup, the argon2 hash (in `spawn_blocking`), the insert and the email. A known email on register gets an "an account already exists, sign in or reset your password" email, so both paths send one mail. A full queue drops the job with a `warn` (the user retries) and the response is the same. Jobs are not durable (no tokens or passwords at rest); a restart loses queued jobs. Login runs `verify_password` against a constant `DUMMY_PASSWORD_HASH` (same argon2 parameters as `hash_password`, checked by a unit test) for an unknown email or an account without a password, so every failed login costs one argon2 verification.
- **R-PLT-25 Passwords.** New passwords (register, reset, change, invite accept) need at least `MIN_PASSWORD_CHARS = 12` characters and at most `MAX_PASSWORD_BYTES = 1024` bytes (bounds argon2 input). Existing shorter passwords keep working. This is not in the spec; it is the hardening floor this sub-project owns, and nothing in the repository sets one today.
- **R-PLT-26 TOTP lockout and replay.** `users.totp_failed_attempts` counts consecutive failures across `/auth/totp/verify`, TOTP re-auth and TOTP disable; the 5th sets `users.totp_locked_until = now() + 15 min` (`MAX_TOTP_FAILURES = 5`, `TOTP_LOCKOUT = 15 min`). While locked, every TOTP check answers 429 `too many attempts, try again later` without checking the code. A success resets the counter. `users.totp_last_step` stores the time step of the last accepted code (`check_current` returns it); a code for a step at or before it is refused, so an observed code cannot be replayed within its 90 s window. Per-IP limits on these routes stay as they are.
- **R-PLT-27 TOTP setup keeps 2FA on.** `POST /auth/totp/setup` writes `users.totp_pending_secret` and leaves `totp_secret`/`totp_enabled` untouched; `POST /auth/totp/enable` verifies a code against the pending secret and swaps it in. Today setup sets `totp_enabled = false`, so anyone holding a session could switch 2FA off. Setup when 2FA is already on needs re-auth (R-PLT-7 of Part 1).
- **R-PLT-28 New account endpoints.** `POST /auth/password/change {current_password, new_password}` (re-auth is the current password; an account without one gets 400 `this account signs in with <provider>; it has no password`) sets `password_changed_at` (other sessions end, per the existing staleness check) and refreshes the current session's `auth_at`. `POST /auth/totp/disable {reauth}` needs re-auth and is refused with 403 `two-factor authentication is required by your organization` while `require_2fa` is on. No "set a password for an OAuth account" feature (not in the spec).
- **R-PLT-29 OIDC.** `openidconnect 4.0.1` with `default-features = false` and an `AsyncHttpClient` adapter over the `outbound` client (Normal mode), so discovery, JWKS and token calls obey the policy; the same adapter replaces `oauth2::reqwest::Client` in the Google/GitHub flow. Discovery is fetched at the first login and cached for `OIDC_METADATA_TTL = 1 h`; the JWKS is re-fetched on an unknown `kid` at most once per minute. Scopes `openid email profile`. PKCE S256, a random `state` and `nonce`, stored in the session with the verifier and a timestamp, removed before any check (single use, at most `OIDC_FLOW_MAX_AGE = 10 min`). The ID token must have `iss` = the configured issuer, `aud` containing the client id (and `azp` = client id when `aud` has several values), `exp` in the future (`iat` not more than 5 min in the future), the session's `nonce`, and a JWKS signature with `alg` in {RS256, ES256}; `none` and HS* are refused. Identity = (`oidc`, `sub`). Email comes from the ID token's `email` claim; a token without one is refused (`the identity provider did not share an email address`). Linking an existing account requires `email_verified == true` and the account's email verified and equal (case-insensitive). Otherwise, `OIDC_AUTO_PROVISION=true` creates the user with global role `OIDC_DEFAULT_ROLE` (default `viewer`), limited to `OIDC_ALLOWED_DOMAINS` (comma list, empty = any), with `email_verified_at` set only when the claim is true; an email clash with an existing unlinked account is 409, never a silent link. Entra ID sends no `email_verified`: its users can be provisioned but never linked to an existing password account (documented). Redirect URI `{OAUTH_REDIRECT_BASE_URL}/auth/oidc/callback`. All OIDC settings come from the environment; OIDC is off unless `OIDC_ISSUER_URL` is set. `oauth_identities.provider` gains `'oidc'`.
- **R-PLT-30 2FA enforcement.** When `instance_settings.require_2fa` is true, `CurrentUser` answers 403 `{"error": "two-factor authentication is required", "code": "2fa_enrollment_required"}` for a user without `totp_enabled`, on sessions, PATs and MCP alike, except when `exempt_oidc_from_2fa` is true and the session was created by an OIDC login (session key `auth_method = "oidc"`) or, for a PAT or MCP request, the user has an `oidc` identity. Exempt endpoints (they do not use `CurrentUser`): `/auth/me` (gains `totp_enabled`, `two_factor_required`), `/auth/logout`, `/auth/totp/setup`, `/auth/totp/enable`. The settings are read in the same query as the user row (`cross join instance_settings`), so enforcement costs no extra round trip. Turning `require_2fa` on needs the acting admin to have TOTP enabled (400 otherwise), so an admin cannot lock everyone out including themselves.
- **R-PLT-31 Invites.** Spec table `project_invites`. Token: 32 random bytes as hex (the existing `auth::token::new_opaque_token`), stored SHA-256 (`bytea`), single use, `INVITE_TTL = 7 days`. Resend rotates the token and the expiry (the old link stops working) and re-sends the mail. Revoke sets `revoked_at`. At most one pending invite per (project, email) (partial unique index; a second → 409 "already invited, resend it instead"), at most `MAX_PENDING_INVITES_PER_PROJECT = 200`, and at most `INVITES_PER_INVITER_PER_HOUR = 20` (429). The inviter is a project admin (or a global admin, who may also invite with no project: `project_id` and `project_role` both null). The create response is the same whether or not the email has an account (no enumeration through invites). Accepting: with no session and no account for the email, the visitor sets a password (R-PLT-25) and the account is created with the email verified, the membership added and the session logged in; with a session whose verified email equals the invite's, the membership is added (an existing membership keeps the higher role); any other case is 403 `this invite is for another email address`. An invite that is expired, used or revoked answers 400 `this invite is no longer valid`, the same text for all three. Invite links are `{UI_BASE_URL}/invite/{token}`. Invites are accepted through their link only; OIDC sign-in does not consume them (OIDC deployments use auto-provisioning).
- **R-PLT-32 Registration mode.** `REGISTRATION_MODE=open|invite_only` (default `open`; `invite_only` in the production compose). In `invite_only`, `POST /auth/register` answers 403 `registration is closed; ask an administrator for an invite` and Google/GitHub sign-in of a new email is refused the same way; existing accounts sign in as before. The very first account of an empty instance can always register (bootstrap admin). `BOOTSTRAP_ADMIN_EMAIL` (optional; required by the production compose) restricts that first account to the given email, which closes the race of a stranger registering first on a fresh deployment.
- **R-PLT-33 Usage.** Global admins: `GET /admin/usage?days=1..=90` (default 30) answers per-day events and bytes for every log source, metric source, trace source and error/RUM application (from each ingest path's daily usage table), ClickHouse storage per table from `system.parts` (`active`, current database: rows, `data_compressed_bytes`, `data_uncompressed_bytes`, `bytes_on_disk`) and Postgres `pg_database_size(current_database())` plus the 10 largest tables (`pg_total_relation_size`). Project admins: `GET /projects/{id}/usage?days=` with their project's ingest rows only (no storage). Where a domain's ingest path has no daily counter yet (metric samples, spans, per the spec), NAMI-PLT-16 adds one in that domain with the Logs pattern (in-memory counters flushed by `logs::ingest::usage::run_forever`'s mechanism, one table per domain in that domain's phase file).
- **R-PLT-34 Security headers.** nami-ui sets CSP through SvelteKit's `csp` option (in the `sveltekit({...})` plugin options of `nami-ui/vite.config.ts`, where this repository's SvelteKit 3 config lives; verify the option name against `@sveltejs/kit` 3.0.x types at phase start) with `mode: 'nonce'` and directives `default-src 'self'; script-src 'self' 'nonce-...'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'`, plus `'unsafe-inline'` for `style-src-attr` only if Svelte's SSR output needs it (verified in the browser console during the task). nami-ui's `handle` hook adds `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=()`, `X-Content-Type-Options: nosniff`. Caddy sets `Strict-Transport-Security: max-age=31536000; includeSubDomains` for every response (TLS is Caddy's). The engine adds `X-Content-Type-Options: nosniff` to every response and `Cache-Control: no-store` to JSON API responses that do not set their own.
- **R-PLT-35 Body limits.** `tower_http::limit::RequestBodyLimitLayer` on the private API and REST with `API_MAX_BODY_BYTES = 1 MiB`, the monitor import route at `IMPORT_MAX_BODY_BYTES = 5 MiB` (4.1 MB content, Uptime Phase 3), `/mcp` at 1 MiB (Part 1), ingest routes unchanged (their own decoded-size budgets). Over the limit → 413 with the error shape, before the body is buffered. The JSON `Value` amplification of parallel import dry runs (Uptime Phase 3 UP-26) is bounded by the 5 MiB limit plus parsing the body only after the role check (the handler takes `Bytes`, checks the role, then parses).
- **R-PLT-36 CORS.** The engine sends no `Access-Control-*` header except on the routes whose specs open them on purpose: Sentry ingest and RUM ingest and config, with per-application allowed origins. The public status JSON and the RUM SDK files are served by nami-ui with the CORS headers their plans set (Status Pages R3, RUM R-RUM1-15); NAMI-PLT-17's nami-ui header hook leaves those routes' CORS as they are. A test sends an `OPTIONS` preflight to every OpenAPI operation and expects no `Access-Control-Allow-Origin`.
- **R-PLT-37 Cookies.** Every cookie Nami sets (session, OAuth/OIDC flow state, private status-page access, anything else found at phase start) is `HttpOnly`, `SameSite=Lax` (stated explicitly), `Path` explicit, and `Secure` when `COOKIE_SECURE=true`. The engine refuses to start with `COOKIE_SECURE=false` when `UI_BASE_URL` is `https://`. nami-ui's cookie relay keeps mirroring the engine's attributes.
- **R-PLT-38 Request logs.** No request log line holds a raw path or query. The engine's request span (added with JSON logs, R-PLT-40) records `MatchedPath` (the route template, e.g. `/api/v1/heartbeat/{token}`), method, status and latency; the query string is never logged (`sentry_key`, RUM client keys). nami-ui's `handleError` logs the route id, not the URL. Caddy writes no access log in the shipped `Caddyfile` (Caddy logs access only with a `log` directive); `docs/operations.md` gives a filtered `log` snippet that replaces the URI for operators who want one.
- **R-PLT-39 Graceful shutdown.** `platform::shutdown::Shutdown` wraps a `tokio_util::sync::CancellationToken`, cancelled on SIGTERM or SIGINT. Listeners stop accepting at once and call hyper's graceful shutdown on open connections (`hyper_util::server::graceful`), which lets in-flight requests finish for up to `SHUTDOWN_DRAIN = 25 s` (the compose `stop_grace_period` is 30 s). SSE tails and export streams end at cancellation. Workers (uptime, alerts, incidents, forwarding, reaper, scrape, AI) check the token between ticks and finish their current tick. The syslog batcher flushes its queued frames and the in-memory usage counters are flushed to Postgres before exit (Logs Part 1 LOG-4 deferral). Anything still running at the deadline is dropped and logged.
- **R-PLT-40 Health, readiness, logs and metrics.** `/health` on both listeners is liveness only (no database). `/ready` on the private API checks Postgres and ClickHouse (2 s each) and every worker loop running in this process (each loop bumps an `AtomicI64` on its own timer, Uptime Phase 1 NB-4; stale after 3 x its interval) and answers 200 or 503 with `{"postgres": "ok"|"down", "clickhouse": .., "workers": {name: "ok"|"stale"}}`. The engine binary gains `nami-engine healthcheck` (GETs `/ready` on the private port on loopback, exit code 0 or 1), because the distroless image has no curl. `LOG_FORMAT=json|text` (default `text`; `json` in the production compose) with `RUST_LOG` honoured. `GET /metrics` on the public listener in Prometheus text format, only when `METRICS_TOKEN` (at least 32 characters) is set (404 otherwise), with `Authorization: Bearer <METRICS_TOKEN>` compared in constant time (401 otherwise): HTTP requests by route template and status, request latency histogram, Postgres pool size and idle, ingest accepted/rejected counters, tail and export slots, worker last tick, audit write failures, forwarding lag. Hand-written exposition, no new crate.
- **R-PLT-41 Required configuration.** `Config::from_env` stops defaulting `CLICKHOUSE_URL`, `CLICKHOUSE_USER`, `CLICKHOUSE_PASSWORD`, `UI_BASE_URL` and `OAUTH_REDIRECT_BASE_URL` (Uptime Phase 1 deferral: required ClickHouse/UI config outside tests); `from_env_for_test` keeps the defaults and `.env.example` sets them. New: `API_BIND_ADDR` (default `127.0.0.1`, so the private API is loopback-only unless the operator opens it; the compose sets `0.0.0.0` inside its private network) and `PUBLIC_BIND_ADDR` (default `0.0.0.0`). The session-store pool uses `core::db::PoolSettings` (statement timeout included) instead of a bare 5-connection pool. `NAMI_ENCRYPTION_KEY` handling moves out of `Config` (R-PLT-42).
- **R-PLT-42 Encryption key and rotation.** `main` reads `NAMI_ENCRYPTION_KEY`, builds the `Cipher` and zeroizes the raw key; `Config` no longer holds it (Uptime Phase 2 UP-9). The `TEST_KEY` refusal moves into the key loader so every entry point gets it. Rotation without a key-id byte: optional `NAMI_ENCRYPTION_KEY_PREVIOUS`; `Cipher::decrypt` tries the current key, then the previous one (AES-GCM authentication tells which matched); `nami-engine reseal` re-encrypts every registered sealed column under the current key in batches (each domain lists its sealed columns, table, column, id column and AAD context pattern, in a `pub const SEALED_COLUMNS` slice; `platform::reseal` walks them all). After `reseal` reports zero rows left under the previous key, the operator removes the variable. The sealed format stays version `0x01`.
- **R-PLT-43 Heartbeat tokens at rest.** Heartbeat ping tokens move to the log-source pattern: `token_hash bytea` (SHA-256, unique) for lookup and `token_enc` sealed under `heartbeats.token:{id}` for display to members; the plaintext column is dropped by the migration after a backfill. Ping URLs do not change.
- **R-PLT-44 Containers.** Engine: builder `rust:<stable at phase start>-bookworm` with BuildKit cache mounts for the registry and `target`, `cargo build --release --locked`; runtime `gcr.io/distroless/cc-debian12:nonroot` (uid 65532, CA certificates included), the binary and `migrations/` only, `HEALTHCHECK` with `nami-engine healthcheck`. UI: builder `node:<LTS>-bookworm-slim` with `npm ci` and `npm run build` (adapter-node), runtime the same slim image as user `node`, `node build`, only `build/`, `package.json` and production `node_modules`. Base images pinned by digest. Ping monitors need `net.ipv4.ping_group_range` covering uid 65532: the compose sets `sysctls: net.ipv4.ping_group_range: "0 2147483647"` (Uptime Phase 2 UP-13 deferral).
- **R-PLT-45 Production compose and Caddy.** Services `caddy` (ports 80 and 443 the only published HTTP ports), `engine`, `ui`, `postgres`, `clickhouse`; no published database ports; `restart: unless-stopped`; health checks; named volumes (`postgres_data`, `clickhouse_data`, `caddy_data`, `caddy_config`); one private network with a fixed subnet (used for `TRUSTED_PROXIES`). Mailpit is gone; `SMTP_*` come from `.env`. Syslog ports (TCP/UDP 5140, TLS 6514) are published from the engine only when the operator uncomments them. Caddy sends the engine's public listener (`engine:8081`) exactly the prefixes in `core::router::PUBLIC_PREFIXES = ["/api/", "/v1/", "/mcp", "/metrics"]`, and everything else to `ui:3000`. Those cover every route a client outside reaches directly: REST (`/api/v1/projects/...`), OpenAPI (`/api/openapi.json`), log, metric and RUM ingest and RUM config (`/api/v1/...`), heartbeat pings (`/api/v1/heartbeat/{token}`), Sentry envelope and store (`/api/{dsn_id}/...`), the status-page RFC 8058 one-click unsubscribe (`/api/v1/status-subscriptions/one-click/{token}`), incident integrations and Slack interactivity (on the public listener under `/api/`; verify the merged paths), OTLP/HTTP (`/v1/traces`, `/v1/logs`, `/v1/metrics`, the standard exporter paths), MCP and `/metrics`. Public status pages, their JSON, feed and badge, the report form and the RUM SDK files are rendered or served by nami-ui (Status Pages R3, RUM R-RUM1-15), which reads the engine over the private API, so they go to `ui:3000`. Rule: every public-listener route starts with one of `PUBLIC_PREFIXES`, and nami-ui never adds a route under them (both are tested in NAMI-PLT-20). When Status Pages' custom domains are merged, `on_demand_tls` asks the engine's domain-check endpoint.
- **R-PLT-46 Authorization matrix.** Routes come from the OpenAPI document (every `api_routes()` operation) plus a fixed table of session-only and admin routes, MCP tools from `platform::mcp::TOOLS`. Each entry carries its minimum role (from the route inventory in the Part 1 decisions doc). Principals: anonymous, viewer, member, admin, admin of another project, global admin, PAT with lower `max_role`, PAT without the project in scope, user pending 2FA enrollment (with `require_2fa` on). The expected status is computed from the rules (401 anonymous, 403 below the minimum or out of scope or pending 2FA, 404 for a foreign id, otherwise the route's success class), and the test asserts it for every pair. A new route without a matrix entry fails the test.
- **R-PLT-47 Fuzzing without nightly.** No `cargo-fuzz` (needs nightly and a separate harness). An in-tree mutator (`tests/common/fuzz.rs`: seeded xorshift, bit flips, truncation, splicing, length-field and nesting inflation, invalid UTF-8, huge numbers) runs over a corpus built from each endpoint's fixtures, `FUZZ_ITERATIONS` per endpoint (default 2,000 in CI, more by env locally). Each run asserts: no panic, a 2xx/4xx answer (never 5xx), a response within the endpoint's deadline, and bounded memory where the logs memory test already measures it.

## Data model

`NNNN_auth_identity.sql`:

```sql
alter table oauth_identities drop constraint oauth_identities_provider_check;
alter table oauth_identities add constraint oauth_identities_provider_check
    check (provider in ('google', 'github', 'oidc'));

alter table users
    add column totp_pending_secret text,
    add column totp_failed_attempts integer not null default 0,
    add column totp_locked_until timestamptz,
    add column totp_last_step bigint,
    add constraint users_totp_failed_attempts_range check (totp_failed_attempts between 0 and 1000);

create table project_invites (                            -- spec shape, names and indexes added
    id uuid primary key default gen_random_uuid(),
    project_id uuid references projects(id) on delete cascade,   -- null = no project (global admin only)
    email text not null,
    project_role text,
    token_hash bytea not null,
    invited_by uuid not null references users(id) on delete cascade,
    expires_at timestamptz not null,
    accepted_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz not null default now(),
    constraint project_invites_token_hash_key unique (token_hash),
    constraint project_invites_role_check check (project_role in ('admin', 'member', 'viewer')),
    constraint project_invites_project_role_pair check ((project_id is null) = (project_role is null)),
    constraint project_invites_email_lower check (email = lower(email) and char_length(email) <= 320)
);
create unique index project_invites_one_pending
    on project_invites (coalesce(project_id, '00000000-0000-0000-0000-000000000000'::uuid), email)
    where accepted_at is null and revoked_at is null;
create index project_invites_inviter_idx on project_invites (invited_by, created_at desc);
```

The baseline's provider check is unnamed today (`check (provider in ('google', 'github'))`); the migration drops it by its Postgres-generated name, which the implementer reads from `\d oauth_identities` at phase start (`oauth_identities_provider_check` is the Postgres default for a column check).

`NNNN_uptime_heartbeat_token_hash.sql` and `NNNN_logs_source_names.sql`: see NAMI-PLT-19.

## Deferred items inventory

Every item the decisions docs (as of 2026-10-24) send to Platform, NAMI-PLT or "hardening", and where it lands. "Verify" means: check whether a later phase already fixed it; if so, close it in the decisions doc with the commit; if not, fix it in the named task. Items in Part 1's scope are listed for completeness.

| # | Item | Source | Lands in |
|---|---|---|---|
| 1 | Crate-wide `cargo fmt` with the CI task | Uptime P1 rulings | NAMI-PLT-1 (done in Part 1) |
| 2 | `/checks` returns the ClickHouse row type; DTO split when the REST API is documented; extra ids per row | Uptime P1 T5 | NAMI-PLT-4 (done in Part 1) |
| 3 | One global `JsonRejection` mapper; `request_headers` echoed in the 422 body | Uptime P2 UP-10 | NAMI-PLT-4 (done in Part 1, R-PLT-11) |
| 4 | Per-project monitor count cap; import amplifier quotas | Uptime P2, P3 UP-26 | NAMI-PLT-4 (caps, done in Part 1); NAMI-PLT-21 (spread the first `next_check_at` of imported monitors over their interval) |
| 5 | Audit on source token reveal/rotate | Logs P1 LOG-1 | NAMI-PLT-3 (done in Part 1) |
| 6 | SSRF opt-in allowlist for monitors, `MONITOR_DENY_CIDRS`, banner grabbing of private networks | Uptime P1 rulings, P2 | NAMI-PLT-10 (`OUTBOUND_*`, R-PLT-21) |
| 7 | RDAP redirect chain may point at private addresses | Uptime P2 | NAMI-PLT-10 (RDAP client under the policy, every hop re-resolved) |
| 8 | Proxy residuals, `MONITOR_PROXY_ALLOWLIST` | Uptime P3 | NAMI-PLT-10 (R-PLT-21: proxy address checked, CONNECT target residual accepted) |
| 9 | IPv4-compatible `::a9fe:a9fe` | Uptime P1 final | NAMI-PLT-10 |
| 10 | Shared outbound client under the policy (incidents, metrics, AI specs) | Incidents, Metrics, AI SRE specs | NAMI-PLT-10 |
| 11 | Trusted-proxy check for `X-Forwarded-For`; rate limit on public ping after it | Foundation (R11.13), Uptime P1 T6 | NAMI-PLT-11 |
| 12 | Per-IP limits on the public listener; per-IP connection limiter; configurable connection caps and fd limits | Logs P1 LOG-3 and final, Uptime P3, Logs P2 R-P2-22 | NAMI-PLT-11 (caps, per-peer limit, failure buckets); NAMI-PLT-20 (`nofile` in the compose) |
| 13 | 2FA-change re-auth, timing-safe auth responses, per-user TOTP attempt limiting | Foundation (R11.13) | NAMI-PLT-12 |
| 14 | Expired session/token reapers | Foundation (R11.13) | NAMI-PLT-18 |
| 15 | Graceful shutdown; queued syslog frames lost on exit | Logs P1 LOG-4 and final, Logs P2 R-P2-22 | NAMI-PLT-18 |
| 16 | `/health` worker liveness; liveness on its own timer (NB-4) | Uptime P1 final | NAMI-PLT-18 |
| 17 | Required ClickHouse/UI config outside tests | Uptime P1 final | NAMI-PLT-18 |
| 18 | `BIND_ADDR` / loopback default for `PORT` | Uptime P1 final | NAMI-PLT-18 (`API_BIND_ADDR`, R-PLT-41) |
| 19 | Session-store pool settings and statement timeout | Uptime P1 final | NAMI-PLT-18 |
| 20 | App-wide security headers hook | Uptime P1 T8 | NAMI-PLT-17 |
| 21 | Production adapter choice (`adapter-node` body limit) | Uptime P3 UP-31 | NAMI-PLT-20 |
| 22 | `net.ipv4.ping_group_range` for the engine container | Uptime P2 UP-13 | NAMI-PLT-20 |
| 23 | Pre-lock JSON amplification by parallel dry runs; body parsed before the role check; global body budget | Uptime P3 UP-26 | NAMI-PLT-17 (R-PLT-35) |
| 24 | Unmatched public routes answer an empty 404 without `no-store`; public-router prune scan under pathological load | Uptime P3 UP-20 | NAMI-PLT-17 (JSON 404 fallback with `no-store` on both listeners); NAMI-PLT-21 (measure the prune scan, fix only with a number) |
| 25 | `Config.encryption_key` cloned with every `AppState`; zeroize; `TEST_KEY` refusal only in `main`; key rotation / key id | Uptime P2 UP-9 | NAMI-PLT-19 (R-PLT-42) |
| 26 | Heartbeat token hash + seal (log-source pattern) | Uptime P1 final | NAMI-PLT-19 (R-PLT-43) |
| 27 | Unique source names; explicit column lists instead of `select *`; rate limit on reveal/rotate | Logs P1 LOG-1 | NAMI-PLT-19 |
| 28 | TLS syslog | Logs P1 LOG-4 | NAMI-PLT-21 |
| 29 | Per-instance global tail cap | Logs P1 LOG-6 | NAMI-PLT-21 (verify: `logs::query::tail::MAX_TAILS_GLOBAL` exists on `main` at writing) |
| 30 | Non-http monitors silently drop request config; invalid-header-name error lacks the index; URL credentials plus a custom `Authorization` header accepted | Uptime P2 UP-10, UP-11 | NAMI-PLT-21 (400 for each) |
| 31 | Missing IPv6 / concurrent-ping / ICMP-not-permitted tests | Uptime P2 UP-13 | NAMI-PLT-21 |
| 32 | UDP test hygiene (binary-safe name, foreign-source datagram test), lossy UTF-8 keyword match and 16 KiB cut, TLS decimal hosts refused as invalid server name | Uptime P2 UP-15 | NAMI-PLT-21 |
| 33 | PATCH cannot null-clear some window fields; weekly-window SQL prefilter; concurrent quota test | Uptime P2 UP-16 | NAMI-PLT-21 |
| 34 | PATCH-join test, weak app-level 404 tests, `name: null` keeps the name | Uptime P2 UP-17 | NAMI-PLT-21 (`name: null` → 400; group-name uniqueness stays a product choice: not unique) |
| 35 | Weak render tests, typed body lost on a method toggle, cosmetic indentation | Uptime P2 UP-18 | NAMI-PLT-21 (UI) |
| 36 | The 2 s ClickHouse timeout arm untested | Uptime P3 UP-21 | NAMI-PLT-21 |
| 37 | Drop empty-value Kuma headers on import | Uptime P3 UP-25 | NAMI-PLT-21 |
| 38 | Viewers read ping output and incident details while tokens are member-only | Uptime P3 | NAMI-PLT-21: verify whether Incident Management ruled; if not, ruling "viewers keep read access (output is sanitized and capped; the viewer role exists to read incidents)" recorded in the decisions doc |
| 39 | Proxied and cookie checks rebuild TLS state per check; ~10 checks/s ceiling; DNS thread bound; tcp happy-eyeballs | Uptime P1 T3, T4, P3 | NAMI-PLT-21 (measure first per STANDARDS §7; change only with a number) |
| 40 | NB-3 alert task place-holding | Uptime P1 final | NAMI-PLT-21 (verify) |
| 41 | Phase 1 "uptime security wave" minors (invisible-character set, control characters in checker errors, doc wording, OAuth provider raw param, role-aware hiding, DOM test harness, stale data on failed refresh, contrast) | Uptime P1 T2..T8 | NAMI-PLT-21 (verify each; most were fixed by the Phase 1-3 waves) |
| 42 | Items that later decisions docs (Incidents, Status pages, Metrics, Traces, Errors, RUM, AI, Platform Part 1) send to Platform | re-grep at phase start | NAMI-PLT-21, unless a more specific task above fits |

Not carried: "an upload store, if ever wanted" (Status Pages Part 1) stays out (no need was found); folding `search::QuerySlots` and `slots::UserSlots` (Logs Part 2 R-P2-15) belongs to the Logs Part 2 security wave; STARTTLS for mail checks is feature backlog, not security (Uptime P2).

## Tasks

### Task 10 (NAMI-PLT-10): one outbound (SSRF) policy for every server-side request

**Covers:** R11.13  **Depends on:** Part 1 merged.

**Files:**
- Create: `docs/superpowers/decisions/2026-10-24-platform-part2.md` (first commit), `nami-engine/tests/core/outbound.rs`.
- Modify: `nami-engine/src/core/outbound.rs` (from NAMI-INC-3), `nami-engine/src/core/config.rs` (`outbound_allow_private_networks`, `outbound_deny_cidrs: Vec<IpNet>`), `nami-engine/Cargo.toml` (`ipnet`), `nami-engine/.env.example`, `nami-engine/src/uptime/checker/` (every check type, the proxy path, `HttpClients`, `is_forbidden_destination` moves into `core::outbound` and `uptime::checker` calls it), the RDAP client, every caller found by the grep in Review Focus (incident channels and tickets, status-page subscribers, scrape targets, the AI provider, audit forwarding, Telegram), `nami-engine/src/auth/routes.rs` and `nami-engine/src/auth/oauth.rs` (the Google/GitHub token and userinfo calls stop using `oauth2::reqwest::Client` and `reqwest::Client::new()`).

**Data shape:** `pub enum Mode { Normal, Strict }`; `pub struct Rules { allow_private: bool, deny: Arc<[IpNet]> }` built once from config; `pub fn check_ip(ip: IpAddr, mode: Mode, rules: &Rules) -> Result<(), DestinationNotAllowed>`; `pub fn client(mode: Mode) -> reqwest::Client` (cached per mode, DNS through the guarded hickory resolver, `.no_proxy()`); `pub struct OidcHttp` (the `openidconnect`/`oauth2` `AsyncHttpClient` adapter over `client(Mode::Normal)`, used from NAMI-PLT-13 and by the OAuth flow here).

**Behaviour:**
1. Classification per R-PLT-21, unit-tested address by address (each always-refused range in plain, mapped, compatible and NAT64 form; each private range; deny CIDRs; public addresses).
2. `Normal` honours `OUTBOUND_ALLOW_PRIVATE_NETWORKS` and the deny list; `Strict` refuses private and denied addresses always.
3. The resolver filters every answer and hands reqwest only vetted addresses; a name whose every address is refused fails with `destination not allowed` (the existing error text, so check history and timelines keep their wording).
4. Non-HTTP checks and the syslog-free paths resolve, call `check_ip` per address, and connect to the vetted `SocketAddr`.
5. A grep test (`no_outbound_request_bypasses_the_policy`) fails on `reqwest::Client::new(`, `reqwest::Client::builder(` outside `core/outbound.rs`, and `oauth2::reqwest`.

**Tests** (`tests/core/outbound.rs`, unit tests in `core::outbound`, plus one test per caller in its domain target):
- `private_targets_are_refused_when_private_networks_are_off`, `private_targets_pass_by_default`, `a_deny_cidr_is_refused_in_normal_mode`, `metadata_and_link_local_are_refused_even_when_private_is_allowed` (incl. `::a9fe:a9fe`).
- `dns_rebinding_cannot_reach_a_refused_address` (a stub DNS server answers public then private; the connection goes to the vetted address only), `a_redirect_to_a_refused_address_is_refused`.
- `subscriber_webhooks_use_strict_mode_regardless_of_the_setting`, `personal_channels_use_strict_mode`.
- One per caller: monitors (each check type), proxies, RDAP, channels, tickets, scrape targets, the LLM provider, audit forwarding, Google/GitHub OAuth, OIDC discovery.
- `no_outbound_request_bypasses_the_policy` (grep test).

**Acceptance:** every outbound call site is listed in the decisions doc with its mode; tests pass with `OUTBOUND_ALLOW_PRIVATE_NETWORKS` both ways.

### Task 11 (NAMI-PLT-11): trusted proxies, per-IP limits and configurable connection caps

**Covers:** R11.13  **Depends on:** NAMI-PLT-10.

**Files:**
- Create: `nami-engine/src/core/client_ip.rs`, `nami-engine/tests/core/client_ip.rs`.
- Modify: `nami-engine/src/core/router.rs` (the governor uses a `TrustedProxyIp` key extractor instead of `SmartIpKeyExtractor`; `with_fallback_connect_info` keeps working for tests), `nami-engine/src/core/serve.rs` (per-peer connection cap; limits from config), `nami-engine/src/core/config.rs` (`trusted_proxies`, `public_max_connections`, `api_max_connections`), `nami-engine/src/platform/audit/` (`ClientMeta` from `client_ip`), the public ingest, heartbeat, REST and MCP auth-failure paths (per-IP failure buckets), `nami-engine/.env.example`, `nami-ui/src/lib/server/core/engine.ts` (comment: the forwarded address is trusted only from `TRUSTED_PROXIES`).

**Data shape:** `pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr` (pure); `pub struct TrustedProxyIp { trusted: Arc<[IpNet]> }` implementing `tower_governor::key_extractor::KeyExtractor`; `AuthFailureLimiter` (per-IP `TokenBuckets`); constants `MAX_CONNECTIONS_PER_PEER = 64`, `AUTH_FAILURES_PER_MINUTE_PER_IP = 60`, `AUTH_FAILURE_BURST = 30`, `MAX_CONFIGURED_CONNECTIONS = 65_536`.

**Behaviour:**
1. `client_ip` per R-PLT-22, on both listeners.
2. The per-peer cap and failure buckets per R-PLT-23. The failure bucket is charged on a refused credential and checked before the next credential lookup from that IP.
3. Invalid `TRUSTED_PROXIES` or a cap outside `1..=65536` stops startup with a message naming the variable.

**Tests:**
- Unit: `an_untrusted_peer_is_the_client_whatever_the_header_says`, `a_trusted_peer_yields_the_rightmost_untrusted_forwarded_address`, `a_chain_of_trusted_proxies_is_skipped`, `x_real_ip_and_forwarded_are_ignored`, `garbage_in_x_forwarded_for_falls_back_to_the_peer`.
- `login_rate_limits_follow_the_trusted_client_address`, `a_forged_forwarded_header_from_an_untrusted_peer_does_not_move_the_bucket`.
- `sixty_four_connections_from_one_peer_pass_and_the_sixty_fifth_is_closed`, `a_trusted_proxy_is_exempt_from_the_per_peer_cap`.
- `unknown_ingest_tokens_from_one_ip_hit_429_before_the_database`, `known_tokens_from_the_same_ip_still_work_after_its_failures_age_out`.
- Unit: config parsing of the caps and CIDR lists (bounds and one past).

**Acceptance:** behind a local Caddy (manual check, noted in the PR) the audit log shows the real client IP, and limits key on it.

### Task 12 (NAMI-PLT-12): auth hardening: re-auth, password change, 2FA disable, timing-safe responses, TOTP lockout

**Covers:** R11.13, R11.04 (events)  **Depends on:** NAMI-PLT-11.

**Files:**
- Create: `nami-engine/src/auth/jobs.rs`, `nami-engine/migrations/NNNN_auth_identity.sql` (TOTP columns; NAMI-PLT-13 and NAMI-PLT-15 append), `nami-engine/tests/auth/hardening.rs`, `nami-engine/tests/auth/totp_lockout.rs`.
- Modify: `nami-engine/src/auth/routes.rs` (register and forgot through jobs, login dummy hash, `/auth/password/change`, `/auth/totp/disable`, setup with pending secret, `/auth/me` fields), `nami-engine/src/auth/password.rs` (`DUMMY_PASSWORD_HASH`, `spawn_blocking` wrappers, length rules), `nami-engine/src/auth/totp.rs` (step-aware verify), `nami-engine/src/auth/reauth.rs` (TOTP failures count toward the lockout), `nami-engine/src/auth/user.rs`, `nami-engine/src/main.rs` (spawn the auth job worker).

**Data shape:** `pub enum AuthJob { Register { email: String, password: SecretString }, Forgot { email: String } }` (manual `Debug` without the password); `AUTH_JOB_QUEUE = 1000`; `MIN_PASSWORD_CHARS`, `MAX_PASSWORD_BYTES`, `MAX_TOTP_FAILURES`, `TOTP_LOCKOUT`. Requests: `{"current_password", "new_password"}` (change), `{"reauth": Reauth}` (disable, and setup when 2FA is on). `/auth/me` adds `totp_enabled: bool`, `two_factor_required: bool` (filled from NAMI-PLT-14).

**API:**
- `POST /auth/password/change` (session) → 200; 400 (rules, no password on the account); 403 `reauth_required` (wrong current password); 429.
- `POST /auth/totp/disable` (session) → 200; 403 `reauth_required` or the organization rule; 429 locked.
- `POST /auth/totp/setup` → 200 `{provisioning_uri}`; 403 `reauth_required` when 2FA is already on and no valid re-auth is given.
- `POST /auth/register`, `POST /auth/password/forgot` → the same generic 200 as today, whatever happens.

**Behaviour:** R-PLT-24 to R-PLT-28. Events: `auth.password.changed`, `auth.totp.disabled`, `auth.totp.enabled` (already from Part 1). argon2 work everywhere in `spawn_blocking`.

**Tests:**
- `register_answers_the_same_bytes_for_new_and_existing_emails`, `register_sends_one_mail_on_both_paths`, `forgot_password_answers_the_same_bytes_for_known_and_unknown_emails`, `a_full_job_queue_still_answers_the_generic_response`.
- `login_for_an_unknown_email_runs_one_argon2_verification` (via a counting hook in `auth::password` compiled for tests), unit `the_dummy_hash_uses_the_same_parameters_as_hash_password`.
- `password_change_needs_the_current_password`, `password_change_ends_other_sessions_and_keeps_this_one`, `an_oauth_only_account_cannot_change_a_password`, `a_password_of_11_chars_is_400_and_12_is_accepted`, `a_password_over_1024_bytes_is_400`.
- `totp_setup_does_not_disable_existing_2fa`, `totp_setup_with_2fa_on_needs_reauth`, `totp_disable_needs_reauth`.
- `five_wrong_totp_codes_lock_for_fifteen_minutes`, `a_locked_account_gets_429_even_with_the_right_code`, `a_success_resets_the_counter`, `a_code_cannot_be_used_twice`, `reauth_totp_failures_count_toward_the_lockout`.

**Acceptance:** all Foundation auth tests still pass, plus the above.

### Task 13 (NAMI-PLT-13): generic OIDC login

**Covers:** R11.06, R11.04 (events)  **Depends on:** NAMI-PLT-12.

**Files:**
- Create: `nami-engine/src/auth/oidc.rs`, `nami-engine/tests/auth/oidc.rs`, `nami-engine/tests/common/oidc_provider.rs` (a wiremock-based provider: discovery, JWKS, token endpoint; signs ID tokens with RSA and EC test keys through `openidconnect`'s signing types), `nami-ui/src/routes/auth/oidc/start/+server.ts`, `nami-ui/src/routes/auth/oidc/callback/+server.ts`.
- Modify: `nami-engine/Cargo.toml` (`openidconnect`), `nami-engine/src/core/config.rs` (`OIDC_ISSUER_URL`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET`, `OIDC_DISPLAY_NAME`, `OIDC_AUTO_PROVISION`, `OIDC_DEFAULT_ROLE`, `OIDC_ALLOWED_DOMAINS`; the client secret held as `SecretString`), `nami-engine/src/auth/routes.rs` (routes, `auth_method` session key on every login path), `nami-engine/src/auth/user.rs` (link or provision), `nami-engine/migrations/NNNN_auth_identity.sql` (append the provider check change), `nami-engine/.env.example`, `nami-ui/src/routes/login/+page.server.ts` and `+page.svelte` (the "Sign in with <display name>" button when enabled).

**API:** `GET /auth/oidc/config` → 200 `{"enabled": bool, "display_name": string | null}` (public, for the login page); `GET /auth/oidc/start` → 303 to the provider (404 when OIDC is off); `GET /auth/oidc/callback?code=&state=` → 200 `{"authenticated": true}` or 400 with one generic message `sign-in failed` for every validation failure (the reason is logged at `warn` without token contents) or 409 for an email clash, 403 for a disallowed domain or closed registration. The BFF routes pass these through like the Google/GitHub ones.

**Behaviour:** R-PLT-29. Session keys `oidc_state`, `oidc_nonce`, `oidc_pkce`, `oidc_started_at` are removed at the start of the callback. A successful login cycles the session id, sets `user_id`, `auth_at`, `auth_method = "oidc"`. Events: `auth.login.succeeded` (method oidc), `auth.oidc.linked` (when an existing account is linked), `auth.login.failed` (anonymous) for refused tokens.

**Tests** (mock provider):
- `a_valid_rs256_and_a_valid_es256_id_token_sign_in`.
- `a_wrong_issuer_is_refused`, `a_wrong_audience_is_refused`, `an_extra_audience_without_matching_azp_is_refused`, `an_expired_id_token_is_refused`, `alg_none_is_refused`, `hs256_signed_with_the_public_key_is_refused`, `an_unknown_kid_refetches_jwks_once_then_fails`.
- `a_replayed_state_is_refused`, `a_replayed_nonce_is_refused`, `a_flow_older_than_ten_minutes_is_refused`, `the_pkce_verifier_is_sent`.
- `an_unverified_email_never_links_an_existing_account`, `a_verified_matching_email_links_once`, `the_same_sub_signs_in_to_the_same_user_after_an_email_change_at_the_idp`.
- `auto_provision_off_refuses_unknown_users`, `auto_provision_creates_a_viewer_by_default`, `allowed_domains_are_enforced_case_insensitively`, `an_email_clash_is_409_not_a_link`, `a_token_without_email_is_refused`.
- `discovery_goes_through_the_outbound_policy` (a private issuer with private networks off is refused).

**Acceptance:** the mock-provider suite passes; manual sign-ins against Keycloak, Authentik and Entra ID are recorded in the decisions doc (spec), with any provider quirk found.

### Task 14 (NAMI-PLT-14): org-wide 2FA enforcement and the instance security settings

**Covers:** R11.07, R11.04 (events)  **Depends on:** NAMI-PLT-13.

**Files:**
- Create: `nami-engine/src/auth/enforcement.rs`, `nami-engine/tests/auth/enforcement.rs`.
- Modify: `nami-engine/src/core/middleware/rbac.rs` (`CurrentUser` loads `require_2fa`/`exempt_oidc_from_2fa` with the user and applies the rule, for sessions and tokens), `nami-engine/src/platform/settings.rs` (read and update), `nami-engine/src/platform/mcp/` (the 403 maps to an HTTP 403 with a JSON-RPC error body), `nami-engine/src/auth/routes.rs` (`/auth/me` fields).

**Data shape:** `instance_settings.require_2fa`, `exempt_oidc_from_2fa` (Part 1 columns). `SecuritySettings { require_2fa: bool, exempt_oidc_from_2fa: bool, users_without_2fa: integer }`. Update request `{"require_2fa"?: bool, "exempt_oidc_from_2fa"?: bool, "reauth": Reauth}`.

**API:** `GET /admin/settings/security` (global admin, session) → 200; `PUT /admin/settings/security` → 200; 400 `enable two-factor authentication on your own account first`; 403 `reauth_required`.

**Behaviour:** R-PLT-30. The 403 body is the same on the private API, REST and MCP. Event `instance.settings.changed` (metadata: changed keys and new values).

**Tests:**
- `with_enforcement_on_a_user_without_totp_gets_2fa_enrollment_required_on_a_project_route`, `on_rest_with_a_pat`, `on_mcp_with_a_pat`.
- `me_logout_setup_and_enable_stay_reachable_while_pending`, `enrolling_lifts_the_block_at_once`.
- `oidc_sessions_are_exempt_only_when_the_exemption_is_on`, `a_pat_of_an_oidc_linked_user_follows_the_exemption`, `a_password_session_of_an_oidc_linked_user_is_not_exempt`.
- `an_admin_without_totp_cannot_turn_enforcement_on`, `disabling_totp_is_refused_while_enforcement_is_on`, `only_global_admins_change_security_settings`, `changes_need_reauth_and_are_audited`.

**Acceptance:** enforcement on: an unenrolled user can do nothing but enroll, log out and read `/auth/me`, through any credential.

### Task 15 (NAMI-PLT-15): invites by email and the invite-only registration mode

**Covers:** R11.08, R11.04 (events)  **Depends on:** NAMI-PLT-14.

**Files:**
- Create: `nami-engine/src/auth/invite/{mod,model,routes}.rs`, `nami-engine/tests/auth/invites.rs`.
- Modify: `nami-engine/migrations/NNNN_auth_identity.sql` (append `project_invites`), `nami-engine/src/core/config.rs` (`REGISTRATION_MODE`, `BOOTSTRAP_ADMIN_EMAIL`), `nami-engine/src/auth/routes.rs` and `nami-engine/src/auth/user.rs` (registration closed in `invite_only`, bootstrap rule, OAuth new-user refusal), `nami-engine/src/core/router.rs`, `nami-engine/.env.example`, `nami-engine/src/core/email.rs` callers (invite mail through `auth::jobs` or the mailer queue).

**Data shape:** table `project_invites` (Data model). `InviteView { id, email, project_id, project_role, invited_by_email, expires_at, accepted_at, revoked_at, created_at }`. Create `{"email", "project_role"}` on a project path, or `{"email"}` on the global path. Public preview `{"email", "project_name" | null, "project_role" | null, "inviter_email", "expires_at", "account_exists_for_session": bool}`; accept `{"password"?: string}`.

**API:**
- `POST /projects/{id}/invites` (project admin) → 201 `InviteView`; 400 (email shape, role); 409 already pending, or 200 pending invites in the project; 429 per inviter. `GET /projects/{id}/invites` (project admin) → 200 pending and recent. `POST /projects/{id}/invites/{invite_id}/resend` → 200; `DELETE /projects/{id}/invites/{invite_id}` → 204 (revoke); 404 foreign ids.
- `POST /admin/invites` (global admin, no project) → 201; `GET /admin/invites`; resend and revoke the same way.
- `GET /invites/{token}` (no auth, per-IP governor) → 200 preview; 400 `this invite is no longer valid`.
- `POST /invites/{token}/accept` (per-IP governor) → 200 `{"authenticated": true, "project_id"}`; 400 invalid, or password rules; 403 another email.
- Tokens: these routes are project-scoped where they have a project and are in `api_routes()`; the global and public ones are not.

**Behaviour:** R-PLT-31 and R-PLT-32. The invite mail goes through a queue, so the create response time does not depend on the mail server. Events: `invite.created`, `invite.resent`, `invite.revoked`, `invite.accepted` (actor = the accepting user).

**Tests:**
- `an_invite_registers_a_new_account_with_a_verified_email_and_the_membership`, `a_logged_in_user_with_the_invited_email_gets_the_membership`, `an_existing_higher_role_is_kept`.
- `an_expired_invite_is_400`, `a_used_invite_is_400`, `a_revoked_invite_is_400`, `the_three_give_the_same_message`, `another_users_session_is_403`, `a_resent_invite_kills_the_old_link`.
- `the_create_response_is_identical_for_registered_and_unregistered_emails`, `a_second_pending_invite_for_the_same_email_is_409`, `twenty_invites_an_hour_pass_and_the_twenty_first_is_429`, `two_hundred_pending_invites_pass_and_the_next_is_409`.
- `a_member_cannot_invite`, `a_project_admin_cannot_invite_without_a_project`, `a_global_admin_invites_without_a_project`.
- `invite_only_closes_register_and_new_oauth_signups`, `existing_accounts_still_sign_in_under_invite_only`, `the_first_account_can_register_under_invite_only`, `bootstrap_admin_email_restricts_the_first_account`.

**Acceptance:** under `REGISTRATION_MODE=invite_only` a new person can join only through an invite link (or OIDC auto-provisioning when enabled).

### Task 16 (NAMI-PLT-16): usage endpoints

**Covers:** R11.09  **Depends on:** NAMI-PLT-15.

**Files:**
- Create: `nami-engine/src/platform/usage.rs`, `nami-engine/tests/platform/usage.rs`.
- Modify: the metrics and traces ingest paths, if they have no daily usage counter yet (R-PLT-33; each adds its table to its own domain's phase file and its counters to the existing flush loop), `nami-engine/src/core/router.rs`.

**Data shape:** `UsageReport { days: [{ day, items: [{ kind: "log_source" | "metric_source" | "trace_source" | "error_app" | "rum_app", id, name, project_id, events, bytes }] }], storage: { clickhouse: [{ table, rows, compressed_bytes, uncompressed_bytes, bytes_on_disk }], postgres: { database_bytes, largest_tables: [{ table, bytes }] } } }`; the project variant omits `storage`. `MAX_USAGE_DAYS = 90`.

**API:** `GET /admin/usage?days=` (global admin, session) → 200; `GET /projects/{id}/usage?days=` (project admin; in `api_routes()`) → 200; 400 for `days` outside `1..=90`.

**Behaviour:** R-PLT-33. ClickHouse reads use `READ_SETTINGS` and the client caps. Postgres size queries run under the pool's statement timeout. Sources and applications deleted since keep their rows (shown by id).

**Tests:** `admins_see_volume_per_source_and_application_per_day`, `storage_lists_every_nami_table_from_system_parts` (test database), `postgres_sizes_are_reported`, `a_project_admin_sees_only_their_project_and_no_storage`, `a_member_is_403`, `ninety_days_pass_and_ninety_one_are_400`.

**Acceptance:** the numbers on a seeded test database match the inserted counters.

### Task 17 (NAMI-PLT-17): security headers, CSP, body limits, closed CORS, cookies and request-log redaction

**Covers:** R11.12, R11.13  **Depends on:** NAMI-PLT-16.

**Files:**
- Create: `nami-ui/src/hooks.server.ts` (headers; `handleError` logging the route id), `nami-ui/src/lib/server/core/headers.ts` (+ test), `nami-engine/tests/core/headers.rs`.
- Modify: `nami-ui/vite.config.ts` (`csp`), `nami-engine/Cargo.toml` (`tower-http` feature `limit`), `nami-engine/src/core/router.rs` (body limit layers, `nosniff`, default `no-store` on JSON, JSON 404 fallback with `no-store` on both listeners), the import route (takes `Bytes`, role check, then parse), every place that sets a cookie (engine and nami-ui), `nami-engine/src/core/config.rs` (the `COOKIE_SECURE` consistency check).

**Behaviour:** R-PLT-34 to R-PLT-38. The CSP is checked page by page in the browser (every route group: auth pages, app pages, status pages, the API reference) and any violation fixed at its source rather than loosened.

**Tests:**
- vitest: `every_response_gets_the_security_headers`, `the_csp_has_a_nonce_and_frame_ancestors_none`, `handle_error_logs_no_url`.
- Engine: `a_body_over_one_mib_is_413_before_it_is_read`, `an_import_of_five_mib_is_accepted_and_one_byte_more_is_413`, `the_import_role_check_runs_before_parsing` (a viewer's 5 MiB invalid JSON gets 403, not 400), `no_openapi_operation_answers_a_cors_preflight`, `sentry_and_rum_ingest_keep_their_cors`, `every_cookie_is_httponly_samesite_and_secure_in_production`, `an_unmatched_public_path_is_a_json_404_with_no_store`, `request_logs_hold_the_route_template_not_the_token` (captures `tracing` output of a heartbeat ping and an ingest call with `sentry_key` in the query).
- Config unit test: `cookie_secure_false_with_an_https_ui_stops_startup`.

**Acceptance:** a headers scan of every page group (manual, `curl -I` and the browser console, noted in the PR) shows the headers and no CSP violation.

### Task 18 (NAMI-PLT-18): production runtime: graceful shutdown, health, readiness, JSON logs, metrics, reapers, required config

**Covers:** R11.10 (runtime), R11.13 (reapers)  **Depends on:** NAMI-PLT-17.

**Files:**
- Create: `nami-engine/src/platform/shutdown.rs`, `nami-engine/src/platform/metrics.rs`, `nami-engine/tests/platform/runtime.rs`.
- Modify: `nami-engine/src/main.rs` (signal handling, the `healthcheck` and `reseal` subcommands dispatch, `LOG_FORMAT`, bind addresses, session-store pool settings and `continuously_delete_expired` from `tower-sessions-sqlx-store`), `nami-engine/src/core/serve.rs` (graceful drain), `nami-engine/src/core/health.rs` (`/health` liveness, `/ready`), every worker loop and stream (shutdown token, liveness counter), `nami-engine/src/logs/syslog/mod.rs` (flush on shutdown), `nami-engine/src/logs/ingest/usage.rs` (final flush), `nami-engine/src/platform/reaper.rs` (more jobs), `nami-engine/src/core/config.rs` (R-PLT-41), `nami-engine/Cargo.toml` (`tokio-util`, `tracing-subscriber` features, `hyper-util` feature), `nami-engine/.env.example`.

**Data shape:** `Shutdown` (clonable, `cancelled()` future, `is_cancelled()`); `Liveness` (per-loop `AtomicI64` of the last tick, registered by name); `SHUTDOWN_DRAIN = 25 s`; `READY_CHECK_TIMEOUT = 2 s`; reaper jobs and retention constants: sessions (the store's own deletion, every 1 h), `email_verification_tokens` and `password_reset_tokens` consumed or expired more than `TOKEN_RETENTION = 7 days` ago, `project_invites` accepted, revoked or expired more than 30 days ago, `api_tokens` revoked or expired more than 30 days ago, incident action-token uses past their expiry, `notification_deliveries` (and status-page subscriber deliveries) in a final state for more than `DELIVERY_RETENTION = 30 days`, audit events (Part 1).

**API:** `GET /health` (both) → 200 `{"status": "ok"}`; `GET /ready` (private) → 200 or 503 (R-PLT-40); `GET /metrics` (public) → 200 text, 401 bad token, 404 when unset.

**Behaviour:** R-PLT-39, R-PLT-40, R-PLT-41. Reaper deletes in batches of 10,000 per table per run.

**Tests:**
- `sigterm_stops_accepting_and_lets_an_in_flight_request_finish` (in-process: cancel the token during a slow handler), `a_request_still_running_at_the_drain_deadline_is_dropped`, `queued_syslog_frames_are_flushed_on_shutdown`, `usage_counters_are_flushed_on_shutdown`, `a_worker_finishes_its_tick_then_stops`, `an_open_tail_ends_at_shutdown`.
- `health_does_not_touch_the_database` (pool closed, still 200), `ready_is_503_when_clickhouse_is_down`, `ready_is_503_when_a_running_worker_is_stale`, `ready_ignores_workers_disabled_on_this_instance`.
- `metrics_need_the_token`, `metrics_are_404_without_a_configured_token`, `metrics_expose_requests_by_route_template_not_path`.
- `the_reaper_deletes_expired_tokens_invites_and_old_deliveries_and_keeps_live_ones`.
- Unit: config parsing for `LOG_FORMAT`, bind addresses, the removed defaults (`from_env` with a missing `CLICKHOUSE_URL` is an error naming it), `METRICS_TOKEN` length.

**Acceptance:** `docker stop` of a running engine (manual, after NAMI-PLT-20) exits within 30 s with no lost syslog frames in a test send; `/ready` reflects a stopped ClickHouse.

### Task 19 (NAMI-PLT-19): secrets and tokens at rest: key handling, rotation, heartbeat token hashing, source names

**Covers:** R11.13  **Depends on:** NAMI-PLT-18.

**Files:**
- Create: `nami-engine/src/platform/reseal.rs`, `nami-engine/migrations/NNNN_uptime_heartbeat_token_hash.sql`, `nami-engine/migrations/NNNN_logs_source_names.sql`, `nami-engine/tests/platform/reseal.rs`, `nami-engine/tests/uptime/heartbeat_token_hash.rs`.
- Modify: `nami-engine/src/core/crypto.rs` (previous-key fallback, loader with the `TEST_KEY` refusal, zeroize), `nami-engine/src/core/config.rs` (key out of `Config`), `nami-engine/src/core/state.rs`, `nami-engine/src/main.rs`, each domain with sealed columns (`pub const SEALED_COLUMNS`), `nami-engine/src/uptime/heartbeat/` (hash lookup, sealed display), `nami-engine/src/logs/source/` (unique names, explicit column lists, reveal/rotate rate limit), `nami-engine/.env.example`, `nami-ui` source forms (409 message).

**Data shape:**
- `NNNN_uptime_heartbeat_token_hash.sql`: adds `heartbeats.token_hash bytea` (constraint-named unique index, `octet_length = 32` check) and `heartbeats.token_enc bytea`, and makes the plaintext token column nullable. SQL cannot seal (the AAD needs the cipher), so the engine backfills at startup: every row with a plaintext token and no hash gets both, then its plaintext is set to null, in one transaction per batch of 500 (idempotent, safe on every instance). From then on no plaintext token is at rest. The column stays, nullable and empty: dropping it needs another uptime migration file, and Platform is the last sub-project, so the decisions doc records it as "won't do" (an empty nullable column costs nothing, and dropping it is a one-line migration whenever uptime next changes its schema).
- `NNNN_logs_source_names.sql`: a unique index on `(project_id, lower(name))`, after renaming existing duplicates to `<name> (2)`, `<name> (3)` in creation order.
- `pub struct SealedColumn { table: &'static str, column: &'static str, id_column: &'static str, context: &'static str }`.
- `MAX_TOKEN_REVEALS_PER_USER_PER_MINUTE = 10`.

**Behaviour:** R-PLT-42 and R-PLT-43. `nami-engine reseal` prints per column the rows resealed and the rows still under the previous key, and exits non-zero while any remain. Source names: 409 `a source with this name already exists` on create and rename.

**Tests:**
- `values_sealed_under_the_previous_key_still_open`, `reseal_moves_every_registered_column_to_the_current_key`, `a_column_missing_from_the_registry_fails_the_registry_test` (scans migrations for `_enc` columns and compares with the registry).
- `the_test_key_is_refused_by_the_loader`, `config_no_longer_holds_the_key` (compile-level: the field is gone).
- `a_heartbeat_ping_is_looked_up_by_hash`, `members_still_see_the_ping_url`, `no_plaintext_token_remains_after_the_backfill`.
- `duplicate_source_names_are_renamed_by_the_migration`, `a_second_source_with_the_same_name_is_409_case_insensitively`, `eleven_reveals_in_a_minute_are_429`.

**Acceptance:** a database restored from before the migration upgrades cleanly; `reseal` after a key change leaves zero rows under the old key.

### Task 20 (NAMI-PLT-20): Dockerfiles, production compose behind Caddy, and the CI image job

**Covers:** R11.10, R11.11 (image builds)  **Depends on:** NAMI-PLT-19.

**Files:**
- Create: `nami-engine/Dockerfile`, `nami-engine/.dockerignore`, `nami-ui/Dockerfile`, `nami-ui/.dockerignore`, `docker-compose.prod.yml`, `Caddyfile`, `.env.prod.example` (every variable the production compose needs, no values for secrets).
- Modify: `nami-ui/package.json` (`@sveltejs/adapter-node` replaces `adapter-auto`), `nami-ui/vite.config.ts` (adapter), `nami-ui/.env.example` (`ORIGIN`, `ADDRESS_HEADER`, `XFF_DEPTH`, `BODY_SIZE_LIMIT`), `.github/workflows/ci.yml` (job `images`), `docs/operations.md` (section "Deployment": first start, `BOOTSTRAP_ADMIN_EMAIL`, backups of the two volumes and `NAMI_ENCRYPTION_KEY`, key rotation with `reseal`, upgrades, the optional filtered Caddy log, syslog ports, `METRICS_TOKEN`, OIDC setup notes for Keycloak, Authentik and Entra ID), `nami-engine/docker-compose.yaml` (unchanged in purpose: it stays the dev compose; a comment points to the production one).

**Data shape:**
- Engine image per R-PLT-44; `EXPOSE 8080 8081`; `ENTRYPOINT ["/nami-engine"]`; `HEALTHCHECK CMD ["/nami-engine", "healthcheck"]`.
- UI image per R-PLT-44; env `NODE_ENV=production`, `PORT=3000`; `HEALTHCHECK` with `node -e` fetching `/` (the slim image has node).
- `docker-compose.prod.yml` per R-PLT-45, with: `engine` env (`API_BIND_ADDR=0.0.0.0`, `TRUSTED_PROXIES=<network subnet>`, `COOKIE_SECURE=true`, `REGISTRATION_MODE=invite_only`, `LOG_FORMAT=json`, `OUTBOUND_ALLOW_PRIVATE_NETWORKS` from `.env`), `ulimits: nofile: 65536`, `sysctls` for ping, `stop_grace_period: 30s`, `depends_on` with `condition: service_healthy`; `ui` env (`ENGINE_URL=http://engine:8080`, `PUBLIC_ENGINE_URL=https://<domain>`, `ORIGIN=https://<domain>`, `ADDRESS_HEADER=X-Forwarded-For`, `XFF_DEPTH=1`, `BODY_SIZE_LIMIT=5M`); `postgres:17` and `clickhouse/clickhouse-server:26.9` with health checks and no `ports:`; `caddy:2` with `Caddyfile`, ports 80/443 (and 443/udp for HTTP/3).
- `Caddyfile`: `{$NAMI_DOMAIN} { header Strict-Transport-Security "max-age=31536000; includeSubDomains"; @engine path /api/* /v1/* /mcp /metrics; reverse_proxy @engine engine:8081; reverse_proxy ui:3000 }`, with `flush_interval -1` on the routes that stream (tail and export through nami-ui, SSE).
- CI job `images`: `docker/build-push-action` (pinned SHA) building both images with `push: false` and the GitHub Actions cache; runs after `engine` and `ui`.

**Behaviour:**
1. Images build from a clean checkout with `--locked`/`npm ci`; nothing from the developer machine (`.dockerignore` excludes `target/`, `node_modules/`, `.env*`).
2. Containers run as non-root (engine uid 65532, ui `node`); the engine filesystem is read-only in the compose (`read_only: true`, `tmpfs: /tmp`).
3. The compose publishes only Caddy's ports; Postgres, ClickHouse and the private API are reachable only on the compose network.
4. The UI build reads no secret at build time.

**Tests:**
- CI `images` job green.
- `every_public_route_starts_with_a_public_prefix` (engine: walks the public router's registered paths through the same registry test NAMI-PLT-5 uses, plus the fixed ingest list) and `the_caddyfile_engine_matcher_lists_exactly_the_public_prefixes` (reads `Caddyfile`, compares the `@engine path` list with `PUBLIC_PREFIXES`); vitest `no_ui_route_lives_under_a_public_prefix` (scans `nami-ui/src/routes` for `api`, `v1`, `mcp` and `metrics` top-level directories).
- Manual end-to-end on a scratch host or local Docker (noted in the PR): `docker compose -f docker-compose.prod.yml up -d`, first-admin registration with `BOOTSTRAP_ADMIN_EMAIL`, a monitor checking an internal service, a ping monitor, log ingest with curl through Caddy, `/api/v1` with a PAT, `/mcp` with a client, `docker compose stop` within 30 s, `docker compose exec engine id` shows uid 65532, `docker compose port postgres 5432` shows nothing.
- vitest: a test that `vite.config.ts` uses adapter-node (import the config and check the adapter name).

**Acceptance:** the production compose runs Nami end to end behind Caddy with TLS (on a real domain or Caddy's internal CA locally), and every check above passes.

### Task 21 (NAMI-PLT-21): sweep of every remaining deferred item

**Covers:** R11.13  **Depends on:** NAMI-PLT-20.

**Files:** as each item requires (engine domains, their tests, nami-ui components and their tests), plus TLS syslog: `nami-engine/src/logs/syslog/mod.rs` (a TLS listener on `SYSLOG_TLS_PORT`, default 6514, enabled only when `SYSLOG_TLS_CERT` and `SYSLOG_TLS_KEY` (PEM file paths) are set, through `tokio-rustls`, already a dependency; RFC 5425 octet-counted framing, the existing parser and caps), `nami-engine/tests/logs/syslog_tls.rs` (`rcgen` test certificate).

**Behaviour:**
1. Work through the inventory rows that point at NAMI-PLT-21, in order. For each: verify it still exists on `main` (some were fixed by later waves); fix it with a test, or close it with the commit that fixed it, or record a "won't do" with the reason. Performance items (row 39, the prune scan in row 24) are measured first (STANDARDS §7: metric, baseline at `main`, result) and changed only when the number justifies it.
2. Row 42: every item later decisions docs sent to Platform, found by the re-grep at phase start.
3. The decisions doc gets one line per inventory row with its outcome.

**Tests:** one regression test per fixed item, named after its behaviour (e.g. `a_non_http_monitor_with_request_config_is_400`, `url_credentials_and_a_custom_authorization_header_are_400`, `patch_with_null_clears_a_window_field`, `name_null_on_a_monitor_is_400`, `empty_value_kuma_headers_are_dropped_on_import`, `imported_monitors_spread_their_first_check_over_the_interval`, `the_two_second_clickhouse_timeout_degrades_the_list`, `syslog_over_tls_ingests_an_octet_counted_frame`, `syslog_tls_refuses_a_plaintext_client`).

**Acceptance:** every inventory row has an outcome in the decisions doc; no row is left "open".

### Task 22 (NAMI-PLT-22): UI for Part 2

**Covers:** R11.06, R11.07, R11.08, R11.09 (UI)  **Depends on:** NAMI-PLT-21.

**Files:**
- Create: `nami-ui/src/routes/(app)/admin/settings/security/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/admin/usage/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/settings/invites/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/projects/[id=uuid]/usage/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/admin/invites/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/invite/[token]/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/enroll-2fa/+page.svelte`, `nami-ui/src/lib/platform/usage.ts`, `nami-ui/src/lib/server/platform/{invite-form,security-form}.ts`, vitest files.
- Modify: `nami-ui/src/routes/login/` (OIDC button), `nami-ui/src/routes/register/` (closed-registration message), `nami-ui/src/routes/(app)/security/` (change password, disable 2FA with re-auth, re-auth prompt on setup when enabled, lockout message), the `(app)` layout load (a `2fa_enrollment_required` answer redirects to `/enroll-2fa`, which embeds the existing TOTP setup), navigation.

**Behaviour:**
1. Login shows "Sign in with `<OIDC_DISPLAY_NAME>`" when `/auth/oidc/config` says enabled.
2. The security page: change password (current, new, confirm; the 12-character rule shown), 2FA setup that keeps the old factor until the new one is confirmed, disable with re-auth, the 429 lockout message with the time left.
3. Enrollment redirect: any page load that gets `2fa_enrollment_required` sends the user to `/enroll-2fa` (log out stays available); after enabling, back to the page they came from (a same-origin relative path only).
4. Admin security settings: the two toggles, the count of users without 2FA, re-auth, and the refusal message when the admin has no TOTP.
5. Invites: project admins list, create, resend and revoke (two-step) on the project settings; global admins also on `/admin/invites`. The public `/invite/[token]` page shows the preview, a password form for new accounts, or an "accept" button for a logged-in user with that email, and the generic invalid message otherwise. `[token]` is checked against `^[0-9a-f]{64}$` before it reaches an engine path.
6. Usage: admin page with per-day stacked totals (chart colours from `seriesColor(i)`), a table per source or application, ClickHouse and Postgres storage tables with human-readable bytes; the project page with its own sources only.
7. Every page uses the design-system components and tokens, `callEngine`, UTC times, two-step deletes, `no-store` where secrets or tokens appear.

**Tests (vitest):** invite form parsing (email lowercased, role), the invite token matcher, the enrollment redirect target (only same-origin paths), usage aggregation by day and kind, byte formatting, the security form (re-auth field shape, password rule message), the OIDC button only when enabled.

**Acceptance:** `npm run check` 0/0, vitest passes, manual walk-through of each flow noted in the PR.

### Task 23 (NAMI-PLT-23): authorization matrix, ingestion fuzzing and dependency audits

**Covers:** R11.14  **Depends on:** NAMI-PLT-22.

**Files:**
- Create: `nami-engine/tests/platform/authz_matrix.rs`, `nami-engine/tests/common/fuzz.rs`, `nami-engine/tests/platform/fuzz_ingest.rs`.
- Modify: `nami-engine/tests/common/mod.rs` (principal fixtures for the matrix), `.github/workflows/ci.yml` (`FUZZ_ITERATIONS` for CI), `nami-engine/.cargo/audit.toml` and `docs/operations.md` (only for justified ignores).

**Behaviour:**
1. The matrix per R-PLT-46, covering every OpenAPI operation, every session-only and admin route in its fixed table, and every MCP tool, against the nine principals. Each route is called with valid ids of the right project and, for the "foreign id" column, with an id from another project.
2. Fuzz targets per R-PLT-47: logs JSON, NDJSON and MessagePack (plain and gzipped), syslog TCP and UDP (and TLS), OTLP logs, metrics and traces (JSON and protobuf), Prometheus text and remote write, Sentry envelope and store (gzip, deflate, br), RUM beacons and replay chunks, incident integrations (generic webhook, Alertmanager, Grafana), the public report form, the RFC 8058 one-click unsubscribe, RUM config, heartbeat pings, the MCP endpoint, and the invite and OIDC callback parameters.
3. `cargo audit` and `npm audit --audit-level=high` clean, or each remaining advisory documented with the reason and a review date.

**Tests:** `every_route_answers_the_expected_status_for_every_principal`, `a_route_without_a_matrix_entry_fails_the_matrix`, `every_mcp_tool_obeys_scope_and_role`, one `fuzz_<endpoint>_never_panics_or_errors_500` per target.

**Acceptance:** the matrix and fuzz suites pass in CI; every finding they surfaced is fixed in this task with a regression test.

### Task 24 (NAMI-PLT-24): Part 2 security review, full-system review and penetration-style pass, fix wave

**Covers:** R11.14  **Depends on:** NAMI-PLT-10 .. NAMI-PLT-23.

**Files:** fix commits as findings require; `docs/superpowers/decisions/2026-10-24-platform-part2.md` (rulings R-PLT-21..47 as kept or changed, limits and environment, the inventory outcomes, the outbound call-site list, manual OIDC results, deferred items with a target or "won't do"); `docs/superpowers/roadmap.md` (tick R11.06 to R11.14; R11.11 and R11.13 now complete across both parts).

**Behaviour:**
1. An opus security review of the Part 2 diff (Review Focus above).
2. A full-system review of the whole engine and UI, not only this phase: authentication and sessions, RBAC and tenant isolation, every public endpoint, outbound requests, secrets at rest, ClickHouse and Postgres query construction, streaming and resource bounds, the BFF, CSP and cookies, the container setup.
3. A penetration-style pass against the production compose (local): unauthenticated surface enumeration through Caddy, credential stuffing against login (rate limits, lockout, timing), session fixation and CSRF attempts through the BFF, token scope escalation, IDOR across projects with REST and MCP, SSRF through every destination field with both outbound settings and DNS rebinding, header injection through names that reach emails, XSS through every telemetry field shown in the UI, oversized and malformed bodies on every listener, slowloris on both listeners, and a container breakout check (non-root, read-only filesystem, no published database ports). Findings and their reproduction steps go into the decisions doc.
4. Fix wave on the branch until no Critical or Important finding remains, with a scoped re-review.
5. Then the decisions doc, the roadmap ticks, push, the phase PR (purpose, tasks with commit ranges, summary, the CI run, breaking changes: `REGISTRATION_MODE`, required configuration, password rule, the heartbeat token migration, adapter-node; risks), and auto-merge when the review is clean (pre-authorized).

**Tests:** the full suite in CI, the matrix and fuzz suites, and a regression test per fixed finding.

**Acceptance:** no Critical or Important finding remains, CI green, PR merged, branch and worktree deleted, roadmap §11 fully ticked.
