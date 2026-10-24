# Platform + Production Hardening, Part 1 (API surface and audit) Implementation Plan

> **Status:** outline. Nothing merged. Next: NAMI-PLT-1 once AI SRE is on `main`.
> **Branch:** `feat/platform-part1` in `.worktrees/platform-part1/`.
> **Depends on:** AI SRE (sub-project 10) and every phase before it, all merged to `main`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. This is an `outline` plan (`docs/superpowers/plan-conventions.md`): tasks, scope, data shapes, endpoints, files, tests and acceptance are fixed here, and the step-level code is written at phase start against the then-current `main`.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R11.01 | API tokens + documented REST API for monitors/heartbeats/incidents | NAMI-PLT-2, NAMI-PLT-4, NAMI-PLT-5, NAMI-PLT-8 |
| R11.02 | REST API coverage for every configurable resource (OpenAPI) | NAMI-PLT-4, NAMI-PLT-5, NAMI-PLT-8 |
| R11.03 | MCP server (streamable HTTP, PAT auth, tool allow/deny headers) | NAMI-PLT-6, NAMI-PLT-8 |
| R11.04 | Audit log of security-relevant and configuration changes (Part 1: table, recorder, every event the Part 1 code paths emit; Part 2 adds invite, OIDC and settings events) | NAMI-PLT-3, NAMI-PLT-8 |
| R11.05 | Audit log forwarding (log source or webhook) | NAMI-PLT-7, NAMI-PLT-8 |
| R11.11 | CI (GitHub Actions), Part 1 half: fmt, clippy, engine tests with service containers, UI check/test, dependency audits, branch protection doc (the image-build job is Part 2, NAMI-PLT-20) | NAMI-PLT-1 |
| R11.13 | Parked Foundation items, Part 1 half: re-authentication primitive and its use on API token creation (the rest is Part 2, NAMI-PLT-12) | NAMI-PLT-2 |
| R11.14 | Part 1 security review + fix wave (the full-system pass is Part 2, NAMI-PLT-24) | NAMI-PLT-9 |

## Why two parts

The spec is one sub-project, but one phase PR for all of it would hold about 24 tasks touching every domain. It is split where the dependency arrow points one way:

- **Part 1 (this plan) is the API surface:** CI, personal access tokens, the audit log, REST coverage with OpenAPI, the MCP server, audit forwarding and their UI. Everything in Part 2 builds on it. 2FA enforcement must cover PAT and MCP requests, so the token principal must exist first. Invites, OIDC logins and the security settings write audit events, so the audit recorder must exist first.
- **Part 2 (`docs/superpowers/plans/2026-10-24-platform-hardening-part2.md`) is identity and production:** the outbound policy, trusted proxies and per-IP limits, auth hardening, OIDC, 2FA enforcement, invites, the usage page, security headers and body limits, the production runtime, secrets at rest, Docker and the production compose, the deferred-items sweep, and the final full-system security pass.
- **CI goes first (NAMI-PLT-1)** although the spec lists it under deployment. Every later task of both parts then runs under CI, and the crate-wide `cargo fmt` that Uptime Phase 1 deferred to "the hardening CI task" lands before anyone else edits those files. The Docker image-build job needs the Dockerfiles, so it is added in Part 2 (NAMI-PLT-20).
- **Re-authentication ships with tokens.** The spec requires it for token creation, so the primitive (`auth::reauth`) is built in NAMI-PLT-2. Part 2 applies it to password change and 2FA changes. R11.13 is therefore split across both parts.

## Goal

Make every configurable resource of Nami reachable by automation. That means personal access tokens with project scope and a role cap, the same handlers served as a documented REST API under `/api/v1` with an OpenAPI 3.1 document, an MCP server over streamable HTTP for AI agents, and an audit log of security-relevant and configuration changes that can be forwarded to a log source or a signed webhook. CI runs the whole test suite on every push.

## Architecture

- **One new domain, `platform`** (`nami-engine/src/platform/`): `token/` (PAT model and routes), `audit/` (events, recorder, mutation layer, admin read API), `rest.rs` (the bearer-only REST router), `openapi.rs` (the document), `mcp/` (the MCP endpoint and tool registry), `forward/` (audit forwarding), `settings.rs` (the `instance_settings` row) and `reaper.rs` (retention loop). `auth::reauth` (new, `nami-engine/src/auth/reauth.rs`) holds re-authentication because it is an auth concern.
- **One principal type.** `core::middleware::rbac::CurrentUser` gains `auth: AuthKind` (`Session` or `Token(TokenGrant)`). Its extractor reads `Authorization: Bearer nami_pat_...` first and never falls back to the session when the header is present. Every project-scoped handler keeps calling `require_project_role` (or `require_project_reader`), which applies the token's project list and role cap. No handler changes for token support.
- **One route registry.** Every domain's project-scoped routes move into `pub fn api_routes() -> utoipa_axum::router::OpenApiRouter<AppState>` built only with `utoipa_axum::routes!`. `core::router::build_router` (private API, `PORT`, session layer) and `platform::rest::router` (public listener, `PUBLIC_PORT`, no session layer) both consume it. The OpenAPI document is produced from the same registration, so a documented route and a served route cannot drift apart.
- **REST on the public listener.** The private API (`PORT`) trusts nami-ui's `X-Forwarded-For` and carries the session layer, so it never faces the internet. The REST router is nested at `/api/v1` on the public listener with no session layer and bearer-only auth, so there is no cookie and no CSRF surface. Ingest paths keep their places (`/api/v1/logs`, `/api/v1/heartbeat/{token}`, `/api/v1/metrics...`, Sentry's `/api/{project}/envelope/`). REST paths all start with `/api/v1/projects`, so nothing collides (axum panics at build on an overlap, and a test builds the full public router).
- **MCP tools call the REST router.** `/mcp` (public listener) is a hand-written JSON-RPC handler. A tool call turns its arguments into one REST request and runs it through the REST router with `tower::ServiceExt::oneshot`, carrying the caller's own `Authorization` header. Same handlers, same RBAC, same audit layer, no second code path.
- **Audit from one layer plus explicit events.** `platform::audit::layer` wraps the API routes and records every successful POST/PUT/PATCH/DELETE as a configuration change (route template, target, project, actor), with no request or response body. Security events (logins, logout, password, 2FA, tokens, members, MCP tool calls, forwarding changes) are recorded explicitly with dotted action names, and those handlers mark the response so the layer does not record them twice.
- **Forwarding by cursor.** `platform::forward` reads `audit_events` in `(created_at, id)` order from a per-target cursor and advances it only after a successful delivery. That is at-least-once and durable (records are kept 365 days) without a per-record queue table.

## Tech Stack

Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, tower 0.5), new crates `utoipa 6.0.0` (features `macros`, `axum_extras`, `uuid`, `chrono`) and `utoipa-axum 0.3.0` (both published 2026-09-22, `utoipa-axum` requires `axum ^0.8.4`, rust 1.88). Postgres 17, ClickHouse 26.9, Mailpit, wiremock 0.6.5 (dev, present). SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest. GitHub Actions.

## Spec

`docs/superpowers/specs/2026-10-13-platform-hardening-design.md`, sections "API tokens + REST API", "MCP server", "Audit log" and "CI". Roadmap: `docs/superpowers/roadmap.md` §11.

## When to start

After the last AI SRE phase PR is merged to `main`. Then, before NAMI-PLT-1:

1. Branch `feat/platform-part1` from the latest `origin/main` in `.worktrees/platform-part1/`.
2. Re-grep every decisions doc written after 2026-10-24 (Incident Management through AI SRE) for `Platform`, `NAMI-PLT`, `hardening`, `deferred` and `parked`. Items aimed at the API surface, tokens, audit or MCP join the matching Part 1 task. Everything else joins Part 2's sweep (NAMI-PLT-21) table.
3. Check the interfaces under "Consumed interfaces" against `main`. A different name or line is adapted minimally (same behaviour, same tests) and noted in the PR. A different behaviour stops the task and goes to the controller.
4. Re-read the current MCP revision (2026-07-28 when this plan was written) before NAMI-PLT-6, and the crate versions in Tech Stack before NAMI-PLT-5.

## Consumed interfaces

