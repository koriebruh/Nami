# Status Pages — Part 1 Implementation Plan

> **Status:** ready (starts after Incidents Part 1 merges). Nothing of this phase is merged; NAMI-SP-1 is next.
> **Branch:** `feat/statuspages-part1` in `.worktrees/statuspages-part1/`.
> **Depends on:** Logs Part 2 and Incidents Part 1, both merged to `main` with their security review and fix wave. Incidents Part 1's migrations are its per-domain phase files (such as `NNNN_incidents_part1.sql`, formerly the plan's 0060-0066); this phase numbers its own files after them.

## Roadmap coverage

Roadmap §5 (`docs/superpowers/roadmap.md`). Part 2 (NAMI-SP-17 onward) delivers the halves marked **partial**.

| Roadmap | Item | Tasks |
|---|---|---|
| R5.01 | Public status page per slug (no auth), branding (name, logo URL, accent colour), plus verified custom domains | NAMI-SP-1, NAMI-SP-2, NAMI-SP-3, NAMI-SP-6, NAMI-SP-13 |
| R5.02 | Theme (light/dark/system), up to 4 navigation links, dark-mode logo | NAMI-SP-1, NAMI-SP-2, NAMI-SP-11, NAMI-SP-13 |
| R5.03 | Sections and components bound to monitors/heartbeats, group import | NAMI-SP-1, NAMI-SP-2, NAMI-SP-11 |
| R5.04 | 90-day uptime bars, overall status banner | NAMI-SP-4, NAMI-SP-6, NAMI-SP-13 |
| R5.05 | Status updates (investigating / identified / monitoring / resolved) and scheduled maintenance announcements | NAMI-SP-5, NAMI-SP-6, NAMI-SP-12, NAMI-SP-13 |
| R5.06 | Per-update "notify subscribers" toggle (email; webhook subscribers honour the same flag once Part 2 adds them) | NAMI-SP-5, NAMI-SP-10, NAMI-SP-12 |
| R5.07 | Automatic reports from bound monitor/heartbeat incidents, per page | NAMI-SP-8, NAMI-SP-10 |
| R5.08 | Email subscribers (double opt-in, manage, unsubscribe, RFC 8058 one-click) | NAMI-SP-9, NAMI-SP-10, NAMI-SP-12, NAMI-SP-14 |
| R5.09 | Component-level email subscriptions | NAMI-SP-9, NAMI-SP-10, NAMI-SP-14 |
| R5.10 | **Partial, Part 1 half: groundwork only.** `allow_webhook_subscribers` column (NAMI-SP-1), shown disabled in the editor (NAMI-SP-11), DTO `subscriptions.webhook: false` (NAMI-SP-6), queue extension with room for webhook rows (NAMI-SP-9). Webhook subscribers themselves are Part 2 | NAMI-SP-1, NAMI-SP-6, NAMI-SP-9, NAMI-SP-11 |
| R5.11 | Password-protected (private) pages | NAMI-SP-1, NAMI-SP-2, NAMI-SP-7, NAMI-SP-13 |
| R5.12 | IP allowlist for private pages; `noindex` toggle | NAMI-SP-1, NAMI-SP-2, NAMI-SP-6, NAMI-SP-7, NAMI-SP-11, NAMI-SP-13 |
| R5.13 | Atom feed (the spec names Atom only, no RSS), embeddable status badge (SVG) | NAMI-SP-15 |
| R5.14 | Public JSON status API | NAMI-SP-15 |
| R5.15 | **Partial, Part 1 half: the data only.** `status.json` with `Access-Control-Allow-Origin: *` and the exact shape the widget reads. The widget itself (`widget.js`) is Part 2 | NAMI-SP-15 |
| R5.16 | **Partial, Part 1 half:** admin list, editor, reports, subscribers and the public pages (subscribe flows, feed, badge, JSON). The editor's widget snippet and the webhook subscriber UI are Part 2 | NAMI-SP-11, NAMI-SP-12, NAMI-SP-13, NAMI-SP-14, NAMI-SP-15 |
| (phase) | Part 1 security review and fix wave | NAMI-SP-16 |

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Nami BetterStack-style status pages. That means public pages per slug or custom domain, with branding (logo, dark logo, accent colour, theme, navigation links), sections and components bound to monitors and heartbeats, the overall banner and 90-day uptime bars, status reports (incidents and scheduled maintenance) with an update timeline, automatic reports from bound incidents, private pages (password and/or IP allowlist), `noindex`, email subscribers (double opt-in, component-level, manage and unsubscribe, one-click `List-Unsubscribe`), notifications sent through the incident delivery queue, an Atom feed, an SVG badge, the public JSON status API, and the nami-ui editor and public pages.

**Architecture:** nami-ui renders every public page server-side, with no client JavaScript (`csr = false`). It maps a verified custom-domain `Host` onto the page with SvelteKit's `reroute` hook. The engine serves one purpose-built DTO per page from an in-process snapshot cache. The cache holds pages for 30 s, rebuilds each page once at a time (single flight), and is invalidated by every admin write. The snapshot is built from Postgres only: monitor/heartbeat state, incidents, maintenance windows and reports. It reuses Uptime Phase 3's `availability::compute` per local day, so a page view never fans out to ClickHouse. The DTO routes sit on the engine's private API under `/public/...`, without a session. nami-ui is their only caller and forwards the client address and the page-access cookie. Private access is an HMAC token bound to the page and its `access_epoch`. Subscriber mail rides the incident plan's `notification_deliveries` queue, extended with status-page rows that carry a lower priority. So retries, leases, poison-row handling and per-bucket caps are shared, and a burst of subscriber mail never delays an on-call page.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, chrono-tz 0.10.4, argon2 0.6, hmac 0.13 (added by NAMI-INC-3), hickory-resolver 0.26, lettre 0.11 `raw_header`), Postgres 17, Mailpit, SvelteKit 3 + Svelte 5 runes + Tailwind 4, `@sveltejs/adapter-node@6.0.0` (new, replaces adapter-auto), vitest. No other new dependency.

**Spec:** `docs/superpowers/specs/2026-10-13-status-pages-design.md`, including its "Parity additions (2026-10-13)". Roadmap: `docs/superpowers/roadmap.md` §5 (the definition of done).

**When to start:** after **Incident Management Part 1** (NAMI-INC-1 .. NAMI-INC-15 and its security review and fix wave, plan `docs/superpowers/plans/2026-10-13-incident-management-part1.md`, branch `feat/incidents-part1`) has merged to `main`. That phase in turn needs Logs Part 2 (NAMI-LOG-14 adds `incidents.log_alert_id`), so Logs Part 2 is a transitive prerequisite. **Uptime Phase 3 NAMI-UP-27** (availability) is already on `main`, so Task 4 has no open dependency.

Which tasks build on Incidents Part 1 (everything else needs only what is on `main` today, `56916be`):

| Task | Needs |
|---|---|
| T1, T2, T4, T5 | nothing beyond `main` |
| T3, T7 | NAMI-INC-3 (`hmac`) |
| T6 | NAMI-INC-1 (`incidents.postmortem`, which the leak fixture seeds) |
| T8 | NAMI-INC-1, NAMI-INC-9 (`resolve_source_with_reason`), NAMI-INC-10 |
| T9, T10 | NAMI-INC-1 (`open_test_incident`), NAMI-INC-3, NAMI-INC-5 (queue, drain, `kick`, `run_incident_ticks`) |
| T11, T12 | NAMI-INC-7 (`data.role`); T12 and T13 also NAMI-INC-12 (`renderMarkdown`) |

### Interfaces assumed from Incidents Part 1 (planned, not on `main` yet)

Each line gives what this plan uses and the task that uses it. If the merged code differs in a name or a line, adapt minimally (same behaviour, same tests) and say so in the phase PR. If it differs in behaviour, stop and ask the controller.