| From | Interface | Used by |
|---|---|---|
| Foundation | `core::middleware::rbac::{CurrentUser, user_from_session, require_project_role, require_project_reader, require_global_role, ProjectReader}` in `nami-engine/src/core/middleware/rbac.rs`. Roles are strings today; STANDARDS names a `Role` enum. If no `Role` enum is on `main` at phase start, NAMI-PLT-2 adds it (`admin > member > viewer`, `rename_all = "snake_case"`) and the rbac functions take it. | PLT-2, PLT-4 |
| Foundation | `auth::token::{new_opaque_token, hash_token}`, `auth::password::{hash_password, verify_password}`, `auth::totp::verify_code`, `auth::user::User` (`password_hash`, `totp_enabled`, `password_changed_at`). | PLT-2, PLT-3 |
| Foundation | `core::router::{build_router, build_public_router, configure_session_layer, with_fallback_connect_info}`, `require_json_body` (415 guard). | PLT-3, PLT-4, PLT-6 |
| Logs Part 1 | `logs::ingest::rate_limit::TokenBuckets` (per-key token buckets), `logs::ingest::store::RowBatch`, `logs::ingest::prepare`, the per-source pipeline and usage counters. | PLT-2, PLT-4, PLT-6, PLT-7 |
| Incidents | `outbound::client()` and `outbound::OutboundPolicy` (planned in NAMI-INC-3; `core/outbound.rs` in the domain layout), the R29 webhook signature (`X-Nami-Timestamp`, `X-Nami-Signature: sha256=<hex HMAC-SHA256(secret, "{ts}.{body}")>`, `X-Nami-Delivery`, planned in NAMI-INC-4), `hmac 0.13.0`, and the delivery backoff (30 s x 4^(n-1), capped at 1 h, planned in NAMI-INC-5). Part 2's NAMI-PLT-10 puts every outbound caller, forwarding included, under the unified policy. | PLT-7 |
| AI SRE | `ai_calls` (one row per LLM call: user, project, feature, model, token counts, cost, status; planned in the AI SRE plan, not written yet) and the AI tool result caps (50 rows, 16 KiB per result). | PLT-6, PLT-7 |
| Every domain | Each project-scoped route of uptime, incidents, logs, statuspages, metrics, traces, errors, rum and ai. NAMI-PLT-4 moves them into `api_routes()`. | PLT-4, PLT-5 |

## Global Constraints

- Everything in `CLAUDE.md`, `docs/STANDARDS.md` and `docs/superpowers/plan-conventions.md`. In particular: 403 for every authorization failure, 404 for a foreign id inside an accessible project, 400 for a foreign id stored in a row, 409 for a name clash or a quota, 429 for any rate limit. Bodies are `#[serde(deny_unknown_fields)]`. Every limit is a named constant with a test at the bound and one past it.
- Error shape, everywhere: `{"error": "<message>"}`, plus `"code": "<machine code>"` when a client must branch on it (`reauth_required`, `quota_exceeded`, later Part 2's `2fa_enrollment_required`). `AppError` gains a `Coded { status, code: &'static str, message: String }` variant (or equivalent) for these.
- Engine tests: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test platform <file>::`. The new target is `nami-engine/tests/platform/main.rs` (mounts `#[path = "../common/mod.rs"] mod common;`, one `mod` per file). Tests that touch another domain's behaviour sit in that domain's target. UI: `npm run check` (0 errors, 0 warnings) and `npx vitest run`.
- Migration: one Postgres file for this phase, `NNNN_platform_api.sql` (NNNN = next free number at phase start). It references `users`, `projects` and `sources`, which live in the `0001_auth.sql`, `0002_projects.sql` and `0005_logs.sql` baselines, so it has no ordering constraint against other phase files. If a task changes another domain's tables (the monitor and heartbeat caps need none, they are counts), it adds that domain's own phase file.
- Secrets never reach a log line, an audit record, an error message or a non-`no-store` response: PATs, the forwarding webhook secret, passwords and TOTP codes from re-auth. Types holding them have a manual `Debug`.
- No new proc-macro crate without a decision record (STANDARDS §8). NAMI-PLT-5's first commit creates the phase decisions doc `docs/superpowers/decisions/2026-10-24-platform-part1.md` with the `utoipa` record; later tasks append their rulings to it.
- Commit messages carry the task ID, e.g. `feat(engine): personal access tokens (NAMI-PLT-2)`.

## Review Focus

- Token scope cannot be widened: a global admin's token is still limited to its listed projects and capped at its `max_role`; a token never reaches a session-only route; a revoked or expired token is refused on the very next request, streams included.
- The REST router has no session layer, and a bearer request skips nothing the session path checks except the session itself (tokens survive a password change by design, R-PLT-6).
- Every mutating API route is audited exactly once, no audit record holds a body, a header value or a secret, and failed logins of unknown emails are indistinguishable in timing and response from known ones (the audit insert happens on both paths).
- The MCP endpoint refuses foreign `Origin`s, unknown tool names in the filter headers, header/body mismatches, batches and oversized bodies, and every tool call goes through the REST router with the caller's own token.
- Forwarded payloads hold no secret, the cursor never skips a record, and a failing destination is visible on the settings page.

## Rulings