| From | Interface | Used by |
|---|---|---|
| NAMI-INC-1 | `incidents::lifecycle::{IncidentSource::{Monitor, Heartbeat, Manual { created_by }, ..}, OpenIncident { project_id, source, title, cause, severity_id, metadata, started_at }, open, resolve, resolve_source}`, each taking `&mut PgConnection` inside the caller's transaction. `open` returns `Option<Uuid>`, `resolve` returns `bool`, `resolve_source` returns `Option<Uuid>`. Incident statuses are `open`/`acknowledged`/`resolved`; Incidents Part 1's phase file (`NNNN_incidents_part1.sql`, planned in NAMI-INC-1) adds `incidents.{source, severity_id, metadata, postmortem, ..}` with a trigger that derives `source` for plain-SQL inserts. `tests/common::open_test_incident(pool, project, created_by) -> Uuid`. | T8, T9 |
| NAMI-INC-3 | `hmac 0.13` as a direct dependency. `outbound::is_non_public(ip)` (Part 2 webhooks only). | T3, T7, T9 |
| NAMI-INC-5 | `notification_deliveries` (created in Incidents Part 1's phase file, planned in NAMI-INC-5) exactly as that plan writes it (`round` 1-32 chars, unnamed checks on `event` and on `(user_id, channel_id)`), the claim SQL in its Task 5 Step 3 (a)-(c), the per-row steps (2a)-(2f), `deliveries::{drain, enqueue, NewDelivery, DeliveryEvent, Target, MAX_ATTEMPTS, DRAIN_BATCH, backoff, LEASE}`, `send::{SendError { message, retry }, Retry::{Never, After}}`, `incidents::worker::kick(&AppState)`, `Config.incidents_worker_enabled`, `tests/common::run_incident_ticks(&AppState)`, the test files `tests/incidents/incident_lifecycle.rs` and `tests/incidents/notification_deliveries.rs`. | T8, T9, T10 |
| NAMI-INC-7 | `GET /projects/{id}` returns `role: "admin" \| "member" \| "viewer"`. The project layout passes it on as `data.role` (on `main` the layout load returns `{ project }` only). | T11, T12 |
| NAMI-INC-9 | `lifecycle::resolve_source_with_reason(conn, source, at, reason)`. | T8 |
| NAMI-INC-10 | `main.rs` spawns `incidents::worker::run_forever`. `alerts::notify_transition` calls `kick`. | T8, T10 |
| NAMI-INC-12 | `renderMarkdown(source: string): string` (escape first, fixed subset, `http(s)` links with `rel="noopener noreferrer nofollow"`), planned as `nami-ui/src/lib/markdown.ts`; import it from the path it merges at. | T12, T13 |

On `main`, re-checked against `56916be` on 2026-10-24 (names below are current): `uptime::maintenance::model::{Span, MaintenanceWindow::{spans, applies_to_monitor, applies_to_heartbeats}, list_for_project, active_windows, ActiveWindows::{covers_monitor, covers_heartbeats}}`, `uptime::availability::{compute, union, subtract, seconds, Availability}` (`Availability` has **no** `Default`; `subtract_sorted` and `incident_spans` are private and not used here), `core::config::Config::{from_env, from_env_for_test}` (both still end in the private `from_env_defaults_only`), `core::crypto::{Cipher::{new, derive_subkey}, TEST_KEY}`, `core::email::Mailer::send` (SMTP failures are `AppError::Validation("could not send email")`), `core::router::{build_router, build_public_router, with_fallback_connect_info}`, `core::validate::{validate_label, nullable, is_invisible_or_bidi_char}` (the last is `pub(crate)`), `core::middleware::rbac::require_project_role`, `auth::password::{hash_password, verify_password}`, `uptime::checker::is_unsafe_in_text`, `uptime::checker::dns::{build_resolver, readable}` (both private; Task 3 makes them `pub(crate)`), and in `tests/common`: `app`, `test_app`, `public_app`, `call`, `login`, `user_with_role`, `seed_user`, `seed_project`, `seed_http_monitor`, `unique_email`, `settled_mail`, `stored_mail_for`, `StoredMail`, `test_cipher`. `heartbeats` still has no `unique (id, project_id)` (`0003_uptime.sql`). This plan no longer uses `AppError::Gone` (NAMI-INC-11): status routes answer their fixed bodies through one local helper (see Global Constraints).

**Drift (2026-10-24).** The UI is no longer the dark theme this plan was written against. Every app page now uses the light "Atom" design tokens of `nami-ui/src/routes/layout.css` and the components of `nami-ui/src/lib/components/ui/` (for example `nami-ui/src/routes/(app)/projects/[id=uuid]/maintenance/+page.svelte:46`, `card`, `text-ink`, `text-muted`), and the project tabs are a `SegmentedTabs` list (`nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte:11-16`). Wherever a task below says "dark theme classes", "copy the maintenance page classes" or "a Tailwind bg class", read: the design tokens and `$lib/components/ui` components named in CLAUDE.md ("UI conventions"), never raw hex values or Tailwind's stock palette. Status colours map onto the `status-*`, `success`/`warning`/`danger`/`info` tokens. The public page keeps its own `light`/`dark`/`system` theme (spec) and the validated accent as `--accent`, built from the same tokens.

**Part 2 follows** (a separate plan, NAMI-SP-17 onward, with its own phase migration files). It covers:
- webhook subscribers: the public form when `allow_webhook_subscribers` is on, admin add, the `{"type":"verification","challenge":…}` handshake within 10 s, strict public-only outbound with DNS-rebinding-safe connects and no redirects, per-subscriber sealed HMAC secret shown once, `X-Nami-Signature`, auto-disable after 20 consecutive failures. They go through the same queue extension as Part 1's email rows (Task 9 leaves `webhook_subscriber_id` room for them);
- the `component.status_changed` event (the snapshot builder records each component's last published status);
- the embeddable announcement widget (`widget.js`, Shadow DOM, constructable stylesheets, 5 KiB gzip budget, DOM tests) and the editor's widget snippet;
- Part 2's final security review.

Part 1 leaves room for these:
- `status_pages.allow_webhook_subscribers` exists, and the editor shows it disabled with "available soon".
- The DTO's `subscriptions.webhook` is always `false` in Part 1.
- `status.json` already sends `Access-Control-Allow-Origin: *` and the exact shape the widget reads.

**Migrations.** The phase adds three Postgres files. NNNN is the next free number in `nami-engine/migrations/` when the phase starts (after Logs Part 2's and Incidents Part 1's phase files), and the files are numbered in this order because their foreign keys depend on each other:
1. `NNNN_uptime_statuspages_part1.sql` (created by NAMI-SP-1): `heartbeats_id_project_key`, the key the components' composite foreign key needs. `heartbeats` is an uptime table, so the change goes in an uptime file.
2. `NNNN_statuspages_part1.sql` (created by NAMI-SP-1; NAMI-SP-5, NAMI-SP-7 and NAMI-SP-9 append to it, which is allowed because it has not merged): pages, sections, components, reports, rate-limit hits, subscribers.
3. `NNNN_incidents_statuspages_part1.sql` (created by NAMI-SP-9): the `notification_deliveries` extension (R17). It references `status_pages`, `status_page_subscribers` and `status_report_updates`, so it comes last.

`sqlx` records each applied file's checksum, so a local dev database that applied the statuspages file before a later task appended to it must be recreated; `#[sqlx::test]` databases are fresh for every test. No ClickHouse DDL. Part 2 adds its own phase files.

**Verification status (2026-10-24):** this plan was written against `main` at `30d24d7` plus the stated end state of the Incident Management Part 1 and Uptime Phase 3 plans, reviewed against `main` at `9599bb8`, rebased onto `a1192a0` (NAMI-UP-23 and NAMI-LOG-10 merged; UP-23 adds `monitors.{ip_version, remember_cookies, proxy_url, proxy_username, proxy_password_enc}`, now in the leak fixture), and re-checked on 2026-10-24 against `main` at `56916be` after the domain restructure (paths, modules, test targets and migration names translated; interfaces above). Signatures spot-checked: `uptime/availability.rs`, `uptime/maintenance/model.rs`, the monitor/heartbeat/group models and `0003_uptime.sql`, `core/{crypto,config,error,email,router,serve,state}.rs`, `tests/common/mod.rs`, nami-ui `lib/server/core/{engine,fake-event}.ts`, `params.ts`, `vite.config.ts`, `package.json`, `@sveltejs/kit` 3 types and the `@sveltejs/adapter-node@6.0.0` source. Its code has **not** been applied to a scratch copy. Tests are given in full where they pin a security property: leaks, tenant isolation, tokens, private access, escaping and rate limits. Code that decides something is given in full: migrations, SQL, token formats, the status and history rules, the queue extension and the DTO shape. Elsewhere the plan gives exact signatures, names and values, and the implementer writes the body (the Incidents Part 1 convention). A test that fails for a reason the plan did not predict is a plan bug: fix it minimally and say so in the phase PR.

**Execution:** one phase, one branch (CLAUDE.md "Git branching workflow", `docs/superpowers/plan-conventions.md`): `feat/statuspages-part1` in `.worktrees/statuspages-part1/`, from the latest `origin/main` once Incidents Part 1 has merged. Each task is one or more commits on that branch with its task ID in the message (Conventional Commits, ending with the `Co-Authored-By` line of Global Constraints), then a task review (spec compliance and quality) with fix commits on the same branch until it is clean, then the next task. Tasks run in order. T3 depends only on T1/T2 and T11 only on T2, T3 and T5; if they run in parallel, they use an extra worktree on a short-lived local branch that is merged back into `feat/statuspages-part1` before its review (CLAUDE.md rule 5). T15 renders from Task 13's helpers but runs after T14. After T15 comes NAMI-SP-16, the phase security review and fix wave (last section), which also writes the decisions doc and ticks the roadmap; then the one phase PR. Engine tests need `docker compose up -d postgres mailpit` (never restart running containers; ClickHouse is not used by this plan).

The **engine command** is used throughout:

```bash
cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test <args>
```

Integration tests live in domain targets: this phase adds `tests/statuspages/main.rs` (Task 2) and also runs `core`, `incidents` and `uptime`. `cargo test` takes one positional filter, so several targets and files go in one command as `--test statuspages --test core -- status_pages_api:: json_bodies::` (every target runs the tests that match any filter). Unit tests by module go in their own command (`--lib -- <filter> <filter>`); never combine `--test x` with a `--lib` filter.

The **UI command** is `cd nami-ui && npm run check && npx vitest run <files>`.

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**, Mailpit 1025/8025). Never install anything natively. Never restart Docker.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`), SQL strings are `&'static str` literals, never `format!`. No ORM.
- axum 0.8 path syntax `{param}`. Authorization failure is 403. A resource of another project inside an accessible project path is 404. A public page that is unknown, unverified (domain) or private without access is **404**, the same body for each (spec: no existence oracle).
- Every state-changing engine route takes a JSON body (`require_json_body`, 415 otherwise) and is added to `tests/core/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json` in the task that adds it. The one exception is the RFC 8058 one-click route on the public listener (Task 10), which takes `application/x-www-form-urlencoded` by definition. Every new table is added to `tests/core/migrations.rs::all_tables_exist`.
- Spec values, verbatim:
  - slug `^[a-z0-9-]{3,63}$`, globally unique;
  - `logo_url`/`logo_dark_url` https only;
  - `accent_color` `#rrggbb`;
  - `theme` `light` | `dark` | `system`;
  - `nav_links` ≤ 4 `{label ≤ 40 chars, url}`, url `https:` or `mailto:` only (`javascript:`, `data:`, `http:` → 400);
  - `visibility` `public` | `private`;
  - `ip_allowlist` ≤ 50 CIDRs, private only;
  - a private page needs a password, an allowlist, or both;
  - the password cookie lives 30 days;
  - unlock is limited to 10 attempts / 15 min / IP / page;
  - subscribe is limited to 5 / hour / IP and per page;
  - DTO cacheable 30 s;
  - badge `Cache-Control: max-age=60`;
  - `status.json` `Cache-Control: max-age=30`, `Access-Control-Allow-Origin: *`, public pages only.
- Component status vocabulary: `operational`, `degraded`, `partial_outage`, `major_outage`, `maintenance`, `no_data`. Report status: incident `investigating` | `identified` | `monitoring` | `resolved`; maintenance `scheduled` | `in_progress` | `completed`.
- RBAC (R22):
  - viewers read pages, reports and settings (never subscriber emails);
  - members create and edit reports and post updates;
  - admins create, edit and delete pages, sections, components and custom domains, delete reports, and list and remove subscribers.
- Public output never contains: monitor or heartbeat names (unless an admin typed them as a display name), URLs, hosts, ports, keywords, headers, causes, incident titles or details, project, monitor, heartbeat, incident or user ids, member or subscriber emails. Component, section and report ids are page-scoped public ids and may appear.
- No type that holds a token key or an access token derives `Debug`. Keys are held in `zeroize::Zeroizing`. Never pass passwords, tokens or subscriber emails to `tracing`.
- nami-ui:
  - every engine call goes through `callEngine` (authenticated) or the new `callPublicEngine` (public routes: no cookie forwarded, no `set-cookie` relayed), and every path through `enginePath`;
  - route params use `[...=uuid]` matchers where they are ids;
  - form actions return `engineFailure(...)`;
  - deletes take two steps (`confirm=yes`);
  - follow SvelteKit 3 conventions (`#lib`, `process.env` in server code, runes, `page` from `$app/state`, `defaultValue`/`defaultChecked`);
  - admin pages use the design tokens and `$lib/components/ui` components (CLAUDE.md "UI conventions"; see Drift (2026-10-24) above).
- Tests: Mailpit assertions filter by their own unique recipient and never delete messages.
- Fixed public error bodies. `AppError` on `main` has no variant for a custom 401/410/429 message (`Unauthorized` says `unauthorized`, `RateLimited` says `too many requests`). Status routes therefore answer their fixed bodies through one helper in `statuspages::public::routes`: `pub(crate) fn json_error(status: StatusCode, message: &'static str) -> Response` (`{"error": message}`). Every message is a `&'static str` literal, so no public error body ever echoes input. A 404 is always `json_error(NOT_FOUND, "not found")`, byte-identical to `AppError::NotFound`.
- Test literals: an array that mixes string literals with `&format!(..)` or `&x.to_string()` does not compile (`expected &str, found &String`). Write `format!(..).as_str()` / `x.to_string().as_str()` instead (temporaries in a `for` iterator expression live for the whole loop).
- `argon2` work on public routes (`verify_password` in unlock) runs in `tokio::task::spawn_blocking`, at most 4 at once per process (`static UNLOCK_SLOTS: Semaphore = Semaphore::const_new(4)`), so a burst of unlock attempts cannot stall the async workers. An `Err` from `verify_password` (a malformed stored hash) is treated as a wrong password, never as a 400.
- Commits: Conventional Commits, message ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **A custom-domain request must never reach the app or another page.** Example: `https://status.acme.test/login`, `/projects/…`, `/status/other-slug`, or `/status/_domain` sent to the app host. Each must be a 404 with no session cookie read or set → Task 13 (`a_custom_domain_reaches_only_its_own_page`, `the_domain_sentinel_is_refused_on_the_app_host`) and Task 6 (`an_unverified_or_foreign_domain_is_a_404`).
- **A page view must stay cheap under a flood.** Example: 500 concurrent requests for one slug right after its cache entry expired. That must cost one snapshot build, and requests beyond the per-IP quota must get 429 → Task 6 (`concurrent_requests_build_the_snapshot_once`, `public_reads_are_rate_limited_per_client_address`).
- **Day boundaries in the page's time zone must not drop or double an hour.** Example: a page in `Europe/Berlin` across the spring-forward and fall-back days, and an incident spanning midnight. Each local day is its own bar, 23 h and 25 h days included, and the total downtime of the 90 bars equals the incident's length → Task 4 (`days_follow_the_page_zone_across_dst`, `an_incident_across_midnight_is_split_between_its_days`).
- **A cached snapshot must never reach a visitor without access.** Example: a public page is viewed (cache warm), then made private by another instance. The next request without a token must be a 404, and a stranger's `If-None-Match` with the right ETag must be a 404, not a 304 → Task 7 (`going_private_applies_at_once_even_with_a_warm_cache`, `a_private_page_never_answers_304_without_access`).
- **A spoofed `X-Forwarded-For` must never widen private access.** Example: a visitor sends `X-Forwarded-For: 10.0.0.1` to nami-ui for a page that allowlists `10.0.0.0/8`. Also: the engine is called without a forwarded address, so its peer is nami-ui's own loopback, and the page allowlists `127.0.0.0/8` → Task 7 (`a_missing_forwarded_address_grants_no_ip_access`) and Task 13 (`the_bff_forwards_only_the_adapter_client_address`).
- **Subscriber mail must not delay pages to on-call responders.** Example: a page with 10,000 subscribers posts an update while a monitor goes down. The on-call email must go out in the next drain → Task 9 (`incident_deliveries_are_claimed_before_status_page_mail`).
- **A visitor must not learn whether an address is subscribed.** Example: posting the same address twice, a confirmed address, and a new address. Each must give the same status and body, and no second email inside the resend cap → Task 9 (`subscribe_answers_the_same_for_new_pending_and_confirmed_addresses`).

## Controller rulings (2026-10-19)

Recorded in the decisions log by NAMI-SP-16, verbatim:

1. **Signed subscriber tokens replace "hashed at rest" (spec deviation).** Subscriber links are HMAC-SHA256 tokens under `cipher.derive_subkey("nami/status-subscriber/v1")` (a label of their own), each with a purpose tag (confirm / manage / unsubscribe), checked in constant time. Confirm tokens expire after **48 h**. Manage and unsubscribe tokens never expire, and are invalidated when the subscriber is deleted (their nonce lives on the subscriber row). The spec's "single-purpose and hashed at rest" becomes "single-purpose, signed, nothing usable stored" (R14).
2. **Bar thresholds accepted:** 100 % operational, ≥ 99 % degraded, ≥ 95 % partial outage, below that major outage (R7).
3. **Caps accepted:** 10,000 confirmed and 500 pending subscribers per page, 3 emails per address per page per day, 20 emailed (notified) updates per report (R8, R15).
4. **Custom domains need DNS TXT verification before they are served (spec addition).** An unverified domain routes nowhere (R5).
5. **Paused monitors and heartbeats show "no data" (grey), never operational** (R6). An active maintenance window still shows `maintenance` for a paused source, since the window is an explicit operator signal; neither is `operational`. Confirmed by the controller after review: maintenance wins over paused.
6. **IP allowlist fails closed without a trusted client-address header (post-review ruling).**
   - nami-ui grants allowlist access only when `ADDRESS_HEADER` is explicitly configured, read once at startup. Otherwise the allowlist grants nothing: password unlock still works, and an allowlist-only page stays locked.
   - The admin editor warns on the allowlist field: "client addresses are not trusted on this deployment; set ADDRESS_HEADER behind your proxy".
   - Implemented in T7 (engine: the BFF forwards `client_ip` only when trusted) and T13 (nami-ui config + warning).
   - Tests: with no header configured, an allowlisted address is still refused; with the header configured, it is admitted.
7. **Accepted additions and limits:**
   - a day wholly in maintenance shows a `maintenance` bar;
   - unlock is limited to 30 attempts per address per 15 min;
   - one-click requests allow a burst of 100 at 50/s;
   - history bars do not show past pause intervals;
   - status changes rely on the 30 s TTL;
   - one-click tokens sit in the URL path, per RFC 8058.

## Plan review (2026-10-19), what changed

The review of PR #62 against `main` at `9599bb8` fixed, among smaller things:
- **Cache and access (R10, R12, T6, T7).** The access decision now uses the page row read fresh on every request, never the cached snapshot, so a visibility, password or allowlist change applies on every instance at once. Snapshots are keyed by `(page_id, version)`, where `version = status_pages.updated_at` and every write that changes the DTO bumps it, so invalidation is correct across instances. `If-None-Match` is evaluated only **after** access is granted (a private page never answers 304 to a visitor without access: that would be an existence and change oracle).
- **adapter-node 6 facts (R4, T13).** There is no `ORIGIN` env var in adapter-node 6 (it is `kit.paths.origin`, which must stay unset). Without `PROTOCOL_HEADER` the protocol defaults to `https`. `ADDRESS_HEADER` unset means the socket peer, which behind a proxy is the proxy itself: an allowlist containing the proxy's network would then admit everyone. Deployment rules are spelled out. CSRF stays on (`csrf.trustedOrigins` never `'*'`; Kit 3 removed `checkOrigin`).
- **Routing (R4, T13).** `/_nami/tls-ask` is exempt from the reroute (Caddy calls it on an internal host name, which the reroute would otherwise treat as a custom domain). `_domain` is refused whenever `APP_ORIGIN` is unset. Domain lookups use the lowercased hostname without a trailing dot. Security headers and `no-store` also apply to 404s on custom domains.
- **Enumeration (R15).** A full page answers 429 for **every** address, so the cap cannot tell a known address from a new one.
- **One-click unsubscribe (R16, T10).** Idempotent: a valid token whose subscriber is already gone answers 200. The governor no longer keys a mail provider's whole burst on one proxy peer, and the test router gets `with_fallback_connect_info`.
- **Unlock (R12, R13).** argon2 off the async threads with a concurrency cap; a per-address bucket across pages (`unlock-ip`, 30 / 15 min).
- **Drift fixes.** `Availability` has no `Default`; `Config::from_env` spreads `from_env_defaults_only`, so `TxtLookup` is set in both places; `bytes` is not a dependency (`axum::body::Bytes`); `Mailer::send` reports SMTP failures as `Validation` too, so the header test compares messages; `PAGE_COLUMNS` must be a `macro_rules!` for `concat!`; mixed-literal test arrays; `tokio::spawn(kick(..))` borrows; INC-9's `resolve_source_with_reason` gets the automatic-report hook too; the delivery-queue constraint swap (now in the incidents file) asserts it dropped exactly two checks.
- **Leak tests** now also seed heartbeat tokens, header names, basic-auth users, proxy URLs and users (UP-23), group names, incident `details` and `postmortem`, and an automatic-report email is checked for monitor names, targets and causes.

## Decisions the spec leaves open (rulings)

- **R1 Split.** Roadmap §5 has 16 items (R5.01..R5.16). Planned whole, they need about 18 tasks. Part 1 is the core named in the header (15 tasks plus the security review, NAMI-SP-16; see Roadmap coverage). Part 2 is webhook subscribers, `component.status_changed` and the widget. Those three are self-contained, and the Part 1 data model already carries their columns and toggles.
- **R2 Migrations.**
  - `NNNN_uptime_statuspages_part1.sql` (T1): `unique (id, project_id)` on heartbeats for the components' composite FKs (monitors have it in `0003_uptime.sql`). Applied first.
  - `NNNN_statuspages_part1.sql`, one file built up by four tasks in this order:
    - T1: pages, sections, components;
    - T5 appends: reports, affected components, updates;
    - T7 appends: DB-backed rate limits (multi-instance safe);
    - T9 appends: email subscribers and their component subsets.
  - `NNNN_incidents_statuspages_part1.sql` (T9): extends `notification_deliveries`. Applied last (it references status pages, subscribers and report updates).
- **R3 Who serves the public pages: nami-ui, with the DTO on the engine's private API.** The spec names `hooks.server.ts` and nami-ui routes, so nami-ui renders HTML, feed, badge and JSON. The DTO routes (`/public/status-pages/…`, `/public/status-domains/…`, `/public/status-subscriptions/…`) are added to `build_router` (the private API), not to the public listener:
  - IP allowlists and rate limits need a client address. The private API gets it from nami-ui (`getClientAddress()` → `X-Forwarded-For`, as login does). The public listener is reached directly by any shipper, and its forwarded headers are forgeable.
  - The public listener would also put a second, cacheable HTML-free surface on the internet with its own rate limiting.

  The one route on the public listener is RFC 8058 one-click unsubscribe (R16), because mail providers POST it cross-site as a form, which SvelteKit's origin check (kept on) refuses.
- **R4 Host routing.**
  - nami-ui gets `APP_ORIGIN` (for example `https://nami.example.com`). When it is unset (an empty value counts as unset), custom domains are off and every host is the app.
  - `src/hooks.ts` `reroute` (server only; it returns `undefined` in the browser) maps every request whose `url.host` is not `APP_ORIGIN`'s host to `/status/_domain` + path. `_domain` can never be a slug (`_` is outside `[a-z0-9-]`). Two prefixes are never rerouted: `/_app/` (static assets) and `/_nami/` (the TLS `ask` endpoint below, which Caddy calls on an internal host name such as `nami-ui:3000`; it is harmless on any host).
  - The `/status/[slug]` loads treat `_domain` as "look the page up by the request's hostname" (`url.hostname`, lowercased, one trailing `.` stripped, then the same hostname rule as `validate::custom_domain`). `_domain` → 404 on the app host, and always when `APP_ORIGIN` is unset (custom domains off).
  - So on a custom domain `/login` becomes `/status/_domain/login` (no such route → 404), and `/status/x` becomes `/status/_domain/status/x` (404). Only the page's own routes exist. `/status/%5Fdomain` on the app host decodes to the slug `_domain` → 404.
  - A custom domain is served only after verification (R5). The app host itself (from `ui_base_url` in the engine, and `APP_ORIGIN` in nami-ui) can never be stored as a custom domain.
  - Forging the Host: a client that sends `Host: status.acme.test` straight to nami-ui gets exactly what visiting that domain gives it (a public page, or a 404 without access). It gains nothing, because access cookies are scoped by the browser to the real host, and the engine decides access.
  - nami-ui switches to `@sveltejs/adapter-node@6.0.0` (peer `@sveltejs/kit ^3.0.0-next.0`; the repo has Kit 3.0.1). Checked against its `handler.js`:
    - **Client address.** `getClientAddress()` is the socket peer unless `ADDRESS_HEADER` is set. Behind a reverse proxy the peer is the proxy, so every visitor would share one address (one rate-limit bucket), and an allowlist containing the proxy's network would admit everyone. Deployments behind a proxy **must** set `ADDRESS_HEADER=x-forwarded-for` and `XFF_DEPTH` = the number of proxies that append to it (1 for a single Caddy/nginx). adapter-node then reads the entry that many places from the right, which the client cannot forge. With `ADDRESS_HEADER` set and the header missing, `getClientAddress()` throws; `callPublicEngine` catches that and sends no address (no IP access, the `unknown` rate bucket).
    - **Origin.** adapter-node 6 has no `ORIGIN` env var: the fixed origin is `kit.paths.origin` in the Kit config, which must stay **unset** (custom domains need the per-request host). The origin is then `PROTOCOL_HEADER` (default `https` when unset) + `HOST_HEADER` (when set) or `Host`. Recommended: the proxy forwards the original `Host` (Caddy does by default, nginx `proxy_set_header Host $host`) and `HOST_HEADER` stays unset. Set `HOST_HEADER=x-forwarded-host` only if the proxy overwrites that header on every request; otherwise a client could choose `url.host` (cache-poisoning risk behind a shared cache). Set `PROTOCOL_HEADER=x-forwarded-proto` (overwritten by the proxy) when the proxy may speak plain http to clients.
    - **CSRF.** Kit 3 checks the `Origin` of form posts against `url.origin` and has no `checkOrigin` switch any more; `csrf.trustedOrigins` stays unset (never `'*'`). Custom-domain forms pass because `url.origin` is the custom domain.
    - **Body size.** `BODY_SIZE_LIMIT` keeps its 512K default (every status form is tiny).
    - `.env.example` and the README document all of this.
  - TLS for custom domains is the operator's job (spec non-goal: automatic TLS). nami-ui exposes `GET /_nami/tls-ask?domain=<host>` (any host, never rerouted): 200 for a verified custom domain, 404 otherwise. That is Caddy's on-demand TLS `ask` endpoint. The README documents the Caddy snippet and the manual alternative (one certificate per domain in any proxy). Internal callers other than the ask endpoint (health checks) must send the `APP_ORIGIN` host, since every other host is treated as a custom domain.
- **R5 Custom-domain verification.**
  - The admin adds a TXT record `_nami-verify.<domain>` with the value `nami-verify=<token>`.
  - `<token>` is the first 32 hex chars of HMAC-SHA256 under `cipher.derive_subkey("nami/status-domain/v1")` over `"{page_id}|{domain}"`. It is stateless and stable for the pair, and changes if either changes.
  - `POST …/custom-domain/verify` looks the record up and sets `custom_domain_verified_at`. Changing `custom_domain` clears it.
  - The lookup goes through `Config.txt_lookup: TxtLookup`: `System` (hickory, system resolver, 5 s timeout) in `from_env`, and `Fixed(BTreeMap<String, Vec<String>>)` only in tests. No deployment can switch it. On `main`, `from_env` ends with `..Self::from_env_defaults_only()` and `from_env_for_test` *is* `from_env_defaults_only`, so: `from_env_defaults_only` sets `txt_lookup: TxtLookup::Fixed(BTreeMap::new())`, and `from_env` lists `txt_lookup: TxtLookup::System` explicitly before the spread. A unit test in `config.rs` pins both.
  - Verification proves DNS control, so a project admin cannot claim another team's hostname, and an unverified domain routes nowhere.
- **R6 Component status.** Derived per component, then overridden:

  | bound source | status |
  |---|---|
  | in an active maintenance window (`ActiveWindows::covers_monitor` / `covers_heartbeats`) | `maintenance` |
  | paused | `no_data` |
  | `current_status = 'up'` | `operational` |
  | `current_status = 'down'` | `major_outage` |
  | `current_status = 'pending'` | `no_data` |

  - Controller ruling 5: paused → `no_data` (grey), never `operational`. Maintenance is checked first, so a paused source inside a window shows `maintenance`. History bars (R7) cannot know past pause intervals (nothing records them), so they reflect incidents and maintenance only.
  - Override: when one or more **active** reports affect the component, its status is the **worst** impact among those reports, even when that is better than the derived status (the spec's "overridable").
  - A report is active when its status is not `resolved`/`completed`. A `scheduled` maintenance report counts as active only while `starts_at <= now < ends_at`.
  - Severity order, worst first: `major_outage` > `partial_outage` > `degraded` > `maintenance` > `operational` > `no_data`. The overall banner is the worst component status; a page with no components is `no_data`.
- **R7 90-day history.**
  - Bars cover the 90 local days ending today, in the page's `timezone`. Each day `[local midnight, next local midnight)`, converted with `earliest()` (a midnight inside a DST gap moves to the first valid instant), is clipped to `[component.created_at, now)`.
  - Per day: `availability::compute(day, incidents, maintenance)`. Incidents are those of the bound monitor/heartbeat. Maintenance is the project's windows that apply to it (`applies_to_monitor` / `applies_to_heartbeats`, `MaintenanceWindow::spans`).
  - A clipped day that is empty → `no_data`. A day whose `uptime_pct` is `None` because maintenance covered all of it (`maintenance_seconds == total_seconds > 0`) → `maintenance`; any other `None` → `no_data`. Otherwise (controller ruling 2) `100.0` → `operational`, `>= 99.0` → `degraded`, `>= 95.0` → `partial_outage`, else `major_outage`.
  - The tooltip shows the date, `uptime_pct` rounded to 2 decimals, downtime minutes, and the titles of **published reports** that affect the component and overlap the day. It never shows incident titles or causes (those carry monitor names and errors). `show_uptime_bars = false` → `bars: null`.
- **R8 Reports.**
  - Title 1-255 chars, one line (`validate_label` semantics with 255). A message is 1 byte to 10 KiB of markdown.
  - At most 100 affected components per report, at most 200 updates per report, at most 20 updates per report with `notify_subscribers = true` (the per-incident email cap, controller ruling 3; the 21st notified update → 400 `this report has reached 20 notified updates; post without notifying subscribers`).
  - Impacts are `degraded`, `partial_outage`, `major_outage`, `maintenance`. Maintenance reports require `starts_at < ends_at` (≤ 366 days apart).
  - Creating a report creates its first update (the initial message and status) in the same transaction. Posting an update sets the report's status (any status of the report's kind, in any order) and `updated_at`.
  - `published_at` = creation time (no drafts in Part 1). The public page lists active reports, upcoming maintenance (`scheduled`, `starts_at > now`), and history (reports created in the last 90 days, newest first, at most 50, each with at most 20 newest updates).
- **R9 Automatic reports.** `lifecycle::open` (Monitor/Heartbeat sources only) calls `statuspages::automatic::on_incident_opened(conn, incident_id, &source, at)` in the same transaction.
  - For every page with `automatic_reports` that has a component bound to the source, it inserts a report: kind `incident`, status `investigating`, impact `major_outage` on each bound component of that page, `incident_id`, `automatic = true`.
  - Its title is `"{display names joined ', '} — service disruption"`, truncated to 255 chars with `…`. Its first update reads `We are investigating a disruption affecting {names}.` with `notify_subscribers = page.automatic_reports_notify`. The insert uses `on conflict (status_page_id, incident_id) where incident_id is not null do nothing`.
  - Every resolve path calls `on_incident_resolved(conn, incident_id, at)`: `lifecycle::resolve`, `resolve_source`, and NAMI-INC-9's `resolve_source_with_reason`. Put the call in the one internal function they share if the merged code has one; otherwise in each of the three. It posts a `resolved` update `This incident has been resolved.` (notify per the same flag) to every report with that `incident_id`, `automatic = true` and a status other than `resolved`.
  - A member resolving the report by hand sets `automatic = false`, which ends automatic handling. An update to another status leaves it automatic.
  - Text uses component display names only.
- **R10 Snapshot cache.** It is `statuspages::cache::SnapshotCache` in `AppState.status_pages`, in-process and per instance.
  - Every public request first reads the page row (`find_by_slug` / `find_by_verified_domain`, one indexed query). **Access (R12) is decided from that fresh row, never from a cached snapshot**, so a password, allowlist or visibility change applies at once on every instance. The snapshot holds only public DTO bytes.
  - An entry is keyed by page id and holds `(version, built_at, Snapshot)`, where `version` is the row's `updated_at`. It is served while `built_at + 30 s > now` **and** its version equals the fresh row's `updated_at`; otherwise it is rebuilt. A `tokio::sync::Mutex` per page id gives single flight: concurrent misses wait for one build.
  - Every write that changes what the DTO shows bumps `status_pages.updated_at = now()` in its own transaction: page PATCH (already does), section and component writes, report create/PATCH/delete, posted updates, and automatic reports (T8, inside the lifecycle transaction). So every instance sees a new version on its next request, and the in-process `invalidate(page_id)` after commit is only a fast path. Monitor and heartbeat state changes rely on the 30 s TTL (accepted).
  - At most 1,000 entries; inserting the 1,001st evicts the oldest.
  - The ETag is `"` + the first 16 hex chars of SHA-256 of the DTO JSON + `"`. It is a hash of bytes the requester is about to receive, so it leaks nothing, and `If-None-Match` is compared **only after access is granted** (a private page answers a visitor without access 404, never 304).
- **R11 Public DTO** (exact field set; nothing else is serialised):

```json
{
  "page": {"slug": "acme", "title": "Acme", "description": null, "logo_url": null, "logo_dark_url": null,
           "accent_color": "#14b8a6", "theme": "system", "nav_links": [{"label": "Docs", "url": "https://docs.acme.test"}],
           "timezone": "Europe/Berlin", "noindex": false, "visibility": "public", "url": "https://status.acme.test"},
  "status": "operational",
  "sections": [{"id": "…", "name": "Core", "components": [
      {"id": "…", "name": "API", "description": null, "status": "operational",
       "bars": [{"date": "2026-07-22", "status": "no_data", "uptime_pct": null, "downtime_minutes": 0, "reports": []}]}]}],
  "active_reports": [Report], "upcoming_maintenance": [Report], "history": [Report],
  "subscriptions": {"email": true, "webhook": false},
  "generated_at": "2026-10-19T10:00:00Z"
}
```

  `Report = {id, title, kind, status, impact (worst affected impact or null), starts_at, ends_at, published_at, updated_at, components: [{id, name, impact}], updates: [{id, status, message, created_at}]}`.
  - `page.url` is `https://{custom_domain}` when verified, else `{ui_base_url}/status/{slug}`.
  - `subscriptions.email` is always `true` in Part 1.
- **R12 Private access.**
  - Token: `v1.{page_id simple hex}.{exp unix}.{mac}`. `mac` is base64url (no padding) of HMAC-SHA256 under `Zeroizing::new(cipher.derive_subkey("nami/status-page-access/v1"))` over `"v1|{page_id}|{exp}|{access_epoch}"`. It is checked with `verify_slice` before trusting `exp`.
  - `access_epoch` is bumped whenever the password, the allowlist or the visibility changes, so every cookie issued before is revoked.
  - nami-ui stores the token in the cookie `nami_sp_access`: HttpOnly, `SameSite=Lax`, `Secure` when the request is https, `Max-Age` 30 days. Its `Path` is `/status/{slug}` on the app host and `/` on a custom domain. nami-ui sends it to the engine as `X-Nami-Page-Access`.
  - The engine grants access when the token verifies for **this** page and epoch and is unexpired, **or** when the client address is inside `ip_allowlist`. Page id, epoch, visibility and allowlist come from the row read fresh for this request (R10).
  - The client address is the `X-Forwarded-For` header **only when it holds exactly one valid IP** (nami-ui sets exactly one, from adapter-node's `getClientAddress()`, R4). Missing, malformed or a list → no address, so no IP access. An IPv4-mapped IPv6 address is unwrapped first. The TCP peer is never used for access (it is nami-ui). `X-Real-IP` and `Forwarded` are ignored. A visitor's own `X-Forwarded-For` never reaches the engine: `callPublicEngine` builds the outgoing headers from scratch.
  - Unlock (`POST …/unlock {password}`) answers 200 `{token, expires_at}`, or 401 `{"error":"wrong password"}` for a wrong password, a page with no password, a public page and an unknown slug alike. Unknown or passwordless pages run `verify_password` against a fixed dummy hash, so timing matches. argon2 parameters are `Argon2::default()` as for user passwords (argon2id, m = 19 MiB, t = 2, p = 1); page passwords are 8-128 chars.
- **R13 Rate limits.**
  - Reads (`GET /public/...`) go through a governor keyed by `SmartIpKeyExtractor` (the forwarded address): `per_millisecond(100)` (10/s), burst 30 → 429 JSON.
  - Unlock: at most 10 hits per `(page, client address)` per 15 min (`unlock:{page id or 'none'}:{client_key}`), at most 30 per client address per 15 min across all pages (`unlock-ip:{client_key}`, so one address cannot spread guesses over many slugs), plus the same governor.
  - Subscribe: at most 5 per client address per hour (`subscribe-ip:{hash}`) and 100 per page per hour (`subscribe-page:{page}`).
  - Token endpoints (`/public/status-subscriptions/*`): a governor of burst 20, `per_second(3)` (one token every 3 s).
  - The governors key on `SmartIpKeyExtractor`, which reads the first `X-Forwarded-For` entry nami-ui set (the same trusted address as the DB buckets; nami-ui never forwards a visitor's own header).
  - RFC 8058 one-click (public listener, R16): `PeerIpKeyExtractor`, burst 100, `per_millisecond(20)`. The peer there may be a reverse proxy shared by every mail provider, so the limit is generous; the token's MAC is checked before any database work, so a flood of forged tokens costs one HMAC each.
  - Hits are counted in `status_page_rate_hits` (`rate_limit::hit`), atomically under `pg_advisory_xact_lock(hashtext(bucket))`. Client addresses are stored only as `hex(HMAC(subkey "nami/status-rate/v1", ip))[..32]`. A request without a client address uses the bucket key `unknown`, so it is limited, never exempt. Rows older than 1 day are deleted by the hit itself (`delete … where bucket = $1 and hit_at < now() - window`), and the delivery worker prunes the rest hourly.
- **R14 Subscriber tokens** (controller ruling 1). Stateless, signed, single-purpose. Nothing usable is stored. Confirm tokens expire after 48 h; manage and unsubscribe tokens never expire and die with the subscriber row (deleting it, or unsubscribing, makes every link of that subscriber invalid; a re-subscription is a new row with new nonces).
  - Format: `base64url(subscriber_id 16 bytes || purpose 1 byte || nonce 16 bytes || mac 32 bytes)`, where `mac` = HMAC-SHA256 under `derive_subkey("nami/status-subscriber/v1")` over the first 33 bytes.
  - Purposes: `1` confirm, `2` manage, `3` unsubscribe.
  - The nonce is `confirm_nonce` (random, rotated on every resend, cleared on confirm, so a confirm link is single-use and only the newest works) for purpose 1, and `token_nonce` (random at insert) for 2 and 3.
  - Verify: decode, length check (exactly 65 bytes, 87 base64url chars), MAC check (`Mac::verify_slice`, constant time), then load the row and compare the nonce with a constant-time comparison (a local `fn ct_eq(a: &[u8], b: &[u8]) -> bool` that XOR-folds equal-length slices; `subtle` is not a direct dependency). Confirm also checks `confirm_expires_at` (48 h).
  - The spec says tokens are "hashed at rest". This stores no token or hash at all: a DB dump alone yields nothing usable. That is needed because the queue builds the email after the request and so must be able to re-derive the link.
- **R15 Enumeration resistance.**
  - `POST …/subscribe` answers `202 {"ok": true}` for every valid address: new, pending or already confirmed. The email itself is sent later by the queue, so timing does not differ either.
  - New → pending row plus a confirmation email.
  - Pending → components replaced, nonce rotated, confirmation re-sent.
  - Confirmed → an "already subscribed" email with the manage link.
  - Each address gets at most 3 such emails per page per 24 h (`confirmation_sends`, `confirmation_window_started_at`). Beyond that, nothing is sent, and the answer is still 202.
  - Caps (controller ruling 3): 10,000 confirmed and 500 pending per page. When either cap is reached, **every** subscribe to that page answers 429 `this page is not accepting new subscriptions right now`, whether the address is new, pending or confirmed. The cap is checked before the address is looked up, so it cannot be used to tell a known address from a new one.
  - Pending rows older than 48 h are deleted by the worker's hourly prune.
  - Addresses are trimmed and lowercased, ≤ 254 chars, parse as `lettre::Address`, have a domain with at least one `.` (lettre accepts `a@b`), and contain no control chars.
- **R16 Subscriber emails.** Plain text.
  - Confirmation subject: `Confirm your subscription to {page title}`. Already-subscribed subject: `You are already subscribed to {page title}`.
  - Update subject: `[{page title}] {report title}` (maintenance: `[{page title}] Maintenance: {report title}`). Body: the status label, the update message (markdown source as text), the report link `{page url}#report-{id}`, then `Manage your subscription: {page url}/subscribe/manage?token=…` and `Unsubscribe: {page url}/unsubscribe?token=…`.
  - Subjects and titles go through `is_unsafe_in_text` → space.
  - Headers: `List-Unsubscribe: <{public_engine_url}/api/v1/status-subscriptions/one-click/{unsubscribe token}>` and `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, via a new `Mailer::send_with_headers` (lettre 0.11.23 `HeaderName::new_from_ascii_str` + `HeaderValue::new` + `MessageBuilder::raw_header`; lettre itself RFC 2047-encodes any word holding CR or LF, and `send_with_headers` refuses such a value before that). lettre may fold a long header line, so tests unfold headers before comparing.
  - One-click is idempotent (RFC 8058, mail providers retry): a token with a valid MAC answers `200 unsubscribed` whether or not the subscriber still exists; only a token whose MAC fails is 404. The same holds for `POST /public/status-subscriptions/unsubscribe`. A MAC-valid token proves Nami issued it, so the 200 tells an attacker nothing.
  - `Config.public_engine_url` (env `PUBLIC_ENGINE_URL`, default `http://localhost:8081`, same validation as nami-ui's) is the base.
- **R17 Queue extension.** `NNNN_incidents_statuspages_part1.sql` makes `notification_deliveries.incident_id` nullable. It adds `status_page_id`, `subscriber_id`, `status_report_update_id` and `priority smallint` (0 = incidents, 1 = status pages), adds the events `sp_confirm`, `sp_subscribed`, `sp_update`, and replaces the due index with `(priority, next_attempt_at)`.
  - The claim orders candidates `by priority, next_attempt_at, id`. Status-page rows bucket as `'p:' || status_page_id` with the cap `STATUS_PAGE_PER_MINUTE = 600`.
  - The drain sends incident rows exactly as before. Rows with `status_page_id` branch off **before** the Incidents plan's per-row step (2a) (they have no incident to check) and go to `statuspages::deliver::send(state, row)`. Those never write `incident_events`, and they are cancelled when the subscriber is gone or unconfirmed (`sp_update`). The claimed-row struct's `incident_id` becomes `Option<Uuid>`.
  - Dedupe: `unique (status_page_id, round, target_key) where status_page_id is not null`, with `round` = the update id in simple hex (32 chars) or `confirm-{n}`, and `target_key = 'sub:{subscriber_id}'`.
  - `INCIDENTS_WORKER_ENABLED=false` stops status-page mail on that instance too (one worker).
- **R18 No uploads.** The spec makes logos `https` URLs (`logo_url`, `logo_dark_url`), so Part 1 has no upload endpoint, and the upload rules (magic bytes, size, `nosniff`, SVG) do not apply.
  - Logos render as `<img referrerpolicy="no-referrer">`. An SVG in an `<img>` cannot run script.
  - The public CSP allows `img-src 'self' https:`.
  - An upload store, if ever wanted, is a Platform item.
- **R19 No custom CSS or JS.** The spec lists custom CSS/JS injection as a non-goal. Branding is exactly `accent_color` (validated `#rrggbb`, rendered only as `style="--accent: #rrggbb"`), `theme`, logos and nav links.
- **R20 No i18n.** "Multi-language pages" is a spec non-goal. Dates render in the page's `timezone` with a fixed English format.
- **R21 Machine endpoints live in nami-ui.**
  - `feed.atom`, `badge.svg` and `status.json` are `+server.ts` routes built from the same DTO. Each sets `ETag` (the engine's) and answers `If-None-Match` with 304.
  - HTML pages send `Cache-Control: public, max-age=30` for public pages and `private, no-store` for private ones.
  - No public response reflects a query parameter or a request header other than the routed host. Public responses are identical with and without a session cookie and never set one (cache-poisoning tests in Task 13).
- **R22 RBAC table.** `GET` page/sections/components/reports: viewer. Report create/edit/update: member. Report delete: admin. Page create/edit/delete, sections, components, custom domain, verify: admin. Subscriber list/remove: admin.
- **R23 Bound sources.** Components FK their monitor/heartbeat with `(id, project_id)` and `on delete cascade`: deleting a monitor removes its components. A section delete cascades to its components (the UI's two-step delete says so).
  - "Import a group" (`POST …/sections/import-group {group_id}`, admin) creates one section named after the group and one component per monitor and heartbeat in it. Their display names are the source names, which the admin is choosing to publish; the editor says so, and they are editable.
  - Limits: 20 pages per project, 20 sections and 100 components per page.
- **R24 Markdown.** Update messages render on the public page and in the admin timeline with INC-12's `renderMarkdown`. That `{@html}` is the only one in the feature. The feed carries messages as `type="text"` (XML-escaped). Emails carry the source text.
- **R25 Private pages.** They are always `noindex` (meta and `X-Robots-Tag`), whatever the toggle. `status.json` → 404 for them. They are never `public`-cacheable. Their subscribe form needs access.

## File Structure

```
nami-engine/
  Cargo.toml                                   (no new crates; lettre raw_header already available)
  .env.example                                 T10 PUBLIC_ENGINE_URL
  migrations/                                  numbered at phase start, applied in this order (Migrations above)
    NNNN_uptime_statuspages_part1.sql          T1 heartbeats_id_project_key
    NNNN_statuspages_part1.sql                 T1 pages, sections, components; T5 reports; T7 rate hits; T9 subscribers
    NNNN_incidents_statuspages_part1.sql       T9 notification_deliveries extension
  src/
    lib.rs                                     T1 pub mod statuspages
    main.rs                                    T6 SnapshotCache in AppState
    core/config.rs                             T3 txt_lookup (TxtLookup); T10 public_engine_url
    core/state.rs                              T6 status_pages: Arc<statuspages::cache::SnapshotCache>
    core/email.rs                              T10 Mailer::send_with_headers
    core/router.rs                             T2/T5 admin routers; T6 public status router (+ governor); T9 token router; T10 one-click on public listener
    uptime/checker/dns.rs                      T3 build_resolver, readable become pub(crate)
    statuspages/
      mod.rs                                   T1, then one `pub mod` per new module
      validate.rs                              T1 pure validators
      domain.rs                                T3 verification record + TXT check
      status.rs                                T4 ComponentStatus, derive, override, worst
      history.rs                               T4 day spans, bars
      snapshot.rs                              T6 builds the DTO (Postgres only)
      cache.rs                                 T6 SnapshotCache
      access.rs                                T7 access tokens, client address, CIDR matching
      rate_limit.rs                            T7 DB-backed hits
      automatic.rs                             T8 on_incident_opened / on_incident_resolved
      tokens.rs                                T9 subscriber tokens
      deliver.rs                               T9 confirmation mail; T10 update mail
      notify.rs                                T10 fan-out of an update to subscribers
      page/{mod,model,routes}.rs               T2 pages, sections, components (admin); T3 custom domain, verify, domain check
      report/{mod,model,routes}.rs             T5 reports and updates
      public/{mod,routes}.rs                   T6 DTO; T7 unlock; T9 subscribe; json_error
      subscriber/{mod,model,routes}.rs         T9 rows, confirm; T10 inspect/manage/unsubscribe/one-click, admin subscriber list
    incidents/lifecycle.rs                     T8 hook calls (file planned in NAMI-INC-1)
    incidents/deliveries.rs, incidents/worker.rs  T9 claim order, bucket, dispatch, hourly prune (planned in NAMI-INC-5)
  tests/
    common/mod.rs                              T1 seed_status_page, seed_component, seed_heartbeat; T5 seed_report; T6 call_raw
    statuspages/main.rs                        T2 (new target: mounts ../common, one `mod` per file below)
    statuspages/status_pages_api.rs            T2, T3
    statuspages/status_history.rs              T4
    statuspages/status_reports_api.rs          T5
    statuspages/status_public.rs               T6, T7
    statuspages/status_automatic.rs            T8
    statuspages/status_subscribers.rs          T9, T10
    core/migrations.rs, core/json_bodies.rs    every task that adds tables/routes

nami-ui/
  package.json                                 T13 @sveltejs/adapter-node@6.0.0 (replaces adapter-auto)
  vite.config.ts                               T13 adapter-node
  .env.example                                 T13 APP_ORIGIN, ADDRESS_HEADER, XFF_DEPTH, HOST_HEADER, PROTOCOL_HEADER
  README.md                                    T13 custom domains + TLS (Caddy on-demand ask)
  src/
    hooks.ts                                   T13 reroute
    hooks.server.ts                            T13 security headers for status routes
    lib/statuspages/status-pages.ts (+ .test.ts)            T11 types, labels, colours; T15 xml escape, badge
    lib/statuspages/status-routing.ts (+ .test.ts)          T13 reroute rule
    lib/server/core/engine.ts (+ engine.test.ts)            T13 callPublicEngine
    lib/server/core/fake-event.ts                           T13 headers and cookies options
    lib/server/statuspages/status-page-form.ts (+ .test)    T11 editor form → engine body
    lib/server/statuspages/status-report-form.ts (+ .test)  T12 composer and update forms
    lib/server/statuspages/status-public.ts (+ .test)       T13 page ref, access cookie, DTO fetch
    lib/components/UptimeBars.svelte                        T13
    routes/(app)/projects/[id=uuid]/
      +layout.svelte                           T11 "Status pages" tab
      status-pages/+page.*                     T11 list + create
      status-pages/[pageId=uuid]/+page.*       T11 editor
      status-pages/[pageId=uuid]/reports/+page.*                 T12 list + composer
      status-pages/[pageId=uuid]/reports/[reportId=uuid]/+page.* T12 timeline + update
      status-pages/[pageId=uuid]/subscribers/+page.*             T12
    routes/status/[slug]/
      +page.server.ts, +page.svelte            T13 public page, unlock action; T14 subscribe action
      subscribe/confirm/+page.*                T14
      subscribe/manage/+page.*                 T14
      unsubscribe/+page.*                      T14
      feed.atom/+server.ts                     T15
      badge.svg/+server.ts                     T15
      status.json/+server.ts                   T15
    routes/_nami/tls-ask/+server.ts            T13
```

---
### Task 1 (NAMI-SP-1): Status page schema and pure validators

**Files:**
- Create: `nami-engine/migrations/NNNN_uptime_statuspages_part1.sql`, `nami-engine/migrations/NNNN_statuspages_part1.sql` (numbered in that order, Migrations above), `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/statuspages/validate.rs`
- Modify: `nami-engine/src/lib.rs` (`pub mod statuspages;`), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/common/mod.rs`
- Test: unit tests in `validate.rs`; `tests/core/migrations.rs`

**Interfaces:**
- Consumes: `core::validate::{validate_label, is_invisible_or_bidi_char}`, `chrono_tz::Tz`, `reqwest::Url`.
- Produces (`nami_engine::statuspages::validate`):

```rust
pub const MAX_PAGES_PER_PROJECT: i64 = 20;
pub const MAX_SECTIONS_PER_PAGE: i64 = 20;
pub const MAX_COMPONENTS_PER_PAGE: i64 = 100;
pub const MAX_NAV_LINKS: usize = 4;
pub const MAX_NAV_LABEL_CHARS: usize = 40;
pub const MAX_ALLOWLIST: usize = 50;
pub const DEFAULT_ACCENT: &str = "#14b8a6";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavLink { pub label: String, pub url: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr { pub addr: IpAddr, pub prefix: u8 }
impl Cidr {
    /// "10.0.0.0/8", "2001:db8::/32", a bare address (= /32 or /128). Host bits set → Err.
    pub fn parse(raw: &str) -> Result<Cidr, String>;
    /// IPv4-mapped IPv6 `ip` is unwrapped first; families never match across.
    pub fn contains(&self, ip: IpAddr) -> bool;
}
impl fmt::Display for Cidr { /* "addr/prefix", Postgres cidr text form */ }

pub fn slug(raw: &str) -> Result<String, String>;
pub fn title(raw: &str) -> Result<String, String>;                 // validate_label("title", raw)
pub fn description(raw: &str) -> Result<String, String>;           // ≤ 1000 chars, no invisible/bidi chars
pub fn accent_color(raw: &str) -> Result<String, String>;          // lowercased
pub fn https_url(field: &str, raw: &str) -> Result<String, String>;
pub fn nav_links(links: Vec<NavLink>) -> Result<Vec<NavLink>, String>;
pub fn theme(raw: &str) -> Result<String, String>;
pub fn visibility(raw: &str) -> Result<String, String>;
pub fn timezone(raw: &str) -> Result<String, String>;
pub fn ip_allowlist(raw: &[String]) -> Result<Vec<Cidr>, String>;
pub fn custom_domain(raw: &str, app_host: &str) -> Result<String, String>;
pub fn password(raw: &str) -> Result<(), String>;                  // 8-128 chars
pub fn display_name(raw: &str) -> Result<String, String>;          // validate_label("display_name", raw)
pub fn section_name(raw: &str) -> Result<String, String>;          // validate_label("name", raw)
```

- [ ] **Step 1: Write the migrations**

`nami-engine/migrations/NNNN_uptime_statuspages_part1.sql` (an uptime table, so an uptime file, applied before the statuspages file):

```sql
-- Uptime changes of Status Pages Part 1 (NAMI-SP-1).
-- Components reference their monitor/heartbeat together with the project,
-- so a source of another project can never be bound. Monitors already have
-- `monitors_id_project_key` (0003_uptime.sql); heartbeats get the same key.
alter table heartbeats add constraint heartbeats_id_project_key unique (id, project_id);
```

`nami-engine/migrations/NNNN_statuspages_part1.sql` (created here; Tasks 5, 7 and 9 append to it):

```sql
-- Status pages Part 1 (NAMI-SP-1, NAMI-SP-5, NAMI-SP-7, NAMI-SP-9).

-- Pages, sections and components (NAMI-SP-1).
create table status_pages (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    slug text not null unique check (slug ~ '^[a-z0-9-]{3,63}$'),
    title text not null check (char_length(title) between 1 and 200),
    description text check (char_length(description) <= 1000),
    logo_url text check (logo_url ~ '^https://' and char_length(logo_url) <= 2048),
    logo_dark_url text check (logo_dark_url ~ '^https://' and char_length(logo_dark_url) <= 2048),
    accent_color text not null default '#14b8a6' check (accent_color ~ '^#[0-9a-f]{6}$'),
    theme text not null default 'system' check (theme in ('light', 'dark', 'system')),
    nav_links jsonb not null default '[]'
        check (jsonb_typeof(nav_links) = 'array' and jsonb_array_length(nav_links) <= 4),
    visibility text not null default 'public' check (visibility in ('public', 'private')),
    password_hash text,
    ip_allowlist cidr[] not null default '{}' check (cardinality(ip_allowlist) <= 50),
    -- Bumped on every password/allowlist/visibility change: revokes access cookies (R12).
    access_epoch int not null default 0,
    custom_domain text unique check (custom_domain ~ '^[a-z0-9.-]{4,253}$'),
    custom_domain_verified_at timestamptz,
    timezone text not null default 'UTC' check (char_length(timezone) between 1 and 64),
    noindex boolean not null default false,
    automatic_reports boolean not null default false,
    automatic_reports_notify boolean not null default true,
    allow_webhook_subscribers boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (id, project_id),
    -- Password and allowlist exist only on private pages, and a private page has at least one.
    check (visibility = 'private' or (password_hash is null and cardinality(ip_allowlist) = 0)),
    check (visibility = 'public' or password_hash is not null or cardinality(ip_allowlist) > 0),
    check (custom_domain is not null or custom_domain_verified_at is null)
);
create index status_pages_project_idx on status_pages (project_id);

create table status_page_sections (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null references status_pages(id) on delete cascade,
    name text not null check (char_length(name) between 1 and 200),
    position int not null default 0 check (position between 0 and 1000000),
    created_at timestamptz not null default now(),
    unique (id, status_page_id)
);
create index status_page_sections_page_idx on status_page_sections (status_page_id);

create table status_page_components (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null,
    project_id uuid not null,
    section_id uuid not null,
    display_name text not null check (char_length(display_name) between 1 and 200),
    description text check (char_length(description) <= 500),
    monitor_id uuid,
    heartbeat_id uuid,
    position int not null default 0 check (position between 0 and 1000000),
    show_uptime_bars boolean not null default true,
    created_at timestamptz not null default now(),
    foreign key (status_page_id, project_id) references status_pages (id, project_id) on delete cascade,
    foreign key (section_id, status_page_id) references status_page_sections (id, status_page_id) on delete cascade,
    -- MATCH SIMPLE: the null one of the two is not checked.
    foreign key (monitor_id, project_id) references monitors (id, project_id) on delete cascade,
    foreign key (heartbeat_id, project_id) references heartbeats (id, project_id) on delete cascade,
    check (num_nonnulls(monitor_id, heartbeat_id) = 1)
);
create index status_page_components_page_idx on status_page_components (status_page_id);
-- Automatic reports look components up by their source.
create index status_page_components_monitor_idx on status_page_components (monitor_id) where monitor_id is not null;
create index status_page_components_heartbeat_idx on status_page_components (heartbeat_id) where heartbeat_id is not null;
```

Add `"status_pages"`, `"status_page_sections"`, `"status_page_components"` to `all_tables_exist`. Add `migrations.rs` tests:

```rust
#[sqlx::test]
async fn a_component_binds_exactly_one_source_of_its_own_project(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let other = common::seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let section: Uuid = sqlx::query_scalar("insert into status_page_sections (status_page_id, name) values ($1, 'Core') returning id")
        .bind(page).fetch_one(&pool).await.unwrap();
    let mine = common::seed_http_monitor(&pool, project, "api", "https://a.test/", 1).await;
    let foreign = common::seed_http_monitor(&pool, other, "api", "https://b.test/", 1).await;
    let hb = common::seed_heartbeat(&pool, project, "cron").await;
    let insert = |monitor: Option<Uuid>, heartbeat: Option<Uuid>| {
        let pool = pool.clone();
        async move {
            sqlx::query("insert into status_page_components (status_page_id, project_id, section_id, display_name, monitor_id, heartbeat_id)
                         values ($1, $2, $3, 'API', $4, $5)")
                .bind(page).bind(project).bind(section).bind(monitor).bind(heartbeat).execute(&pool).await
        }
    };
    assert!(insert(Some(mine), None).await.is_ok());
    assert!(insert(None, Some(hb)).await.is_ok());
    assert!(insert(Some(mine), Some(hb)).await.is_err(), "both");
    assert!(insert(None, None).await.is_err(), "neither");
    let err = insert(Some(foreign), None).await.unwrap_err();
    assert_eq!(err.as_database_error().unwrap().code().as_deref(), Some("23503"), "another project's monitor");
}

#[sqlx::test]
async fn private_pages_need_a_password_or_an_allowlist_and_public_pages_have_neither(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let set = |sql: &'static str| { let pool = pool.clone(); async move { sqlx::query(sql).bind(page).execute(&pool).await } };
    assert!(set("update status_pages set visibility = 'private' where id = $1").await.is_err());
    assert!(set("update status_pages set password_hash = 'x' where id = $1").await.is_err(), "public with a password");
    assert!(set("update status_pages set visibility = 'private', ip_allowlist = '{10.0.0.0/8}' where id = $1").await.is_ok());
    assert!(set("update status_pages set ip_allowlist = '{10.0.0.1/8}' where id = $1").await.is_err(), "host bits set");
    assert!(set("update status_pages set custom_domain_verified_at = now() where id = $1").await.is_err(), "verified without a domain");
}
```

`tests/common/mod.rs` gains:

```rust
/// A public page with the default settings, inserted directly.
pub async fn seed_status_page(pool: &sqlx::PgPool, project: Uuid, slug: &str) -> Uuid
/// A heartbeat inserted directly (period 300 s), for status page tests.
pub async fn seed_heartbeat(pool: &sqlx::PgPool, project: Uuid, name: &str) -> Uuid
/// A section named "Core" (created on first use) plus one component bound to `monitor` or `heartbeat`.
pub async fn seed_component(pool: &sqlx::PgPool, page: Uuid, display_name: &str,
    monitor: Option<Uuid>, heartbeat: Option<Uuid>) -> Uuid
```

(`main` has no `seed_heartbeat` in `tests/common`. `tests/core/json_bodies.rs`, `tests/uptime/heartbeats.rs` and `tests/uptime/uptime_worker_timing.rs` each define a file-local `seed_heartbeat` with another signature; they import `common` items by name, never by glob, so the new `common::seed_heartbeat` does not clash. Leave those local helpers alone.)

- [ ] **Step 2: Write the failing validator tests** (`validate.rs` `mod tests`)

```rust
#[test]
fn slugs_follow_the_spec() {
    assert_eq!(slug(" acme-status "), Ok("acme-status".into()));
    for bad in ["ab", "Acme", "acme_status", "a".repeat(64).as_str(), "_domain", "acme/../x", ""] {
        assert!(slug(bad).is_err(), "{bad}");
    }
}

#[test]
fn nav_links_allow_https_and_mailto_only() {
    let link = |l: &str, u: &str| NavLink { label: l.into(), url: u.into() };
    assert_eq!(nav_links(vec![link("Docs", "https://docs.acme.test/x"), link("Support", "mailto:help@acme.test")]).unwrap().len(), 2);
    for bad in ["javascript:alert(1)", "JaVaScRiPt:alert(1)", " javascript:alert(1)", "data:text/html,<b>",
                "http://docs.acme.test", "//docs.acme.test", "https://user:pw@acme.test", "mailto:", "mailto:a@b.test%0d%0aBcc:x@y.test",
                "vbscript:x", "https://"] {
        assert_eq!(nav_links(vec![link("x", bad)]), Err("navigation links must be https: or mailto: URLs".into()), "{bad}");
    }
    assert!(nav_links(vec![link(&"x".repeat(41), "https://a.test")]).is_err(), "label > 40 chars");
    assert!(nav_links(vec![link("", "https://a.test")]).is_err(), "empty label");
    assert!(nav_links(vec![link("a\u{202e}b", "https://a.test")]).is_err(), "bidi in label");
    assert!(nav_links((0..5).map(|_| link("x", "https://a.test")).collect()).is_err(), "more than 4");
}

#[test]
fn logos_are_https_only() {
    assert!(https_url("logo_url", "https://cdn.acme.test/logo.svg").is_ok());
    for bad in ["http://cdn.acme.test/l.png", "data:image/png;base64,AAAA", "javascript:x", "https://u:p@cdn.acme.test/l.png",
                format!("https://a.test/{}", "x".repeat(2048)).as_str()] {
        assert_eq!(https_url("logo_url", bad), Err("logo_url must be an https:// URL of at most 2048 characters".into()), "{bad}");
    }
}

#[test]
fn colors_themes_visibility_and_zones() {
    assert_eq!(accent_color("#14B8A6"), Ok("#14b8a6".into()));
    for bad in ["14b8a6", "#14b8a", "#14b8a6;background:url(x)", "red", "#14b8a6 "] {
        assert_eq!(accent_color(bad), Err("accent_color must be #rrggbb".into()), "{bad}");
    }
    assert!(theme("system").is_ok() && theme("sepia").is_err());
    assert!(visibility("private").is_ok() && visibility("hidden").is_err());
    assert_eq!(timezone("Europe/Berlin"), Ok("Europe/Berlin".into()));
    assert!(timezone("Mars/Base").is_err());
}

#[test]
fn cidrs_parse_canonically_and_match_both_families() {
    let c = Cidr::parse("10.0.0.0/8").unwrap();
    assert!(c.contains("10.255.0.1".parse().unwrap()));
    assert!(c.contains("::ffff:10.1.2.3".parse().unwrap()), "IPv4-mapped is unwrapped");
    assert!(!c.contains("11.0.0.1".parse().unwrap()));
    assert!(!c.contains("::a00:1".parse().unwrap()), "an IPv6 address never matches an IPv4 range");
    assert_eq!(Cidr::parse("203.0.113.7").unwrap().to_string(), "203.0.113.7/32");
    assert_eq!(Cidr::parse("2001:db8::/32").unwrap().to_string(), "2001:db8::/32");
    assert!(Cidr::parse("2001:db8::1").unwrap().contains("2001:db8::1".parse().unwrap()));
    assert!(Cidr::parse("0.0.0.0/0").unwrap().contains("8.8.8.8".parse().unwrap()));
    for bad in ["10.0.0.1/8", "10.0.0.0/33", "2001:db8::/129", "10.0.0.0/-1", "example.com", "", "10.0.0.0/8/8"] {
        assert!(Cidr::parse(bad).is_err(), "{bad}");
    }
    assert!(ip_allowlist(&vec!["10.0.0.0/8".to_string(); 51]).is_err(), "more than 50");
}

#[test]
fn custom_domains_are_hostnames_other_than_the_app() {
    assert_eq!(custom_domain("Status.Acme.Test.", "nami.acme.test"), Ok("status.acme.test".into()));
    for bad in ["nami.acme.test", "NAMI.acme.test", "localhost", "acme", "203.0.113.7", "[::1]", "status.acme.test:8443",
                "-bad.acme.test", "a..b.test", "https://status.acme.test", "status.acme.test/x", format!("{}.test", "a".repeat(64)).as_str()] {
        assert!(custom_domain(bad, "nami.acme.test").is_err(), "{bad}");
    }
}
```

Run: engine command `--lib -- statuspages::validate`, then engine command `--test core -- migrations::`. Expected: FAIL (module missing).

- [ ] **Step 3: Implement** `validate.rs`.
  - `nav_links`: trim the url. Accept `https:` only when `reqwest::Url::parse` gives scheme `https`, a host, no userinfo and ≤ 2048 chars. Accept `mailto:` only when the rest (percent-decoded) is one address that parses as `lettre::Address` and has no control characters. Anything else → the fixed message. Labels: trimmed, 1-40 chars, no control or `is_invisible_or_bidi_char` characters.
  - `custom_domain`: lowercase, strip one trailing `.`, refuse anything `IpAddr::from_str` accepts or that contains `:`, `/`, `[` or `@`. At least two labels, each `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`, total ≤ 253. Refuse `localhost`/`*.localhost` and equality with `app_host` (compared lowercase, without port).
  - `Cidr::parse`: split at `/`, parse the address and prefix (≤ 32 / ≤ 128), and refuse host bits set: `"{raw} has host bits set; use {network}/{prefix}"`.

- [ ] **Step 4: Run** both commands again. Expected: PASS. Then the full engine suite and `cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/NNNN_uptime_statuspages_part1.sql nami-engine/migrations/NNNN_statuspages_part1.sql nami-engine/src/statuspages nami-engine/src/lib.rs nami-engine/tests
git commit -m "feat(engine): status page schema and validators (NAMI-SP-1)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2 (NAMI-SP-2): Admin API — pages, sections, components, group import

**Files:**
- Create: `nami-engine/src/statuspages/page/{mod,model,routes}.rs`, `nami-engine/tests/statuspages/main.rs` (the new domain target: `#[path = "../common/mod.rs"] mod common;` and one `mod <file>;` per test file, each later task adding its own), `nami-engine/tests/statuspages/status_pages_api.rs`
- Modify: `nami-engine/src/statuspages/mod.rs` (`pub mod page;`), `nami-engine/src/core/router.rs` (`build_router` merges `router()` of `statuspages::page`), `nami-engine/tests/core/json_bodies.rs`

**Interfaces:**
- Consumes: Task 1 validators; `auth::password::hash_password`; `core::middleware::rbac::require_project_role`; `core::validate::nullable`.
- Produces:

```rust
// statuspages::page::model
#[derive(Clone, sqlx::FromRow)]   // no derived Debug: it would print password_hash. `impl fmt::Debug` by hand, omitting it.
pub struct StatusPage { pub id: Uuid, pub project_id: Uuid, pub slug: String, pub title: String,
    pub description: Option<String>, pub logo_url: Option<String>, pub logo_dark_url: Option<String>,
    pub accent_color: String, pub theme: String, pub nav_links: sqlx::types::Json<Vec<NavLink>>,
    pub visibility: String, pub password_hash: Option<String>, pub ip_allowlist: Vec<String>, // selected as ip_allowlist::text[]
    pub access_epoch: i32, pub custom_domain: Option<String>, pub custom_domain_verified_at: Option<DateTime<Utc>>,
    pub timezone: String, pub noindex: bool, pub automatic_reports: bool, pub automatic_reports_notify: bool,
    pub allow_webhook_subscribers: bool, pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize)]
pub struct PageView { /* every StatusPage field except password_hash, plus */ pub has_password: bool,
    pub custom_domain_verified: bool }
impl From<&StatusPage> for PageView
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Section { pub id: Uuid, pub status_page_id: Uuid, pub name: String, pub position: i32 }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Component { pub id: Uuid, pub status_page_id: Uuid, pub section_id: Uuid, pub display_name: String,
    pub description: Option<String>, pub monitor_id: Option<Uuid>, pub heartbeat_id: Option<Uuid>,
    pub position: i32, pub show_uptime_bars: bool, pub created_at: DateTime<Utc>,
    /// Admin view only: the bound source's own name.
    pub source_name: String }
/// The select list with `ip_allowlist::text[] as ip_allowlist` (sqlx has no `cidr` decoding without the
/// `ipnet` feature). A `macro_rules! page_columns` expanding to a string literal, used as
/// `concat!("select ", page_columns!(), " from status_pages where …")`: a `const &str` cannot go into
/// `concat!`, and SQL strings are never `format!`ed (same pattern as `maintenance::maybe_in_effect!`).
macro_rules! page_columns { () => { "id, project_id, slug, …, ip_allowlist::text[] as ip_allowlist, …" } }
pub async fn find_in_project(pool: &PgPool, project_id: Uuid, page_id: Uuid) -> Result<Option<StatusPage>, sqlx::Error>;
pub async fn find_by_slug(pool: &PgPool, slug: &str) -> Result<Option<StatusPage>, sqlx::Error>;
pub async fn find_by_verified_domain(pool: &PgPool, host: &str) -> Result<Option<StatusPage>, sqlx::Error>;
pub async fn sections(pool: &PgPool, page_id: Uuid) -> Result<Vec<Section>, sqlx::Error>;     // by position, name
pub async fn components(pool: &PgPool, page_id: Uuid) -> Result<Vec<Component>, sqlx::Error>; // by position, display_name
```

Routes (all under `/projects/{id}/status-pages`; R22 roles):

| Method + path | Role | Body | Answer |
|---|---|---|---|
| `GET ""` | viewer | | `200 [PageView]` by title |
| `POST ""` | admin | `{slug, title, description?, timezone?}` | `201 PageView`; slug taken → `409 slug is taken`; 21st page → `409 a project can have at most 20 status pages` |
| `GET /{page_id}` | viewer | | `200 {page: PageView, sections: [Section], components: [Component]}` |
| `PATCH /{page_id}` | admin | `PatchPageRequest` | `200 PageView` |
| `DELETE /{page_id}` | admin | | `204` |
| `POST /{page_id}/sections` | admin | `{name, position?}` | `201 Section`; 21st → 409 |
| `PATCH`/`DELETE /{page_id}/sections/{section_id}` | admin | `{name?, position?}` | `200 Section` / `204` (cascades to its components) |
| `POST /{page_id}/sections/import-group` | admin | `{group_id}` | `201 {section, components}`; group of another project → `400 group not found in this project`; would exceed 100 components → 409 |
| `POST /{page_id}/components` | admin | `{section_id, display_name, description?, monitor_id?, heartbeat_id?, position?, show_uptime_bars?}` | `201 Component` |
| `PATCH`/`DELETE /{page_id}/components/{component_id}` | admin | `{section_id?, display_name?, description?, position?, show_uptime_bars?}` | `200` / `204` |

`PatchPageRequest` (every field optional; `#[serde(default, deserialize_with = "nullable")]` where null clears): `slug`, `title`, `description`?, `logo_url`?, `logo_dark_url`?, `accent_color`, `theme`, `nav_links`, `visibility`, `password`? (string sets, null clears; never echoed), `ip_allowlist` (`Vec<String>`), `custom_domain`? (Task 3 handles it; Task 2 rejects the field with `400 custom domains arrive in NAMI-SP-3` so the PR is self-contained), `timezone`, `noindex`, `automatic_reports`, `automatic_reports_notify`, `allow_webhook_subscribers`. `#[serde(deny_unknown_fields)]`.

Patch semantics, in one transaction with `select … for update` on the page:
1. Validate every given field (Task 1); `password` through `validate::password` then `hash_password`.
2. Merge onto the row. If the merged `visibility` is `public`: `password_hash = null`, `ip_allowlist = '{}'`. If `private` with neither → `400 a private page needs a password, an IP allowlist, or both`.
3. `access_epoch = access_epoch + 1` when the password was given (set or cleared), the allowlist changed (compared as a set with the stored one, not "was present in the body": the editor sends it on every save), or visibility changed.
4. `updated_at = now()`. Unique violation on slug → `409 slug is taken`.

Section and component writes also run `update status_pages set updated_at = now() where id = $1` in their transaction (R10).

A section or component id that belongs to another page of the same project, or a page id of another project inside an accessible project path → 404. A `section_id`/`monitor_id`/`heartbeat_id`/`group_id` in a body that is not in this page/project → 400 `<field> not found in this project`. `monitor_id` and `heartbeat_id` both or neither → `400 set exactly one of monitor_id and heartbeat_id`. Rebinding a component's source is not offered (delete and re-add).

- [ ] **Step 1: Write the failing tests** — `nami-engine/tests/statuspages/status_pages_api.rs`:

```rust
// Status page admin API. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

async fn create_page(app: &axum::Router, project: Uuid, admin: &axum::http::HeaderValue, slug: &str) -> Value {
    let (status, body) = call(app, "POST", &format!("/projects/{project}/status-pages"), Some(admin),
        Some(json!({ "slug": slug, "title": "Acme" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[sqlx::test]
async fn roles_gate_every_route(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/status-pages");

    let (status, _) = call(&app, "POST", &base, Some(&member), Some(json!({ "slug": "acme", "title": "Acme" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let page = create_page(&app, project, &admin, "acme").await;
    let page_uri = format!("{base}/{}", page["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &page_uri, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &page_uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PATCH", &page_uri, Some(&member), Some(json!({ "title": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &page_uri, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &page_uri, Some(&admin), None).await.0, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn slugs_are_validated_and_globally_unique(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (p1, p2) = (seed_project(&pool).await, seed_project(&pool).await);
    let (_, _, a1) = user_with_role(&app, &pool, p1, Some("admin")).await;
    let (_, _, a2) = user_with_role(&app, &pool, p2, Some("admin")).await;
    create_page(&app, p1, &a1, "acme").await;
    let (status, body) = call(&app, "POST", &format!("/projects/{p2}/status-pages"), Some(&a2),
        Some(json!({ "slug": "acme", "title": "Other" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("slug is taken")));
    let (status, _) = call(&app, "POST", &format!("/projects/{p2}/status-pages"), Some(&a2),
        Some(json!({ "slug": "Bad_Slug", "title": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn navigation_links_refuse_script_data_and_plain_http(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let page = create_page(&app, project, &admin, "acme").await;
    let uri = format!("/projects/{project}/status-pages/{}", page["id"].as_str().unwrap());
    for bad in ["javascript:alert(1)", "data:text/html,x", "http://docs.acme.test"] {
        let (status, body) = call(&app, "PATCH", &uri, Some(&admin),
            Some(json!({ "nav_links": [{ "label": "Docs", "url": bad }] }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, body) = call(&app, "PATCH", &uri, Some(&admin),
        Some(json!({ "nav_links": [{ "label": "Docs", "url": "https://docs.acme.test" }], "accent_color": "#FF0000", "theme": "dark" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["accent_color"].as_str(), body["theme"].as_str()), (Some("#ff0000"), Some("dark")));
}

#[sqlx::test]
async fn private_pages_keep_the_password_write_only_and_bump_the_epoch(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let page = create_page(&app, project, &admin, "acme").await;
    let uri = format!("/projects/{project}/status-pages/{}", page["id"].as_str().unwrap());

    let (status, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "visibility": "private" }))).await;
    assert_eq!((status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("a private page needs a password, an IP allowlist, or both")));
    let (status, body) = call(&app, "PATCH", &uri, Some(&admin),
        Some(json!({ "visibility": "private", "password": "correct horse" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["has_password"].as_bool(), body["access_epoch"].as_i64()), (Some(true), Some(1)));
    let text = body.to_string();
    assert!(!text.contains("argon2") && !text.contains("password_hash") && !text.contains("correct horse"), "{text}");
    let (_, detail) = call(&app, "GET", &uri, Some(&admin), None).await;
    assert!(!detail.to_string().contains("argon2"));

    let (_, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "title": "Renamed" }))).await;
    assert_eq!(body["access_epoch"].as_i64(), Some(1), "an unrelated change keeps cookies valid");
    let (_, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "ip_allowlist": ["10.0.0.0/8"] }))).await;
    assert_eq!(body["access_epoch"].as_i64(), Some(2));
    let (status, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "ip_allowlist": ["10.0.0.1/8"] }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "visibility": "public" }))).await;
    assert_eq!((body["has_password"].as_bool(), body["ip_allowlist"].clone(), body["access_epoch"].as_i64()),
        (Some(false), json!([]), Some(3)));
}

#[sqlx::test]
async fn components_bind_one_source_of_the_same_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (project, other) = (seed_project(&pool).await, seed_project(&pool).await);
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let page = create_page(&app, project, &admin, "acme").await;
    let base = format!("/projects/{project}/status-pages/{}", page["id"].as_str().unwrap());
    let (_, section) = call(&app, "POST", &format!("{base}/sections"), Some(&admin), Some(json!({ "name": "Core" }))).await;
    let mine = seed_http_monitor(&pool, project, "db-primary", "https://internal.test/", 1).await;
    let foreign = seed_http_monitor(&pool, other, "x", "https://x.test/", 1).await;
    let hb = common::seed_heartbeat(&pool, project, "backup").await;
    let add = |body: Value| { let (app, base, admin) = (app.clone(), base.clone(), admin.clone());
        async move { call(&app, "POST", &format!("{base}/components"), Some(&admin), Some(body)).await } };

    let (status, body) = add(json!({ "section_id": section["id"], "display_name": "API", "monitor_id": mine })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["source_name"], "db-primary", "admins see the source name");
    let (status, body) = add(json!({ "section_id": section["id"], "display_name": "X", "monitor_id": mine, "heartbeat_id": hb })).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("set exactly one of monitor_id and heartbeat_id")));
    let (status, body) = add(json!({ "section_id": section["id"], "display_name": "X", "monitor_id": foreign })).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("monitor_id not found in this project")));
    let (status, _) = add(json!({ "section_id": Uuid::new_v4(), "display_name": "X", "heartbeat_id": hb })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Another project's page id inside this project's path.
    let foreign_page = common::seed_status_page(&pool, other, "other").await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}/status-pages/{foreign_page}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Deleting the monitor removes its component (R23).
    sqlx::query("delete from monitors where id = $1").bind(mine).execute(&pool).await.unwrap();
    let (_, detail) = call(&app, "GET", &base, Some(&admin), None).await;
    assert_eq!(detail["components"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn importing_a_group_adds_its_monitors_and_heartbeats(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let page = create_page(&app, project, &admin, "acme").await;
    let group: Uuid = sqlx::query_scalar("insert into monitor_groups (project_id, name) values ($1, 'Payments') returning id")
        .bind(project).fetch_one(&pool).await.unwrap();
    let m = seed_http_monitor(&pool, project, "pay-api", "https://pay.test/", 1).await;
    let h = common::seed_heartbeat(&pool, project, "pay-cron").await;
    seed_http_monitor(&pool, project, "ungrouped", "https://u.test/", 1).await;
    sqlx::query("update monitors set group_id = $1 where id = $2").bind(group).bind(m).execute(&pool).await.unwrap();
    sqlx::query("update heartbeats set group_id = $1 where id = $2").bind(group).bind(h).execute(&pool).await.unwrap();
    let (status, body) = call(&app, "POST", &format!("/projects/{project}/status-pages/{}/sections/import-group", page["id"].as_str().unwrap()),
        Some(&admin), Some(json!({ "group_id": group }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["section"]["name"], "Payments");
    let mut names: Vec<&str> = body["components"].as_array().unwrap().iter().map(|c| c["display_name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["pay-api", "pay-cron"]);
}

#[sqlx::test]
async fn limits_are_enforced(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    for i in 0..20 { common::seed_status_page(&pool, project, &format!("page-{i:02}")).await; }
    let (status, body) = call(&app, "POST", &format!("/projects/{project}/status-pages"), Some(&admin),
        Some(json!({ "slug": "one-more", "title": "x" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("a project can have at most 20 status pages")));
}
```

Add every new POST/PATCH route to `json_bodies.rs`, seeding one page, section and component the way the existing entries seed monitors.

Run: engine command `--test statuspages --test core -- status_pages_api:: json_bodies::`. Expected: FAIL.

- [ ] **Step 2: Implement** the model and routes. Counts are checked inside the insert's transaction (`select count(*) … for update` on the project's pages is not possible; use `pg_advisory_xact_lock(hashtext('status_pages:' || project_id))` before counting, and the same with the page id for sections/components). `import-group` inserts in one transaction: section `name = group.name` and position after the last, then components for `select id, name from monitors where group_id = $1 and project_id = $2` and the same for heartbeats, ordered by name.

- [ ] **Step 3: Run** the command again, then the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests
git commit -m "feat(engine): status page admin API with sections, components and group import (NAMI-SP-2)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3 (NAMI-SP-3): Custom domains with DNS TXT verification

**Files:**
- Create: `nami-engine/src/statuspages/domain.rs`
- Modify: `nami-engine/src/core/config.rs` (`txt_lookup`), `nami-engine/src/uptime/checker/dns.rs` (`build_resolver`, `readable` become `pub(crate)`), `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/statuspages/page/routes.rs` (accept `custom_domain` in PATCH; verify route; `custom_domain_verification` in `GET /{page_id}`), `nami-engine/src/statuspages/page/model.rs`, `nami-engine/tests/statuspages/status_pages_api.rs`, `nami-engine/tests/core/json_bodies.rs`

**Interfaces:**
- Consumes: Task 1 `validate::custom_domain`, Task 2 routes, `core::crypto::Cipher::derive_subkey`, `hmac 0.13` + `sha2`, `hickory_resolver`.
- Produces:

```rust
// statuspages::domain
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxtLookup { System, Fixed(std::collections::BTreeMap<String, Vec<String>>) }   // Config.txt_lookup (R5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationRecord { pub name: String, pub value: String }   // "_nami-verify.<domain>", "nami-verify=<32 hex>"
pub fn verification_record(cipher: &Cipher, page_id: Uuid, domain: &str) -> VerificationRecord;
/// TXT strings of `name` (each record's character-strings concatenated). Timeout 5 s, no search domains.
pub async fn lookup_txt(lookup: &TxtLookup, name: &str) -> Result<Vec<String>, String>;
/// The host part of `Config.ui_base_url`, lowercase, no port: what `validate::custom_domain` refuses.
pub fn app_host(config: &Config) -> String;
```

Routes:
- `PATCH …/{page_id}` accepts `custom_domain` (string → `validate::custom_domain(raw, &app_host(&config))`, null clears). Only an actual change (normalised value differs from the stored one) clears `custom_domain_verified_at`; the editor sends the field on every save, and resending the same domain keeps it verified. Unique violation → `409 custom domain is taken by another page`.
- `GET …/{page_id}` adds `custom_domain_verification: VerificationRecord | null` to `page`.
- `POST …/{page_id}/custom-domain/verify` (admin, `{}`) → `200 {"verified": true}` and sets `custom_domain_verified_at = now()` when one TXT string equals `value`. Otherwise `400 {"error": "TXT record <name> with value <value> not found"}`. A lookup failure → `400 could not look up <name>`. No domain set → `400 set a custom domain first`.

- `GET /public/status-domain-check/{host}` (no session; nami-ui's `/_nami/tls-ask` is its only caller, R4) → `204` when `host` is a verified custom domain of any page, public or private, otherwise `404`. It says nothing else about the page.

`from_env` sets `TxtLookup::System` explicitly; `from_env_defaults_only` (which `from_env_for_test` is, and which `from_env` spreads last) sets `TxtLookup::Fixed(BTreeMap::new())`. Neither reads an environment variable for it. A `core/config.rs` unit test asserts `from_env_for_test().txt_lookup == Fixed(empty)`; `from_env` needs `DATABASE_URL` and the key, so its `System` value is pinned by reading the code in review.

The domain-check route is added to a `public_router()` in `statuspages::page::routes` in this task (no governor yet); Task 6 moves it behind the public status governor with the DTO routes.

- [ ] **Step 1: Write the failing tests** (append to `tests/statuspages/status_pages_api.rs`)

```rust
#[sqlx::test]
async fn a_custom_domain_routes_only_after_its_txt_record_is_verified(pool: sqlx::PgPool) {
    let mut state = nami_engine::core::state::AppState::for_test(pool.clone());
    state.config.ui_base_url = "https://nami.acme.test".into();
    let project = seed_project(&pool).await;
    let app = common::app(state.clone());
    let (_, admin_email, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let page = create_page(&app, project, &admin, "acme").await;
    let page_id: Uuid = page["id"].as_str().unwrap().parse().unwrap();
    let uri = format!("/projects/{project}/status-pages/{page_id}");

    let (status, _) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "custom_domain": "nami.acme.test" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the app's own host");
    let (status, body) = call(&app, "PATCH", &uri, Some(&admin), Some(json!({ "custom_domain": "Status.Acme.Test" }))).await;
    assert_eq!((status, body["custom_domain"].as_str(), body["custom_domain_verified"].as_bool()),
        (StatusCode::OK, Some("status.acme.test"), Some(false)));
    assert!(nami_engine::statuspages::page::model::find_by_verified_domain(&pool, "status.acme.test").await.unwrap().is_none());

    let record = nami_engine::statuspages::domain::verification_record(&state.cipher, page_id, "status.acme.test");
    assert_eq!(record.name, "_nami-verify.status.acme.test");
    assert!(record.value.starts_with("nami-verify=") && record.value.len() == "nami-verify=".len() + 32);
    let (_, detail) = call(&app, "GET", &uri, Some(&admin), None).await;
    assert_eq!(detail["page"]["custom_domain_verification"]["value"], record.value.as_str());

    let verify = format!("{uri}/custom-domain/verify");
    let (status, body) = call(&app, "POST", &verify, Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // The record appears (a wrong value first, then the right one among others).
    let mut with_record = state.clone();
    with_record.config.txt_lookup = nami_engine::statuspages::domain::TxtLookup::Fixed(
        [(record.name.clone(), vec!["nami-verify=wrong".into(), "v=spf1 -all".into(), record.value.clone()])].into());
    let app2 = common::app(with_record);
    let admin2 = common::login(&app2, &admin_email).await;
    let (status, body) = call(&app2, "POST", &verify, Some(&admin2), Some(json!({}))).await;
    assert_eq!((status, body["verified"].as_bool()), (StatusCode::OK, Some(true)), "{body}");
    let found = nami_engine::statuspages::page::model::find_by_verified_domain(&pool, "status.acme.test").await.unwrap();
    assert_eq!(found.map(|p| p.id), Some(page_id));
    assert_eq!(call(&app2, "GET", "/public/status-domain-check/status.acme.test", None, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call(&app2, "GET", "/public/status-domain-check/other.acme.test", None, None).await.0, StatusCode::NOT_FOUND);

    // Resending the same domain (the editor does on every save) keeps it; changing it clears verification.
    let (_, body) = call(&app2, "PATCH", &uri, Some(&admin2), Some(json!({ "custom_domain": "Status.Acme.Test.", "title": "x" }))).await;
    assert_eq!(body["custom_domain_verified"].as_bool(), Some(true), "{body}");
    let (_, body) = call(&app2, "PATCH", &uri, Some(&admin2), Some(json!({ "custom_domain": "status2.acme.test" }))).await;
    assert_eq!(body["custom_domain_verified"].as_bool(), Some(false));
}

#[test]
fn a_verified_token_is_bound_to_the_page_and_the_domain() {
    let cipher = common::test_cipher();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    use nami_engine::statuspages::domain::verification_record as r;
    assert_eq!(r(&cipher, a, "s.acme.test"), r(&cipher, a, "s.acme.test"));
    assert_ne!(r(&cipher, a, "s.acme.test").value, r(&cipher, b, "s.acme.test").value);
    assert_ne!(r(&cipher, a, "s.acme.test").value, r(&cipher, a, "t.acme.test").value);
}

#[sqlx::test]
async fn custom_domains_are_unique(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let a = create_page(&app, project, &admin, "page-a").await;
    let b = create_page(&app, project, &admin, "page-b").await;
    for (page, expected) in [(&a, StatusCode::OK), (&b, StatusCode::CONFLICT)] {
        let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/status-pages/{}", page["id"].as_str().unwrap()),
            Some(&admin), Some(json!({ "custom_domain": "status.acme.test" }))).await;
        assert_eq!(status, expected);
    }
}
```

Each `common::app(state)` has its own in-memory session store, so the test logs in again on `app2`.

Add the verify route to `json_bodies.rs`.

Run: engine command `--test statuspages --test core -- status_pages_api:: json_bodies::`, then `--lib -- statuspages::domain`. Expected: FAIL.

- [ ] **Step 2: Implement.** `verification_record`: `hex(HMAC-SHA256(Zeroizing(derive_subkey("nami/status-domain/v1")), "{page_id}|{domain}"))[..32]`. `lookup_txt(System, ..)` reuses `uptime::checker::dns`: make `build_resolver(nameserver, timeout)` and `readable(&RData)` `pub(crate)` (no behaviour change; the DNS monitor tests stay green) and call `build_resolver(None, Duration::from_secs(4))` (it adds 1 s, so 5 s in all; system config read once off the async threads, hosts file never, no cache). Query `"{name}."` for `RecordType::TXT`; `readable` already concatenates a record's strings. A NXDOMAIN or no-records error (`is_nx_domain()` / `is_no_records_found()`) is `Ok(vec![])`. `Fixed` reads the map.

- [ ] **Step 3: Run** the commands again, the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests
git commit -m "feat(engine): custom domains for status pages with TXT verification (NAMI-SP-3)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4 (NAMI-SP-4): Component status and 90-day history

**Files:**
- Create: `nami-engine/src/statuspages/status.rs`, `nami-engine/src/statuspages/history.rs`, `nami-engine/tests/statuspages/status_history.rs`
- Modify: `nami-engine/src/statuspages/mod.rs`

**Interfaces:**
- Consumes: `uptime::availability::{compute, Availability}`, `uptime::maintenance::model::{Span, MaintenanceWindow, list_for_project, active_windows, ActiveWindows}`, Task 2 `statuspages::page::model::Component`.
- Produces:

```rust
// statuspages::status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus { NoData, Operational, Maintenance, Degraded, PartialOutage, MajorOutage }  // declaration order = severity, worst last
impl ComponentStatus { pub fn as_str(self) -> &'static str; pub fn parse(raw: &str) -> Option<ComponentStatus>;
    pub fn label(self) -> &'static str }   // "Operational", "Degraded performance", "Partial outage", "Major outage", "Under maintenance", "No data"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceState<'a> { pub current_status: &'a str, pub paused: bool, pub in_maintenance: bool }
pub fn derive(source: SourceState<'_>) -> ComponentStatus;                         // R6 table
/// `impacts` = impacts of active reports affecting the component; the worst wins over `derived` (R6).
pub fn effective(derived: ComponentStatus, impacts: &[ComponentStatus]) -> ComponentStatus;
pub fn worst(statuses: impl IntoIterator<Item = ComponentStatus>) -> ComponentStatus;  // empty → NoData

// statuspages::history
pub const DAYS: usize = 90;
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bar { pub date: NaiveDate, pub status: ComponentStatus, pub uptime_pct: Option<f64>,
    pub downtime_minutes: i64, pub reports: Vec<BarReport> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BarReport { pub id: Uuid, pub title: String }
/// The 90 local days ending with `now`'s local date: [start, end) instants, oldest first.
pub fn day_spans(tz: Tz, now: DateTime<Utc>) -> Vec<(NaiveDate, Span)>;
pub fn status_for(availability: &Availability) -> ComponentStatus;    // R7 thresholds; None + all maintenance → Maintenance
/// Pure: one bar per day. `incidents`/`maintenance` unclipped; `reports` = (id, title, start, end-or-now).
pub fn bars(tz: Tz, now: DateTime<Utc>, created_at: DateTime<Utc>, incidents: &[Span], maintenance: &[Span],
    reports: &[(Uuid, String, Span)]) -> Vec<Bar>;
/// Per component of `page_id`: its incident spans in the last 91 days (open ones end at `now`) and the
/// maintenance spans that apply to its source. Three queries for the whole page.
pub async fn load_inputs(pool: &PgPool, project_id: Uuid, page_id: Uuid, tz: Tz, now: DateTime<Utc>)
    -> Result<HashMap<Uuid, ComponentInputs>, sqlx::Error>;
#[derive(Debug, Clone, Default)]
pub struct ComponentInputs { pub incidents: Vec<Span>, pub maintenance: Vec<Span> }
```

- [ ] **Step 1: Write the failing unit tests**

`status.rs`:

```rust
#[test]
fn derivation_follows_r6() {
    let s = |current_status, paused, in_maintenance| derive(SourceState { current_status, paused, in_maintenance });
    assert_eq!(s("up", false, false), ComponentStatus::Operational);
    assert_eq!(s("down", false, false), ComponentStatus::MajorOutage);
    assert_eq!(s("pending", false, false), ComponentStatus::NoData);
    assert_eq!(s("up", true, false), ComponentStatus::NoData);
    assert_eq!(s("down", true, false), ComponentStatus::NoData, "paused is grey, whatever the last state (ruling 5)");
    assert_eq!(s("down", false, true), ComponentStatus::Maintenance, "maintenance wins over down");
    assert_eq!(s("down", true, true), ComponentStatus::Maintenance, "maintenance wins over paused");
}

#[test]
fn an_active_report_overrides_even_downwards_and_the_worst_wins() {
    use ComponentStatus::*;
    assert_eq!(effective(MajorOutage, &[Degraded]), Degraded);
    assert_eq!(effective(Operational, &[Degraded, PartialOutage]), PartialOutage);
    assert_eq!(effective(Operational, &[]), Operational);
    assert_eq!(worst([Operational, NoData, Maintenance]), Maintenance);
    assert_eq!(worst([Operational, MajorOutage, Degraded]), MajorOutage);
    assert_eq!(worst([]), NoData);
    assert_eq!(serde_json::to_string(&PartialOutage).unwrap(), "\"partial_outage\"");
}
```

`history.rs` (helpers `fn utc(y, m, d, h, mi) -> DateTime<Utc>` and `fn span(a, b) -> Span`):

```rust
#[test]
fn days_follow_the_page_zone_across_dst() {
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    let now = utc(2026, 11, 1, 12, 0);
    let days = day_spans(tz, now);
    assert_eq!(days.len(), DAYS);
    assert_eq!(days.last().unwrap().0, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
    for w in days.windows(2) { assert_eq!(w[0].1.end, w[1].1.start, "contiguous"); }
    let len = |d: (i32, u32, u32)| { let date = NaiveDate::from_ymd_opt(d.0, d.1, d.2).unwrap();
        let s = days.iter().find(|(x, _)| *x == date).unwrap().1; (s.end - s.start).num_hours() };
    assert_eq!(len((2026, 10, 25)), 25, "fall back");
    assert_eq!(len((2026, 10, 24)), 24);
    let spring = day_spans(tz, utc(2026, 4, 15, 12, 0));
    let s = spring.iter().find(|(d, _)| *d == NaiveDate::from_ymd_opt(2026, 3, 29).unwrap()).unwrap().1;
    assert_eq!((s.end - s.start).num_hours(), 23, "spring forward");
    assert_eq!(s.start, utc(2026, 3, 28, 23, 0), "local midnight in CET");
}

#[test]
fn an_incident_across_midnight_is_split_between_its_days() {
    let tz: Tz = "UTC".parse().unwrap();
    let now = utc(2026, 10, 19, 12, 0);
    let bars = bars(tz, now, utc(2026, 1, 1, 0, 0), &[span(utc(2026, 10, 17, 23, 0), utc(2026, 10, 18, 1, 0))], &[], &[]);
    let by = |d: u32| bars.iter().find(|b| b.date == NaiveDate::from_ymd_opt(2026, 10, d).unwrap()).unwrap();
    assert_eq!((by(17).downtime_minutes, by(18).downtime_minutes), (60, 60));
    assert_eq!(bars.iter().map(|b| b.downtime_minutes).sum::<i64>(), 120, "nothing lost or doubled");
    assert_eq!(by(17).status, ComponentStatus::PartialOutage, "60 min of 24 h = 95.8 %");
    assert_eq!(by(16).status, ComponentStatus::Operational);
    assert_eq!(by(19).uptime_pct, Some(100.0), "today counts up to now only");
}

#[test]
fn days_before_creation_have_no_data_and_maintenance_is_not_downtime() {
    let tz: Tz = "UTC".parse().unwrap();
    let now = utc(2026, 10, 19, 12, 0);
    let created = utc(2026, 10, 10, 6, 0);
    let bars = bars(tz, now, created, &[span(utc(2026, 10, 12, 0, 0), utc(2026, 10, 12, 2, 0))],
        &[span(utc(2026, 10, 12, 0, 0), utc(2026, 10, 12, 1, 0))], &[]);
    let by = |d: u32| bars.iter().find(|b| b.date == NaiveDate::from_ymd_opt(2026, 10, d).unwrap()).unwrap().clone();
    assert_eq!((by(9).status, by(9).uptime_pct), (ComponentStatus::NoData, None));
    assert_eq!(by(10).uptime_pct, Some(100.0), "a partial first day counts from creation");
    assert_eq!(by(12).downtime_minutes, 60, "the maintenance hour is excluded");
    assert_eq!(by(12).status, ComponentStatus::PartialOutage);
}

#[test]
fn thresholds_and_report_titles() {
    let tz: Tz = "UTC".parse().unwrap();
    let now = utc(2026, 10, 19, 23, 59);
    let r = Uuid::new_v4();
    let bars = bars(tz, now, utc(2026, 1, 1, 0, 0), &[span(utc(2026, 10, 18, 0, 0), utc(2026, 10, 18, 0, 10))], &[],
        &[(r, "API slow".into(), span(utc(2026, 10, 18, 0, 0), utc(2026, 10, 18, 3, 0)))]);
    let day = bars.iter().find(|b| b.date == NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()).unwrap();
    assert_eq!(day.status, ComponentStatus::Degraded, "10 min = 99.3 %");
    assert_eq!(day.reports, vec![BarReport { id: r, title: "API slow".into() }]);
    // `Availability` has no `Default` on main: build it literally (do not add a derive to Uptime's type for a test).
    let a = |pct: Option<f64>, maintenance_seconds: i64| crate::uptime::availability::Availability {
        from: now, to: now, total_seconds: 86_400, maintenance_seconds, downtime_seconds: 0,
        uptime_pct: pct, incidents: 0, longest_incident_seconds: 0 };
    assert_eq!(status_for(&a(Some(100.0), 0)), ComponentStatus::Operational);
    assert_eq!(status_for(&a(Some(99.0), 0)), ComponentStatus::Degraded);
    assert_eq!(status_for(&a(Some(95.0), 0)), ComponentStatus::PartialOutage);
    assert_eq!(status_for(&a(Some(94.99), 0)), ComponentStatus::MajorOutage);
    assert_eq!(status_for(&a(None, 0)), ComponentStatus::NoData);
    assert_eq!(status_for(&a(None, 86_400)), ComponentStatus::Maintenance, "a day wholly in maintenance");
}
```

`tests/statuspages/status_history.rs`:

```rust
// Loading a page's history inputs. Requires `docker compose up -d postgres`.
use crate::common;
use chrono::{TimeDelta, Utc};
use nami_engine::statuspages::history;

#[sqlx::test]
async fn inputs_are_per_component_and_respect_window_scope(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let m1 = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    let m2 = common::seed_http_monitor(&pool, project, "b", "https://b.test/", 1).await;
    let hb = common::seed_heartbeat(&pool, project, "cron").await;
    let c1 = common::seed_component(&pool, page, "A", Some(m1), None).await;
    let c2 = common::seed_component(&pool, page, "B", Some(m2), None).await;
    let c3 = common::seed_component(&pool, page, "Cron", None, Some(hb)).await;
    let now = Utc::now();
    // m1: one resolved incident 2 days ago (1 h) and one still open; m2: one 100 days ago (outside).
    sqlx::query("insert into incidents (project_id, monitor_id, title, status, started_at, resolved_at) values
                 ($1, $2, 'x', 'resolved', $4, $4 + interval '1 hour'),
                 ($1, $2, 'y', 'open', $5, null),
                 ($1, $3, 'z', 'resolved', $6, $6 + interval '1 hour')")
        .bind(project).bind(m1).bind(m2)
        .bind(now - TimeDelta::days(2)).bind(now - TimeDelta::minutes(30)).bind(now - TimeDelta::days(100))
        .execute(&pool).await.unwrap();
    // A window scoped to m2 and a project-wide one-off window.
    sqlx::query("insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at) values
                 ($1, $2, 'm2 only', 'once', $3, $3 + interval '1 hour'),
                 ($1, null, 'all', 'once', $4, $4 + interval '1 hour')")
        .bind(project).bind(m2).bind(now - TimeDelta::days(3)).bind(now - TimeDelta::days(4))
        .execute(&pool).await.unwrap();

    let inputs = history::load_inputs(&pool, project, page, chrono_tz::UTC, now).await.unwrap();
    assert_eq!(inputs[&c1].incidents.len(), 2);
    assert_eq!(inputs[&c1].incidents[1].end, now, "an open incident ends now");
    assert!(inputs[&c2].incidents.is_empty(), "older than the window");
    assert_eq!(inputs[&c1].maintenance.len(), 1, "only the project-wide window applies to m1");
    assert_eq!(inputs[&c2].maintenance.len(), 2);
    assert_eq!(inputs[&c3].maintenance.len(), 1, "heartbeats: project-wide windows only");
}
```

Run: engine command `--lib -- statuspages::status statuspages::history`, then engine command `--test statuspages -- status_history::`. Expected: FAIL.

- [ ] **Step 2: Implement.**
  - `day_spans`: `today = now.with_timezone(&tz).date_naive()`. For `d` in `today - 89 ..= today`: `start = tz.from_local_datetime(&d.and_hms(0,0,0)).earliest()`, else step forward a minute at a time (at most 180 steps) until one exists. `end` is the next day's start.
  - `bars`: per day, `total = Span { start: max(day.start, created_at), end: min(day.end, now) }`. If `total.start >= total.end`: `no_data`, `uptime_pct: None`, minutes 0, but reports overlapping the day are still listed. Otherwise `a = compute(total, incidents, maintenance)`, `status = status_for(&a)`, `downtime_minutes = a.downtime_seconds / 60` (floor). Reports overlap when `start < day.end && end > day.start`.
  - `load_inputs` returns an entry for **every** component of the page (empty vectors when nothing applies; the test indexes the map directly). (1) `select c.id, c.monitor_id, c.heartbeat_id from status_page_components c where c.status_page_id = $1`; (2) one incidents query for all bound sources: `select monitor_id, heartbeat_id, started_at, resolved_at from incidents where project_id = $1 and (monitor_id = any($2) or heartbeat_id = any($3)) and (resolved_at is null or resolved_at > $4) and started_at < $5 order by started_at` with `$4 = now - 91 days`, `$5 = now`; (3) `maintenance::list_for_project` once, then `spans(now - 91 days, now)` per window, assigned per `applies_to_monitor` / `applies_to_heartbeats`.

- [ ] **Step 3: Run** both commands again, then `TZ=Pacific/Chatham` and `TZ=America/Santiago` for the `--lib` command (the code must not depend on the host zone). Then the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src/statuspages nami-engine/tests/statuspages/status_history.rs
git commit -m "feat(engine): status page component status and 90-day history (NAMI-SP-4)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5 (NAMI-SP-5): Status reports — incidents and maintenance with updates

**Files:**
- Create: `nami-engine/src/statuspages/report/{mod,model,routes}.rs`, `nami-engine/tests/statuspages/status_reports_api.rs`
- Modify: `nami-engine/migrations/NNNN_statuspages_part1.sql` (append), `nami-engine/src/statuspages/mod.rs` (`pub mod report;`), `nami-engine/src/core/router.rs` (merge `router()` of `statuspages::report`), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`, `nami-engine/tests/common/mod.rs` (`seed_report`)

**Interfaces:**
- Consumes: Task 2 pages and components; Task 4 `ComponentStatus` (impacts).
- Produces:

```rust
// statuspages::report::model
pub const MAX_UPDATES: i64 = 200;
pub const MAX_NOTIFIED_UPDATES: i64 = 20;
pub const MAX_AFFECTED: usize = 100;
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024;
pub const INCIDENT_STATUSES: [&str; 4] = ["investigating", "identified", "monitoring", "resolved"];
pub const MAINTENANCE_STATUSES: [&str; 3] = ["scheduled", "in_progress", "completed"];
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Report { pub id: Uuid, pub status_page_id: Uuid, pub title: String, pub kind: String, pub status: String,
    pub starts_at: Option<DateTime<Utc>>, pub ends_at: Option<DateTime<Utc>>, pub incident_id: Option<Uuid>,
    pub automatic: bool, pub published_at: DateTime<Utc>, pub updated_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Affected { pub component_id: Uuid, pub impact: String }
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Update { pub id: Uuid, pub report_id: Uuid, pub status: String, pub message: String,
    pub notify_subscribers: bool, pub created_by: Option<Uuid>, pub created_at: DateTime<Utc> }
pub struct NewReport<'a> { pub page_id: Uuid, pub title: &'a str, pub kind: &'a str, pub status: &'a str,
    pub starts_at: Option<DateTime<Utc>>, pub ends_at: Option<DateTime<Utc>>, pub affected: &'a [(Uuid, ComponentStatus)],
    pub message: &'a str, pub notify_subscribers: bool, pub created_by: Option<Uuid>, pub incident_id: Option<Uuid>,
    pub automatic: bool }
/// Inserts the report, its affected rows and its first update. Returns (report id, update id).
/// `None` when `incident_id` is set and that page already has a report for it (R9).
pub async fn insert(conn: &mut PgConnection, r: &NewReport<'_>) -> Result<Option<(Uuid, Uuid)>, sqlx::Error>;
/// Appends an update and moves the report's status; a manual `resolved`/`completed` clears `automatic`
/// when `by_hand`. Returns the update id.
pub async fn post_update(conn: &mut PgConnection, report_id: Uuid, status: &str, message: &str, notify: bool,
    created_by: Option<Uuid>, by_hand: bool) -> Result<Uuid, sqlx::Error>;
pub fn validate_title(raw: &str) -> Result<String, String>;     // 1-255 chars, one line, no invisible/bidi
pub fn validate_message(raw: &str) -> Result<String, String>;   // trimmed, 1 B - 10 KiB
pub fn validate_status(kind: &str, status: &str) -> Result<(), String>;
pub fn validate_impact(kind: &str, impact: &str) -> Result<ComponentStatus, String>;  // incident: degraded|partial_outage|major_outage; maintenance: maintenance
```

Routes (under `/projects/{id}/status-pages/{page_id}/reports`):

| Method + path | Role | Body | Answer |
|---|---|---|---|
| `GET ""` | viewer | `?limit=1..100 (50)&before=<rfc3339>` | `200 [ReportView]` newest first |
| `POST ""` | member | `{title, kind, status?, starts_at?, ends_at?, affected: [{component_id, impact}], message, notify_subscribers?: true}` | `201 ReportView` |
| `GET /{report_id}` | viewer | | `200 ReportView` (all updates, oldest first) |
| `PATCH /{report_id}` | member | `{title?, affected?, starts_at?, ends_at?}` | `200 ReportView` |
| `POST /{report_id}/updates` | member | `{status, message, notify_subscribers?: true}` | `201 Update`; 201st → `409 a report can have at most 200 updates`; 21st notified → 400 (R8) |
| `DELETE /{report_id}` | admin | | `204` |

`ReportView = Report + {affected: [Affected], updates: [Update]}`. `status` defaults to `investigating` / `scheduled`. A `component_id` not on this page → `400 component_id not found on this page`. Every successful write calls `state.status_pages.invalidate(page_id)` once Task 6 adds the cache. Until then the call does not exist; Task 6 adds it to these handlers.

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_statuspages_part1.sql`:

```sql
-- Status reports: incidents and maintenance announced on a status page (NAMI-SP-5).
create table status_reports (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null references status_pages(id) on delete cascade,
    title text not null check (char_length(title) between 1 and 255),
    kind text not null check (kind in ('incident', 'maintenance')),
    status text not null,
    starts_at timestamptz,
    ends_at timestamptz,
    -- Automatic reports (R9): the incident that opened it, unique per page.
    incident_id uuid references incidents(id) on delete set null,
    automatic boolean not null default false,
    published_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check ((kind = 'incident' and status in ('investigating', 'identified', 'monitoring', 'resolved'))
        or (kind = 'maintenance' and status in ('scheduled', 'in_progress', 'completed'))),
    check (kind = 'incident' or (starts_at is not null and ends_at is not null and starts_at < ends_at)),
    check (not automatic or kind = 'incident')
);
create index status_reports_page_idx on status_reports (status_page_id, published_at desc);
create unique index status_reports_one_per_page_incident on status_reports (status_page_id, incident_id)
    where incident_id is not null;
create index status_reports_incident_idx on status_reports (incident_id) where incident_id is not null;

create table status_report_components (
    report_id uuid not null references status_reports(id) on delete cascade,
    component_id uuid not null references status_page_components(id) on delete cascade,
    impact text not null check (impact in ('degraded', 'partial_outage', 'major_outage', 'maintenance')),
    primary key (report_id, component_id)
);
create index status_report_components_component_idx on status_report_components (component_id);

create table status_report_updates (
    id uuid primary key default gen_random_uuid(),
    report_id uuid not null references status_reports(id) on delete cascade,
    status text not null,
    message text not null check (octet_length(message) between 1 and 10240),
    notify_subscribers boolean not null default true,
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now()
);
create index status_report_updates_report_idx on status_report_updates (report_id, created_at);
```

Add the three tables to `all_tables_exist`. `common::seed_report(pool, page, title, kind, status, affected: &[(Uuid, &str)]) -> Uuid` inserts a report and one update directly.

- [ ] **Step 2: Write the failing tests** — `tests/statuspages/status_reports_api.rs`:

```rust
// Status reports API. Requires `docker compose up -d postgres`.
use crate::common;
use axum::http::StatusCode;
use common::{call, seed_project, test_app, user_with_role};
use serde_json::json;

#[sqlx::test]
async fn members_report_and_post_updates_admins_delete(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let m = common::seed_http_monitor(&pool, project, "api", "https://a.test/", 1).await;
    let component = common::seed_component(&pool, page, "API", Some(m), None).await;
    let base = format!("/projects/{project}/status-pages/{page}/reports");

    let body = json!({ "title": "API errors", "kind": "incident", "affected": [{ "component_id": component, "impact": "partial_outage" }],
                       "message": "We are looking into it." });
    assert_eq!(call(&app, "POST", &base, Some(&viewer), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
    let (status, report) = call(&app, "POST", &base, Some(&member), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!((report["status"].as_str(), report["updates"].as_array().unwrap().len()), (Some("investigating"), 1));
    assert_eq!(report["updates"][0]["notify_subscribers"], true, "default on");

    let uri = format!("{base}/{}", report["id"].as_str().unwrap());
    let (status, update) = call(&app, "POST", &format!("{uri}/updates"), Some(&member),
        Some(json!({ "status": "identified", "message": "Bad deploy.", "notify_subscribers": false }))).await;
    assert_eq!(status, StatusCode::CREATED, "{update}");
    let (_, report) = call(&app, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(report["status"], "identified");
    let (status, _) = call(&app, "POST", &format!("{uri}/updates"), Some(&member),
        Some(json!({ "status": "completed", "message": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a maintenance status on an incident");
    assert_eq!(call(&app, "DELETE", &uri, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &uri, Some(&admin), None).await.0, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn maintenance_needs_a_window_and_the_maintenance_impact(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let m = common::seed_http_monitor(&pool, project, "api", "https://a.test/", 1).await;
    let c = common::seed_component(&pool, page, "API", Some(m), None).await;
    let base = format!("/projects/{project}/status-pages/{page}/reports");
    let mk = |extra: serde_json::Value| { let mut b = json!({ "title": "DB upgrade", "kind": "maintenance", "message": "Planned.",
        "affected": [{ "component_id": c, "impact": "maintenance" }] }); b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone()); b };
    let (status, _) = call(&app, "POST", &base, Some(&member), Some(mk(json!({})))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no window");
    let (status, _) = call(&app, "POST", &base, Some(&member), Some(mk(json!({
        "starts_at": "2026-11-01T02:00:00Z", "ends_at": "2026-11-01T01:00:00Z" })))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "end before start");
    let mut bad_impact = mk(json!({ "starts_at": "2026-11-01T01:00:00Z", "ends_at": "2026-11-01T03:00:00Z" }));
    bad_impact["affected"][0]["impact"] = json!("major_outage");
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(bad_impact)).await.0, StatusCode::BAD_REQUEST);
    let (status, report) = call(&app, "POST", &base, Some(&member), Some(mk(json!({
        "starts_at": "2026-11-01T01:00:00Z", "ends_at": "2026-11-01T03:00:00Z" })))).await;
    assert_eq!((status, report["status"].as_str()), (StatusCode::CREATED, Some("scheduled")));
}

#[sqlx::test]
async fn text_caps_and_cross_page_components(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let other_page = common::seed_status_page(&pool, project, "other").await;
    let m = common::seed_http_monitor(&pool, project, "api", "https://a.test/", 1).await;
    let foreign_component = common::seed_component(&pool, other_page, "API", Some(m), None).await;
    let base = format!("/projects/{project}/status-pages/{page}/reports");
    for (title, message) in [("", "x"), ("a\nb", "x"), (&"t".repeat(256)[..], "x"), ("ok", ""), ("ok", &"m".repeat(10 * 1024 + 1)[..])] {
        let (status, _) = call(&app, "POST", &base, Some(&member),
            Some(json!({ "title": title, "kind": "incident", "affected": [], "message": message }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{title:?}");
    }
    let (status, body) = call(&app, "POST", &base, Some(&member), Some(json!({ "title": "x", "kind": "incident",
        "affected": [{ "component_id": foreign_component, "impact": "degraded" }], "message": "x" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("component_id not found on this page")));
}

#[sqlx::test]
async fn notified_updates_are_capped_per_report(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let report = common::seed_report(&pool, page, "x", "incident", "investigating", &[]).await;   // 1 notified update
    let uri = format!("/projects/{project}/status-pages/{page}/reports/{report}/updates");
    for _ in 1..20 {
        assert_eq!(call(&app, "POST", &uri, Some(&member), Some(json!({ "status": "monitoring", "message": "x" }))).await.0, StatusCode::CREATED);
    }
    let (status, body) = call(&app, "POST", &uri, Some(&member), Some(json!({ "status": "monitoring", "message": "x" }))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST,
        Some("this report has reached 20 notified updates; post without notifying subscribers")));
    let (status, _) = call(&app, "POST", &uri, Some(&member),
        Some(json!({ "status": "resolved", "message": "fixed", "notify_subscribers": false }))).await;
    assert_eq!(status, StatusCode::CREATED);
}
```

Add `POST`/`PATCH` report routes and `POST …/updates` to `json_bodies.rs`.

Run: engine command `--test statuspages --test core -- status_reports_api:: migrations:: json_bodies::`. Expected: FAIL.

- [ ] **Step 3: Implement** the model and routes. `post_update` locks the report row (`for update`), counts updates (`MAX_UPDATES`) and notified updates (`MAX_NOTIFIED_UPDATES`, only when `notify`), validates the status for the kind, inserts, then `update status_reports set status = $2, updated_at = now(), automatic = automatic and not $3` with `$3 = by_hand and status in ('resolved','completed')`. The route passes `by_hand = true`.

- [ ] **Step 4: Run** the command again, the full suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/NNNN_statuspages_part1.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): status reports with updates, maintenance windows and caps (NAMI-SP-5)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6 (NAMI-SP-6): Public DTO, snapshot cache, ETag and rate limit

**Files:**
- Create: `nami-engine/src/statuspages/snapshot.rs`, `nami-engine/src/statuspages/cache.rs`, `nami-engine/src/statuspages/public/{mod,routes}.rs`, `nami-engine/tests/statuspages/status_public.rs`
- Modify: `nami-engine/src/core/state.rs` (`pub status_pages: Arc<SnapshotCache>`, `SnapshotCache::default()` in `for_test` and in `main.rs`'s construction), `nami-engine/src/main.rs`, `nami-engine/src/core/router.rs` (merge the public status router behind its own governor), `nami-engine/src/statuspages/page/routes.rs` and `nami-engine/src/statuspages/report/routes.rs` (`state.status_pages.invalidate(page_id)` after each successful write), `nami-engine/src/statuspages/mod.rs`, `nami-engine/tests/common/mod.rs` (`call_raw`)

**Interfaces:**
- Consumes: Task 2 `statuspages::page::model::{find_by_slug, find_by_verified_domain, sections, components}`, Task 3 `domain::app_host`, Task 4 `status::*`, `history::{bars, load_inputs}`, Task 5 `statuspages::report::model`, `uptime::maintenance::model::active_windows`.
- Produces:

```rust
// statuspages::snapshot — the DTO types serialise to exactly R11's shape (no other fields, no skip_serializing_if).
#[derive(Debug, Clone, Serialize)] pub struct PublicDto { pub page: PublicPage, pub status: ComponentStatus,
    pub sections: Vec<PublicSection>, pub active_reports: Vec<PublicReport>, pub upcoming_maintenance: Vec<PublicReport>,
    pub history: Vec<PublicReport>, pub subscriptions: Subscriptions, pub generated_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize)] pub struct PublicPage { pub slug: String, pub title: String, pub description: Option<String>,
    pub logo_url: Option<String>, pub logo_dark_url: Option<String>, pub accent_color: String, pub theme: String,
    pub nav_links: Vec<NavLink>, pub timezone: String, pub noindex: bool, pub visibility: String, pub url: String }
#[derive(Debug, Clone, Serialize)] pub struct PublicSection { pub id: Uuid, pub name: String, pub components: Vec<PublicComponent> }
#[derive(Debug, Clone, Serialize)] pub struct PublicComponent { pub id: Uuid, pub name: String, pub description: Option<String>,
    pub status: ComponentStatus, pub bars: Option<Vec<Bar>> }
#[derive(Debug, Clone, Serialize)] pub struct PublicReport { pub id: Uuid, pub title: String, pub kind: String, pub status: String,
    pub impact: Option<ComponentStatus>, pub starts_at: Option<DateTime<Utc>>, pub ends_at: Option<DateTime<Utc>>,
    pub published_at: DateTime<Utc>, pub updated_at: DateTime<Utc>, pub components: Vec<PublicAffected>, pub updates: Vec<PublicUpdate> }
#[derive(Debug, Clone, Serialize)] pub struct PublicAffected { pub id: Uuid, pub name: String, pub impact: ComponentStatus }
#[derive(Debug, Clone, Serialize)] pub struct PublicUpdate { pub id: Uuid, pub status: String, pub message: String, pub created_at: DateTime<Utc> }
#[derive(Debug, Clone, Serialize)] pub struct Subscriptions { pub email: bool, pub webhook: bool }
/// Public bytes only. Access is never decided from a snapshot (R10): the route reads the page row fresh.
/// `axum::body::Bytes` (the `bytes` crate is not a direct dependency).
pub struct Snapshot { pub json: axum::body::Bytes, pub etag: String }
pub fn page_url(config: &Config, page: &StatusPage) -> String;   // R11
/// Builds from Postgres only (R10). Report lists per R8; at most 20 updates per report, newest first.
pub async fn build(state: &AppState, page: &StatusPage, now: DateTime<Utc>) -> Result<Snapshot, sqlx::Error>;

// statuspages::cache
pub const TTL: Duration = Duration::from_secs(30);
pub const MAX_ENTRIES: usize = 1000;
#[derive(Default)] pub struct SnapshotCache { /* std Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<Option<Entry>>>>>,
    Entry { version: DateTime<Utc>, built_at: Instant, snapshot: Arc<Snapshot> }, builds: AtomicU64 */ }
impl SnapshotCache {
    /// An entry with this `version` (the fresh row's `updated_at`) built less than TTL ago → it. Otherwise holds
    /// the page's lock while `build` runs (re-checking after the lock is taken), so concurrent misses build once.
    pub async fn get_or_build<F, Fut>(&self, page_id: Uuid, version: DateTime<Utc>, build: F) -> Result<Arc<Snapshot>, sqlx::Error>
        where F: FnOnce() -> Fut, Fut: Future<Output = Result<Snapshot, sqlx::Error>>;
    pub fn invalidate(&self, page_id: Uuid);
    /// Snapshots built since start (tests).
    pub fn builds(&self) -> u64;
}

// statuspages::public::routes
pub const READS_PER_MS: u64 = 100;   // governor: one token per 100 ms (10/s)
pub const READ_BURST: u32 = 30;
pub fn router() -> Router<AppState>;   // GET /public/status-pages/{slug}, GET /public/status-domains/{host}
```

Response rules (both GET routes), in this order:
1. Read the page row fresh (`find_by_slug` / `find_by_verified_domain`). Not found, domain unverified, or private (Task 7 adds the access check against this fresh row; in this task a private page is always 404) → `json_error(404, "not found")`.
2. Only then `get_or_build(page.id, page.updated_at, ..)`.
3. `If-None-Match` equal to the ETag → `304` with the same `etag` header and no body. (Never before step 1: a private page without access must not answer 304.)
4. 200 body = `snapshot.json` (`content-type: application/json`). Headers: `etag`; `cache-control: public, max-age=30` for public pages; `x-robots-tag: noindex` when the fresh row has `noindex`.

This task also moves Task 3's `/public/status-domain-check/{host}` behind the same governor, and adds the R10 `updated_at` bump plus `state.status_pages.invalidate(page_id)` to every Task 2 and Task 5 write handler.
- Reads go through a `GovernorLayer` (`SmartIpKeyExtractor`, `per_millisecond(100)`, `burst_size(30)`, error handler `AppError::RateLimited`). It applies only to the public status router (merged into `build_router` like `rate_limited_with_governor`).
- The handler never reads the session.

`build` steps:
1. `sections`, `components` (Task 2).
2. Statuses: one query `select id, current_status, paused from monitors where id = any($1)` and the same for heartbeats. `active_windows(project, now)` once. Active reports' impacts per component: `select rc.component_id, rc.impact from status_report_components rc join status_reports r on r.id = rc.report_id where r.status_page_id = $1 and r.status not in ('resolved','completed') and (r.status <> 'scheduled' or (r.starts_at <= $2 and r.ends_at > $2))`.
3. Bars: `history::load_inputs`, plus reports (id, title, published_at, the resolved/completed update's time or `now`) per component for the tooltip.
4. Reports per R8.
5. `json = serde_json::to_vec(&dto)`, `etag` per R10.

- [ ] **Step 1: Write the failing tests** — `nami-engine/tests/statuspages/status_public.rs`:

```rust
// The public status page DTO. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call_raw, seed_project, test_app};
use nami_engine::core::state::AppState;
use serde_json::Value;
use uuid::Uuid;

/// A page with every kind of secret behind it: what must never leak (R11, spec "Public DTO leak test").
struct Fixture { project: Uuid, page: Uuid, monitor: Uuid, heartbeat: Uuid, incident: Uuid, member_email: String,
    project_slug: String, heartbeat_token: String }

async fn fixture(pool: &sqlx::PgPool, app: &axum::Router) -> Fixture {
    let project = seed_project(pool).await;
    let (_, member_email, _) = common::user_with_role(app, pool, project, Some("member")).await;
    let page = common::seed_status_page(pool, project, "acme").await;
    let monitor = common::seed_http_monitor(pool, project, "db-primary-internal",
        "https://secret-internal.example:8443/health?token=hunter2", 1).await;
    let group: Uuid = sqlx::query_scalar("insert into monitor_groups (project_id, name) values ($1, 'internal-payments-group') returning id")
        .bind(project).fetch_one(pool).await.unwrap();
    sqlx::query("update monitors set check_type = 'keyword', keyword = 'SECRETKW', current_status = 'down',
                 auth_username = 'svc-basic-user', request_header_names = '{X-Internal-Token}', group_id = $2,
                 proxy_url = 'http://proxy-internal.example:3128', proxy_username = 'svc-proxy-user' where id = $1")
        .bind(monitor).bind(group).execute(pool).await.unwrap();
    let heartbeat = common::seed_heartbeat(pool, project, "nightly-backup-internal").await;
    let heartbeat_token: String = sqlx::query_scalar("select token from heartbeats where id = $1")
        .bind(heartbeat).fetch_one(pool).await.unwrap();
    common::seed_component(pool, page, "API", Some(monitor), None).await;
    common::seed_component(pool, page, "Backups", None, Some(heartbeat)).await;
    let incident: Uuid = sqlx::query_scalar("insert into incidents (project_id, monitor_id, title, cause, details, status, started_at)
        values ($1, $2, 'db-primary-internal is DOWN', 'connection refused to 10.0.0.5:5432', 'SECRETDETAIL stack trace',
                'open', now() - interval '1 hour') returning id")
        .bind(project).bind(monitor).fetch_one(pool).await.unwrap();
    sqlx::query("update incidents set postmortem = 'SECRETPM root cause' where id = $1").bind(incident).execute(pool).await.unwrap();
    let project_slug: String = sqlx::query_scalar("select slug from projects where id = $1").bind(project).fetch_one(pool).await.unwrap();
    Fixture { project, page, monitor, heartbeat, incident, member_email, project_slug, heartbeat_token }
}

async fn dto(app: &axum::Router, slug: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let (status, headers, body) = call_raw(app, "GET", &format!("/public/status-pages/{slug}"), &[("x-forwarded-for", "203.0.113.9")], None).await;
    (status, headers, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[sqlx::test]
async fn the_public_dto_leaks_nothing_private(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let f = fixture(&pool, &app).await;
    let (status, _, body) = dto(&app, "acme").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body.to_string();
    for secret in [f.project.to_string(), f.page.to_string(), f.monitor.to_string(), f.heartbeat.to_string(), f.incident.to_string(),
                   "secret-internal".into(), "hunter2".into(), "8443".into(), "SECRETKW".into(), "db-primary-internal".into(),
                   "nightly-backup-internal".into(), "10.0.0.5".into(), "connection refused".into(), f.member_email.clone(),
                   "svc-basic-user".into(), "X-Internal-Token".into(), "internal-payments-group".into(), "SECRETDETAIL".into(),
                   "SECRETPM".into(), f.project_slug.clone(), f.heartbeat_token.clone(), "proxy-internal".into(), "svc-proxy-user".into(),
                   "password".into(), "ip_allowlist".into(), "access_epoch".into(), "custom_domain".into()] {
        assert!(!text.contains(&secret), "leaked {secret}: {text}");
    }
    let api = &body["sections"][0]["components"][0];
    assert_eq!((api["name"].as_str(), api["status"].as_str()), (Some("API"), Some("major_outage")));
    assert_eq!(body["status"], "major_outage", "the banner is the worst component");
}

#[sqlx::test]
async fn the_dto_has_exactly_the_r11_fields(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let f = fixture(&pool, &app).await;
    let c: Uuid = sqlx::query_scalar("select id from status_page_components where status_page_id = $1 and display_name = 'API'")
        .bind(f.page).fetch_one(&pool).await.unwrap();
    common::seed_report(&pool, f.page, "API errors", "incident", "identified", &[(c, "degraded")]).await;
    let (_, _, body) = dto(&app, "acme").await;
    let keys = |v: &Value| { let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect(); k.sort(); k };
    assert_eq!(keys(&body), ["active_reports", "generated_at", "history", "page", "sections", "status", "subscriptions", "upcoming_maintenance"]);
    assert_eq!(keys(&body["page"]), ["accent_color", "description", "logo_dark_url", "logo_url", "nav_links", "noindex", "slug",
        "theme", "timezone", "title", "url", "visibility"]);
    assert_eq!(keys(&body["sections"][0]["components"][0]), ["bars", "description", "id", "name", "status"]);
    assert_eq!(body["sections"][0]["components"][0]["bars"].as_array().unwrap().len(), 90);
    let report = &body["active_reports"][0];
    assert_eq!(keys(report), ["components", "ends_at", "id", "impact", "kind", "published_at", "starts_at", "status", "title", "updated_at", "updates"]);
    assert_eq!(keys(&report["updates"][0]), ["created_at", "id", "message", "status"]);
    assert_eq!(body["sections"][0]["components"][0]["status"], "degraded", "an active report overrides the derived status (R6)");
    assert_eq!(body["page"]["url"], "http://localhost:5173/status/acme");
}

#[sqlx::test]
async fn unknown_and_private_pages_are_the_same_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "secret-page").await;
    sqlx::query("update status_pages set visibility = 'private', password_hash = 'x' where id = $1").bind(page).execute(&pool).await.unwrap();
    let (s1, _, b1) = call_raw(&app, "GET", "/public/status-pages/secret-page", &[], None).await;
    let (s2, _, b2) = call_raw(&app, "GET", "/public/status-pages/no-such-page", &[], None).await;
    assert_eq!((s1, &b1), (StatusCode::NOT_FOUND, &b2));
    assert_eq!(s2, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn an_unverified_or_foreign_domain_is_a_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let a = common::seed_status_page(&pool, project, "page-a").await;
    let b = common::seed_status_page(&pool, project, "page-b").await;
    sqlx::query("update status_pages set custom_domain = 'a.acme.test', custom_domain_verified_at = now() where id = $1").bind(a).execute(&pool).await.unwrap();
    sqlx::query("update status_pages set custom_domain = 'b.acme.test' where id = $1").bind(b).execute(&pool).await.unwrap();
    let (status, _, body) = call_raw(&app, "GET", "/public/status-domains/a.acme.test", &[], None).await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, body["page"]["slug"].as_str(), body["page"]["url"].as_str()), (StatusCode::OK, Some("page-a"), Some("https://a.acme.test")));
    for host in ["b.acme.test", "c.acme.test", "A.ACME.TEST.evil", "page-a"] {
        assert_eq!(call_raw(&app, "GET", &format!("/public/status-domains/{host}"), &[], None).await.0, StatusCode::NOT_FOUND, "{host}");
    }
}

#[sqlx::test]
async fn concurrent_requests_build_the_snapshot_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    fixture(&pool, &app).await;
    let mut tasks = Vec::new();
    for i in 0..25 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            call_raw(&app, "GET", "/public/status-pages/acme", &[("x-forwarded-for", &format!("198.51.100.{i}"))], None).await.0
        }));
    }
    for t in tasks { assert_eq!(t.await.unwrap(), StatusCode::OK); }
    assert_eq!(state.status_pages.builds(), 1);
}

#[sqlx::test]
async fn admin_writes_invalidate_the_snapshot_and_etags_match(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let f = fixture(&pool, &app).await;
    let (_, _, admin) = common::user_with_role(&app, &pool, f.project, Some("admin")).await;
    let (_, headers, _) = dto(&app, "acme").await;
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();
    assert!(etag.starts_with('"') && etag.len() == 18, "{etag}");
    assert_eq!(headers.get("cache-control").unwrap(), "public, max-age=30");
    let (status, _, body) = call_raw(&app, "GET", "/public/status-pages/acme", &[("if-none-match", &etag)], None).await;
    assert_eq!((status, body.len()), (StatusCode::NOT_MODIFIED, 0));
    common::call(&app, "PATCH", &format!("/projects/{}/status-pages/{}", f.project, f.page), Some(&admin),
        Some(serde_json::json!({ "title": "Acme Cloud" }))).await;
    let (_, headers, body) = dto(&app, "acme").await;
    assert_eq!(body["page"]["title"], "Acme Cloud", "no 30 s wait after an edit");
    assert_ne!(headers.get("etag").unwrap().to_str().unwrap(), etag);
    assert_eq!(state.status_pages.builds(), 2);
}

#[sqlx::test]
async fn public_reads_are_rate_limited_per_client_address(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    fixture(&pool, &app).await;
    // Burst 30, then one per 100 ms: 60 quick requests must hit the limit even on a slow debug build.
    let mut limited = 0;
    for _ in 0..60 {
        if call_raw(&app, "GET", "/public/status-pages/acme", &[("x-forwarded-for", "192.0.2.1")], None).await.0
            == StatusCode::TOO_MANY_REQUESTS { limited += 1; }
    }
    assert!(limited > 0, "no 429 after 60 requests");
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/acme", &[("x-forwarded-for", "192.0.2.2")], None).await.0, StatusCode::OK);
}

#[sqlx::test]
async fn noindex_sends_the_robots_header(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let f = fixture(&pool, &app).await;
    let (_, headers, _) = dto(&app, "acme").await;
    assert!(headers.get("x-robots-tag").is_none());
    sqlx::query("update status_pages set noindex = true, slug = 'acme2' where id = $1").bind(f.page).execute(&pool).await.unwrap();
    let (_, headers, body) = dto(&app, "acme2").await;
    assert_eq!(headers.get("x-robots-tag").unwrap(), "noindex");
    assert_eq!(body["page"]["noindex"], true);
}
```

`tests/common/mod.rs` gains:

```rust
/// Like `call`, with extra request headers; returns status, headers and the raw body.
pub async fn call_raw(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: Option<Value>)
    -> (StatusCode, axum::http::HeaderMap, Vec<u8>)
```

Run: engine command `--test statuspages -- status_public::`. Expected: FAIL.

- [ ] **Step 2: Implement.** Use `tokio::sync::Mutex` per page in the cache. Eviction: when the map holds `MAX_ENTRIES` before an insert, remove the entry with the oldest build instant (or no build yet). `builds` increments inside `get_or_build` when `build` runs. Route lookup: `/public/status-pages/{slug}` → `validate::slug` (a malformed slug is 404 without a query) → `find_by_slug`. `/public/status-domains/{host}` → lowercase, `find_by_verified_domain`.

- [ ] **Step 3: Run** the command again, then the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests
git commit -m "feat(engine): public status page snapshot with cache, ETag and rate limit (NAMI-SP-6)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7 (NAMI-SP-7): Private pages — password unlock, page-scoped access token, IP allowlist

**Files:**
- Create: `nami-engine/src/statuspages/access.rs`, `nami-engine/src/statuspages/rate_limit.rs`
- Modify: `nami-engine/migrations/NNNN_statuspages_part1.sql` (append), `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/statuspages/public/routes.rs` (access check, unlock), `nami-engine/tests/statuspages/status_public.rs`, `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Interfaces:**
- Consumes: Task 1 `Cidr`, Task 6 routes (fresh-row lookup before the cache), `auth::password::verify_password`, `core::crypto::Cipher::derive_subkey`, `hmac`.
- Produces:

```rust
// statuspages::access
pub const ACCESS_TTL: TimeDelta = TimeDelta::days(30);
pub const HEADER: &str = "x-nami-page-access";
/// R12: exactly one valid IP in X-Forwarded-For, IPv4-mapped unwrapped; anything else None.
pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr>;
pub fn sign(cipher: &Cipher, page_id: Uuid, access_epoch: i32, exp: i64) -> String;
pub fn verify(cipher: &Cipher, token: &str, page_id: Uuid, access_epoch: i32, now: i64) -> bool;
/// Public pages: always. Private: a valid token for this page/epoch, or `ip` inside the allowlist.
/// `page` is the row read fresh for this request (R10), never a cached snapshot; its `ip_allowlist`
/// strings are parsed with `Cidr::parse` (a row that fails to parse grants nothing for that entry).
pub fn granted(cipher: &Cipher, page: &StatusPage, token: Option<&str>, ip: Option<IpAddr>, now: DateTime<Utc>) -> bool;

// statuspages::rate_limit
/// Counts one hit in `bucket` and says whether it is within `max` per `window` (R13). Runs its own transaction.
pub async fn hit(pool: &PgPool, bucket: &str, max: i64, window: Duration) -> Result<bool, sqlx::Error>;
/// hex(HMAC(subkey "nami/status-rate/v1", ip))[..32], or "unknown".
pub fn client_key(cipher: &Cipher, ip: Option<IpAddr>) -> String;
pub const UNLOCK_MAX: i64 = 10;
pub const UNLOCK_IP_MAX: i64 = 30;
pub const UNLOCK_WINDOW: Duration = Duration::from_secs(15 * 60);
```

Routes: `POST /public/status-pages/{slug}/unlock` and `POST /public/status-domains/{host}/unlock`, body `{password}` (≤ 128 chars, otherwise the same 401). Order:
1. Resolve the page (no 404 here: unknown pages continue with the dummy hash).
2. `hit(pool, "unlock:{page id or 'none'}:{client_key}", 10, 15 min)`, then `hit(pool, "unlock-ip:{client_key}", 30, 15 min)`; either over → `json_error(429, "too many attempts, try again later")`.
3. Verify (in `spawn_blocking`, holding an `UNLOCK_SLOTS` permit, Global Constraints) against `password_hash`, or against `DUMMY_HASH` (a `OnceLock<String>` hashed once at first use) when the page is unknown, public or has no password. `Err` counts as a wrong password.
4. Success → `200 {"token": sign(.., now + 30 d), "expires_at": …}`. Anything else → `json_error(401, "wrong password")`.

The GET routes from Task 6 now read `X-Nami-Page-Access` and `client_ip(headers)`, and call `granted` with the **fresh row** (R10), before the cache and before `If-None-Match`. Not granted → the same 404. Granted private → `cache-control: private, no-store` and `x-robots-tag: noindex` (R25), whatever the toggle.

- [ ] **Step 1: Write the migration**

Append to `nami-engine/migrations/NNNN_statuspages_part1.sql`:

```sql
-- DB-backed rate limits for status page unlock and subscribe (NAMI-SP-7, R13):
-- shared by every engine instance. Client addresses are stored only as keyed hashes.
create table status_page_rate_hits (
    bucket text not null check (char_length(bucket) between 1 and 160),
    hit_at timestamptz not null default now()
);
create index status_page_rate_hits_bucket_idx on status_page_rate_hits (bucket, hit_at);
```

`hit` runs, in one transaction: `select pg_advisory_xact_lock(hashtext($1))`, `delete from status_page_rate_hits where bucket = $1 and hit_at < now() - make_interval(secs => $2)`, `select count(*) …`, and when `count < max`, `insert`. Add the table to `all_tables_exist`.

- [ ] **Step 2: Write the failing tests**

`access.rs` `mod tests`:

```rust
#[test]
fn tokens_are_bound_to_page_epoch_and_expiry() {
    let cipher = Cipher::new(&crate::core::crypto::TEST_KEY);
    let (page, other) = (Uuid::new_v4(), Uuid::new_v4());
    let t = sign(&cipher, page, 3, 2_000);
    assert!(verify(&cipher, &t, page, 3, 1_999));
    assert!(!verify(&cipher, &t, page, 3, 2_000), "expired at exp");
    assert!(!verify(&cipher, &t, other, 3, 1_000), "another page");
    assert!(!verify(&cipher, &t, page, 4, 1_000), "password changed since");
    let mut forged: Vec<&str> = t.split('.').collect();
    let later = "9999999999".to_string();
    forged[2] = &later;
    assert!(!verify(&cipher, &forged.join("."), page, 3, 1_000), "a pushed-out expiry breaks the MAC");
    for junk in ["", "v1", "v1...", "v2.x.y.z", format!("{t}.extra").as_str()] { assert!(!verify(&cipher, junk, page, 3, 0), "{junk}"); }
    let other_key = Cipher::new(b"another-test-key-of-32-bytes-xyz");
    assert!(!verify(&other_key, &t, page, 3, 1_000));
}

#[test]
fn the_client_address_is_one_forwarded_ip_or_nothing() {
    let h = |v: &str| { let mut m = HeaderMap::new(); m.insert("x-forwarded-for", v.parse().unwrap()); m };
    assert_eq!(client_ip(&h("203.0.113.7")), Some("203.0.113.7".parse().unwrap()));
    assert_eq!(client_ip(&h(" 2001:db8::1 ")), Some("2001:db8::1".parse().unwrap()));
    assert_eq!(client_ip(&h("::ffff:10.1.2.3")), Some("10.1.2.3".parse().unwrap()));
    for bad in ["10.0.0.1, 127.0.0.1", "unknown", "10.0.0.1:443", ""] { assert_eq!(client_ip(&h(bad)), None, "{bad}"); }
    assert_eq!(client_ip(&HeaderMap::new()), None);
}
```

Append to `tests/statuspages/status_public.rs`:

```rust
async fn private_page(pool: &sqlx::PgPool, slug: &str, password: Option<&str>, allowlist: &[&str]) -> Uuid {
    let project = seed_project(pool).await;
    let page = common::seed_status_page(pool, project, slug).await;
    let hash = password.map(|p| nami_engine::auth::password::hash_password(p).unwrap());
    let list: Vec<String> = allowlist.iter().map(|s| s.to_string()).collect();
    sqlx::query("update status_pages set visibility = 'private', password_hash = $2, ip_allowlist = $3::text[]::cidr[] where id = $1")
        .bind(page).bind(hash).bind(list).execute(pool).await.unwrap();
    page
}

async fn unlock(app: &axum::Router, slug: &str, ip: &str, password: &str) -> (StatusCode, Value) {
    let (status, _, body) = call_raw(app, "POST", &format!("/public/status-pages/{slug}/unlock"),
        &[("x-forwarded-for", ip)], Some(serde_json::json!({ "password": password }))).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[sqlx::test]
async fn private_content_needs_a_token_for_this_page_and_epoch(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let page = private_page(&pool, "secret-a", Some("correct horse"), &[]).await;
    private_page(&pool, "secret-b", Some("battery staple"), &[]).await;
    let get = |token: Option<String>| { let app = app.clone(); async move {
        let mut h = vec![("x-forwarded-for", "203.0.113.5".to_string())];
        if let Some(t) = token { h.push(("x-nami-page-access", t)); }
        let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
        call_raw(&app, "GET", "/public/status-pages/secret-a", &h, None).await } };

    assert_eq!(unlock(&app, "secret-a", "203.0.113.5", "wrong").await, (StatusCode::UNAUTHORIZED, serde_json::json!({ "error": "wrong password" })));
    assert_eq!(get(None).await.0, StatusCode::NOT_FOUND);
    let (status, body) = unlock(&app, "secret-a", "203.0.113.5", "correct horse").await;
    assert_eq!(status, StatusCode::OK);
    let token = body["token"].as_str().unwrap().to_string();
    let (status, headers, _) = get(Some(token.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "private, no-store");
    assert_eq!(headers.get("x-robots-tag").unwrap(), "noindex", "private pages are always noindex");
    assert_eq!(get(Some(format!("{token}x"))).await.0, StatusCode::NOT_FOUND, "tampered");
    let (_, other) = unlock(&app, "secret-b", "203.0.113.5", "battery staple").await;
    assert_eq!(get(Some(other["token"].as_str().unwrap().into())).await.0, StatusCode::NOT_FOUND, "another page's token");
    let expired = nami_engine::statuspages::access::sign(&common::test_cipher(), page, 0, chrono::Utc::now().timestamp() - 1);
    assert_eq!(get(Some(expired)).await.0, StatusCode::NOT_FOUND, "expired");
}

#[sqlx::test]
async fn a_private_page_never_answers_304_without_access(pool: sqlx::PgPool) {
    // R10: If-None-Match is compared only after access. A 304 to a stranger would confirm the page exists
    // and tell whether it changed.
    let app = test_app(pool.clone());
    private_page(&pool, "secret-e", Some("correct horse"), &[]).await;
    let (_, body) = unlock(&app, "secret-e", "203.0.113.6", "correct horse").await;
    let token = body["token"].as_str().unwrap().to_string();
    let (status, headers, _) = call_raw(&app, "GET", "/public/status-pages/secret-e", &[("x-nami-page-access", &token)], None).await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();
    let (status, _, _) = call_raw(&app, "GET", "/public/status-pages/secret-e", &[("if-none-match", &etag)], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no token: 404 even with the right ETag");
    let (status, _, _) = call_raw(&app, "GET", "/public/status-pages/secret-e",
        &[("if-none-match", &etag), ("x-nami-page-access", &token)], None).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
}

#[sqlx::test]
async fn going_private_applies_at_once_even_with_a_warm_cache(pool: sqlx::PgPool) {
    // Access comes from the fresh row (R10), not from the cached snapshot.
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "turning").await;
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/turning", &[], None).await.0, StatusCode::OK);
    // A write that bypasses the routes (another instance, say): no in-process invalidate ran.
    sqlx::query("update status_pages set visibility = 'private', password_hash = 'x', updated_at = now() where id = $1")
        .bind(page).execute(&pool).await.unwrap();
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/turning", &[], None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn changing_the_password_revokes_issued_tokens(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = common::user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, page) = common::call(&app, "POST", &format!("/projects/{project}/status-pages"), Some(&admin),
        Some(serde_json::json!({ "slug": "secret-c", "title": "x" }))).await;
    let uri = format!("/projects/{project}/status-pages/{}", page["id"].as_str().unwrap());
    common::call(&app, "PATCH", &uri, Some(&admin), Some(serde_json::json!({ "visibility": "private", "password": "first-password" }))).await;
    let (_, body) = unlock(&app, "secret-c", "203.0.113.5", "first-password").await;
    let token = body["token"].as_str().unwrap().to_string();
    let read = |t: String| { let app = app.clone(); async move {
        call_raw(&app, "GET", "/public/status-pages/secret-c", &[("x-nami-page-access", &t)], None).await.0 } };
    assert_eq!(read(token.clone()).await, StatusCode::OK);
    common::call(&app, "PATCH", &uri, Some(&admin), Some(serde_json::json!({ "password": "second-password" }))).await;
    assert_eq!(read(token).await, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn unlock_is_limited_to_ten_attempts_per_address_and_page(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    private_page(&pool, "secret-a", Some("correct horse"), &[]).await;
    private_page(&pool, "secret-b", Some("correct horse"), &[]).await;
    for _ in 0..10 { assert_eq!(unlock(&app, "secret-a", "198.51.100.1", "wrong").await.0, StatusCode::UNAUTHORIZED); }
    assert_eq!(unlock(&app, "secret-a", "198.51.100.1", "correct horse").await.0, StatusCode::TOO_MANY_REQUESTS, "even the right password");
    assert_eq!(unlock(&app, "secret-a", "198.51.100.2", "correct horse").await.0, StatusCode::OK, "another address");
    assert_eq!(unlock(&app, "secret-b", "198.51.100.1", "correct horse").await.0, StatusCode::OK, "another page");
    let stored: Vec<String> = sqlx::query_scalar("select bucket from status_page_rate_hits").fetch_all(&pool).await.unwrap();
    assert!(stored.iter().all(|b| !b.contains("198.51.100")), "addresses are stored hashed: {stored:?}");
}

#[sqlx::test]
async fn one_address_cannot_spread_guesses_over_many_pages(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    for i in 0..4 { private_page(&pool, format!("spread-{i}").as_str(), Some("correct horse"), &[]).await; }
    let mut statuses = Vec::new();
    for i in 0..4 { for _ in 0..8 { statuses.push(unlock(&app, format!("spread-{i}").as_str(), "198.51.100.9", "wrong").await.0); } }
    assert_eq!(statuses.iter().filter(|s| **s == StatusCode::UNAUTHORIZED).count(), 30, "unlock-ip caps at 30 per 15 min");
    assert!(statuses[30..].iter().all(|s| *s == StatusCode::TOO_MANY_REQUESTS));
}

#[sqlx::test]
async fn unknown_and_public_pages_unlock_like_a_wrong_password(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    common::seed_status_page(&pool, project, "open-page").await;
    let wrong = (StatusCode::UNAUTHORIZED, serde_json::json!({ "error": "wrong password" }));
    assert_eq!(unlock(&app, "no-such-page", "198.51.100.3", "x").await, wrong);
    assert_eq!(unlock(&app, "open-page", "198.51.100.3", "x").await, wrong);
}

#[sqlx::test]
async fn the_ip_allowlist_grants_access_for_v4_v6_and_ranges(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    private_page(&pool, "office", None, &["10.0.0.0/8", "2001:db8:abcd::/48", "203.0.113.7"]).await;
    let read = |ip: Option<&'static str>| { let app = app.clone(); async move {
        let h: Vec<(&str, &str)> = ip.map(|v| vec![("x-forwarded-for", v)]).unwrap_or_default();
        call_raw(&app, "GET", "/public/status-pages/office", &h, None).await.0 } };
    for ok in ["10.0.0.0", "10.255.255.255", "::ffff:10.9.9.9", "2001:db8:abcd:ffff::1", "203.0.113.7"] {
        assert_eq!(read(Some(ok)).await, StatusCode::OK, "{ok}");
    }
    for no in ["11.0.0.0", "9.255.255.255", "2001:db8:abce::1", "203.0.113.8", "10.0.0.1, 8.8.8.8"] {
        assert_eq!(read(Some(no)).await, StatusCode::NOT_FOUND, "{no}");
    }
}

#[sqlx::test]
async fn a_missing_forwarded_address_grants_no_ip_access(pool: sqlx::PgPool) {
    // The engine's TCP peer is nami-ui (loopback). It must never count as the visitor.
    let app = test_app(pool.clone());
    private_page(&pool, "loopback", None, &["127.0.0.0/8", "::1"]).await;
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/loopback", &[], None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/loopback", &[("x-forwarded-for", "8.8.8.8, 127.0.0.1")], None).await.0,
        StatusCode::NOT_FOUND);
    assert_eq!(call_raw(&app, "GET", "/public/status-pages/loopback", &[("x-forwarded-for", "127.0.0.1")], None).await.0,
        StatusCode::OK, "only nami-ui's forwarded address counts");
}
```

Add both unlock routes to `json_bodies.rs`.

Run: engine command `--lib -- statuspages::access`, then engine command `--test statuspages --test core -- status_public:: migrations:: json_bodies::`. Expected: FAIL.

- [ ] **Step 3: Implement.** `sign`: `format!("v1.{}.{exp}.{}", page_id.simple(), b64url(mac))`. `verify`: split into exactly 4 parts, check `v1` and that the page part equals `page_id.simple()`, recompute the MAC over the parsed `exp` and `verify_slice` it, then check `exp > now`. Hold the subkey in `Zeroizing`.

- [ ] **Step 4: Run** both commands again, the full suite and clippy. Expected: green.

- [ ] **Step 5: Commit**

```bash
git add nami-engine/migrations/NNNN_statuspages_part1.sql nami-engine/src nami-engine/tests
git commit -m "feat(engine): private status pages with password unlock, access tokens and IP allowlist (NAMI-SP-7)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8 (NAMI-SP-8): Automatic reports from bound incidents

**Files:**
- Create: `nami-engine/src/statuspages/automatic.rs`, `nami-engine/tests/statuspages/status_automatic.rs`
- Modify: `nami-engine/src/incidents/lifecycle.rs` (three hook calls; the file is planned in NAMI-INC-1), `nami-engine/src/statuspages/mod.rs`, `nami-engine/tests/statuspages/main.rs`

**Interfaces:**
- Consumes: NAMI-INC-1 `lifecycle::{open, resolve, resolve_source, OpenIncident, IncidentSource}`, Task 5 `statuspages::report::model::{insert, post_update, NewReport}`.
- Produces:

```rust
// statuspages::automatic
pub const OPENED_MESSAGE: &str = "We are investigating a disruption affecting {names}.";
pub const RESOLVED_MESSAGE: &str = "This incident has been resolved.";
/// R9. Monitor/Heartbeat sources only; anything else returns Ok(0). Returns how many reports were created.
pub async fn on_incident_opened(conn: &mut PgConnection, incident_id: Uuid, source: &IncidentSource, at: DateTime<Utc>)
    -> Result<u64, sqlx::Error>;
/// Posts the `resolved` update to every automatic, unresolved report of the incident. Returns how many.
pub async fn on_incident_resolved(conn: &mut PgConnection, incident_id: Uuid, at: DateTime<Utc>) -> Result<u64, sqlx::Error>;
pub fn title_for(names: &[String]) -> String;   // "A, B — service disruption", ≤ 255 chars with "…"
```

Hook placement in `lifecycle.rs`:
- `open`: after the `opened` event, when `open` returns `Some(id)`, call `on_incident_opened`.
- `resolve`: when it returns `true`, call `on_incident_resolved`.
- `resolve_source` and NAMI-INC-9's `resolve_source_with_reason`: when they resolve `Some(id)`, call `on_incident_resolved`. If the merged code routes all three through one internal function, put the single call there.

Both hooks also run `update status_pages set updated_at = now() where id = any($1)` for the pages they touched (R10), so every instance serves the new report on its next request.

`reopen` adds nothing: the page keeps its resolved report, and a new incident gets a new one.

Selection SQL for `on_incident_opened` (Monitor shown; Heartbeat uses `heartbeat_id`):

```sql
select p.id as page_id, p.automatic_reports_notify, c.id as component_id, c.display_name
from status_page_components c
join status_pages p on p.id = c.status_page_id
where c.monitor_id = $1 and p.automatic_reports
order by p.id, c.position, c.display_name
```

Group by page. For each page, `statuspages::report::model::insert` with `title_for(names)`, kind `incident`, status `investigating`, `affected = [(component, MajorOutage)]` for each row, message `OPENED_MESSAGE` with `{names}` replaced, `notify_subscribers = automatic_reports_notify`, `created_by = None`, `incident_id = Some(id)`, `automatic = true`. `on_incident_resolved`: `select id, status_page_id from status_reports r join status_pages p … where incident_id = $1 and automatic and status <> 'resolved'`, then `post_update(.., "resolved", RESOLVED_MESSAGE, p.automatic_reports_notify, None, by_hand = false)`. Task 10 adds `notify::enqueue_update` after both inserts.

- [ ] **Step 1: Write the failing tests** — `tests/statuspages/status_automatic.rs`:

```rust
// Automatic status reports (R9). Requires `docker compose up -d postgres`.
use crate::common;

use chrono::Utc;
use nami_engine::incidents::lifecycle::{self, IncidentSource, OpenIncident};
use uuid::Uuid;

async fn open_for(pool: &sqlx::PgPool, project: Uuid, source: IncidentSource) -> Uuid {
    let mut tx = pool.begin().await.unwrap();
    let id = lifecycle::open(&mut tx, &OpenIncident { project_id: project, source, title: "db-primary-internal is DOWN",
        cause: Some("connection refused to 10.0.0.5"), severity_id: None, metadata: None, started_at: Utc::now() })
        .await.unwrap().unwrap();
    tx.commit().await.unwrap();
    id
}

async fn reports(pool: &sqlx::PgPool, page: Uuid) -> Vec<(String, String, bool, Option<Uuid>)> {
    sqlx::query_as("select title, status, automatic, incident_id from status_reports where status_page_id = $1 order by published_at")
        .bind(page).fetch_all(pool).await.unwrap()
}

#[sqlx::test]
async fn an_incident_on_a_bound_monitor_opens_one_report_per_opted_in_page(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let on = common::seed_status_page(&pool, project, "auto-on").await;
    let off = common::seed_status_page(&pool, project, "auto-off").await;
    sqlx::query("update status_pages set automatic_reports = true where id = $1").bind(on).execute(&pool).await.unwrap();
    let m = common::seed_http_monitor(&pool, project, "db-primary-internal", "https://secret-internal.test/", 1).await;
    let c1 = common::seed_component(&pool, on, "Database", Some(m), None).await;
    let c2 = common::seed_component(&pool, on, "API", Some(m), None).await;
    common::seed_component(&pool, off, "Database", Some(m), None).await;

    let incident = open_for(&pool, project, IncidentSource::Monitor(m)).await;
    let rows = reports(&pool, on).await;
    assert_eq!(rows, vec![("Database, API — service disruption".to_string(), "investigating".into(), true, Some(incident))]);
    assert!(reports(&pool, off).await.is_empty(), "automatic_reports off");
    let impacts: Vec<(Uuid, String)> = sqlx::query_as("select component_id, impact from status_report_components order by component_id")
        .fetch_all(&pool).await.unwrap();
    let mut expected = vec![(c1, "major_outage".to_string()), (c2, "major_outage".to_string())];
    expected.sort();
    assert_eq!(impacts, expected);
    let texts: Vec<String> = sqlx::query_scalar("select title || ' ' || u.message from status_reports r join status_report_updates u on u.report_id = r.id")
        .fetch_all(&pool).await.unwrap();
    for t in &texts {
        for secret in ["db-primary-internal", "secret-internal", "10.0.0.5", "refused", m.to_string().as_str(), incident.to_string().as_str()] {
            assert!(!t.contains(secret), "{secret} in {t}");
        }
    }
    assert_eq!(texts[0], "Database, API — service disruption We are investigating a disruption affecting Database, API.");
}

#[sqlx::test]
async fn recovery_posts_resolved_and_a_hand_resolved_report_is_left_alone(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "auto").await;
    sqlx::query("update status_pages set automatic_reports = true where id = $1").bind(page).execute(&pool).await.unwrap();
    let m = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    let hb = common::seed_heartbeat(&pool, project, "cron").await;
    common::seed_component(&pool, page, "API", Some(m), None).await;
    common::seed_component(&pool, page, "Jobs", None, Some(hb)).await;

    open_for(&pool, project, IncidentSource::Monitor(m)).await;
    let mut tx = pool.begin().await.unwrap();
    lifecycle::resolve_source(&mut tx, &IncidentSource::Monitor(m), Utc::now()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(reports(&pool, page).await[0].1, "resolved");
    let last: String = sqlx::query_scalar("select message from status_report_updates order by created_at desc limit 1").fetch_one(&pool).await.unwrap();
    assert_eq!(last, "This incident has been resolved.");

    // Heartbeat: a member resolves the report by hand first; recovery then posts nothing more.
    open_for(&pool, project, IncidentSource::Heartbeat(hb)).await;
    let report: Uuid = sqlx::query_scalar("select id from status_reports where title like 'Jobs%'").fetch_one(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    nami_engine::statuspages::report::model::post_update(&mut tx, report, "resolved", "Fixed by hand.", true, None, true).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    lifecycle::resolve_source(&mut tx, &IncidentSource::Heartbeat(hb), Utc::now()).await.unwrap();
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from status_report_updates where report_id = $1").bind(report).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2, "opened + the hand-written resolution; automatic handling ended");
}

#[sqlx::test]
async fn notifications_follow_the_page_flag_and_manual_incidents_are_ignored(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "auto").await;
    sqlx::query("update status_pages set automatic_reports = true, automatic_reports_notify = false where id = $1")
        .bind(page).execute(&pool).await.unwrap();
    let m = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    common::seed_component(&pool, page, "API", Some(m), None).await;
    open_for(&pool, project, IncidentSource::Monitor(m)).await;
    let notify: bool = sqlx::query_scalar("select notify_subscribers from status_report_updates").fetch_one(&pool).await.unwrap();
    assert!(!notify);
    let user = common::seed_user(&pool, &common::unique_email("u"), "member").await;
    open_for(&pool, project, IncidentSource::Manual { created_by: user }).await;
    assert_eq!(reports(&pool, page).await.len(), 1);
}

#[test]
fn titles_are_capped() {
    let names: Vec<String> = (0..40).map(|i| format!("Component number {i}")).collect();
    let t = nami_engine::statuspages::automatic::title_for(&names);
    assert!(t.chars().count() <= 255 && t.ends_with('…'), "{t}");
}
```

Run: engine command `--test statuspages -- status_automatic::`. Expected: FAIL.

- [ ] **Step 2: Implement** `automatic.rs` and the hooks. The existing `tests/incidents/incident_lifecycle.rs` (planned in NAMI-INC-1), `tests/uptime/uptime_worker.rs` and `tests/uptime/heartbeats.rs` tests must stay green unchanged (no page → no work: one indexed query per open/resolve).

In Part 1 only log alerts call `resolve_source_with_reason`, and a log alert cannot be bound to a component, so no test drives that path. The task review checks that the call site is there.

- [ ] **Step 3: Run** the engine command with `--test statuspages --test incidents --test uptime -- status_automatic:: incident_lifecycle:: uptime_worker:: heartbeats::` (`incident_lifecycle` is planned in NAMI-INC-1), the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests/statuspages/status_automatic.rs
git commit -m "feat(engine): automatic status reports from bound monitor and heartbeat incidents (NAMI-SP-8)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9 (NAMI-SP-9): Email subscribers — double opt-in through the delivery queue

**Files:**
- Create: `nami-engine/migrations/NNNN_incidents_statuspages_part1.sql` (numbered after the statuspages file), `nami-engine/src/statuspages/tokens.rs`, `nami-engine/src/statuspages/deliver.rs`, `nami-engine/src/statuspages/subscriber/{mod,model,routes}.rs`, `nami-engine/tests/statuspages/status_subscribers.rs` (plus its `mod` line in `tests/statuspages/main.rs`)
- Modify: `nami-engine/migrations/NNNN_statuspages_part1.sql` (append the subscriber tables), `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/incidents/deliveries.rs` (claim order, page bucket, dispatch), `nami-engine/src/incidents/worker.rs` (hourly prune) (both planned in NAMI-INC-5), `nami-engine/src/statuspages/public/routes.rs` (subscribe), `nami-engine/src/core/router.rs` (token router + governor), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`

**Interfaces:**
- Consumes: NAMI-INC-5 queue and drain, Task 6 page resolution, Task 7 `access::granted`, `rate_limit::{hit, client_key}`, `core::email::Mailer::send`, `json_error` of `statuspages::public::routes`.
- Produces:

```rust
// statuspages::tokens
#[derive(Debug, Clone, Copy, PartialEq, Eq)] #[repr(u8)]
pub enum Purpose { Confirm = 1, Manage = 2, Unsubscribe = 3 }
pub fn sign(cipher: &Cipher, subscriber_id: Uuid, purpose: Purpose, nonce: &[u8; 16]) -> String;   // R14
/// MAC-checked decode: (subscriber id, nonce). The caller compares the nonce with the row (constant time).
pub fn open(cipher: &Cipher, token: &str, purpose: Purpose) -> Option<(Uuid, [u8; 16])>;

// statuspages::subscriber::model
pub const MAX_CONFIRMED: i64 = 10_000;
pub const MAX_PENDING: i64 = 500;
pub const CONFIRM_TTL: TimeDelta = TimeDelta::hours(48);
pub const MAX_SENDS_PER_DAY: i32 = 3;
pub fn validate_email(raw: &str) -> Result<String, String>;   // R15
pub enum SubscribeOutcome { Queued, Suppressed, Full }        // Full → 429
/// Upsert + enqueue in one transaction (R15).
pub async fn subscribe(conn: &mut PgConnection, page_id: Uuid, email: &str, components: Option<&[Uuid]>)
    -> Result<SubscribeOutcome, sqlx::Error>;
pub enum Confirm { Confirmed { page_id: Uuid }, Expired, Invalid }
pub async fn confirm(conn: &mut PgConnection, cipher: &Cipher, token: &str) -> Result<Confirm, sqlx::Error>;

// statuspages::deliver
pub const STATUS_PAGE_PER_MINUTE: i64 = 600;
/// `Failed` carries INC-5's struct: a Mailer error is `SendError { message: "email not delivered".into(),
/// retry: Retry::After(None) }`, recorded with the Incidents plan's retry rules (its Task 5 Step 3 (2f)).
pub enum Outcome { Sent, Cancelled, Failed(crate::incidents::send::SendError) }
pub struct StatusPageRow { pub id: Uuid, pub event: String, pub round: String, pub status_page_id: Uuid,
    pub subscriber_id: Uuid, pub status_report_update_id: Option<Uuid>, pub attempts: i32 }
/// Builds and sends one status-page email. Subscriber gone → Cancelled. Mailer error → Failed (above).
pub async fn send(state: &AppState, row: &StatusPageRow) -> Outcome;
```

Routes:
- `POST /public/status-pages/{slug}/subscribe` and `POST /public/status-domains/{host}/subscribe`, body `{email, components: [uuid] | null}`. Order:
  1. Page plus access, from the fresh row (no access → the Task 6 404).
  2. `validate_email` → 400 with its message. A component not on the page → `400 component not found on this page`.
  3. `hit("subscribe-ip:{client_key}", 5, 1 h)` and `hit("subscribe-page:{page}", 100, 1 h)` → `json_error(429, "too many subscriptions, try again later")`.
  4. `subscribe`. `Queued`/`Suppressed` → `202 {"ok": true}`, then `let s = state.clone(); tokio::spawn(async move { incidents::worker::kick(&s).await });` (`kick` borrows). `Full` → `json_error(429, "this page is not accepting new subscriptions right now")`.
- `POST /public/status-subscriptions/confirm {token}` → `Confirmed` → `200 {"page": {"title", "url"}}`. `Expired` (MAC fine but nonce cleared, rotated or past `confirm_expires_at`) → `json_error(410, "this link has expired or was already used; subscribe again")` (no dependency on INC-11's `AppError::Gone`). `Invalid` (MAC fails, or the subscriber row is gone) → `json_error(404, "not found")`.
- The token router has its own governor (`SmartIpKeyExtractor`, burst 20, `per_second(3)`).

- [ ] **Step 1: Write the migrations**

Append to `nami-engine/migrations/NNNN_statuspages_part1.sql`:

```sql
-- Email subscribers of status pages (NAMI-SP-9). No token or token hash is
-- stored: links are HMAC-signed over (id, purpose, nonce) (R14).
create table status_page_subscribers (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null references status_pages(id) on delete cascade,
    email text not null check (char_length(email) between 3 and 254 and email = lower(email)),
    -- R15: an explicit flag, so a subset whose components were all deleted
    -- does not silently widen to every component.
    all_components boolean not null default true,
    confirmed_at timestamptz,
    confirm_nonce bytea check (octet_length(confirm_nonce) = 16),
    confirm_expires_at timestamptz,
    token_nonce bytea not null check (octet_length(token_nonce) = 16),
    confirmation_sends int not null default 0,
    confirmation_window_started_at timestamptz,
    confirmation_sends_total int not null default 0,
    created_at timestamptz not null default now(),
    unique (status_page_id, email),
    check (confirmed_at is null or confirm_nonce is null)
);
create index status_page_subscribers_confirmed_idx on status_page_subscribers (status_page_id) where confirmed_at is not null;
create index status_page_subscribers_pending_idx on status_page_subscribers (created_at) where confirmed_at is null;

-- The components a subset subscriber follows (all_components = false).
create table status_page_subscriber_components (
    subscriber_id uuid not null references status_page_subscribers(id) on delete cascade,
    component_id uuid not null references status_page_components(id) on delete cascade,
    primary key (subscriber_id, component_id)
);
create index status_page_subscriber_components_component_idx on status_page_subscriber_components (component_id);
```

`nami-engine/migrations/NNNN_incidents_statuspages_part1.sql` (R17; `notification_deliveries` is an incidents table, so an incidents file, numbered after the statuspages file it references):

```sql
-- Status page mail rides the incident delivery queue (NAMI-SP-9, R17).
-- Incident rows keep every rule they had; status-page rows are priority 1.
alter table notification_deliveries
    alter column incident_id drop not null,
    add column status_page_id uuid references status_pages(id) on delete cascade,
    add column subscriber_id uuid references status_page_subscribers(id) on delete cascade,
    add column status_report_update_id uuid references status_report_updates(id) on delete cascade,
    add column priority smallint not null default 0 check (priority in (0, 1));

-- The unnamed checks on `event` and on (user_id, channel_id) that Incidents Part 1's phase file
-- creates (planned in NAMI-INC-5) are replaced by one rule per row kind.
-- Matched by definition, not by the generated names (`notification_deliveries_event_check`,
-- `notification_deliveries_check`/`_check1` depend on declaration order). Exactly two must go:
-- anything else means that file drifted from its plan, and the migration stops instead of guessing.
do $$
declare c record; dropped int := 0;
begin
    for c in select conname from pg_constraint
             where conrelid = 'notification_deliveries'::regclass and contype = 'c'
               and (pg_get_constraintdef(oid) like '%event%triggered%'
                 or pg_get_constraintdef(oid) like '%user_id IS NOT NULL%channel_id IS NOT NULL%')
    loop
        execute format('alter table notification_deliveries drop constraint %I', c.conname);
        dropped := dropped + 1;
    end loop;
    if dropped <> 2 then
        raise exception 'expected to replace 2 checks on notification_deliveries, found %', dropped;
    end if;
end $$;

alter table notification_deliveries add constraint notification_deliveries_row_kind check (
    (incident_id is not null and status_page_id is null and subscriber_id is null
        and status_report_update_id is null and priority = 0
        and event in ('triggered', 'resolved') and (user_id is not null or channel_id is not null))
    or (incident_id is null and status_page_id is not null and subscriber_id is not null
        and user_id is null and channel_id is null and priority = 1
        and event in ('sp_confirm', 'sp_subscribed', 'sp_update')
        and ((event = 'sp_update') = (status_report_update_id is not null)))
);

drop index notification_deliveries_due_idx;
create index notification_deliveries_due_idx on notification_deliveries (priority, next_attempt_at)
    where status in ('pending', 'sending');
create unique index notification_deliveries_status_page_round on notification_deliveries (status_page_id, round, target_key)
    where status_page_id is not null;
create index notification_deliveries_page_sent_idx on notification_deliveries (status_page_id, sent_at)
    where status_page_id is not null and status = 'sent';
```

(Part 2 relaxes `subscriber_id is not null` to "subscriber or webhook subscriber".)

Migration tests: `a_delivery_row_is_either_an_incident_row_or_a_status_page_row` (an incident row with `status_page_id` set, and a status-page row with `user_id` set, both fail with `23514`). Add both tables to `all_tables_exist`.

- [ ] **Step 2: Change the claim** (NAMI-INC-5 Step 3 (c)) to this statement. `$4` is `STATUS_PAGE_PER_MINUTE`.

```sql
with candidates as (
    select id, channel_id, user_id, status_page_id, priority, next_attempt_at,
           case when channel_id is not null then 'c:' || channel_id::text
                when status_page_id is not null then 'p:' || status_page_id::text
                else 'u:' || user_id::text end as bucket
    from notification_deliveries
    where ((status = 'pending' and next_attempt_at <= now())
        or (status = 'sending' and locked_until < now()))
      and attempts < 5
    order by priority, next_attempt_at, id
    limit $1
    for update skip locked
), recent as (
    select case when channel_id is not null then 'c:' || channel_id::text
                when status_page_id is not null then 'p:' || status_page_id::text
                else 'u:' || user_id::text end as bucket,
           count(*) as used
    from notification_deliveries
    where ((status = 'sent' and sent_at > now() - interval '1 minute')
        or (status = 'sending' and locked_until >= now()))
      and (channel_id in (select channel_id from candidates where channel_id is not null)
        or status_page_id in (select status_page_id from candidates where status_page_id is not null)
        or (channel_id is null and status_page_id is null
            and user_id in (select user_id from candidates where channel_id is null and status_page_id is null)))
    group by 1
), ranked as (
    select c.id,
           coalesce(r.used, 0) + row_number() over (partition by c.bucket order by c.priority, c.next_attempt_at, c.id) as nth,
           case when c.bucket like 'c:%' then $2::bigint when c.bucket like 'p:%' then $4::bigint else $3::bigint end as cap
    from candidates c left join recent r on r.bucket = c.bucket
), deferred as (
    update notification_deliveries d
    set status = 'pending', locked_until = null, next_attempt_at = now() + interval '60 seconds'
    from ranked r where d.id = r.id and r.nth > r.cap
    returning d.id
)
update notification_deliveries d
set status = 'sending', locked_until = now() + interval '60 seconds', attempts = d.attempts + 1
from ranked r
where d.id = r.id and r.nth <= r.cap
returning d.id, d.incident_id, d.event, d.round, d.user_id, d.channel_id, d.attempts,
          d.status_page_id, d.subscriber_id, d.status_report_update_id
```

Dispatch per claimed row: `incident_id` set → the existing path, unchanged. Otherwise → `deliver::send`. Its `Outcome` is recorded with the same status/backoff rules (`Sent` → `sent`, `Cancelled` → `cancelled`, `Failed` → the Incidents plan's retry rules (its R12; Task 5 Step 3 (2f))), without any `incident_events` row. The incidents worker's loop also runs, at most once per hour per process, `delete from status_page_subscribers where confirmed_at is null and created_at < now() - interval '48 hours'` and `delete from status_page_rate_hits where hit_at < now() - interval '1 day'`.

- [ ] **Step 3: Write the failing tests** — `tests/statuspages/status_subscribers.rs`:

```rust
// Status page email subscribers. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{call_raw, run_incident_ticks, seed_project, settled_mail, stored_mail_for, unique_email};
use nami_engine::core::state::AppState;
use serde_json::{json, Value};
use uuid::Uuid;

async fn subscribe(app: &axum::Router, slug: &str, ip: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, bytes) = call_raw(app, "POST", &format!("/public/status-pages/{slug}/subscribe"),
        &[("x-forwarded-for", ip)], Some(body)).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn link_token(text: &str, marker: &str) -> String {
    let start = text.find(marker).unwrap_or_else(|| panic!("{marker} not in {text}")) + marker.len();
    text[start..].split(|c: char| c.is_whitespace()).next().unwrap().to_string()
}

#[sqlx::test]
async fn double_opt_in_confirms_once_by_post(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    common::seed_status_page(&pool, project, "acme").await;
    let email = unique_email("sub");
    assert_eq!(subscribe(&app, "acme", "198.51.100.10", json!({ "email": email.to_uppercase(), "components": null })).await,
        (StatusCode::ACCEPTED, json!({ "ok": true })));
    run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 1).await;
    assert_eq!(mail[0].0, "Confirm your subscription to Acme");
    let token = link_token(&mail[0].1, "/subscribe/confirm?token=");
    let confirmed: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select confirmed_at from status_page_subscribers where email = $1")
        .bind(&email).fetch_one(&pool).await.unwrap();
    assert!(confirmed.is_none(), "nothing is confirmed by sending or opening the link");
    let confirm = |t: String| { let app = app.clone(); async move {
        call_raw(&app, "POST", "/public/status-subscriptions/confirm", &[], Some(json!({ "token": t }))).await.0 } };
    assert_eq!(confirm(token.clone()).await, StatusCode::OK);
    assert_eq!(confirm(token.clone()).await, StatusCode::GONE, "single use");
    let mut tampered = token.clone().into_bytes();
    let last = tampered.len() - 2; tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
    assert_eq!(confirm(String::from_utf8(tampered).unwrap()).await, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn subscribe_answers_the_same_for_new_pending_and_confirmed_addresses(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let (fresh, pending, confirmed) = (unique_email("new"), unique_email("pending"), unique_email("confirmed"));
    subscribe(&app, "acme", "198.51.100.20", json!({ "email": pending, "components": null })).await;
    sqlx::query("insert into status_page_subscribers (status_page_id, email, confirmed_at, token_nonce) values ($1, $2, now(), decode(md5(random()::text), 'hex'))")
        .bind(page).bind(&confirmed).execute(&pool).await.unwrap();
    let mut answers = Vec::new();
    for (i, email) in [&fresh, &pending, &confirmed].into_iter().enumerate() {
        answers.push(subscribe(&app, "acme", &format!("198.51.100.{}", 30 + i), json!({ "email": email, "components": null })).await);
    }
    assert!(answers.iter().all(|a| *a == (StatusCode::ACCEPTED, json!({ "ok": true }))), "{answers:?}");
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&confirmed, 1).await[0].0, "You are already subscribed to Acme");
    assert_eq!(settled_mail(&pending, 2).await.len(), 2, "pending: re-sent, newest link only works");
}

#[sqlx::test]
async fn resends_are_capped_at_three_per_day(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    common::seed_status_page(&pool, project, "acme").await;
    let email = unique_email("cap");
    for i in 0..5 {
        assert_eq!(subscribe(&app, "acme", &format!("192.0.2.{}", 40 + i), json!({ "email": email, "components": null })).await.0,
            StatusCode::ACCEPTED);
    }
    run_incident_ticks(&state).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(stored_mail_for(&email).await.len(), 3);
}

#[sqlx::test]
async fn subscribe_is_rate_limited_per_address_and_capped_per_page(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    for _ in 0..5 {
        assert_eq!(subscribe(&app, "acme", "203.0.113.50", json!({ "email": unique_email("rl"), "components": null })).await.0, StatusCode::ACCEPTED);
    }
    assert_eq!(subscribe(&app, "acme", "203.0.113.50", json!({ "email": unique_email("rl"), "components": null })).await.0,
        StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("insert into status_page_subscribers (status_page_id, email, token_nonce)
                 select $1, 'p' || g || '@pending.test', decode(md5(random()::text), 'hex') from generate_series(1, 500) g")
        .bind(page).execute(&pool).await.unwrap();
    let (status, body) = subscribe(&app, "acme", "203.0.113.51", json!({ "email": unique_email("full"), "components": null })).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("this page is not accepting new subscriptions right now")));
    // A known (pending) address gets the very same answer: the cap is no enumeration oracle (R15).
    let (status, body2) = subscribe(&app, "acme", "203.0.113.52", json!({ "email": "p1@pending.test", "components": null })).await;
    assert_eq!((status, body2), (StatusCode::TOO_MANY_REQUESTS, body));
}

#[sqlx::test]
async fn components_must_be_on_the_page_and_private_pages_need_access(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let other = common::seed_status_page(&pool, project, "other").await;
    let m = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    let foreign = common::seed_component(&pool, other, "API", Some(m), None).await;
    let (status, _) = subscribe(&app, "acme", "203.0.113.60", json!({ "email": unique_email("c"), "components": [foreign] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for bad in ["not-an-email", "a@b", "x@y.test\r\nBcc: z@w.test", format!("{}@x.test", "a".repeat(250)).as_str()] {
        assert_eq!(subscribe(&app, "acme", "203.0.113.61", json!({ "email": bad, "components": null })).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    sqlx::query("update status_pages set visibility = 'private', password_hash = 'x' where id = $1").bind(page).execute(&pool).await.unwrap();
    assert_eq!(subscribe(&app, "acme", "203.0.113.62", json!({ "email": unique_email("p"), "components": null })).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn incident_deliveries_are_claimed_before_status_page_mail(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    // 100 older status-page rows ahead of one incident row; DRAIN_BATCH is 50.
    sqlx::query("with s as (insert into status_page_subscribers (status_page_id, email, token_nonce)
                   select $1, 'q' || g || '@queue.test', decode(md5(random()::text), 'hex') from generate_series(1, 100) g returning id)
                 insert into notification_deliveries (status_page_id, subscriber_id, event, round, target_key, priority, next_attempt_at)
                 select $1, id, 'sp_confirm', 'confirm-1', 'sub:' || id, 1, now() - interval '1 hour' from s")
        .bind(page).execute(&pool).await.unwrap();
    let (user, email, _) = common::user_with_role(&app, &pool, project, Some("member")).await;
    let incident = common::open_test_incident(&pool, project, user).await;
    let mut conn = pool.acquire().await.unwrap();
    nami_engine::incidents::deliveries::enqueue(&mut conn, &[nami_engine::incidents::deliveries::NewDelivery {
        incident_id: incident, event: nami_engine::incidents::deliveries::DeliveryEvent::Triggered, round: "p0r0s0".into(),
        target: nami_engine::incidents::deliveries::Target::Email { user_id: user } }]).await.unwrap();
    run_incident_ticks(&state).await;
    let status: String = sqlx::query_scalar("select status from notification_deliveries where incident_id = $1 and round = 'p0r0s0'")
        .bind(incident).fetch_one(&pool).await.unwrap();
    assert_eq!(status, "sent", "the on-call email is not stuck behind subscriber mail");
    assert!(!settled_mail(&email, 1).await.is_empty());
}

#[sqlx::test]
async fn a_page_never_sends_more_than_its_per_minute_cap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    sqlx::query("with s as (insert into status_page_subscribers (status_page_id, email, token_nonce, confirmed_at)
                   select $1, 'c' || g || '@cap.test', decode(md5(random()::text), 'hex'), now() from generate_series(1, 601) g returning id)
                 insert into notification_deliveries (status_page_id, subscriber_id, event, round, target_key, priority, status, sent_at, attempts)
                 select $1, id, 'sp_subscribed', 'confirm-1', 'sub:' || id, 1, 'sent', now(), 1 from s")
        .bind(page).execute(&pool).await.unwrap();
    // Make one of them pending again: the 601st this minute.
    sqlx::query("update notification_deliveries set status = 'pending', sent_at = null, attempts = 0
                 where id = (select id from notification_deliveries where status_page_id = $1 limit 1)")
        .bind(page).execute(&pool).await.unwrap();
    run_incident_ticks(&state).await;
    let (pending, attempts): (i64, i64) = sqlx::query_as("select count(*), coalesce(sum(attempts), 0) from notification_deliveries where status = 'pending'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!((pending, attempts), (1, 0), "deferred without spending an attempt");
}
```

`tokens.rs` `mod tests`: a token of each purpose opens only under its own purpose; flipping any byte of the decoded token makes `open` return `None`; a token from another key is refused; inputs of wrong length, non-base64url characters and the empty string give `None`.

Add the subscribe and confirm routes to `json_bodies.rs`.

Run: engine command `--lib -- statuspages::tokens`, then engine command `--test statuspages --test core --test incidents -- status_subscribers:: migrations:: json_bodies:: notification_deliveries::`. Expected: FAIL (`notification_deliveries` must stay green after the change).

- [ ] **Step 4: Implement.** `subscribe` (one transaction, `pg_advisory_xact_lock(hashtext('subscribers:' || page_id))`):
  0. Count confirmed and pending rows of the page. Either at its cap (10,000 / 500) → `Full`, **before** looking at the address (R15: the answer must not depend on whether the address is known).
  1. `select … for update` the `(page, email)` row.
  2. None → insert with `confirm_nonce = random`, `confirm_expires_at = now() + 48 h`, `token_nonce = random`, the component rows (`all_components = components.is_none()`), and enqueue `sp_confirm`.
  3. Pending → rotate `confirm_nonce`, extend expiry, replace components, enqueue `sp_confirm` if under the send cap.
  4. Confirmed → enqueue `sp_subscribed` if under the send cap (components unchanged).
  5. The send cap: when `confirmation_window_started_at` is null or older than 24 h, reset `confirmation_sends = 0` and the window start to now. Then `confirmation_sends < 3` → increment it and `confirmation_sends_total`, and enqueue with `round = 'confirm-' || confirmation_sends_total`. Otherwise `Suppressed`.

  Every enqueue inserts `priority = 1`, `target_key = 'sub:' || id`, `on conflict do nothing`. `deliver::send` for `sp_confirm` re-reads the row and signs `Purpose::Confirm` with the **current** `confirm_nonce`. A nonce that is null (already confirmed) → `Cancelled`. The link is `{page_url}/subscribe/confirm?token=…`. `sp_subscribed` links `{page_url}/subscribe/manage?token=…` (`Purpose::Manage`).

- [ ] **Step 5: Run** both commands again, then the full suite (all incident tests included) and clippy. Expected: green.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/migrations/NNNN_statuspages_part1.sql nami-engine/migrations/NNNN_incidents_statuspages_part1.sql \
  nami-engine/src nami-engine/tests
git commit -m "feat(engine): status page email subscribers with double opt-in via the delivery queue (NAMI-SP-9)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10 (NAMI-SP-10): Update notifications, manage/unsubscribe, one-click, admin subscriber list

**Files:**
- Create: `nami-engine/src/statuspages/notify.rs`
- Modify: `nami-engine/src/core/email.rs` (`send_with_headers`), `nami-engine/src/core/config.rs` (`public_engine_url`), `nami-engine/.env.example`, `nami-engine/src/statuspages/deliver.rs` (`sp_update`), `nami-engine/src/statuspages/automatic.rs` and `nami-engine/src/statuspages/report/routes.rs` (call `notify::enqueue_update`, then `kick` after commit in the routes), `nami-engine/src/statuspages/subscriber/routes.rs`, `nami-engine/src/core/router.rs` (one-click on the public listener), `nami-engine/tests/statuspages/status_subscribers.rs`, `nami-engine/tests/core/json_bodies.rs`

**Interfaces:**
- Consumes: Task 5 updates, Task 8 automatic reports, Task 9 tokens, rows and dispatch.
- Produces:

```rust
// statuspages::notify
/// Queues one `sp_update` row per confirmed subscriber the update reaches (R17's SQL below). Returns the row count.
pub async fn enqueue_update(conn: &mut PgConnection, update_id: Uuid) -> Result<u64, sqlx::Error>;
// core::email::Mailer
/// `send` plus extra headers; a header value with CR or LF is refused (Validation) before anything is sent.
pub async fn send_with_headers(&self, to: &str, subject: &str, body: String, headers: &[(&'static str, String)]) -> Result<(), AppError>;
// Config
pub public_engine_url: String,   // env PUBLIC_ENGINE_URL, default "http://localhost:8081"; absolute http(s), no userinfo/query/fragment
```

`enqueue_update` SQL:

```sql
insert into notification_deliveries (status_page_id, subscriber_id, status_report_update_id, event, round, target_key, priority)
select s.status_page_id, s.id, u.id, 'sp_update', replace(u.id::text, '-', ''), 'sub:' || s.id::text, 1
from status_report_updates u
join status_reports r on r.id = u.report_id
join status_page_subscribers s on s.status_page_id = r.status_page_id and s.confirmed_at is not null
where u.id = $1 and u.notify_subscribers
  and (s.all_components
       or not exists (select 1 from status_report_components rc where rc.report_id = r.id)
       or exists (select 1 from status_report_components rc
                  join status_page_subscriber_components sc on sc.component_id = rc.component_id and sc.subscriber_id = s.id
                  where rc.report_id = r.id))
on conflict (status_page_id, round, target_key) where status_page_id is not null do nothing
```

Routes:
- `POST /public/status-subscriptions/inspect {token}`:
  - a Manage token → `200 {"purpose": "manage", "page": {"title", "url"}, "email": "j***@example.com", "all_components": bool, "components": [{"id", "name", "section", "selected"}]}`;
  - an Unsubscribe token → `200 {"purpose": "unsubscribe", "page": {…}, "email": "j***@…"}`;
  - anything else → 404. The masked address is the first character, `***`, then `@domain`.
- `POST /public/status-subscriptions/manage {token, components: [uuid] | null}` (Manage only) → `200 {"ok": true}`. A component not on the page → 400.
- `POST /public/status-subscriptions/unsubscribe {token}` (Manage or Unsubscribe) → deletes the subscriber (its queued rows cascade) → `200 {"ok": true}`. Idempotent (R16): a MAC-valid token whose subscriber is already gone also answers 200. A token whose MAC fails → 404.
- Public listener: `POST /api/v1/status-subscriptions/one-click/{token}` (RFC 8058). It accepts any body up to 1 KiB of any content type (`DefaultBodyLimit::max(1024)`; the public router has no JSON guard), verifies an Unsubscribe token's MAC **before** touching the database, deletes the subscriber if it still exists, and answers `200 unsubscribed` (text/plain) either way (idempotent; mail providers retry). A token whose MAC fails or whose purpose is not Unsubscribe → 404. CSRF is not a concern here (no cookie; the token is the only credential). It sits behind a governor keyed by the TCP peer (R13: `PeerIpKeyExtractor`, burst 100, `per_millisecond(20)`). Production serves the public listener with `ConnectInfo` (`serve.rs`); tests wrap `common::public_app(state)` in `nami_engine::core::router::with_fallback_connect_info`, or the peer extractor fails.
- Admin: `GET /projects/{id}/status-pages/{page_id}/subscribers` (admin) → `[{id, email, confirmed, all_components, components: [uuid], created_at}]`. `DELETE …/subscribers/{subscriber_id}` (admin) → 204. A member or viewer → 403.

`deliver::send` for `sp_update`:
- It loads the update, report, page and subscriber. If the subscriber is unconfirmed or missing → `Cancelled`.
- The subject and body follow R16.
- It sends with `send_with_headers(.., &[("List-Unsubscribe", format!("<{public_engine_url}/api/v1/status-subscriptions/one-click/{token}>")), ("List-Unsubscribe-Post", "List-Unsubscribe=One-Click".into())])`, where `token` is `sign(Unsubscribe, token_nonce)`.

- [ ] **Step 1: Write the failing tests** (append to `tests/statuspages/status_subscribers.rs`)

```rust
async fn confirmed(pool: &sqlx::PgPool, page: Uuid, email: &str, components: Option<&[Uuid]>) -> Uuid {
    let id: Uuid = sqlx::query_scalar("insert into status_page_subscribers (status_page_id, email, confirmed_at, token_nonce, all_components)
        values ($1, $2, now(), decode(md5(random()::text), 'hex'), $3) returning id")
        .bind(page).bind(email).bind(components.is_none()).fetch_one(pool).await.unwrap();
    for c in components.unwrap_or_default() {
        sqlx::query("insert into status_page_subscriber_components values ($1, $2)").bind(id).bind(c).execute(pool).await.unwrap();
    }
    id
}

#[sqlx::test]
async fn an_update_reaches_confirmed_subscribers_of_affected_components_only(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = common::user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let m = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    let (c1, c2) = (common::seed_component(&pool, page, "API", Some(m), None).await, common::seed_component(&pool, page, "Web", Some(m), None).await);
    let (all, sub1, sub2, pending) = (unique_email("all"), unique_email("c1"), unique_email("c2"), unique_email("pending"));
    confirmed(&pool, page, &all, None).await;
    confirmed(&pool, page, &sub1, Some(&[c1])).await;
    confirmed(&pool, page, &sub2, Some(&[c2])).await;
    sqlx::query("insert into status_page_subscribers (status_page_id, email, token_nonce) values ($1, $2, decode(md5(random()::text), 'hex'))")
        .bind(page).bind(&pending).execute(&pool).await.unwrap();
    let base = format!("/projects/{project}/status-pages/{page}/reports");
    common::call(&app, "POST", &base, Some(&member), Some(json!({ "title": "API down", "kind": "incident",
        "affected": [{ "component_id": c1, "impact": "major_outage" }], "message": "Looking." }))).await;
    common::call(&app, "POST", &base, Some(&member), Some(json!({ "title": "Quiet", "kind": "incident",
        "affected": [], "message": "x", "notify_subscribers": false }))).await;
    common::call(&app, "POST", &base, Some(&member), Some(json!({ "title": "Everything", "kind": "incident",
        "affected": [], "message": "x" }))).await;
    run_incident_ticks(&state).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let subjects = |m: Vec<common::StoredMail>| { let mut s: Vec<String> = m.into_iter().map(|x| x.subject).collect(); s.sort(); s };
    assert_eq!(subjects(stored_mail_for(&all).await), ["[Acme] API down", "[Acme] Everything"]);
    assert_eq!(subjects(stored_mail_for(&sub1).await), ["[Acme] API down", "[Acme] Everything"]);
    assert_eq!(subjects(stored_mail_for(&sub2).await), ["[Acme] Everything"]);
    assert!(stored_mail_for(&pending).await.is_empty());
}

#[sqlx::test]
async fn emails_carry_one_click_unsubscribe_and_single_purpose_links(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    // The one-click governor keys on the TCP peer: oneshot requests carry no ConnectInfo without this.
    let public = nami_engine::core::router::with_fallback_connect_info(common::public_app(state.clone()));
    let project = seed_project(&pool).await;
    let (_, _, member) = common::user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let email = unique_email("oneclick");
    let subscriber = confirmed(&pool, page, &email, None).await;
    common::call(&app, "POST", &format!("/projects/{project}/status-pages/{page}/reports"), Some(&member),
        Some(json!({ "title": "<script>alert(1)</script> Bcc: x@evil.test", "kind": "incident", "affected": [], "message": "x" }))).await;
    run_incident_ticks(&state).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let mail = stored_mail_for(&email).await.remove(0);
    assert!(mail.bcc.is_empty(), "no extra recipient");
    assert_eq!(mail.subject, "[Acme] <script>alert(1)</script> Bcc: x@evil.test", "plain text, one line");
    // lettre may fold a long header line: read header values unfolded (RFC 5322 §2.2.3).
    let header = |name: &str| -> Option<String> {
        let mut lines = mail.raw.split("\n\n").next().unwrap().lines().peekable();
        while let Some(line) = lines.next() {
            if let Some(rest) = line.strip_prefix(&format!("{name}:")) {
                let mut value = rest.trim().to_string();
                while lines.peek().is_some_and(|l| l.starts_with([' ', '\t'])) {
                    value.push_str(lines.next().unwrap().trim());
                }
                return Some(value);
            }
        }
        None
    };
    let list_unsubscribe = header("List-Unsubscribe").unwrap_or_else(|| panic!("{}", mail.raw));
    assert!(list_unsubscribe.starts_with("<http://localhost:8081/api/v1/status-subscriptions/one-click/") && list_unsubscribe.ends_with('>'),
        "{list_unsubscribe}");
    assert_eq!(header("List-Unsubscribe-Post").as_deref(), Some("List-Unsubscribe=One-Click"));
    let manage = link_token(&mail.text, "/subscribe/manage?token=");
    let unsubscribe = link_token(&mail.text, "/unsubscribe?token=");
    let one_click = |t: &str| { let public = public.clone(); let uri = format!("/api/v1/status-subscriptions/one-click/{t}");
        async move { call_raw(&public, "POST", &uri, &[("content-type", "application/x-www-form-urlencoded")], None).await.0 } };
    assert_eq!(one_click(&manage).await, StatusCode::NOT_FOUND, "a manage token is not an unsubscribe token");
    let (status, _, body) = call_raw(&app, "POST", "/public/status-subscriptions/manage", &[], Some(json!({ "token": unsubscribe, "components": null }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unsubscribe token cannot manage: {}", String::from_utf8_lossy(&body));
    let (status, _, body) = call_raw(&app, "POST", "/public/status-subscriptions/inspect", &[], Some(json!({ "token": manage }))).await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, body["purpose"].as_str()), (StatusCode::OK, Some("manage")));
    assert!(!body["email"].as_str().unwrap().contains(&email[1..email.find('@').unwrap()]), "masked");
    assert_eq!(one_click(&unsubscribe).await, StatusCode::OK);
    let left: i64 = sqlx::query_scalar("select count(*) from status_page_subscribers where id = $1").bind(subscriber).fetch_one(&pool).await.unwrap();
    assert_eq!(left, 0);
    assert_eq!(one_click(&unsubscribe).await, StatusCode::OK, "idempotent: a provider's retry is still a success");
    let mut forged = unsubscribe.clone().into_bytes();
    let i = forged.len() - 2; forged[i] = if forged[i] == b'A' { b'B' } else { b'A' };   // inside the MAC, as in the confirm test
    assert_eq!(one_click(&String::from_utf8(forged).unwrap()).await, StatusCode::NOT_FOUND, "a token whose MAC fails");
    // The manage link of a deleted subscriber is dead too (ruling 1).
    let (status, _, _) = call_raw(&app, "POST", "/public/status-subscriptions/inspect", &[], Some(json!({ "token": manage }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn automatic_report_emails_carry_component_names_only(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    sqlx::query("update status_pages set automatic_reports = true where id = $1").bind(page).execute(&pool).await.unwrap();
    let m = common::seed_http_monitor(&pool, project, "db-primary-internal", "https://secret-internal.test:8443/h?k=hunter2", 1).await;
    common::seed_component(&pool, page, "Database", Some(m), None).await;
    let email = unique_email("auto");
    confirmed(&pool, page, &email, None).await;
    let mut tx = pool.begin().await.unwrap();
    nami_engine::incidents::lifecycle::open(&mut tx, &nami_engine::incidents::lifecycle::OpenIncident {
        project_id: project, source: nami_engine::incidents::lifecycle::IncidentSource::Monitor(m),
        title: "db-primary-internal is DOWN", cause: Some("connection refused to 10.0.0.5"), severity_id: None,
        metadata: None, started_at: chrono::Utc::now() }).await.unwrap().unwrap();
    tx.commit().await.unwrap();
    run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 1).await;
    let text = format!("{} {}", mail[0].0, mail[0].1);
    assert!(text.contains("Database"), "{text}");
    for secret in ["db-primary-internal", "secret-internal", "hunter2", "8443", "10.0.0.5", "refused", m.to_string().as_str()] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[sqlx::test]
async fn manage_switches_to_a_subset_and_back(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let m = common::seed_http_monitor(&pool, project, "a", "https://a.test/", 1).await;
    let c1 = common::seed_component(&pool, page, "API", Some(m), None).await;
    let subscriber = confirmed(&pool, page, &unique_email("m"), None).await;
    let nonce: Vec<u8> = sqlx::query_scalar("select token_nonce from status_page_subscribers where id = $1").bind(subscriber).fetch_one(&pool).await.unwrap();
    let token = nami_engine::statuspages::tokens::sign(&state.cipher, subscriber, nami_engine::statuspages::tokens::Purpose::Manage,
        &nonce.try_into().unwrap());
    let (status, _, _) = call_raw(&app, "POST", "/public/status-subscriptions/manage", &[], Some(json!({ "token": token, "components": [c1] }))).await;
    assert_eq!(status, StatusCode::OK);
    let all: bool = sqlx::query_scalar("select all_components from status_page_subscribers where id = $1").bind(subscriber).fetch_one(&pool).await.unwrap();
    assert!(!all);
    // Deleting the only followed component does not widen the subscription (R15).
    sqlx::query("delete from status_page_components where id = $1").bind(c1).execute(&pool).await.unwrap();
    let all: bool = sqlx::query_scalar("select all_components from status_page_subscribers where id = $1").bind(subscriber).fetch_one(&pool).await.unwrap();
    assert!(!all);
}

#[sqlx::test]
async fn only_admins_see_subscribers_and_the_dto_never_does(pool: sqlx::PgPool) {
    let app = common::test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = common::user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = common::user_with_role(&app, &pool, project, Some("member")).await;
    let page = common::seed_status_page(&pool, project, "acme").await;
    let email = unique_email("listed");
    let id = confirmed(&pool, page, &email, None).await;
    let uri = format!("/projects/{project}/status-pages/{page}/subscribers");
    assert_eq!(common::call(&app, "GET", &uri, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    let (status, list) = common::call(&app, "GET", &uri, Some(&admin), None).await;
    assert_eq!((status, list[0]["email"].as_str()), (StatusCode::OK, Some(email.as_str())));
    let (_, _, body) = call_raw(&app, "GET", "/public/status-pages/acme", &[("x-forwarded-for", "203.0.113.70")], None).await;
    assert!(!String::from_utf8_lossy(&body).contains(&email));
    assert_eq!(common::call(&app, "DELETE", &format!("{uri}/{id}"), Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(common::call(&app, "DELETE", &format!("{uri}/{id}"), Some(&admin), None).await.0, StatusCode::NO_CONTENT);
}
```

`core/email.rs` unit test: `send_with_headers` refuses a value containing `\r` or `\n` and sends nothing. Note that on `main` `Mailer::send` reports SMTP failures as `AppError::Validation("could not send email")` too, so the test must compare the **message**: the refusal is `AppError::Validation("invalid email header")`, and with the Mailer pointed at `127.0.0.1:9` an attempted send would come back as `"could not send email"` instead. A second case sends a clean header to the same dead port and asserts `"could not send email"`, proving the two are told apart.

Add `inspect`, `manage` and `unsubscribe` to `json_bodies.rs`. The one-click route is exempt by design: `emails_carry_one_click_unsubscribe_and_single_purpose_links` (above) posts it as `application/x-www-form-urlencoded` on the public listener. Also add `the_one_click_route_takes_a_form_body` to `json_bodies.rs`, next to `pings_on_the_public_listener_take_any_body`: a form body with a junk token gets 404, not 415.

Run: engine command `--test statuspages --test core -- status_subscribers:: json_bodies:: status_automatic::`, then engine command `--lib -- core::email`. Expected: FAIL.

- [ ] **Step 2: Implement.**
  - `statuspages::report::routes` calls `enqueue_update` for the first update of a new report and for every posted update, inside the write's transaction, then `tokio::spawn(kick)` after commit.
  - `automatic.rs` calls it after each insert/update (the uptime worker's `notify_transition` kick drains them). It runs inside the uptime worker's or heartbeat handler's transaction, so it is one `insert … select` (up to 10,000 rows for a full page), never a loop.
  - `public_engine_url` is validated in `from_env` like nami-ui's `publicEngineUrl` (absolute http(s), no credentials, query or fragment; trailing slashes trimmed). Document it in `.env.example` next to `UI_BASE_URL`.

- [ ] **Step 3: Run** both commands again, the full suite and clippy. Expected: green.

- [ ] **Step 4: Commit**

```bash
git add nami-engine/src nami-engine/tests nami-engine/.env.example
git commit -m "feat(engine): status page update emails with one-click unsubscribe and subscriber management (NAMI-SP-10)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 11 (NAMI-SP-11): UI — status page list and editor

**Files:**
- Create: `nami-ui/src/lib/statuspages/status-pages.ts` (+ `status-pages.test.ts`), `nami-ui/src/lib/server/statuspages/status-page-form.ts` (+ `status-page-form.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/status-pages/+page.server.ts`, `+page.svelte`, `status-pages.test.ts`, `nami-ui/src/routes/(app)/projects/[id=uuid]/status-pages/[pageId=uuid]/+page.server.ts`, `+page.svelte`, `editor.test.ts`
- Modify: `nami-ui/src/routes/(app)/projects/[id=uuid]/+layout.svelte` (a "Status pages" tab after "Maintenance")

**Interfaces:**
- Consumes: Task 2 and Task 3 routes; NAMI-INC-7 `data.role`; the monitors, heartbeats and monitor-groups list endpoints (ids and names only are kept, as the maintenance loader does).
- Produces:

```ts
// lib/statuspages/status-pages.ts
export const COMPONENT_STATUSES = ['operational', 'degraded', 'partial_outage', 'major_outage', 'maintenance', 'no_data'] as const;
export type ComponentStatus = (typeof COMPONENT_STATUSES)[number];
export const STATUS_LABELS: Record<ComponentStatus, string>;   // R6 labels, same text as the engine's `label()`
export function statusClass(s: string): string;                // token bg class per status (`status-*` / `success` / `warning` / `danger`); unknown → the no_data class
export const BANNER_TEXT: Record<ComponentStatus, string>;     // operational → "All systems operational", …
export type PageView = { id: string; slug: string; title: string; description: string | null; logo_url: string | null;
  logo_dark_url: string | null; accent_color: string; theme: 'light' | 'dark' | 'system'; nav_links: { label: string; url: string }[];
  visibility: 'public' | 'private'; has_password: boolean; ip_allowlist: string[]; access_epoch: number;
  custom_domain: string | null; custom_domain_verified: boolean; custom_domain_verification?: { name: string; value: string } | null;
  timezone: string; noindex: boolean; automatic_reports: boolean; automatic_reports_notify: boolean; allow_webhook_subscribers: boolean };
export type Section = { id: string; name: string; position: number };
export type Component = { id: string; section_id: string; display_name: string; description: string | null;
  monitor_id: string | null; heartbeat_id: string | null; position: number; show_uptime_bars: boolean; source_name: string };
/** The public URL of a page: https://<verified domain> or `${origin}/status/<slug>`. */
export function publicUrl(page: PageView, origin: string): string;

// lib/server/statuspages/status-page-form.ts
export type Parsed<T> = { ok: true; body: T } | { ok: false; message: string };
/** Editor "settings" form → PATCH body. Empty optional text → null; password empty → omitted; clear_password=yes → null. */
export function parsePageSettings(form: FormData): Parsed<Record<string, unknown>>;
/** "monitor:<uuid>" | "heartbeat:<uuid>" → { monitor_id } | { heartbeat_id }; anything else → error. */
export function parseSource(raw: string): Parsed<{ monitor_id: string } | { heartbeat_id: string }>;
```

Editor (`/projects/{id}/status-pages/{pageId}`), every form shown only when `data.role === 'admin'` (R22). The engine stays the authority; a 403 shows as the form message.
- **Settings:** title, slug, description, timezone, theme, accent colour (`<input type="color">` plus text), logo and dark logo URLs, four nav-link rows, `noindex`, automatic reports + "notify subscribers" for them. "Webhook subscribers" is shown disabled with "available soon" (Part 2).
- **Visibility:** public/private, a password (write-only, placeholder "unchanged" when `has_password`, a "remove password" checkbox), an IP allowlist textarea (one CIDR per line). The note says: "Changing the password, allowlist or visibility signs everyone out of this page."
- **Custom domain:** the domain, then the TXT record name/value to add and a "Verify" button (action `verifyDomain`). Status is verified or not. A help line points at the README's TLS section. Saving the settings form resends the current domain; the engine keeps the verification when the value is unchanged (Task 3).
- **IP allowlist note:** "The allowlist matches the visitor address nami-ui sees. Behind a reverse proxy this needs ADDRESS_HEADER and XFF_DEPTH set (README), or every visitor looks like the proxy."
- **Sections and components:** add/rename sections; up/down buttons (actions `moveSection`/`moveComponent` swap the `position` of two neighbours with two PATCHes); add a component (display name, description, source `<select>` grouped Monitors/Heartbeats showing source names, "show uptime bars"); "Import a group" (select of monitor groups) with the note "Component names will be the monitor and heartbeat names. They are public; rename them if needed."; two-step deletes (`confirm=yes`) for sections ("deletes its components"), components and the page.
- **Links:** the public URL, `/feed.atom`, `/badge.svg` and `/status.json` (the last only for public pages).
- The load sets `cache-control: private, no-store` (the page shows the TXT value and the allowlist).

- [ ] **Step 1: Failing tests**

`status-page-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { parsePageSettings, parseSource } from './status-page-form';

const fd = (o: Record<string, string>) => { const f = new FormData(); for (const [k, v] of Object.entries(o)) f.set(k, v); return f; };
const base = { title: ' Acme ', slug: 'acme', timezone: 'UTC', theme: 'system', accent_color: '#14B8A6', visibility: 'public' };

describe('parsePageSettings', () => {
	it('maps the form to a PATCH body', () => {
		const r = parsePageSettings(fd({ ...base, description: '', logo_url: 'https://cdn.acme.test/l.svg', nav_label_1: 'Docs',
			nav_url_1: 'https://docs.acme.test', nav_label_2: '', nav_url_2: '', noindex: 'on', custom_domain: '', ip_allowlist: '' }));
		expect(r).toEqual({ ok: true, body: { title: 'Acme', slug: 'acme', timezone: 'UTC', theme: 'system', accent_color: '#14b8a6',
			visibility: 'public', description: null, logo_url: 'https://cdn.acme.test/l.svg', logo_dark_url: null,
			nav_links: [{ label: 'Docs', url: 'https://docs.acme.test' }], noindex: true, automatic_reports: false,
			automatic_reports_notify: false, custom_domain: null, ip_allowlist: [] } });
	});

	it('keeps the password write-only', () => {
		expect(parsePageSettings(fd({ ...base, visibility: 'private', password: '' }))).toMatchObject({ ok: true });
		const r = parsePageSettings(fd({ ...base, visibility: 'private', password: '' }));
		expect(r.ok && 'password' in r.body).toBe(false);
		expect(parsePageSettings(fd({ ...base, visibility: 'private', password: 'correct horse' }))).toMatchObject({ body: { password: 'correct horse' } });
		expect(parsePageSettings(fd({ ...base, clear_password: 'yes' }))).toMatchObject({ body: { password: null } });
	});

	it('splits the allowlist by line and refuses unsafe links early', () => {
		expect(parsePageSettings(fd({ ...base, ip_allowlist: '10.0.0.0/8\r\n\n 2001:db8::/32 ' }))).toMatchObject({ body: { ip_allowlist: ['10.0.0.0/8', '2001:db8::/32'] } });
		for (const url of ['javascript:alert(1)', 'http://docs.acme.test', 'data:text/html,x'])
			expect(parsePageSettings(fd({ ...base, nav_label_1: 'x', nav_url_1: url }))).toEqual({ ok: false, message: 'Navigation links must be https: or mailto: URLs' });
		expect(parsePageSettings(fd({ ...base, nav_label_1: 'Docs', nav_url_1: '' }))).toEqual({ ok: false, message: 'Give each navigation link a label and a URL' });
	});
});

describe('parseSource', () => {
	it('accepts only a known kind and a UUID', () => {
		expect(parseSource('monitor:f41ffdb1-c497-4b17-8d3f-70477470ef6e')).toEqual({ ok: true, body: { monitor_id: 'f41ffdb1-c497-4b17-8d3f-70477470ef6e' } });
		for (const bad of ['monitor:../x', 'group:f41ffdb1-c497-4b17-8d3f-70477470ef6e', '', 'heartbeat:'])
			expect(parseSource(bad).ok).toBe(false);
	});
});
```

`editor.test.ts` (uses `fakeEvent` with `params: { pageId: PAGE_ID }`):
- the load calls `GET /projects/{id}/status-pages/{pageId}` plus the monitors, heartbeats and monitor-groups lists, keeps only `{id, name}` of each source (a monitor `url` in the mock never reaches the returned data), and sets `private, no-store`;
- `settings` PATCHes the parsed body; a 400 from the engine shows its message;
- `moveComponent` with `direction=up` PATCHes the two neighbours' positions (two calls, in order);
- `deleteSection` without `confirm=yes` returns `{ confirm: 'section', id }` and calls nothing; with it, it sends `DELETE`;
- `verifyDomain` posts `{}` to `…/custom-domain/verify` and shows `Domain verified.` or the engine message;
- `addComponent` refuses `source=group:…` before calling the engine;
- a render test of `+page.svelte` with `role: 'viewer'` shows no `<form>` and with `role: 'admin'` shows the settings form; a component display name `<img src=x onerror=alert(1)>` renders escaped.

`status-pages.test.ts` (list): load returns the pages; `create` posts `{slug, title}` and redirects 303 to the editor; a 409 shows `slug is taken`.

Run: UI command with `src/lib/server/statuspages/status-page-form.test.ts src/lib/statuspages/status-pages.test.ts "src/routes/(app)/projects/[id=uuid]/status-pages"`. Expected: FAIL.

- [ ] **Step 2: Implement** the modules and pages with the design tokens and `$lib/components/ui` components, as the maintenance pages do (Drift (2026-10-24)).
- [ ] **Step 3: Run** the UI command again. Expected: PASS, `npm run check` with 0 errors and 0 warnings.
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): status page list and editor with sections, components and custom domain (NAMI-SP-11)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12 (NAMI-SP-12): UI — report composer, update timeline, subscribers

**Files:**
- Create: `nami-ui/src/lib/server/statuspages/status-report-form.ts` (+ `.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/status-pages/[pageId=uuid]/reports/+page.server.ts`, `+page.svelte`, `reports.test.ts`, `…/reports/[reportId=uuid]/+page.server.ts`, `+page.svelte`, `report.test.ts`, `…/subscribers/+page.server.ts`, `+page.svelte`, `subscribers.test.ts`
- Modify: the editor page (links to "Reports" and "Subscribers")

**Interfaces:**
- Consumes: Task 5 routes, Task 10 admin subscriber routes, NAMI-INC-12 `renderMarkdown`.
- Produces:

```ts
// lib/server/statuspages/status-report-form.ts
/** Composer → POST body: title, kind, status, starts_at/ends_at (datetime-local in UTC → "…:00Z", maintenance only),
 *  affected from `component=<uuid>` checkboxes with `impact_<uuid>` selects, message, notify_subscribers (checkbox, default on). */
export function parseReportForm(form: FormData): Parsed<Record<string, unknown>>;
export function parseUpdateForm(form: FormData, kind: 'incident' | 'maintenance'): Parsed<{ status: string; message: string; notify_subscribers: boolean }>;
```

Behaviour:
- **Reports list:** active first, then the rest, newest first. Each row shows its kind, status, title, affected components and updated time. The composer is shown for `member`/`admin`. It has kind radio buttons. The status select shows the kind's statuses. Maintenance shows start/end inputs and forces the impact to `maintenance`. "Notify subscribers" is checked by default. A message textarea shows the hint "Markdown: headings, emphasis, code, lists, https links".
- **Report page:** title, kind/status badges, affected components, the timeline (oldest first; each update's status, time and `{@html renderMarkdown(message)}`, the only `{@html}`; a "not emailed" tag when `notify_subscribers` is false), the post-update form (member+), title edit (member+), and a two-step delete (admin). An `automatic` report shows "Created automatically from a monitor incident; resolving it here ends automatic updates."
- **Subscribers** (admin only; for others the page shows the engine's 403 as "Only admins can see subscribers."): email, confirmed or pending, "all components" or the component names, created; two-step remove. The load sets `private, no-store`.

- [ ] **Step 1: Failing tests**

`status-report-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { parseReportForm, parseUpdateForm } from './status-report-form';

const C1 = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';
const fd = (pairs: [string, string][]) => { const f = new FormData(); for (const [k, v] of pairs) f.append(k, v); return f; };

describe('parseReportForm', () => {
	it('builds an incident with affected components and notify on by default', () => {
		expect(parseReportForm(fd([['title', ' API errors '], ['kind', 'incident'], ['status', 'investigating'],
			['component', C1], [`impact_${C1}`, 'partial_outage'], ['message', 'Looking.'], ['notify_subscribers', 'on']]))).toEqual({
			ok: true, body: { title: 'API errors', kind: 'incident', status: 'investigating',
				affected: [{ component_id: C1, impact: 'partial_outage' }], message: 'Looking.', notify_subscribers: true } });
	});

	it('turns notify off only when the box is unchecked', () => {
		const r = parseReportForm(fd([['title', 'x'], ['kind', 'incident'], ['message', 'x']]));
		expect(r).toMatchObject({ body: { notify_subscribers: false } });
	});

	it('requires a window for maintenance and forces the maintenance impact', () => {
		expect(parseReportForm(fd([['title', 'x'], ['kind', 'maintenance'], ['message', 'x']]))).toEqual({ ok: false, message: 'Enter a start and an end' });
		expect(parseReportForm(fd([['title', 'x'], ['kind', 'maintenance'], ['message', 'x'], ['starts_at', '2026-11-01T01:00'],
			['ends_at', '2026-11-01T03:00'], ['component', C1], [`impact_${C1}`, 'major_outage']]))).toMatchObject({
			body: { starts_at: '2026-11-01T01:00:00Z', ends_at: '2026-11-01T03:00:00Z', affected: [{ component_id: C1, impact: 'maintenance' }] } });
	});

	it('drops component values that are not UUIDs and refuses an empty title or message', () => {
		expect(parseReportForm(fd([['title', 'x'], ['kind', 'incident'], ['message', 'x'], ['component', '../x']]))).toMatchObject({ body: { affected: [] } });
		expect(parseReportForm(fd([['title', ' '], ['kind', 'incident'], ['message', 'x']])).ok).toBe(false);
		expect(parseReportForm(fd([['title', 'x'], ['kind', 'incident'], ['message', ' ']])).ok).toBe(false);
	});
});

describe('parseUpdateForm', () => {
	it('accepts only the statuses of the kind', () => {
		expect(parseUpdateForm(fd([['status', 'resolved'], ['message', 'Fixed']]), 'incident')).toEqual({ ok: true, body: { status: 'resolved', message: 'Fixed', notify_subscribers: false } });
		expect(parseUpdateForm(fd([['status', 'completed'], ['message', 'x']]), 'incident').ok).toBe(false);
	});
});
```

`report.test.ts`: the load fetches the report and sets `private, no-store`; `update` posts the parsed body to `…/updates`; the engine's 400 cap message shows; `delete` is two-step. Render tests:
- an update message `<script>alert(1)</script>` renders as text inside the markdown output (`&lt;script&gt;`);
- a title with `<img onerror>` is escaped;
- with `role: 'viewer'` no form renders.

`subscribers.test.ts`: the load returns the list with `private, no-store`; a 403 → the page data `{ forbidden: true }`; remove without `confirm=yes` does nothing.

Run: UI command with `src/lib/server/statuspages/status-report-form.test.ts "src/routes/(app)/projects/[id=uuid]/status-pages"`. Expected: FAIL.

- [ ] **Step 2: Implement.** - [ ] **Step 3: Run** the UI command again. Expected: PASS, check clean. - [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): status report composer, update timeline and subscriber list (NAMI-SP-12)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13 (NAMI-SP-13): UI — public status page, custom-domain routing, password form

**Files:**
- Create: `nami-ui/src/hooks.ts`, `nami-ui/src/hooks.server.ts` (+ `hooks.server.test.ts`), `nami-ui/src/lib/statuspages/status-routing.ts` (+ `.test.ts`), `nami-ui/src/lib/server/statuspages/status-public.ts` (+ `.test.ts`), `nami-ui/src/lib/components/UptimeBars.svelte`, `nami-ui/src/routes/status/[slug]/+page.server.ts`, `+page.svelte`, `+error.svelte`, `public-page.test.ts`, `nami-ui/src/routes/_nami/tls-ask/+server.ts` (+ `server.test.ts`)
- Modify: `nami-ui/package.json` + lock (`npm install -D @sveltejs/adapter-node@6.0.0`, `npm uninstall @sveltejs/adapter-auto`), `nami-ui/vite.config.ts` (`import adapter from '@sveltejs/adapter-node'`; there is no `svelte.config.js` in this repo, the Kit config lives in the `sveltekit({...})` plugin call; do **not** add `paths.origin` or `csrf.trustedOrigins`), `nami-ui/src/lib/server/core/engine.ts` (`callPublicEngine`), `nami-ui/src/lib/server/core/engine.test.ts`, `nami-ui/src/lib/server/core/fake-event.ts`, `nami-ui/.env.example`, `nami-ui/README.md`
- `fake-event.ts` extension (backwards compatible; every existing suite must stay green): options `headers?: Record<string, string>` (set on the fake `request`, also for form posts) and `cookies?: Record<string, string>` (read by a new `cookies.get(name)`; `cookies.set` stays a `vi.fn()`); `calls()` entries gain `headers: Record<string, string>`, built from `new Headers(init.headers)` with lowercased names, so a `Headers` object and a plain object both work.

**Interfaces:**
- Consumes: Task 6 DTO and routes, Task 7 unlock, Task 3 domain check, NAMI-INC-12 `renderMarkdown`, Task 11 `STATUS_LABELS`, `statusClass`, `BANNER_TEXT`.
- Produces:

```ts
// lib/statuspages/status-routing.ts
export const DOMAIN_SENTINEL = '_domain';
/** R4. `appHost` null (APP_ORIGIN unset) → never reroutes. Paths under /_app/ (static assets) and /_nami/
 *  (the TLS ask endpoint, called on internal host names) are never rerouted. Compares `url.host` (with port). */
export function statusReroute(url: URL, appHost: string | null): string | undefined;
// hooks.ts (universal hooks: this file is also bundled for the browser)
/** `typeof window !== 'undefined'` → undefined, checked first; then statusReroute(url, host of
 *  globalThis.process?.env?.APP_ORIGIN). `process` does not exist in the browser, so it is never touched there. */
export const reroute: Reroute;

// lib/server/core/engine.ts
/** Public status routes: headers built from scratch (no visitor header is copied, so no cookie, no visitor
 *  X-Forwarded-For / X-Real-IP / Forwarded), X-Forwarded-For = event.getClientAddress() only (a throw, which
 *  adapter-node does when ADDRESS_HEADER is set but absent, → no X-Forwarded-For), `set-cookie` never relayed.
 *  `access` (the nami_sp_access cookie value) goes out as X-Nami-Page-Access. */
export async function callPublicEngine(event: RequestEvent, path: string, init?: RequestInit & { access?: string }): Promise<Response>;

// lib/server/statuspages/status-public.ts
export type PageRef = { kind: 'slug'; slug: string } | { kind: 'domain'; host: string };
export const ACCESS_COOKIE = 'nami_sp_access';
/** A slug must match ^[a-z0-9-]{3,63}$. `_domain` → `{kind: 'domain', host}` with host = `url.hostname`
 *  lowercased, one trailing "." stripped, matching the hostname rule; only when APP_ORIGIN is set and
 *  `url.host` is not its host. Anything else → null. */
export function pageRef(event: RequestEvent): PageRef | null;
export function enginePrefix(ref: PageRef): string;             // enginePath`/public/status-pages/${slug}` | enginePath`/public/status-domains/${host}`
export function basePath(ref: PageRef): string;                 // "/status/<slug>" | ""
export function accessCookieOptions(event: RequestEvent, ref: PageRef): { path: string; httpOnly: true; sameSite: 'lax';
  secure: boolean; maxAge: number };                            // path = basePath || "/", maxAge 30 days
/** GET the DTO with the access cookie; 404 → null. Other failures throw error(502). */
export async function fetchPage(event: RequestEvent, ref: PageRef): Promise<{ dto: PublicDto; etag: string; cacheControl: string; robots: string | null } | null>;
export type PublicDto = { /* R11, typed */ };
```

Page behaviour (`/status/[slug]`, `export const csr = false`, `export const prerender = false`):
- `load`:
  - `pageRef` null → `error(404)`.
  - `fetchPage` null → `error(404, 'Page not found')`. The `+error.svelte` of this route shows "This page does not exist or needs a password" and a password form posting to `?/unlock`, so unknown and locked pages look alike (spec).
  - Otherwise it sets `cache-control` from the engine (`public, max-age=30` or `private, no-store`) and `x-robots-tag: noindex` when the engine sent it. It returns `{ dto, base: basePath(ref) }`.
  - It never reads the session, `url.searchParams` or headers other than the host.
- `actions.unlock`:
  - It sends the password to `enginePrefix(ref) + '/unlock'`.
  - 200 → `cookies.set(ACCESS_COOKIE, token, accessCookieOptions(..))`, then `redirect(303, base || '/')`.
  - 401 → `fail(401, { message: 'Wrong password' })`; 429 → `fail(429, { message: 'Too many attempts. Try again in 15 minutes.' })`.
- Markup:
  - The root carries `data-theme={page.theme}`, and `style="--accent: {accent_color}"` (the only dynamic style).
  - The logo is a `<picture>` with the dark source for `system` (`media="(prefers-color-scheme: dark)"`) or the matching one for a fixed theme, with `referrerpolicy="no-referrer"` and `alt={title}`.
  - Nav links get `rel="noopener noreferrer"`. The banner shows `BANNER_TEXT[status]` in the status colour.
  - Sections list components with status and `UptimeBars` (90 `<span>`s with `title="{date} · {uptime}% · {minutes} min down · {report titles}"`, CSS only, no script).
  - Then active reports and upcoming maintenance, each update with `{@html renderMarkdown(message)}` and `id="report-{id}"` anchors, then history, then the subscribe form (Task 14), then the footer "Powered by Nami".
  - Dates are formatted with `Intl.DateTimeFormat('en-GB', { timeZone: page.timezone, … })`.
  - `<svelte:head>` gets `<meta name="robots" content="noindex">` when `noindex` or private.
- `hooks.server.ts` (new; `main` has none) `handle`: for every response whose route id starts `/status/[slug]`, **and** for every response on a custom-domain host (`APP_ORIGIN` set and `url.host` differs; this covers the 404s of unmatched paths, whose route id is null), it adds:
  - `content-security-policy: default-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' https:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'`;
  - `x-content-type-options: nosniff`, `referrer-policy: no-referrer`, `x-frame-options: DENY`;
  - on a status ≥ 400: `cache-control: no-store` (a shared cache must never keep the "locked or unknown" page, and it carries no `Last-Modified` for heuristic caching anyway).

  Feed, badge and JSON override what they need (Task 15). `handle` passes everything else through unchanged. `/_nami/` responses are left alone.
- `/_nami/tls-ask?domain=<host>`: served on any host (never rerouted, R4). The host goes through a lowercase hostname regex, then `GET /public/status-domain-check/{host}` via `callPublicEngine`. Answers 200/404 with an empty body. It reveals only whether a name is a verified custom domain, which public DNS already shows.
- `.env.example` documents (R4): `APP_ORIGIN`; `ADDRESS_HEADER=x-forwarded-for` and `XFF_DEPTH=<proxies that append>` (required behind a proxy, or rate limits and IP allowlists see the proxy's address); `PROTOCOL_HEADER=x-forwarded-proto` (the protocol defaults to `https` when unset); `HOST_HEADER` left unset with the proxy forwarding the original `Host` (or `x-forwarded-host` only if the proxy always overwrites it); `BODY_SIZE_LIMIT` left at its default; and that adapter-node 6 has no `ORIGIN` variable and `kit.paths.origin` must stay unset for custom domains. The README gets a "Custom domains and TLS" section: the TXT record, the Caddy `on_demand_tls { ask http://nami-ui:3000/_nami/tls-ask }` snippet, a Caddy `reverse_proxy` block that sets `X-Forwarded-For`/`X-Forwarded-Proto` and keeps `Host`, the manual-certificate alternative, and "health checks must use the APP_ORIGIN host".

- [ ] **Step 1: Failing tests**

`status-routing.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { statusReroute } from './status-routing';

const r = (u: string, app: string | null = 'nami.acme.test') => statusReroute(new URL(u), app);

describe('statusReroute', () => {
	it('leaves the app host and an unset APP_ORIGIN alone', () => {
		expect(r('https://nami.acme.test/projects/x')).toBeUndefined();
		expect(r('https://nami.acme.test/status/acme')).toBeUndefined();
		expect(r('https://status.acme.test/login', null)).toBeUndefined();
	});

	it('maps every custom-domain path under the domain sentinel', () => {
		expect(r('https://status.acme.test/')).toBe('/status/_domain');
		expect(r('https://status.acme.test/feed.atom')).toBe('/status/_domain/feed.atom');
		expect(r('https://status.acme.test/subscribe/confirm?token=x')).toBe('/status/_domain/subscribe/confirm');
	});

	it('a custom domain reaches only its own page', () => {
		// The app's routes and other pages become paths under the sentinel, where no route exists.
		for (const p of ['/login', '/projects/c13bc10a-d17f-443b-8976-c6d9b54e2798', '/status/other-slug', '/status/_domain', '/logout'])
			expect(r(`https://status.acme.test${p}`)).toBe(`/status/_domain${p}`);
	});

	it('serves static assets and the TLS ask endpoint on every host', () => {
		expect(r('https://status.acme.test/_app/immutable/assets/app.css')).toBeUndefined();
		// Caddy calls the ask endpoint on an internal name such as nami-ui:3000.
		expect(r('http://nami-ui:3000/_nami/tls-ask?domain=status.acme.test')).toBeUndefined();
	});

	it('compares hosts without case or a default port', () => {
		expect(r('https://NAMI.acme.test/projects')).toBeUndefined();
		expect(r('https://nami.acme.test:443/projects')).toBeUndefined();
	});
});
```

`engine.test.ts` (added):

```ts
describe('callPublicEngine', () => {
	it('the bff forwards only the adapter client address, no cookie, and relays no set-cookie', async () => {
		const { event, calls } = fakeEvent({
			headers: { cookie: 'id=SESSION', 'x-forwarded-for': '10.0.0.1' },
			respond: () => new Response('{}', { headers: { 'set-cookie': 'id=evil; Path=/' } })
		});
		await callPublicEngine(event, '/public/status-pages/acme', { access: 'v1.tok' });
		const [call] = calls();
		expect(call.headers['x-forwarded-for']).toBe('203.0.113.5');   // getClientAddress(), not the visitor's header
		expect(call.headers['cookie']).toBeUndefined();
		expect(call.headers['x-nami-page-access']).toBe('v1.tok');
		expect(call.headers['x-real-ip']).toBeUndefined();
		expect(call.headers['forwarded']).toBeUndefined();
		expect(event.cookies.set).not.toHaveBeenCalled();
	});

	it('sends no address when adapter-node cannot determine one', async () => {
		const { event, calls } = fakeEvent({ headers: { 'x-forwarded-for': '10.0.0.1', 'x-real-ip': '10.0.0.1' } });
		event.getClientAddress = () => { throw new Error('Address header was specified with ADDRESS_HEADER=x-forwarded-for but is absent from request'); };
		await callPublicEngine(event, '/public/status-pages/acme');
		expect(calls()[0].headers['x-forwarded-for']).toBeUndefined();
		expect(calls()[0].headers['x-real-ip']).toBeUndefined();
	});
});
```

`status-public.test.ts` (env through `vi.stubEnv('APP_ORIGIN', …)` with `afterEach(() => vi.unstubAllEnvs())`, never by assigning `process.env`): `pageRef` returns `{kind: 'slug'}` for `acme`, null for `Acme`, `a`, `../x`, for `_domain` on the app host and for `_domain` on any host when `APP_ORIGIN` is unset, and `{kind: 'domain', host: 'status.acme.test'}` for `_domain` on `https://Status.Acme.Test./` and on `https://status.acme.test:8443/` (hostname only, no port). `accessCookieOptions` has path `/status/acme` for a slug and `/` for a domain, `secure` true only on https, and maxAge `2592000`.

`public-page.test.ts`:

```ts
import { render } from 'svelte/server';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { actions, load } from './+page.server';
import Page from './+page.svelte';

const dto = (over: Record<string, unknown> = {}) => ({ page: { slug: 'acme', title: 'Acme', description: null, logo_url: null,
	logo_dark_url: null, accent_color: '#14b8a6', theme: 'system', nav_links: [], timezone: 'UTC', noindex: false, visibility: 'public',
	url: 'https://nami.acme.test/status/acme' }, status: 'operational', sections: [], active_reports: [], upcoming_maintenance: [],
	history: [], subscriptions: { email: true, webhook: false }, generated_at: '2026-10-19T10:00:00Z', ...over });
const ok = (body = dto(), headers: Record<string, string> = { etag: '"abc"', 'cache-control': 'public, max-age=30' }) =>
	() => new Response(JSON.stringify(body), { headers });

afterEach(() => vi.unstubAllEnvs());

describe('public status page load', () => {
	it('the domain sentinel is refused everywhere when custom domains are off', async () => {
		vi.stubEnv('APP_ORIGIN', '');
		const { event, calls } = fakeEvent({ params: { slug: '_domain' }, url: 'https://status.acme.test/status/_domain' });
		await expect(load(event)).rejects.toMatchObject({ status: 404 });
		expect(calls()).toEqual([]);
	});

	it('loads by slug and passes the cache headers on', async () => {
		const { event, calls, setHeaders } = fakeEvent({ params: { slug: 'acme' }, url: 'https://nami.acme.test/status/acme', respond: ok() });
		expect(await load(event)).toMatchObject({ dto: { page: { slug: 'acme' } }, base: '/status/acme' });
		expect(calls()[0].path).toBe('/public/status-pages/acme');
		expect(setHeaders).toHaveBeenCalledWith(expect.objectContaining({ 'cache-control': 'public, max-age=30' }));
	});

	it('the domain sentinel is refused on the app host', async () => {
		vi.stubEnv('APP_ORIGIN', 'https://nami.acme.test');
		const { event, calls } = fakeEvent({ params: { slug: '_domain' }, url: 'https://nami.acme.test/status/_domain' });
		await expect(load(event)).rejects.toMatchObject({ status: 404 });
		expect(calls()).toEqual([]);
	});

	it('a custom domain looks the page up by host', async () => {
		vi.stubEnv('APP_ORIGIN', 'https://nami.acme.test');
		const { event, calls } = fakeEvent({ params: { slug: '_domain' }, url: 'https://status.acme.test/', respond: ok() });
		expect(await load(event)).toMatchObject({ base: '' });
		expect(calls()[0].path).toBe('/public/status-domains/status.acme.test');
	});

	it('is identical with or without a session and ignores the query string (no cache poisoning)', async () => {
		const a = fakeEvent({ params: { slug: 'acme' }, url: 'https://nami.acme.test/status/acme', respond: ok() });
		const b = fakeEvent({ params: { slug: 'acme' }, url: 'https://nami.acme.test/status/acme?x=<script>&theme=dark',
			headers: { cookie: 'id=SESSION' }, respond: ok() });
		expect(await load(b.event)).toEqual(await load(a.event));
		expect(b.calls()[0].search).toBe('');
		expect(b.event.cookies.set).not.toHaveBeenCalled();
	});

	it('forwards the access cookie and marks private pages no-store and noindex', async () => {
		const { event, calls, setHeaders } = fakeEvent({ params: { slug: 'acme' }, url: 'https://nami.acme.test/status/acme',
			cookies: { nami_sp_access: 'v1.tok' },
			respond: ok(dto(), { etag: '"x"', 'cache-control': 'private, no-store', 'x-robots-tag': 'noindex' }) });
		await load(event);
		expect(calls()[0].headers['x-nami-page-access']).toBe('v1.tok');
		expect(setHeaders).toHaveBeenCalledWith({ 'cache-control': 'private, no-store', 'x-robots-tag': 'noindex' });
	});

	it('404s for an unknown or locked page', async () => {
		await expect(load(fakeEvent({ params: { slug: 'acme' }, status: 404, body: '{"error":"not found"}' }).event)).rejects.toMatchObject({ status: 404 });
	});
});

describe('unlock action', () => {
	it('sets a page-scoped HttpOnly cookie and redirects', async () => {
		const { event } = fakeEvent({ params: { slug: 'acme' }, url: 'https://nami.acme.test/status/acme', form: { password: 'pw' },
			respond: () => new Response('{"token":"v1.tok","expires_at":"2026-11-18T00:00:00Z"}') });
		await expect(actions.unlock(event)).rejects.toMatchObject({ status: 303, location: '/status/acme' });
		expect(event.cookies.set).toHaveBeenCalledWith('nami_sp_access', 'v1.tok',
			{ path: '/status/acme', httpOnly: true, sameSite: 'lax', secure: true, maxAge: 2592000 });
	});

	it('says wrong password without detail and maps 429', async () => {
		const wrong = fakeEvent({ params: { slug: 'acme' }, form: { password: 'x' }, status: 401, body: '{"error":"wrong password"}' });
		expect(await actions.unlock(wrong.event)).toMatchObject({ status: 401, data: { message: 'Wrong password' } });
		const many = fakeEvent({ params: { slug: 'acme' }, form: { password: 'x' }, status: 429, body: '{}' });
		expect(await actions.unlock(many.event)).toMatchObject({ status: 429 });
	});
});

describe('public page render', () => {
	it('renders user text as text, the accent only as a CSS variable, and noindex for private pages', () => {
		const evil = '<script>alert(1)</script>';
		const body = dto({ page: { ...dto().page, title: evil, visibility: 'private', accent_color: '#ff0000' },
			sections: [{ id: 's', name: evil, components: [{ id: 'c', name: '"><img src=x onerror=alert(1)>', description: null, status: 'degraded', bars: null }] }],
			active_reports: [{ id: 'r', title: evil, kind: 'incident', status: 'identified', impact: 'degraded', starts_at: null, ends_at: null,
				published_at: '2026-10-19T09:00:00Z', updated_at: '2026-10-19T09:00:00Z', components: [],
				updates: [{ id: 'u', status: 'identified', message: `**bold** ${evil}`, created_at: '2026-10-19T09:00:00Z' }] }] });
		const { head, body: html } = render(Page, { props: { data: { dto: body, base: '/status/acme' }, form: null } });
		expect(html).not.toContain('<script>alert(1)');
		expect(html).not.toContain('<img src=x');
		expect(html).toContain('<strong>bold</strong> &lt;script&gt;');
		expect(html).toContain('--accent: #ff0000');
		expect(head).toContain('<meta name="robots" content="noindex">');
		expect(html).toContain('Powered by Nami');
	});
});
```

`tls-ask/server.test.ts`: a verified domain → 200 (engine 204), unknown → 404, an invalid `domain` (`a b`, `../x`, empty) → 404 without an engine call; a request on the internal host `http://nami-ui:3000` behaves the same as on the app host (Caddy's call).

`hooks.server.test.ts`: `handle` adds the CSP, `nosniff`, `no-referrer` and `DENY` headers to a `/status/[slug]` response and to a 404 on a custom-domain host (route id null), adds `cache-control: no-store` to a status ≥ 400 there, and leaves an app route (`/projects/…`) and `/_nami/tls-ask` untouched.

Run: UI command with `src/lib/statuspages/status-routing.test.ts src/lib/server/statuspages/status-public.test.ts src/lib/server/core/engine.test.ts src/hooks.server.test.ts src/routes/status src/routes/_nami`. Expected: FAIL.

- [ ] **Step 2: Implement.** Install adapter-node and switch `vite.config.ts`. Run `npm run build` once to prove the adapter builds (`build/` stays gitignored).
- [ ] **Step 3: Run** the UI command again, then the full `npx vitest run` (existing suites must stay green with the extended `fakeEvent`) and `npm run check`. Expected: PASS, 0 errors and 0 warnings.
- [ ] **Step 4: Commit**

```bash
git add nami-ui
git commit -m "feat(ui): public status pages with custom-domain routing, password unlock and adapter-node (NAMI-SP-13)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14 (NAMI-SP-14): UI — subscribe, confirm, manage and unsubscribe pages

**Files:**
- Create: `nami-ui/src/routes/status/[slug]/subscribe/confirm/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/status/[slug]/subscribe/manage/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/status/[slug]/unsubscribe/+page.server.ts`, `+page.svelte`, `nami-ui/src/routes/status/[slug]/subscriptions.test.ts`
- Modify: `nami-ui/src/routes/status/[slug]/+page.server.ts` (action `subscribe`), `+page.svelte` (subscribe form)

**Interfaces:**
- Consumes: Task 9 subscribe/confirm routes, Task 10 inspect/manage/unsubscribe, Task 13 `pageRef`, `enginePrefix`, `callPublicEngine`.
- Produces: the routes below. Each token page sets `cache-control: no-store`, `referrer-policy: no-referrer`, `x-robots-tag: noindex`, and `csr = false`.

Behaviour:
- **Subscribe form** (on the page when `subscriptions.email`): email, then "All components" (default) or "Only these" with one checkbox per component. Action `subscribe` → `POST enginePrefix(ref)/subscribe {email, components: null | [ids that pass isUuid]}` with the access cookie.
  - 202 → `{ subscribed: true, message: 'Check your inbox to confirm your subscription.' }` for every address (R15).
  - 400 → the engine message. 429 → `Too many requests. Try again later.` or the engine's "not accepting" message.
- **Confirm** `/subscribe/confirm?token=…`: the `load` makes **no engine call** (a GET never mutates; mail scanners only see a button). It returns `{ hasToken }`. The token must match `^[A-Za-z0-9_-]{40,200}$`, else the page says the link is invalid. Action `confirm` posts `{token}` (from a hidden field) to `/public/status-subscriptions/confirm`. 200 → "You are subscribed to {title}." 410 → "This link has expired or was already used. Subscribe again from the status page." 404 → "This link is invalid."
- **Manage** `/subscribe/manage?token=…`: the `load` calls `inspect` (read-only) and shows the masked email and the component checkboxes. Actions `save` (`manage`) and `unsubscribe`. Both carry the token in a hidden field, never in a redirect.
- **Unsubscribe** `/unsubscribe?token=…`: the `load` calls `inspect` (purpose `unsubscribe` or `manage`) and shows "Unsubscribe {masked} from {title}?" with one button. Action `unsubscribe` → "You will no longer receive emails from {title}."

- [ ] **Step 1: Failing tests** — `subscriptions.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { fakeEvent } from '#lib/server/core/fake-event';
import { actions as pageActions } from './+page.server';
import { actions as confirmActions, load as confirmLoad } from './subscribe/confirm/+page.server';
import { actions as manageActions, load as manageLoad } from './subscribe/manage/+page.server';
import { actions as unsubActions } from './unsubscribe/+page.server';

const TOKEN = 'A'.repeat(88);
const C1 = '0b9d1f6c-5a3e-4c2f-9d47-6e8a1c3b7f25';

describe('subscribe action', () => {
	it('posts the address with all or some components and always answers the same', async () => {
		const all = fakeEvent({ params: { slug: 'acme' }, form: { email: ' A@B.test ', scope: 'all' }, status: 202, body: '{"ok":true}' });
		expect(await pageActions.subscribe(all.event)).toEqual({ subscribed: true, message: 'Check your inbox to confirm your subscription.' });
		expect(all.calls()[0]).toMatchObject({ method: 'POST', path: '/public/status-pages/acme/subscribe', body: '{"email":"A@B.test","components":null}' });
		const f = new FormData(); f.set('email', 'a@b.test'); f.set('scope', 'some'); f.append('component', C1); f.append('component', '../x');
		const some = fakeEvent({ params: { slug: 'acme' }, status: 202, body: '{"ok":true}' });
		some.event.request = new Request('http://ui.test/', { method: 'POST', body: f });
		await pageActions.subscribe(some.event);
		expect(JSON.parse(some.calls()[0].body!)).toEqual({ email: 'a@b.test', components: [C1] });
	});
});

describe('confirm page', () => {
	it('a GET never calls the engine', async () => {
		const { event, calls } = fakeEvent({ params: { slug: 'acme' }, url: `https://nami.acme.test/status/acme/subscribe/confirm?token=${TOKEN}` });
		expect(await confirmLoad(event)).toEqual({ hasToken: true });
		expect(calls()).toEqual([]);
	});

	it('confirms by POST with the token in the body and maps 410/404', async () => {
		const { event, calls } = fakeEvent({ params: { slug: 'acme' }, form: { token: TOKEN },
			body: '{"page":{"title":"Acme","url":"https://x"}}' });
		expect(await confirmActions.confirm(event)).toMatchObject({ confirmed: true, title: 'Acme' });
		expect(calls()[0]).toMatchObject({ method: 'POST', path: '/public/status-subscriptions/confirm', search: '', body: JSON.stringify({ token: TOKEN }) });
		expect(await confirmActions.confirm(fakeEvent({ params: { slug: 'acme' }, form: { token: TOKEN }, status: 410, body: '{}' }).event))
			.toMatchObject({ status: 410 });
		const bad = fakeEvent({ params: { slug: 'acme' }, form: { token: '<script>' } });
		expect(await confirmActions.confirm(bad.event)).toMatchObject({ status: 400 });
		expect(bad.calls()).toEqual([]);
	});
});

describe('manage and unsubscribe', () => {
	it('inspects on load, saves and unsubscribes by POST', async () => {
		const inspected = { purpose: 'manage', page: { title: 'Acme', url: 'x' }, email: 'a***@b.test', all_components: true, components: [] };
		const { event, calls, setHeaders } = fakeEvent({ params: { slug: 'acme' }, url: `https://x/status/acme/subscribe/manage?token=${TOKEN}`,
			body: JSON.stringify(inspected) });
		expect(await manageLoad(event)).toMatchObject({ inspected });
		expect(calls()[0]).toMatchObject({ path: '/public/status-subscriptions/inspect', body: JSON.stringify({ token: TOKEN }) });
		expect(setHeaders).toHaveBeenCalledWith(expect.objectContaining({ 'cache-control': 'no-store', 'referrer-policy': 'no-referrer' }));
		const save = fakeEvent({ params: { slug: 'acme' }, form: { token: TOKEN, scope: 'all' }, body: '{"ok":true}' });
		await manageActions.save(save.event);
		expect(save.calls()[0]).toMatchObject({ path: '/public/status-subscriptions/manage', body: JSON.stringify({ token: TOKEN, components: null }) });
		const unsub = fakeEvent({ params: { slug: 'acme' }, form: { token: TOKEN }, body: '{"ok":true}' });
		expect(await unsubActions.unsubscribe(unsub.event)).toMatchObject({ unsubscribed: true });
	});
});
```

Run: UI command with `src/routes/status`. Expected: FAIL.

- [ ] **Step 2: Implement.** - [ ] **Step 3: Run** the UI command again and the full vitest run. Expected: PASS, check clean. - [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): status page subscribe, confirm, manage and unsubscribe flows (NAMI-SP-14)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15 (NAMI-SP-15): UI — Atom feed, SVG badge, public JSON status API

Uses Task 13's helpers; runs after Task 14.

**Files:**
- Create: `nami-ui/src/routes/status/[slug]/feed.atom/+server.ts`, `badge.svg/+server.ts`, `status.json/+server.ts`, `nami-ui/src/routes/status/[slug]/machine.test.ts`
- Modify: `nami-ui/src/lib/statuspages/status-pages.ts` (+ test): `xmlEscape`, `atomFeed`, `badgeSvg`, `statusJson`

**Interfaces:**
- Consumes: Task 13 `pageRef`, `fetchPage`, `ACCESS_COOKIE`.
- Produces:

```ts
// lib/statuspages/status-pages.ts
/** & < > " ' → entities; strips characters XML 1.0 forbids (C0 controls except \t \n \r, U+FFFE/U+FFFF, lone surrogates). */
export function xmlEscape(s: string): string;
/** One <entry> per update (newest first, at most 100): id `${url}#update-${id}`, title `${report} — ${status label}`,
 *  updated = update time, link to `${url}#report-${reportId}`, content type="text". */
export function atomFeed(dto: PublicDto): string;
/** Shields-style flat badge: label "status", message STATUS_LABELS[status], width from text length (7 px per char + 10). */
export function badgeSvg(status: string): string;
/** Spec shape, built only from DTO fields. */
export function statusJson(dto: PublicDto): {
  version: 1; page: { name: string; url: string; updated_at: string }; status: string;
  components: { name: string; section: string; status: string }[];
  active_reports: { title: string; kind: string; status: string; impact: string | null; updated_at: string;
    latest_update: { status: string; message: string; created_at: string } | null }[];
  upcoming_maintenance: { title: string; kind: string; status: string; impact: string | null; updated_at: string;
    starts_at: string; ends_at: string; latest_update: { status: string; message: string; created_at: string } | null }[] };
```

Route rules:

| Route | Access | Headers |
|---|---|---|
| `feed.atom` | as the page (cookie or allowlist) | `content-type: application/atom+xml; charset=utf-8`, `etag` (engine's), engine `cache-control`, `x-robots-tag: noindex` when noindex/private, `x-content-type-options: nosniff` |
| `badge.svg` | as the page | `content-type: image/svg+xml`, `cache-control: public, max-age=60` (private: `private, no-store`), `content-security-policy: default-src 'none'; style-src 'unsafe-inline'`, `nosniff`, `etag` |
| `status.json` | **public pages only** (a private page → 404 even with a valid cookie, R25) | `content-type: application/json`, `access-control-allow-origin: *` (never `-allow-credentials`), `cache-control: public, max-age=30`, `x-robots-tag: noindex` when noindex, `etag`, `nosniff` |

Each answers `If-None-Match` equal to its ETag with 304 and no body, compared only after `fetchPage` returned the DTO (access granted, R10); a locked page is a 404 whatever `If-None-Match` says. For JSON the ETag is the engine's; for the feed it is the engine's with `-atom`; for the badge, `-badge`. Unknown or locked → 404 with `text/plain` "not found". JSON `OPTIONS` is not needed (a simple GET, no preflight).

- [ ] **Step 1: Failing tests**

`status-pages.test.ts` (added):

```ts
describe('xml output', () => {
	it('escapes markup, CDATA ends, quotes and drops forbidden characters', () => {
		expect(xmlEscape(`<script>"x" & 'y' ]]> \u0001\u0008 ok`)).toBe('&lt;script&gt;&quot;x&quot; &amp; &apos;y&apos; ]]&gt;  ok');
	});

	it('builds a feed whose user text is all escaped', () => {
		const xml = atomFeed(dtoWith({ title: 'A & B', report: '</title><script>alert(1)</script>', message: ']]><x/>' }));
		expect(xml.startsWith('<?xml version="1.0" encoding="utf-8"?>\n<feed xmlns="http://www.w3.org/2005/Atom">')).toBe(true);
		expect(xml).not.toContain('<script>');
		expect(xml).not.toContain(']]><x/>');
		expect(xml).toContain('<content type="text">]]&gt;&lt;x/&gt;</content>');
		expect(xml).toContain('<title>A &amp; B</title>');
	});

	it('badges carry only the fixed vocabulary, escaped', () => {
		const svg = badgeSvg('partial_outage');
		expect(svg).toContain('>Partial outage<');
		expect(badgeSvg('<script>')).toContain('>No data<');
		expect(svg).not.toMatch(/<script|on\w+=/);
	});

	it('the json api exposes only the spec fields', () => {
		const json = statusJson(dtoWith({ title: 'Acme', report: 'API slow', message: 'Looking' }));
		expect(Object.keys(json).sort()).toEqual(['active_reports', 'components', 'page', 'status', 'upcoming_maintenance', 'version']);
		expect(Object.keys(json.page).sort()).toEqual(['name', 'updated_at', 'url']);
		expect(Object.keys(json.components[0]).sort()).toEqual(['name', 'section', 'status']);
		expect(JSON.stringify(json)).not.toMatch(/"id"|logo|accent|nav_links|bars|timezone/);
	});
});
```

(`dtoWith({title, report, message})` is a test helper building an R11 DTO with one section, one component and one active report with one update.)

`machine.test.ts`:
- `status.json` for a public page: 200, ACAO `*`, no `access-control-allow-credentials`, `public, max-age=30`, body keys as above; with `If-None-Match` equal to the ETag → 304 with no body;
- `status.json` for a private page (engine 200 with `visibility: 'private'`, a valid cookie forwarded) → 404;
- `feed.atom` forwards the access cookie (`x-nami-page-access`) and sends `noindex` for a private page;
- `badge.svg` has `max-age=60`, the SVG CSP and `nosniff`; a 404 from the engine → 404 `not found`;
- each route on the custom-domain sentinel calls `/public/status-domains/<host>` (Task 13's `pageRef`).

Run: UI command with `src/lib/statuspages/status-pages.test.ts src/routes/status`. Expected: FAIL.

- [ ] **Step 2: Implement.** - [ ] **Step 3: Run** the UI command again, the full vitest run and `npm run check`. Expected: PASS. (The roadmap ticks come at the end of NAMI-SP-16, not here.)
- [ ] **Step 4: Commit**

```bash
git add nami-ui/src
git commit -m "feat(ui): status page Atom feed, SVG badge and public JSON API (NAMI-SP-15)" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16 (NAMI-SP-16): Part 1 security review and fix wave

Per CLAUDE.md every phase ends with a dedicated security review and fix wave. It runs on the phase branch `feat/statuspages-part1` after NAMI-SP-15's task review is clean (no separate branch or PR). Part 2's tasks start at NAMI-SP-17, so this ID is Part 1's own. The reviewer reads the whole phase diff (`origin/main...feat/statuspages-part1`) for `nami-engine/` and `nami-ui/` and checks at least:

1. **Leaks.**
   - Grep every serialised public type (`statuspages::snapshot`, `statuspages::public::routes`, `statuspages::subscriber::routes`, the nami-ui machine routes) and confirm no field beyond R11 and the spec's JSON shape.
   - Re-run the leak test with a monitor carrying basic-auth credentials, custom headers, a proxy and a keyword, and with an automatic report.
   - The tooltip never shows incident titles or causes.
   - Error bodies of `/public/...` are the fixed set (`not found`, `wrong password`, rate-limit texts, validation messages that echo nothing).
2. **Tenant isolation and host routing.**
   - Every admin route × foreign project (403) × foreign page/section/component/report/subscriber id (404).
   - On a custom-domain host no app route, no other slug and no `/_nami` route resolves.
   - `_domain` on the app host is 404.
   - An unverified domain routes nowhere, and the app host cannot be stored as a domain.
   - Status routes never read or set the session cookie.
3. **Private access.**
   - Argon2 for page passwords, the dummy hash for unknown pages, the 10/15 min limit per (page, address).
   - Tokens are bound to page + `access_epoch` + expiry, MAC-checked before parsing, and the key is held in `Zeroizing`.
   - The cookie is HttpOnly, page-scoped and Secure on https.
   - `X-Forwarded-For` from a browser never reaches the engine (only `getClientAddress()`); a missing address grants nothing.
   - Access is decided from the fresh row on every request; no path serves a cached snapshot, or a 304, before the access check.
   - Document the adapter-node `ADDRESS_HEADER`/`XFF_DEPTH` deployment requirement in the PR, and confirm `vite.config.ts` sets neither `paths.origin` nor `csrf.trustedOrigins`.
4. **Subscribers.**
   - Same answer for new, pending and confirmed addresses. 3 emails per address per day, 5/hour/IP, 100/hour/page, 10,000 confirmed and 500 pending per page, 20 notified updates per report.
   - Confirm only by POST, single use, 48 h. Tokens single-purpose (confirm ≠ manage ≠ unsubscribe).
   - One-click unsubscribe per RFC 8058, idempotent, MAC checked before any database work. No header injection through titles or addresses.
   - A full page answers 429 for known and new addresses alike.
5. **Queue.**
   - Incident rows keep every Incidents Part 1 behaviour (their tests are green and unchanged except the claim statement).
   - Status-page rows never write `incident_events`, never block incident rows (priority), respect the page cap, and cascade away with their subscriber, update or page.
   - Poison-row handling covers them.
6. **Escaping.** Markdown (INC-12 renderer), Atom (`]]>`, controls), SVG badge, HTML attributes (accent only through the validated `#rrggbb`, nav links only `https:`/`mailto:`), emails (plain text, subjects one line).
7. **Caching and DoS.**
   - One snapshot build per page per 30 s under concurrency, invalidated by writes. No ClickHouse query on any public path. At most 1,000 cache entries.
   - The read governor plus DB-backed limits. Public responses do not vary by query string or cookie, and private ones are never `public`.
8. **CSP.** Public HTML runs no script (`csr = false`); the CSP header is present on every status route; logos load with `no-referrer`.
9. **RBAC.** Route × role table (outsider, viewer, member, admin, global admin) matches R22. `tests/core/json_bodies.rs` lists every new state-changing route except the one-click route.

Then, all on `feat/statuspages-part1`:
1. The **fix wave**: one or more `fix(...)` commits with `(NAMI-SP-16)` in the message, for every Important finding and the Minor ones marked fix-now, each with a test.
2. A scoped re-review of the fixes, repeated until clean.
3. The decisions doc `docs/superpowers/decisions/<date>-status-pages-part1.md` recording R1 … R25, the controller rulings of 2026-10-19 and the deferred items (same format as the earlier logs).
4. The roadmap ticks in `docs/superpowers/roadmap.md` §5 for the rows of Roadmap coverage that Part 1 delivers in full: R5.01-R5.09 and R5.11-R5.14. R5.10, R5.15 and R5.16 stay unticked (partial; Part 2 finishes them).
5. Push, then open the one phase PR (purpose and scope, the task list NAMI-SP-1 .. NAMI-SP-16 with commit ranges, implementation summary, tests executed with results, breaking changes such as the switch to `@sveltejs/adapter-node` and its deployment variables, risks and follow-ups) and merge it once its review is clean (pre-authorized, CLAUDE.md). Then delete the branch and the worktree. Part 2 (NAMI-SP-17 onward) branches from the updated `origin/main`.

No CI is configured on this repo yet. The PR description states that and lists the engine and UI commands run with their results.