- **R-PLT-1 PAT format.** `nami_pat_` followed by 43 characters of base64url without padding (32 random bytes from `rand::rngs::OsRng`), 52 characters in total. Stored: `token_hash bytea` = SHA-256 of the whole token string (32 bytes, unique index), and `prefix` = the first 15 characters (`nami_pat_` + 6) for display. The token is returned once, in the create response, with `Cache-Control: no-store`. A string that does not match `^nami_pat_[A-Za-z0-9_-]{43}$` is refused before any database read. No checksum suffix: nothing in this repository scans for leaked secrets, so it would buy nothing yet.
- **R-PLT-2 Token scope.** A token lists 1 to `MAX_TOKEN_PROJECTS = 100` projects and one `max_role` (`viewer`, `member`, `admin`). At request time the effective role in a project is `min(max_role, the user's real role there)`, where a global admin's real role is `admin` in every project. A project outside the list is 403, whatever the user's real role. There is no "all projects" wildcard: a new project needs a new token, which keeps a leaked token's reach explicit. A project deleted later just drops out of the list.
- **R-PLT-3 What tokens can reach.** Only project-scoped routes (`/projects/{id}/...`) and `GET /projects` (filtered to the token's projects, so automation can discover ids). `require_global_role` refuses every token with 403, so project creation, instance settings, AI provider settings, audit forwarding, the audit log and every `/admin/...` route stay session-only: a leaked token can never redirect audit forwarding or change the LLM provider. `/auth/...` and `/account/...` routes never read the bearer header. The spec's "AI settings" REST item is the per-project AI opt-out (project-scoped); the instance-wide provider config stays session-only by this ruling.
- **R-PLT-4 Expiry and quota.** `expires_in_days` is `1..=365` or `null` (never expires). At most `MAX_ACTIVE_TOKENS_PER_USER = 50` tokens that are neither revoked nor expired (409 `quota_exceeded`). Name: `core::validate::validate_label("name", ..)`, at most 100 characters, not unique.
- **R-PLT-5 `last_used_at`.** Updated at most once per `TOKEN_TOUCH_INTERVAL = 60 s` per token (`update ... where id = $1 and (last_used_at is null or last_used_at < now() - interval '60 seconds')`), together with `last_used_ip`, so a busy token does not write on every request.
- **R-PLT-6 Tokens and password changes.** A password change or reset does not revoke tokens (they are separate credentials, as on GitHub). The tokens page lists them with `last_used_at` and `last_used_ip`, and a global admin can revoke any user's token (`/admin/tokens`), which is the incident-response path. A deleted user's tokens go with the user (`on delete cascade`).
- **R-PLT-7 Re-authentication.** `auth::reauth::verify(state, user, session, reauth)` accepts, in this order: `{"password": ".."}` when the user has a password; `{"totp_code": ".."}` when TOTP is enabled; and, for a user with neither (Google/GitHub-only today, OIDC-only in Part 2), a session whose `auth_at` is at most `REAUTH_WINDOW = 5 min` old (the UI says "sign in again to continue"). Failure is 403 `{"error": "re-authentication failed", "code": "reauth_required"}`, never 401 (the BFF treats 401 as logged out). At most `MAX_REAUTH_FAILURES = 5` failures per user per 15 minutes (in-memory `TokenBuckets` keyed by user id), then 429. argon2 runs in `spawn_blocking`. Part 2 (NAMI-PLT-12) counts TOTP failures here against the per-user TOTP lockout.
- **R-PLT-8 REST prefix and listener.** REST is `/api/v1` + the existing resource path (`/api/v1/projects/{id}/monitors`) on the public listener, bearer-only, no session layer, with `require_json_body` kept (415 on a non-JSON mutating request). The private API keeps serving the same handlers at their unprefixed paths for nami-ui. The OpenAPI document is `GET /api/openapi.json` on both listeners, unauthenticated (the repository is the same document). Caddy (Part 2, R-PLT-45) sends `/api/`, `/v1/` (OTLP), `/mcp` and `/metrics` to the public listener.
- **R-PLT-9 Per-token rate limit.** REST requests: `TOKEN_REQUESTS_PER_SECOND = 10`, burst `TOKEN_REQUEST_BURST = 50`, per token per instance (`TokenBuckets`), 429 past it. MCP tool calls: `MCP_CALLS_PER_MINUTE = 60`, burst `MCP_CALL_BURST = 20`, per token, counted on top of the REST bucket their inner request uses.
- **R-PLT-10 Lists and pagination.** Lists whose size a per-project cap already bounds (monitors, heartbeats, groups, windows, sources, views, alerts, policies, schedules, channels, severities, integrations, status pages, dashboards, applications, ...) keep returning a full JSON array, and the OpenAPI document states the cap. NAMI-PLT-4 adds the two missing caps: `MAX_MONITORS_PER_PROJECT = 1000` and `MAX_HEARTBEATS_PER_PROJECT = 1000` (409 `quota_exceeded`, counted in the insert transaction like `uptime::quota::ProjectQuota`; bulk import counts each entry). Lists that grow without a cap (incidents, audit events, error issues and events, RUM sessions, status reports, subscribers, check history, ...) take `limit` (default 100, at most 500) and an opaque `cursor`, and answer `{"items": [...], "next_cursor": string | null}`. Lists that already paginate with their own cursor keep their shape. NAMI-PLT-4 inventories every list at phase start and records the outcome per list in the decisions doc.
- **R-PLT-11 Body rejections.** One extractor, `core::json::Json<T>` (a newtype over `axum::Json` whose rejection is `AppError`), replaces `axum::Json` in every API handler. A syntax error answers 400 `request body is not valid JSON`; a shape error answers 400 `request body does not match the expected shape` with no serde text (serde's message can quote a submitted value, which closes Uptime Phase 2's UP-10 item about `request_headers` echoed in the 422 body). A missing or wrong content type stays 415 from `require_json_body`. Handlers that validate further keep their own field messages.
- **R-PLT-12 OpenAPI generation.** `utoipa` + `utoipa-axum` instead of a hand-written document: about 150 operations over 9 domains would drift by hand, and `OpenApiRouter` registers a route and its documentation in one call, which is what makes "a route missing from the document" testable at all (axum's `Router` cannot list its routes). Cost: a proc-macro dependency, one `#[utoipa::path]` per handler and `ToSchema` on every DTO. The decision record is the first commit of NAMI-PLT-5. If utoipa 6 (one month old at writing) blocks at phase start, utoipa 5.4 with utoipa-axum 0.2 is the fallback (same annotations).
- **R-PLT-13 MCP implementation.** Hand-written, no `rmcp`: the server needs `server/discover`, `tools/list`, `tools/call` and `ping` (plus `initialize` for legacy clients), answers every request with one JSON object, keeps no session, and never sends server-initiated messages. That is a few hundred lines with no proc-macro and no second schema system next to utoipa. Tool input schemas come from the same `ToSchema` types as the REST parameters. The protocol moves fast, so the supported revisions are a constant (`SUPPORTED_PROTOCOL_VERSIONS = ["2026-07-28", "2025-11-25"]`) with a test per revision.
- **R-PLT-14 Audit actions.** Configuration changes recorded by the layer: `action = "<METHOD> <matched route template>"` (e.g. `PATCH /projects/{id}/monitors/{monitor_id}`), `target_type` = the last static segment before the trailing id parameter (`monitors`), `target_id` = the last UUID path parameter, or for a `201` create the top-level `id` of the JSON response (read only when the body is JSON and at most `AUDIT_ID_SNIFF_BYTES = 64 KiB`, otherwise null), `project_id` = the `{id}` of a `/projects/{id}/...` path, `metadata = {"status": <code>}`. Security events use dotted names: `auth.login.succeeded`, `auth.login.failed`, `auth.logout`, `auth.password.reset`, `auth.totp.enabled`, `token.created`, `token.revoked`, `member.added`, `member.removed`, `member.role_changed`, `project.created`, `source.token.revealed`, `source.token.rotated`, `mcp.tool_call`, `audit.forwarding.changed`, `ai.call` (R-PLT-17). Part 2 adds `auth.password.changed`, `auth.totp.disabled`, `auth.oidc.linked`, `invite.*` and `instance.settings.changed`. A handler that records its own event inserts `platform::audit::Recorded` into its response extensions and the layer skips it.
- **R-PLT-15 Failed logins.** A failed login for an existing account records `actor_type = 'user'` with that user; for an unknown email it records `actor_type = 'anonymous'` with no email (a password typed into the email field must not land in the audit log). Both paths do one insert, so the audit write adds no timing difference.
- **R-PLT-16 Audit write failures.** The layer writes after the response is produced. A failed insert logs `error` and increments a counter (exported by Part 2's `/metrics`); the change itself stands. Writing in the handler's transaction would mean touching every handler, which the single layer exists to avoid. Cost if wrong: a configuration change without its audit row during a database fault.
- **R-PLT-17 AI calls in the audit stream.** Each `ai_calls` insert also inserts an `ai.call` audit event in the same transaction (metadata: feature, model, token counts, cost, status; never prompt or output text). Forwarding then has one stream and one cursor per target, and the audit page shows AI usage next to everything else.
- **R-PLT-18 Forwarding semantics.** Per target (`log_source`, `webhook`), the forwarder claims the target's state row with a lease, reads up to `FORWARD_BATCH = 100` events after its cursor in `(created_at, id)` order, delivers them, and advances the cursor only on success. A failure keeps the cursor, records `last_error` (fixed text: `HTTP 503`, `timeout`, `connection failed`, `destination not allowed`, `source not found`), and retries with the incidents backoff (30 s x 4^(n-1), at most 1 h) forever: at-least-once, nothing skipped, the lag is shown on the settings page. Records are kept 365 days, so a destination down for less than that loses nothing. Changing a target resets its cursor to "now" (no replay of history to a new destination); an explicit `replay_from` date is not offered (YAGNI).
- **R-PLT-19 Audit visibility.** Only global admins read the audit log (`/admin/audit-events`). Project admins do not get a project view in this phase; the filters on the admin page cover the need.
- **R-PLT-20 Live tail and tokens.** `GET /projects/{id}/logs/tail` (SSE) stays session-only and is not registered in `api_routes()`. Streams that are on REST (log and check-history exports) re-check a token's revocation and expiry wherever the session path re-checks the session.

## Data model (`NNNN_platform_api.sql`)

Created by NAMI-PLT-2 (`api_tokens`, `api_token_projects`) and appended to by NAMI-PLT-3 (`audit_events`) and NAMI-PLT-7 (`instance_settings`, `audit_forward_state`). The file has not merged when later tasks append, so that is allowed (plan conventions).

```sql
create table api_tokens (
    id uuid primary key,                                   -- v7, generated in Rust
    user_id uuid not null references users(id) on delete cascade,
    name text not null,
    token_hash bytea not null,
    prefix text not null,
    max_role text not null,
    expires_at timestamptz,
    last_used_at timestamptz,
    last_used_ip inet,
    revoked_at timestamptz,
    created_at timestamptz not null default now(),
    constraint api_tokens_token_hash_key unique (token_hash),
    constraint api_tokens_token_hash_len check (octet_length(token_hash) = 32),
    constraint api_tokens_name_len check (char_length(name) between 1 and 100),
    constraint api_tokens_prefix_shape check (prefix ~ '^nami_pat_[A-Za-z0-9_-]{6}$'),
    constraint api_tokens_max_role_check check (max_role in ('admin', 'member', 'viewer'))
);
create index api_tokens_user_idx on api_tokens (user_id, created_at desc);

create table api_token_projects (
    token_id uuid not null references api_tokens(id) on delete cascade,
    project_id uuid not null references projects(id) on delete cascade,
    constraint api_token_projects_pkey primary key (token_id, project_id)
);
create index api_token_projects_project_idx on api_token_projects (project_id);

create table audit_events (
    id uuid primary key,                                   -- v7, generated in Rust
    created_at timestamptz not null default now(),
    actor_type text not null,
    actor_user_id uuid references users(id) on delete set null,
    actor_token_id uuid references api_tokens(id) on delete set null,
    actor_email text,                                      -- snapshot, survives user deletion
    action text not null,
    target_type text,
    target_id uuid,
    project_id uuid references projects(id) on delete set null,
    ip inet,
    user_agent text,
    metadata jsonb not null default '{}',
    constraint audit_events_actor_type_check check (actor_type in ('user', 'token', 'system', 'anonymous')),
    constraint audit_events_action_len check (char_length(action) between 1 and 200),
    constraint audit_events_target_type_len check (char_length(target_type) <= 64),
    constraint audit_events_user_agent_len check (char_length(user_agent) <= 512),
    constraint audit_events_metadata_object check (jsonb_typeof(metadata) = 'object'),
    constraint audit_events_metadata_size check (octet_length(metadata::text) <= 8192)
);
create index audit_events_created_idx on audit_events (created_at desc, id desc);
create index audit_events_project_idx on audit_events (project_id, created_at desc);
create index audit_events_actor_idx on audit_events (actor_user_id, created_at desc);
create index audit_events_action_idx on audit_events (action text_pattern_ops, created_at desc);

create table instance_settings (                           -- single row, spec shape
    id boolean primary key default true,
    require_2fa boolean not null default false,            -- used from Part 2 (NAMI-PLT-14)
    exempt_oidc_from_2fa boolean not null default false,   -- used from Part 2
    audit_forward_source_id uuid references sources(id) on delete set null,
    audit_forward_webhook_url text,
    audit_forward_secret_enc bytea,
    constraint instance_settings_single_row check (id),
    constraint instance_settings_webhook_url_len check (char_length(audit_forward_webhook_url) <= 2048),
    constraint instance_settings_webhook_secret check ((audit_forward_webhook_url is null) = (audit_forward_secret_enc is null))
);
insert into instance_settings default values;

create table audit_forward_state (
    target text primary key,
    cursor_created_at timestamptz,
    cursor_id uuid,
    attempts integer not null default 0,
    next_attempt_at timestamptz not null default now(),
    lease_until timestamptz,
    last_attempt_at timestamptz,
    last_success_at timestamptz,
    failing_since timestamptz,
    last_error text,
    constraint audit_forward_state_target_check check (target in ('log_source', 'webhook')),
    constraint audit_forward_state_error_len check (char_length(last_error) <= 200),
    constraint audit_forward_state_cursor_pair check ((cursor_created_at is null) = (cursor_id is null))
);
insert into audit_forward_state (target) values ('log_source'), ('webhook');
```

## Tasks

### Task 1 (NAMI-PLT-1): run fmt, clippy, tests and audits in GitHub Actions

**Covers:** R11.11 (Part 1 half)  **Depends on:** nothing in this plan.

**Files:**
- Create: `.github/workflows/ci.yml`, `nami-engine/.cargo/audit.toml` (only if an advisory must be ignored, each with a reason and a review date), `docs/operations.md` (sections "CI" and "Branch protection"; Part 2 adds "Deployment").
- Modify: every engine file `cargo fmt` touches (one commit, nothing else in it), files with clippy warnings, `CLAUDE.md` (the "No CI is configured" paragraph and the PR-description rule), `docs/superpowers/plan-conventions.md` (its "There is no CI yet" sentence).

**Data shape:** the workflow, triggered on `push` to `main` and on `pull_request`; `permissions: contents: read`; `concurrency: ci-${{ github.ref }}` with `cancel-in-progress: true`. Every action is pinned by full commit SHA with the version in a comment.
- Job `engine` (ubuntu-latest), working directory `nami-engine`:
  - Services: `postgres:17` (env `POSTGRES_USER/PASSWORD/DB = nami`, ports `5433:5432`, so `DATABASE_URL=postgres://nami:nami@localhost:5433/nami` is the same string as on the dev machine; `--health-cmd pg_isready`), `clickhouse/clickhouse-server:26.9` (env `CLICKHOUSE_USER/PASSWORD/DB = nami`, ports `8123:8123`, options `--ulimit nofile=262144:262144` and a health check on `/ping`), `axllent/mailpit` pinned to a release tag, not `latest` (ports `1025:1025`, `8025:8025`).
  - Steps: checkout; Rust stable with `rustfmt` and `clippy`; `Swatinem/rust-cache`; `sudo sysctl -w net.ipv4.ping_group_range="0 2147483647"` (unprivileged ICMP for the ping checker tests); `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test` with `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`; `cargo audit` (installed with `cargo install cargo-audit --locked` at a pinned version, cached).
- Job `ui` (ubuntu-latest), working directory `nami-ui`: Node (the Active LTS at phase start, 24 at writing) with npm cache; `npm ci`; `npm run check -- --fail-on-warnings`; `npm test`; `npm audit --audit-level=high`.

**Behaviour:**
1. The first commit of the task is `chore: cargo fmt the whole crate (NAMI-PLT-1)` with no other change (Uptime Phase 1 ruling: crate-wide fmt waits for the CI task).
2. Clippy findings are fixed in the code, not allowed crate-wide. A local `#[allow(clippy::..)]` needs a comment saying why.
3. A `cargo audit` or `npm audit` finding is fixed by an update. When no fixed version exists, it is ignored in `nami-engine/.cargo/audit.toml` (or documented in `docs/operations.md` for npm) with the advisory id, why Nami is not affected, and a review date.
4. `docs/operations.md` "Branch protection": `main` requires the `engine` and `ui` checks (and Part 2's `images`), a PR, linear history, no force pushes; the phase PR flow in `CLAUDE.md` is unchanged except that "required checks" now means these jobs.
5. `CLAUDE.md` and `plan-conventions.md` stop saying there is no CI; PR descriptions list the CI run instead of local runs.

**Tests:** the workflow itself. A PR from the phase branch shows `engine` and `ui` green.

**Acceptance:** both jobs pass on the phase branch; `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` pass locally; `docs/operations.md` exists with both sections.

### Task 2 (NAMI-PLT-2): personal access tokens, the token principal, and re-authentication

**Covers:** R11.01 (tokens), R11.13 (re-auth on token creation)  **Depends on:** NAMI-PLT-1.

**Files:**
- Create: `nami-engine/src/platform/mod.rs`, `nami-engine/src/platform/token/{mod,model,routes}.rs`, `nami-engine/src/auth/reauth.rs`, `nami-engine/migrations/NNNN_platform_api.sql` (the two token tables), `nami-engine/tests/platform/main.rs`, `nami-engine/tests/platform/tokens.rs`, `nami-engine/tests/platform/reauth.rs`.
- Modify: `nami-engine/src/lib.rs` (`pub mod platform;`), `nami-engine/src/auth/mod.rs`, `nami-engine/src/core/middleware/rbac.rs` (`AuthKind`, `TokenGrant`, bearer resolution, scope and cap in `require_project_role`, tokens refused in `require_global_role`), `nami-engine/src/core/error.rs` (the coded variant), `nami-engine/src/core/router.rs` (mount the token routes on the private API), `nami-engine/src/core/state.rs` (`platform: Arc<PlatformState>` holding the per-user re-auth buckets and, from NAMI-PLT-4, the per-token request buckets), `nami-engine/src/projects/routes.rs` (`GET /projects` filtered for tokens).

**Data shape:**
- Tables `api_tokens`, `api_token_projects` (see Data model).
- Rust: `pub enum AuthKind { Session, Token(TokenGrant) }`; `pub struct TokenGrant { pub token_id: Uuid, pub max_role: Role, pub projects: Arc<[Uuid]> }`; `CurrentUser { id, email, global_role, auth: AuthKind }`. `pub struct NewToken { name: String, project_ids: Vec<Uuid>, max_role: Role, expires_in_days: Option<u16> }` (validated). `pub enum Reauth { Password(SecretString), TotpCode(String) }`, deserialized from `{"password": ".."}` or `{"totp_code": ".."}` (exactly one key). `TokenView { id, name, prefix, max_role, project_ids, expires_at, last_used_at, last_used_ip, created_at }` (never the hash). Constants `MAX_TOKEN_PROJECTS = 100`, `MAX_ACTIVE_TOKENS_PER_USER = 50`, `MAX_TOKEN_DAYS = 365`, `TOKEN_TOUCH_INTERVAL`, `REAUTH_WINDOW`, `MAX_REAUTH_FAILURES`.
- Create request: `{"name": "terraform", "project_ids": ["..."], "max_role": "member", "expires_in_days": 90, "reauth": {"password": "..."}}`. Response 201: `TokenView` plus `"token": "nami_pat_..."`, `Cache-Control: no-store`.

**API (private listener, session only):**
- `GET /account/tokens` → 200 `[TokenView]` (own tokens, newest first, revoked ones excluded).
- `POST /account/tokens` → 201; 400 validation (empty or over 100 projects, unknown role, days out of range, a project the user cannot read: `project_ids[i]: project not found or not accessible`); 403 `reauth_required`; 409 `quota_exceeded`; 429 re-auth failures.
- `DELETE /account/tokens/{token_id}` → 204; 404 for another user's or an unknown id.
- `GET /admin/tokens` (global admin) → 200 `[TokenView + user_email]`; `DELETE /admin/tokens/{token_id}` (global admin) → 204, 404 unknown.
- A token on any of these → 403 (they read the session only).

**Behaviour:**
1. The `CurrentUser` extractor: an `Authorization` header that starts with `Bearer ` is a token attempt. Malformed, unknown, revoked or expired → 401 `{"error": "unauthorized"}`, the same body for every case. A valid token loads the user row in the same query (`api_tokens join users`), fills `TokenGrant`, and touches `last_used_at` per R-PLT-5. No session is read.
2. `require_project_role(pool, user, project_id, min)`: for `Token`, the project must be in `projects` (else 403), and `min(max_role, real_role) >= min` (else 403). For `Session`, unchanged. `require_project_reader` follows: a token capped at `viewer` reads as `ProjectReader::Viewer` even for a global admin.
3. `require_global_role` with a `Token` → 403, whatever the user's global role.
4. `GET /projects` with a token lists only the token's projects the user can still read.
5. Re-auth per R-PLT-7. Password verification and the unknown-user dummy work run in `spawn_blocking`. A TOTP code is checked with `auth::totp::verify_code`.
6. Revocation sets `revoked_at` (rows are kept until Part 2's reaper deletes those revoked or expired more than 30 days ago).
7. Events: `token.created` (metadata: name, prefix, max_role, project count, expires_at) and `token.revoked` (metadata: prefix, by_admin) are recorded once NAMI-PLT-3 lands; NAMI-PLT-3 adds the calls.

**Tests** (`tests/platform/tokens.rs`, `tests/platform/reauth.rs`, unit tests in `platform::token`):
- `a_created_token_is_shown_once_and_stored_only_as_its_hash`, `the_token_matches_the_documented_format` (unit), `a_malformed_token_is_refused_without_a_database_read` (unit on the parser).
- `a_token_reads_a_listed_project`, `a_token_gets_403_on_a_project_outside_its_list_even_for_a_global_admin`, `a_viewer_capped_token_cannot_create_a_monitor_for_an_admin_user`, `a_member_capped_token_of_a_project_viewer_is_still_a_viewer`.
- `a_token_cannot_reach_global_admin_routes`, `a_token_cannot_manage_tokens`, `projects_list_with_a_token_shows_only_its_projects`.
- `a_revoked_token_is_refused_on_the_next_request`, `an_expired_token_is_refused`, `every_refused_token_gets_the_same_401_body`.
- `last_used_at_is_written_at_most_once_a_minute`.
- `token_creation_needs_the_current_password`, `token_creation_accepts_a_totp_code`, `an_oauth_only_user_reauthenticates_with_a_fresh_session`, `a_session_older_than_the_window_must_sign_in_again`, `six_failed_reauths_in_a_row_are_rate_limited`.
- Limits: `fifty_active_tokens_are_allowed_and_the_fifty_first_is_409`, `one_hundred_projects_are_allowed_and_one_hundred_one_are_400`, `expiry_of_365_days_is_allowed_and_366_is_400`.
- `a_user_cannot_revoke_another_users_token` (404), `a_global_admin_revokes_any_token`.

**Acceptance:** a token created through the API reads a listed project's monitors over the private API with `Authorization: Bearer` and gets 403 elsewhere; all tests above pass; no token value appears in any log line (checked by grepping test output for `nami_pat_`).

### Task 3 (NAMI-PLT-3): audit log, recorder, mutation layer, admin read API and retention

**Covers:** R11.04  **Depends on:** NAMI-PLT-2.

**Files:**
- Create: `nami-engine/src/platform/audit/{mod,model,routes,layer}.rs`, `nami-engine/src/platform/reaper.rs`, `nami-engine/tests/platform/audit.rs`.
- Modify: `nami-engine/migrations/NNNN_platform_api.sql` (append `audit_events`), `nami-engine/src/core/router.rs` (wrap the API routes in the layer; mount `/admin/audit-events`), `nami-engine/src/main.rs` (spawn `platform::reaper::run_forever`), `nami-engine/src/auth/routes.rs` (login success/failure, logout, password reset, TOTP enable), `nami-engine/src/projects/routes.rs` (project created, members added/removed/role changed), `nami-engine/src/logs/source/routes.rs` (token reveal and rotate), `nami-engine/src/platform/token/routes.rs` (token events), `nami-engine/src/core/middleware/rbac.rs` (fill the audit actor slot).

**Data shape:**
- Table `audit_events` (see Data model).
- Rust: `pub enum Actor { User { id: Uuid, email: String }, Token { token_id: Uuid, user_id: Uuid, email: String }, System, Anonymous }`; `pub struct AuditEvent { actor: Actor, action: Cow<'static, str>, target_type: Option<&'static str or String>, target_id: Option<Uuid>, project_id: Option<Uuid>, metadata: serde_json::Map }`; `pub struct ClientMeta { ip: Option<IpAddr>, user_agent: Option<String> }` (extractor: IP from `ConnectInfo` and, on the private API, nami-ui's `X-Forwarded-For` as today; Part 2's NAMI-PLT-11 replaces this with the trusted-proxy rule; user agent truncated to 512 characters, control characters stripped). `pub async fn record(executor, &ClientMeta, AuditEvent) -> Result<(), sqlx::Error>`. `pub struct Recorded;` response marker. `pub struct ActorSlot(Arc<OnceLock<Actor>>)` request extension. Constants `AUDIT_RETENTION_DAYS = 365`, `AUDIT_ID_SNIFF_BYTES = 64 * 1024`, `MAX_AUDIT_PAGE = 500`, `REAPER_INTERVAL = 1 h`, `REAPER_BATCH = 10_000`.
- Read response: `{"items": [{"id", "created_at", "actor_type", "actor_user_id", "actor_token_id", "actor_email", "action", "target_type", "target_id", "project_id", "ip", "user_agent", "metadata"}], "next_cursor": "..." | null}`.

**API:** `GET /admin/audit-events?actor_user_id=&action=&action_prefix=&project_id=&target_type=&from=&to=&cursor=&limit=` (global admin, session only) → 200; 400 on bad filters (`from >= to`, limit outside `1..=500`, malformed cursor); 403 otherwise.

**Behaviour:**
1. The layer wraps the API routes (both listeners, from NAMI-PLT-4 on). Before the handler it inserts an `ActorSlot`, which `CurrentUser` fills. After the handler, for POST/PUT/PATCH/DELETE with a 2xx status and no `Recorded` marker, it records per R-PLT-14 using axum's `MatchedPath`. Requests with no filled slot (no principal) are not recorded by the layer.
2. Explicit events per R-PLT-14, with R-PLT-15 for failed logins. `auth.login.succeeded` is written when the session is fully authenticated (after TOTP when enabled). Metadata never holds a password, token, code, secret, request body or header value; event metadata keys are a fixed list per action.
3. An insert failure follows R-PLT-16.
4. `platform::reaper::run_forever(state)` runs on every instance every hour and deletes audit events older than 365 days in batches of 10,000 (`delete ... where id in (select id ... limit 10000)`), stopping when a batch deletes fewer. Part 2 adds sessions, tokens, invites and deliveries to the same loop.
5. The read API uses keyset pagination on `(created_at, id)` descending; `action_prefix` uses the `text_pattern_ops` index (`action like $1 || '%'` with `$1` escaped for `%` and `_`).

**Tests** (`tests/platform/audit.rs`, unit tests in `platform::audit::layer`):
- `creating_a_monitor_records_one_configuration_event_with_its_new_id`, `patching_and_deleting_record_the_route_template_and_target`, `a_failed_request_records_nothing`, `a_get_records_nothing`.
- `a_token_request_records_the_token_as_actor`.
- `login_success_and_failure_are_recorded`, `a_failed_login_for_an_unknown_email_records_an_anonymous_actor_without_the_email`, `logout_password_reset_and_totp_enable_are_recorded`.
- `member_changes_are_recorded_once_not_twice`, `token_create_and_revoke_are_recorded`, `revealing_or_rotating_a_source_token_is_recorded`.
- `no_audit_row_contains_a_secret` (creates a monitor with basic auth and a token, then scans every row's text for the password and `nami_pat_`).
- `only_global_admins_read_the_audit_log`, `filters_and_cursor_page_through_events_in_order`, `a_page_of_500_is_allowed_and_501_is_400`.
- `the_reaper_deletes_events_older_than_365_days_and_keeps_newer_ones`.
- Unit: `target_type_is_the_last_static_segment`, `the_project_comes_from_the_projects_id_segment`.

**Acceptance:** every mutating request of the existing test suites writes exactly one audit row (a debug assertion helper in `tests/common` counts them in a handful of representative tests); the admin API pages through them; no row holds a secret.

### Task 4 (NAMI-PLT-4): one route registry and the bearer-only REST API under `/api/v1`

**Covers:** R11.01, R11.02  **Depends on:** NAMI-PLT-3.

**Files:**
- Create: `nami-engine/src/platform/rest.rs`, `nami-engine/src/core/json.rs` (the `Json<T>` newtype, R-PLT-11), `nami-engine/tests/platform/rest.rs`.
- Modify: every domain's route files (`src/projects/routes.rs`, `src/incidents/` routes, `src/uptime/{monitor,heartbeat,group,maintenance}/routes.rs` and the import and export routes, `src/logs/{source,query,view}/routes.rs`, the log alert and pipeline routes, and the statuspages, metrics, traces, errors, rum and ai route files as they exist at phase start): each exposes `pub fn api_routes() -> OpenApiRouter<AppState>` (registered with `routes!`; the doc annotations themselves arrive in NAMI-PLT-5, so this task adds bare `#[utoipa::path]` stubs with method and path only) and keeps its session-only or public routes in separate functions. `nami-engine/src/core/router.rs` (private API built from `api_routes()`; public router gains the REST nest), `nami-engine/src/core/state.rs` (per-token buckets), `nami-engine/src/uptime/monitor/model.rs` and `nami-engine/src/uptime/heartbeat/model.rs` (caps), the `/checks` handler and its DTO, `nami-engine/Cargo.toml` (`utoipa`, `utoipa-axum`), UI callers of any list whose shape changes (`nami-ui/src/lib/server/...`, `nami-ui/src/routes/...`).

**Data shape:**
- `platform::rest::router(state) -> Router` = `Router::new().nest("/api/v1", api_routes_of_every_domain)` with `require_json_body`, the audit layer and `TokenRateLimit` (R-PLT-9) layered, and no session layer.
- Pagination: `pub struct Page { limit: u16, cursor: Option<String> }` (query), `pub struct Paged<T> { items: Vec<T>, next_cursor: Option<String> }`; cursors are opaque base64url of `(timestamp, id)`, at most 128 bytes, checked before decoding (same rule as Logs Part 2's tail cursor). Constants `DEFAULT_PAGE = 100`, `MAX_PAGE = 500`.
- `uptime::store` check rows reach the API only through `CheckView { checked_at, status, response_time_ms, status_code, error }` (Uptime Phase 1 deferral: `/checks` returned the ClickHouse row type and exposed `project_id`/`monitor_id` per row).
- Caps: `MAX_MONITORS_PER_PROJECT = 1000`, `MAX_HEARTBEATS_PER_PROJECT = 1000` next to their types.

**API:** every project-scoped route of every domain, unchanged in method, path tail, roles and status codes, also served at `/api/v1/...` on the public listener, bearer-only (a request with only a cookie is 401). Session-only routes listed in R-PLT-3 and R-PLT-20 are absent from `/api/v1` (404).

**Behaviour:**
1. Inventory first: list every route of every domain with its role, whether it is project-scoped, and whether it is a list (bounded or not). The inventory goes into the decisions doc and decides which routes enter `api_routes()` (all project-scoped ones, except the tail) and which lists change shape (R-PLT-10).
2. Each domain's `api_routes()` is the only place its project-scoped routes are registered. `core::router::build_router` merges `api_routes()` of every domain (split with `split_for_parts`, document dropped), the auth and account routes, `/admin/...`, health and the rate-limited group, with the session layer. The REST router merges the same `api_routes()` under `/api/v1`.
3. Bearer resolution happens once per request in `CurrentUser`; the REST router never sees a cookie (no session layer), so a cookie-only request fails with 401.
4. Every API handler takes `core::json::Json<T>`; rejections per R-PLT-11.
5. Unbounded lists change to `Paged<T>` (R-PLT-10); nami-ui callers are updated in the same commit, and their vitest suites still pass.
6. The monitor and heartbeat caps are counted inside the insert transaction; bulk import counts every entry it would create and refuses the whole import with 409 past the cap (dry runs report it per entry).
7. Per-token buckets per R-PLT-9; the bucket is keyed by `token_id`, so two tokens of one user are separate.

**Tests** (`tests/platform/rest.rs`, plus each domain's own target where its list shape changes):
- `a_monitor_is_created_read_updated_and_deleted_over_rest_with_a_token`, `incidents_are_listed_acknowledged_and_resolved_over_rest`, `a_heartbeat_is_managed_over_rest`.
- `a_cookie_is_not_accepted_on_rest`, `the_tail_is_not_on_rest`, `session_only_routes_are_404_on_rest`.
- `rest_answers_the_same_status_codes_as_the_private_api` (table of 403/404/400/409 cases run against both).
- `a_malformed_body_gets_the_generic_400_without_echoing_values` (sends `request_headers` with a secret-looking value), `a_non_json_post_is_415_on_rest`.
- `the_public_router_builds_with_ingest_and_rest_together` (axum would panic on overlap), `ingest_still_works_next_to_rest`.
- `ten_requests_per_second_per_token_pass_and_the_burst_overflow_is_429`, `two_tokens_have_separate_buckets`.
- `a_thousand_monitors_are_allowed_and_the_next_is_409`, `an_import_over_the_cap_is_refused_whole`, same for heartbeats.
- `incidents_page_with_a_cursor_and_limit_500_and_501_is_400`, `a_cursor_over_128_bytes_is_400_before_decoding`.
- `check_history_rows_do_not_expose_project_or_monitor_ids`.
- `a_revoked_token_ends_a_running_export_at_the_next_recheck`.

**Acceptance:** every project-scoped route answers at `/api/v1` with a token and the same semantics as the private API; the UI test suite passes with the new list shapes; the route inventory is in the decisions doc.

### Task 5 (NAMI-PLT-5): OpenAPI 3.1 document generated with utoipa, with coverage tests

**Covers:** R11.01 (documented), R11.02 (OpenAPI)  **Depends on:** NAMI-PLT-4.

**Files:**
- Create: `docs/superpowers/decisions/2026-10-24-platform-part1.md` (first commit: the utoipa decision record per STANDARDS §8 with R-PLT-12's reasoning, versions and the fallback), `nami-engine/src/platform/openapi.rs`, `nami-engine/tests/platform/openapi.rs`.
- Modify: every `#[utoipa::path]` stub from NAMI-PLT-4 (summary, tag, parameters, request body, responses with status codes and the error shape, `security(("pat" = []))`), every request and response DTO (`#[derive(ToSchema)]`, `IntoParams` on query structs; `serde_json::Value` fields as `#[schema(value_type = Object)]`), `nami-engine/src/core/router.rs` (mount `GET /api/openapi.json` on both listeners).

**Data shape:** `platform::openapi::document() -> utoipa::openapi::OpenApi`, built once (`LazyLock`) from the merged `OpenApiRouter`s: `openapi: 3.1.0`, `info { title: "Nami API", version: env!("CARGO_PKG_VERSION") }`, `servers: [{ url: "/api/v1" }]`, security scheme `pat` (HTTP bearer, `bearerFormat: "nami_pat"`), one tag per domain area (`monitors`, `heartbeats`, `incidents`, `escalation-policies`, ...), the shared schemas `Error { error: string, code?: string }` and `Paged`. Ingest endpoints are not in the document (they are not the REST API; their setup pages document them).

**API:** `GET /api/openapi.json` → 200 `application/json`, public, `Cache-Control: public, max-age=300`.

**Behaviour:**
1. Every operation lists 401 and 403, plus 404 when it has an id, 400 when it takes input, 409 and 429 where the handler can return them. Bounded lists state their cap in the description (R-PLT-10).
2. `operationId` = the handler name, unique across the document.
3. Secrets in schemas are marked `writeOnly` (proxy passwords, channel secrets, AI keys), and read DTOs never contain them.

**Tests** (`tests/platform/openapi.rs`):
- `every_documented_operation_is_routed_on_rest`: for each path and method in the document, fill path parameters with random UUIDs and send an unauthenticated request to the REST router; the answer is 401, never 404 or 405.
- `no_api_route_bypasses_the_openapi_registry`: scans `nami-engine/src/**/*.rs` for `.route(` and `Router::new()` route registrations and fails on any outside a fixed allow-list of session-only and public modules (`src/auth/routes.rs`, `src/core/health.rs`, the account and admin route files, the public ingest and ping routers, `src/platform/mcp/routes.rs`). This is the test that fails when a route is added without documentation.
- `the_document_is_openapi_3_1_with_unique_operation_ids_and_tags`, `every_operation_documents_401_and_403`, `no_read_schema_contains_a_write_only_secret`.
- `openapi_json_is_served_on_both_listeners`.

**Acceptance:** the document validates as OpenAPI 3.1 (checked once with an external validator during the task, noted in the PR), covers every route of `api_routes()`, and the two coverage tests pass.

### Task 6 (NAMI-PLT-6): MCP server over streamable HTTP with PAT auth and tool filters

**Covers:** R11.03  **Depends on:** NAMI-PLT-5.

**Files:**
- Create: `nami-engine/src/platform/mcp/{mod,protocol,tools,routes}.rs`, `nami-engine/tests/platform/mcp.rs`.
- Modify: `nami-engine/src/core/router.rs` (mount `/mcp` on the public listener with a body limit), `nami-engine/src/core/config.rs` (`MCP_ALLOWED_ORIGINS`), `nami-engine/.env.example`.

**Data shape:**
- Protocol constants: `SUPPORTED_PROTOCOL_VERSIONS = ["2026-07-28", "2025-11-25"]`, `MCP_MAX_BODY_BYTES = 1 MiB`, `MCP_MAX_ROWS = 50`, `MCP_MAX_RESULT_BYTES = 16 KiB` (equal to the AI SRE tool caps; reuse that constant if AI SRE exports it), `MAX_FILTER_HEADER_BYTES = 4096`, `MAX_FILTER_NAMES = 50`.
- `pub struct ToolDef { name: &'static str, title: &'static str, description: &'static str, read_only: bool, input_schema: fn() -> serde_json::Value, request: fn(&serde_json::Map) -> Result<RestCall, String> }`; `pub struct RestCall { method: Method, path: String, query: Vec<(String, String)>, body: Option<serde_json::Value> }`; `pub static TOOLS: &[ToolDef]` in one place.
- Tools (each maps to one REST route; `project_id` is a required argument of every tool except `list_projects`):
  - read (`readOnlyHint: true`): `list_projects`, `list_monitors`, `get_monitor` (with stats and availability), `list_heartbeats`, `get_heartbeat`, `list_incidents`, `get_incident` (with timeline), `who_is_on_call`, `search_logs`, `log_patterns`, `query_metrics`, `list_metric_names`, `search_traces`, `get_trace`, `list_error_issues`, `get_error_issue`, `list_error_events`, `list_status_pages`, `get_status_page`;
  - actions (`readOnlyHint: false`, `destructiveHint: false`): `acknowledge_incident`, `resolve_incident`, `comment_on_incident`.
- Config: `MCP_ALLOWED_ORIGINS` = comma-separated exact origins (`scheme://host[:port]`), default empty.

**API:** `POST /mcp` (public listener). `GET /mcp`, `DELETE /mcp` → 405. Status codes: 401 (missing or bad token; JSON-RPC error body, no id), 403 (foreign `Origin`; a tool call the token's scope forbids is a tool result with `isError: true`, not an HTTP 403, rule 6), 400 (malformed JSON-RPC, a batch array, header mismatch `-32020`, unsupported version, unknown names in filter headers), 404 (`-32601` unknown method), 413 (body over 1 MiB), 429 (per-token rate), 202 (accepted notification), 200 (result).

**Behaviour:**
1. Order of checks: `Origin` (present and not in `MCP_ALLOWED_ORIGINS` → 403, absent is allowed: non-browser clients send none), then the bearer token (the same resolution as `CurrentUser`; PATs only), then the per-token rate (R-PLT-9), then the body.
2. Body: one JSON-RPC 2.0 request or notification; an array → 400 `-32600`. A notification → 202 with no body.
3. Modern revision (`2026-07-28`): `MCP-Protocol-Version` must equal `params._meta["io.modelcontextprotocol/protocolVersion"]`, `Mcp-Method` must equal `method`, and for `tools/call` `Mcp-Name` (Base64 sentinel decoded) must equal `params.name`; otherwise 400 `-32020` HeaderMismatch. Methods: `server/discover` (supported versions, `capabilities: { tools: {} }`, server identity `nami` and the engine version), `tools/list`, `tools/call`, `ping`.
4. Era detection, from the 2026-07-28 "Backward Compatibility" section. A request whose body method is `initialize` is legacy whatever its headers: legacy clients send no `MCP-Protocol-Version` header on `initialize` and put the version in `params.protocolVersion`. The answer is `protocolVersion: "2025-11-25"` (the only legacy revision supported; a client that asked for another one decides whether to continue), `capabilities: { tools: { listChanged: false } }` and `serverInfo`. After that, legacy clients send `MCP-Protocol-Version: 2025-11-25` on every request: `notifications/initialized` → 202, then `tools/list`, `tools/call` and `ping` with legacy result shapes. A request with `MCP-Protocol-Version: 2026-07-28` follows rule 3. Any other version, or no header on a method other than `initialize` (which a legacy client of 2025-03-26 would send), → 400 with an `UnsupportedProtocolVersionError` listing both supported versions. No `Mcp-Session-Id` is minted or echoed (an incoming one is ignored), and `Last-Event-ID` is ignored. The implementer checks the exact result shapes against the published schema of each revision at task start.
5. Filters: `X-MCP-Tools-Only: a,b` keeps only the named tools; `X-MCP-Tools-Except: c` removes the named tools; both may be given (Only first, then Except). Names are trimmed, must match a tool in `TOOLS`, at most 50 names and 4096 bytes per header; an unknown name → 400. The filter applies to `tools/list` and `tools/call` (a filtered-out tool is "unknown tool", `-32602`).
6. `tools/call`: validate `arguments` against the tool's schema (a failure is a tool result with `isError: true`, not an HTTP error), build the `RestCall`, add `limit=50` to list routes, and run it through the REST router with `oneshot`, carrying the caller's `Authorization` header and `ClientMeta`. The REST status and body become the result: 2xx → `content: [{ type: "text", text: <JSON> }]` (plus `structuredContent` when the body is a JSON object and the revision supports it); otherwise `isError: true` with `{"status": 403, "error": "forbidden"}`. A body over 16 KiB is cut at a UTF-8 boundary and ends with `\n[truncated: result over 16 KiB]`.
7. Every `tools/call` records `mcp.tool_call` (actor = the token, project = `project_id`, metadata `{tool, status, result_bytes, truncated}`). Action tools are also recorded by the audit layer through the inner REST request; the MCP event marks the outer call.
8. Tool descriptions say that results are telemetry (untrusted text) and that action tools change incident state.

**Tests** (`tests/platform/mcp.rs`, unit tests in `platform::mcp::protocol`):
- `discover_lists_both_supported_revisions`, `a_legacy_initialize_without_the_version_header_succeeds`, `a_legacy_client_initializes_and_lists_tools`, `a_session_id_header_is_ignored_and_never_minted`, `an_unsupported_version_is_400_with_the_supported_list`, `a_header_that_does_not_match_the_body_is_400_header_mismatch` (version, method, name each), `a_batch_is_400`, `a_notification_is_202`, `get_on_mcp_is_405`, `a_body_over_one_mib_is_413`.
- `a_foreign_origin_is_403`, `an_allowed_origin_passes`, `no_origin_passes`, `a_session_cookie_is_not_accepted`, `a_missing_token_is_401`.
- `tools_only_and_tools_except_narrow_the_list`, `an_unknown_tool_name_in_a_filter_header_is_400`, `a_filtered_out_tool_cannot_be_called`, `fifty_names_pass_and_fifty_one_are_400`.
- `a_tool_call_obeys_the_token_scope` (403 as a tool error for a project outside the token), `a_viewer_capped_token_cannot_acknowledge_an_incident`, `a_member_token_acknowledges_resolves_and_comments`.
- `list_tools_return_at_most_fifty_rows`, `a_result_over_16_kib_is_truncated_and_says_so`.
- `every_tool_call_is_audited_with_its_result_size`, `sixty_calls_a_minute_pass_and_the_overflow_is_429`.
- Unit: `every_tool_maps_to_a_documented_rest_operation` (each `RestCall` path template exists in the OpenAPI document), `tool_names_are_unique_and_snake_case`, `base64_sentinel_names_are_decoded_before_comparison`.

**Acceptance:** an MCP client (MCP Inspector, run once manually and noted in the PR) connects with a PAT, lists tools, runs `list_monitors` and `acknowledge_incident`; all tests pass.

### Task 7 (NAMI-PLT-7): audit forwarding to a log source and a signed webhook

**Covers:** R11.05  **Depends on:** NAMI-PLT-3 (and NAMI-PLT-6 for `mcp.tool_call` events to forward).

**Files:**
- Create: `nami-engine/src/platform/settings.rs` (`instance_settings` model), `nami-engine/src/platform/forward/{mod,model,routes}.rs`, `nami-engine/tests/platform/forward.rs`.
- Modify: `nami-engine/migrations/NNNN_platform_api.sql` (append `instance_settings`, `audit_forward_state`), `nami-engine/src/main.rs` (spawn `platform::forward::run_forever` on every instance; the state-row lease keeps one sender per target), the AI SRE module that inserts `ai_calls` (also insert the `ai.call` audit event in the same transaction, R-PLT-17), `nami-engine/src/core/router.rs` (mount the settings routes).

**Data shape:**
- Tables `instance_settings`, `audit_forward_state` (see Data model). The secret is sealed with `core::crypto::Cipher` under the context `instance_settings.audit_forward_secret`.
- Log-source line per event: `message = <action>`, `level = "warn"` for `auth.login.failed` and failed `mcp.tool_call`, otherwise `"info"`, `dt = created_at`, `fields = the event object` (the read API's item shape). It enters through `logs::ingest::prepare` like any ingested line, so the source's pipeline, quota and usage counters apply.
- Webhook request: `POST <url>`, body `{"events": [<event objects>]}` (1 to 100, in order), headers `Content-Type: application/json`, `X-Nami-Timestamp`, `X-Nami-Signature` (R29 scheme, planned in NAMI-INC-4), `X-Nami-Delivery: <id of the first event>`; 10 s timeout; a 2xx is success.
- Settings view: `{"log_source": {"source_id", "source_name", "project_id"} | null, "webhook": {"url_host", "has_secret": true} | null, "status": {"log_source": StateView, "webhook": StateView}}` with `StateView { last_success_at, last_attempt_at, failing_since, last_error, pending: integer (events after the cursor, counted up to 10,000 and shown as "10000+") }`.
- Update request: `{"log_source_id": uuid | null, "webhook_url": string | null, "rotate_secret": bool, "reauth": Reauth}` (`deny_unknown_fields`; absent fields keep their value). Response: the settings view, plus `"webhook_secret"` once when it is created or rotated, `Cache-Control: no-store`.
- Constants: `FORWARD_BATCH = 100`, `FORWARD_TICK = 5 s`, `FORWARD_LEASE = 60 s`, `FORWARD_TIMEOUT = 10 s`, `MAX_FORWARD_BACKOFF = 1 h`.

**API (private listener, global admin, session only):**
- `GET /admin/audit-forwarding` → 200 settings view.
- `PUT /admin/audit-forwarding` → 200; 400 (unknown source, a webhook URL that is not `https://` or `http://`, has credentials, is over 2048 bytes, or names a forbidden destination per `outbound` rules); 403 `reauth_required`.
- `POST /admin/audit-forwarding/test` → 200 `{"log_source": "ok" | <error>, "webhook": "ok" | <error>}` (sends one synthetic `audit.forwarding.test` event, not stored).

**Behaviour:**
1. Forwarding semantics per R-PLT-18. Each target's state row is claimed with `update ... set lease_until = now() + 60 s where target = $1 and next_attempt_at <= now() and (lease_until is null or lease_until < now()) returning ...`, so only one instance sends per target.
2. A change of target (new source or new URL) resets that target's cursor to the newest event and clears its error state, in the same transaction as the settings update. It records `audit.forwarding.changed` (metadata: which target, source id, URL host; never the secret or the full URL query).
3. The webhook uses `outbound::client()` (operator-chosen destination, private networks allowed as for webhook channels). Part 2's NAMI-PLT-10 moves it under the unified outbound policy with every other caller.
4. Records never contain secrets (they are audit rows, which by NAMI-PLT-3 hold none). `ai.call` events carry only the R-PLT-17 metadata.
5. A deleted source sets `audit_forward_source_id` to null (FK), and the status shows "source not found" until the admin picks another.

**Tests** (`tests/platform/forward.rs`, wiremock for the webhook, a `nami_test_*` ClickHouse database for the log source):
- `events_reach_the_log_source_as_lines_with_the_event_in_fields`, `events_reach_the_webhook_in_order_with_a_valid_signature`.
- `a_failing_webhook_keeps_its_cursor_and_redelivers_after_backoff`, `a_recovered_webhook_receives_every_event_once_in_order` (at-least-once, no gap), `the_failure_is_visible_in_the_status`.
- `two_instances_never_send_the_same_batch_at_once` (lease), `a_batch_holds_at_most_100_events`.
- `changing_the_webhook_starts_from_now_and_is_audited`, `the_secret_is_shown_once_and_sealed_at_rest`.
- `forwarded_payloads_contain_no_secret` (a run that creates tokens, monitors with credentials and a forwarding secret, then scans every delivered body).
- `ai_calls_are_forwarded_as_ai_call_events`.
- `only_global_admins_manage_forwarding`, `a_token_cannot_change_forwarding`, `forwarding_changes_need_reauth`.

**Acceptance:** with both targets configured, every audit event created during the test run reaches both, in order, exactly once when the destinations are healthy and at least once after an outage.

### Task 8 (NAMI-PLT-8): UI for tokens, the audit log, forwarding, the API reference and MCP setup

**Covers:** R11.01, R11.02, R11.03, R11.04, R11.05 (UI)  **Depends on:** NAMI-PLT-7.

**Files:**
- Create: `nami-ui/src/lib/platform/{tokens,audit,openapi}.ts` (types, formatters, the OpenAPI walker), `nami-ui/src/lib/server/platform/{token-form,forwarding-form}.ts`, `nami-ui/src/routes/(app)/account/tokens/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/admin/audit/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/admin/audit/forwarding/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/admin/tokens/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/(app)/api-docs/+page.server.ts`, `+page.svelte`, and vitest files next to the lib modules.
- Modify: the app shell navigation (account menu: "API tokens"; global admins: "Audit log"), `nami-ui/.env.example` if a new variable is needed (none planned).

**Behaviour:**
1. Tokens page: list (name, prefix, role cap, projects, expiry, last used with IP, two-step revoke), create form (name, projects multi-select from the user's projects, max role, expiry 7/30/90/365 days or never, re-auth field: password, or TOTP code, or "sign in again" when the account has neither), a show-once panel with a copy button and the warning that it will not be shown again. The page is served `Cache-Control: private, no-store`.
2. MCP setup card on the tokens page: the URL `${PUBLIC_ENGINE_URL}/mcp`, a JSON config snippet with `Authorization: Bearer <token>` and the optional `X-MCP-Tools-Only` header, and the tool list.
3. Audit log page (global admin): filters (actor email, action prefix, project, target type, date range in UTC), a table, "Load more" with the cursor. All values render as text.
4. Forwarding page (global admin): source picker, webhook URL, show-once secret on create/rotate, test button, status per target (last success, failing since, last error, pending), re-auth field.
5. Admin tokens page: every user's tokens, two-step revoke.
6. API reference page (`/api-docs`, any logged-in user): fetches `/api/openapi.json` through the BFF, groups operations by tag, shows method, `/api/v1` path, summary, parameters, request schema and responses, and links each schema by name. No `{@html}`; no third-party renderer (no CDN, CSP-safe).
7. Every form goes through `callEngine`; ids pass `isUuid`; deletes are two-step; times are UTC; non-admins get the 403 page on admin routes.

**Tests (vitest):** token form parsing (`expires_in_days` mapping, re-auth field shape, project ids validated), audit filter to query string round trip, OpenAPI walker (groups by tag, resolves `$ref`, survives an operation without a body), forwarding form (null clears, absent keeps), the show-once panel never renders after navigation (state not persisted).

**Acceptance:** `npm run check` 0/0, vitest passes, and a manual pass creates a token, uses it with curl against `/api/v1`, connects an MCP client, reads the audit log and configures forwarding (noted in the PR).

### Task 9 (NAMI-PLT-9): Part 1 security review and fix wave

**Covers:** R11.14 (Part 1 review)  **Depends on:** NAMI-PLT-1 .. NAMI-PLT-8.

**Files:** fix commits as findings require; `docs/superpowers/decisions/2026-10-24-platform-part1.md` (rulings R-PLT-1..20 as kept or changed, limits and environment, the route inventory, deferred items with a target: Part 2 task or "won't do"); `docs/superpowers/roadmap.md` (tick R11.01..R11.05; R11.11, R11.13 and R11.14 stay unticked until Part 2).

**Behaviour:**
1. A dedicated opus security review of the whole phase diff. Focus, in addition to the Review Focus list: token parsing and lookup (timing, error uniformity), scope and cap enforcement on every route (spot-check that no handler authorizes by `global_role` directly), `require_global_role` refusing tokens, the REST router's missing session layer, audit completeness and secret-freedom, MCP header validation and Origin handling, forwarding signature and SSRF (destination validation at write and at send), and that no new path logs a token or a body.
2. Fix wave on the phase branch for every Critical and Important finding and the Minors the reviewer marks fix-now, then a scoped re-review until clean.
3. Then the decisions doc, the roadmap ticks, push, the phase PR (purpose, task list with commit ranges, summary, CI run, breaking changes: the paginated list shapes, the 400 body for malformed JSON; risks), and auto-merge when the review is clean (pre-authorized).

**Tests:** the whole suite in CI, plus any regression test a finding needs.

**Acceptance:** review clean, CI green, PR merged, branch and worktree deleted.
