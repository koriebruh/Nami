# Status Pages, Part 2: Implementation Plan

> **Status:** outline. Nothing of this phase is merged. It starts when Status Pages Part 1 (NAMI-SP-1 .. NAMI-SP-16) has merged; NAMI-SP-17 is first.
> **Branch:** `feat/statuspages-part2` in `.worktrees/statuspages-part2/`.
> **Depends on:** Status Pages Part 1 (`feat/statuspages-part1`, plan `docs/superpowers/plans/2026-10-13-status-pages-part1.md`) merged to `main` with its security review and fix wave, and through it Incidents Part 1 (the guarded outbound client of NAMI-INC-3, the signature helper of NAMI-INC-4, the delivery queue of NAMI-INC-5).

## Roadmap coverage

Roadmap §5 (`docs/superpowers/roadmap.md`). Part 1 delivers every other §5 item and the Part 1 halves of the rows below.

| Roadmap | Item | Tasks |
|---|---|---|
| R5.10 | **Part 2 half, the feature itself:** webhook subscribers (admin add, public form behind `allow_webhook_subscribers`, verification handshake, strict public-only outbound, sealed per-subscriber secret shown once, `X-Nami-Signature`, retries through the delivery queue, auto-disable after 20 failures, unsubscribe), events `report.created`, `report.updated`, `component.status_changed` | NAMI-SP-17, NAMI-SP-18, NAMI-SP-19, NAMI-SP-20, NAMI-SP-21, NAMI-SP-23, NAMI-SP-24 |
| R5.06 | **Webhook half:** webhook subscribers honour the per-update "notify subscribers" toggle and the 20-notified-updates cap | NAMI-SP-20 |
| R5.07 | **Webhook half:** automatic reports reach webhook subscribers when `automatic_reports_notify` is on | NAMI-SP-20 |
| R5.15 | **Part 2 half, the widget itself:** `widget.js` (Shadow DOM, constructable stylesheets, no cookies or storage, CSP-safe, 5 KiB gzip budget, DOM tests), served for public pages only | NAMI-SP-22, NAMI-SP-23 |
| R5.16 | **UI half:** the editor's webhook toggle and widget snippet, the admin webhook subscriber list, the public webhook subscribe form and the webhook unsubscribe page | NAMI-SP-23, NAMI-SP-24 |
| (phase) | Part 2 security review and fix wave | NAMI-SP-25 |

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. This is an **outline** plan (see `docs/superpowers/plan-conventions.md`): tasks, scope, data shapes, endpoints, files, tests and acceptance are fixed here; the step-level code is written at phase start against the then-current `main`, because Part 1 and Incidents Part 1 have not merged yet.

**Goal:** Finish roadmap §5. Add webhook subscribers to status pages, with a verification handshake, signed payloads and retries through the shared delivery queue; the `component.status_changed` event; the embeddable announcement widget (`widget.js`) with the editor's snippet; and the UI for all of it.

**Architecture:** Webhook subscribers are a new area `nami-engine/src/statuspages/webhook/` (`mod.rs` logic and sender, `model.rs` rows and SQL, `routes.rs` admin and public routes). Every outbound request goes through the public-only client of Incidents Part 1 (`outbound::public_client()`, `PublicOnlyResolver`, no proxy, no redirects, no idle pool, so each connect is resolved and judged again: DNS-rebinding safe) and is re-checked with `outbound::check_url_for_send(.., UrlRule::Https, Reach::PublicOnly, policy)` before every attempt. The handshake, the report events and the component events are rows in `notification_deliveries`, on the status-page branch that Part 1 added (priority 1, bucket `p:<page>`, the page cap), so retries, leases, poison rows and backoff are the Incidents Part 1 rules. Report events fan out from the same `notify::enqueue_update` call that queues subscriber mail, so the notify toggle, the 20-notified cap, component subsets and automatic reports apply to both kinds with one rule. Component changes are detected by a tick in the incidents worker loop, not by page views: it evaluates the components of pages that have an active webhook subscriber with the same status function the snapshot uses, records each change once (atomic compare-and-set, safe across instances) and queues one event per reached subscriber. The widget is one static script served by nami-ui at `/status/{slug}/widget.js` (and `/widget.js` on a custom domain); it reads the existing `status.json` with `credentials: "omit"` and renders into a closed Shadow DOM styled only through constructable stylesheets.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, tokio, reqwest 0.13, `hmac` 0.13 and `sha2` 0.11 through NAMI-INC-4's `format::hmac_hex`, `base64` 0.22, `rand` 0.8), Postgres 17, wiremock 0.6.5 (dev, present), SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest. **One new dev dependency:** `happy-dom` in nami-ui, for the widget's Shadow DOM tests (R-SP2-17). No new crate.

**Spec:** `docs/superpowers/specs/2026-10-13-status-pages-design.md`, including "Parity additions (2026-10-13)": sections "Subscribers" (webhook bullet), "Public JSON API and announcement widget", "Security requirements (tested)" (webhook, widget and DTO-leak bullets) and "Data model additions" (`status_page_webhook_subscribers`). Roadmap §5 is the definition of done.

**When to start:** after Status Pages Part 1 has merged to `main` with its fix wave. Every task builds on that `main`. Before NAMI-SP-17, re-read the merged code for each interface below and record any rename in the phase PR.

### Interfaces assumed from unmerged plans

If the merged code differs in a name, adapt minimally (same behaviour, same tests) and say so in the phase PR. If it differs in behaviour, stop and ask the controller.

| From | Interface | Used by |
|---|---|---|
| NAMI-INC-3 | The guarded outbound module `incidents::outbound` (planned in NAMI-INC-3 as `nami-engine/src/incidents/outbound.rs`; this plan writes `outbound::...` for it): `public_client()`, `is_non_public(ip)`, `public_addresses`, `PublicOnlyResolver`, `check_url(raw, rule, reach)`, `check_url_for_send(raw, rule, reach, policy)`, `describe_error(&reqwest::Error)`, `OutboundPolicy::{Strict, Relaxed}`, `Reach::PublicOnly`, `UrlRule::Https`, `OUTBOUND_TIMEOUT` (10 s), `CONNECT_TIMEOUT` (5 s). `Config.outbound_policy` is `Strict` in `from_env` and `Relaxed` only in `from_env_for_test`. | T17, T18, T19 |
| NAMI-INC-4 | `incidents::format::hmac_hex(secret: &[u8], bytes: &[u8]) -> String` (lowercase hex HMAC-SHA256). Not `format::signature`, which signs `{timestamp}.{body}` (R-SP2-6). | T19 |
| NAMI-INC-5 | `notification_deliveries`, the drain (claim (a)-(c), per-row steps (2a)-(2f), poison rows in (b)), `send::{SendError, Retry::{Never, After}}`, `MAX_ATTEMPTS = 5`, `backoff`, `incidents::worker::{kick, run_forever}`, `tests/common::run_incident_ticks`. | T19, T20, T21 |
| NAMI-SP-1, NAMI-SP-2 | `status_pages.allow_webhook_subscribers`, `status_page_components`, `statuspages::page::model::{find_by_slug, find_by_verified_domain, StatusPage}`, `tests/common::{seed_status_page, seed_component, seed_heartbeat}`. | T17, T18, T21 |
| NAMI-SP-4, NAMI-SP-6 | `statuspages::status::{ComponentStatus, derive, override, worst}` and the status step of `statuspages::snapshot::build` (step 2: one monitor query, one heartbeat query, `active_windows`, active-report impacts). `PublicDto.subscriptions.webhook`, `statuspages::snapshot::page_url`, the R10 rule that DTO writes bump `status_pages.updated_at`, `tests/common::call_raw`. | T18, T21 |
| NAMI-SP-7 | `statuspages::access::granted`, `statuspages::rate_limit::{hit, client_key}`, the fixed-body helper `json_error(status, &'static str)` in `nami-engine/src/statuspages/public/routes.rs`, the public status router and its read governor. | T18 |
| NAMI-SP-8 | `statuspages::automatic::{on_incident_opened, on_incident_resolved}`, which call `notify::enqueue_update` for the updates they post. | T20 |
| NAMI-SP-9 | `statuspages::tokens::{Purpose, sign, open}` (format `id 16 B, purpose 1 B, nonce 16 B, mac 32 B`), `statuspages::deliver::{send, Outcome, StatusPageRow, STATUS_PAGE_PER_MINUTE}`, the status-page branch of the drain, the check `notification_deliveries_row_kind`, the dedupe index on `(status_page_id, round, target_key)`, the hourly prune in the incidents worker, the shared subscribe buckets `subscribe-ip:{client_key}` (5 / h) and `subscribe-page:{page}` (100 / h). | T18, T19, T20 |
| NAMI-SP-10 | `statuspages::notify::enqueue_update(conn, update_id)`, `POST /public/status-subscriptions/{inspect, unsubscribe}`, `GET`/`DELETE /projects/{id}/status-pages/{page_id}/subscribers`. | T18, T20 |
| NAMI-SP-11, NAMI-SP-12 | The editor `routes/(app)/projects/[id=uuid]/status-pages/[pageId=uuid]/+page.*`, its "available soon" webhook toggle, `parsePageSettings` (`#lib/server/statuspages/status-page-form`), `publicUrl(page, origin)` (`#lib/statuspages/status-pages`), the subscribers page `.../subscribers/+page.*`. | T23 |
| NAMI-SP-13, NAMI-SP-14, NAMI-SP-15 | `callPublicEngine` (`#lib/server/core/engine`), `pageRef`, `enginePrefix`, `basePath`, `fetchPage`, `ACCESS_COOKIE` (`#lib/server/statuspages/status-public`), `hooks.ts` reroute (a custom domain's `/widget.js` becomes `/status/_domain/widget.js`), `hooks.server.ts` security headers, the public page and its `subscribe` action, the unsubscribe page, `status.json` and `statusJson(dto)`. | T22, T23, T24 |

Reused from `main` (`56916be`): `core::crypto::Cipher::{encrypt, decrypt, derive_subkey}` (AES-GCM with a per-row context string), `core::validate::{nullable, validate_label}`, `core::middleware::rbac::require_project_role`, `core::router::{build_router, with_fallback_connect_info}`, `uptime::checker::{is_unsafe_in_text, DESTINATION_NOT_ALLOWED}`, `nami-ui/src/lib/components/CopyButton.svelte`, `#lib/core/uuid` (`isUuid`), `#lib/server/core/fake-event` (`fakeEvent`), the design system in `nami-ui/src/lib/components/ui/`.

**Migrations.** Two phase files, numbered after the newest file in `nami-engine/migrations/` when the phase starts, in this order (the second references tables of the first):
1. `NNNN_statuspages_webhooks.sql` (created by T17, appended by T21): webhook subscribers, their component subsets, the component-change log and its columns.
2. `NNNN_incidents_status_page_webhooks.sql` (created by T19, appended by T21): the `notification_deliveries` extension. It replaces Part 1's `notification_deliveries_row_kind` check.

No ClickHouse DDL.

**Execution.** One branch `feat/statuspages-part2` in `.worktrees/statuspages-part2/`, from the latest `origin/main`. Tasks run in order, T17 to T25. Each task is one or more commits with its task ID (for example `feat(engine): webhook subscriber handshake and delivery (NAMI-SP-19)`), then a task review with fix commits on the same branch. T22 (widget) depends only on Part 1 and may run in a parallel worktree on a short-lived local branch, merged back before T23. After T24: the phase security review and fix wave (T25), the decisions doc, the roadmap ticks and one phase PR (see `CLAUDE.md`, "Per-phase flow"). No CI is configured on this repo yet; the PR lists the local test runs.

Engine command: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test statuspages <file>::` (unit tests: `cargo test --lib -- statuspages::webhook`). Engine tests need `docker compose up -d postgres mailpit`. UI command: `cd nami-ui && npm run check && npx vitest run <files>` (0 errors, 0 warnings).

## Global Constraints

- Everything in Part 1's Global Constraints still holds: SQLx runtime queries with literal SQL, axum `{param}` paths, 403 for authorization failures, 404 for a foreign id inside an accessible project path, the same fixed 404 body for an unknown, unverified or locked public page, `require_json_body` on every state-changing route (each new one is added to `tests/core/json_bodies.rs::every_state_changing_route_refuses_bodies_that_are_not_json`), every new table added to `tests/core/migrations.rs::all_tables_exist`, fixed public error bodies through `json_error`, and the public-output ban list (no monitor or heartbeat names unless typed as a display name, no URLs, hosts, ports, keywords, headers, causes, incident titles or details, no project, monitor, heartbeat, incident or user ids, no member or subscriber emails).
- Spec values, verbatim:
  - webhook URLs are `https:` only, delivered in strict outbound mode: public unicast addresses only whatever the operator's outbound setting, checked after DNS resolution, redirects not followed;
  - activation needs the handshake: Nami POSTs `{"type": "verification", "challenge": ...}` and the endpoint must echo `challenge` within 10 s; nothing else is delivered before that;
  - event body `{event_id, event: report.created | report.updated | component.status_changed, page: {slug, title, url}, report?, update?, component?, sent_at, unsubscribe_url}`;
  - `X-Nami-Signature: sha256=<HMAC(secret, body)>` with a per-subscriber secret shown once;
  - the same delivery queue and retries as emails; 20 consecutive failed deliveries disable the subscriber;
  - the same subscribe rate limits as email (5 / hour / IP, per page);
  - widget: `<script src="https://<host>/status/{slug}/widget.js" async></script>`, a small banner while an incident or maintenance is active, linking to the page; fetches `status.json` with `credentials: "omit"`; no cookies or storage; no `eval`, `new Function` or inline scripts; text through `textContent` only; Shadow DOM styled through constructable stylesheets; runs under a CSP that allows the status host in `script-src` and `connect-src`; 5 KiB gzip;
  - private pages have no widget and no cross-origin JSON.
- Roles (Part 1 R22 extended): webhook subscriber list, add, re-verify and remove are **admin** only, like email subscribers. Viewers and members get 403. The public form needs no session.
- Secrets: a webhook secret is 32 random bytes from `rand::rngs::OsRng`, shown as unpadded base64url (43 chars), sealed with `Cipher::encrypt` under the per-row context `status_page_webhook_subscribers.secret:{id}`. It is returned exactly once, in the 201/202 body that creates the subscriber, and never logged, never in a `Debug` output (types that hold it implement `Debug` by hand), never in an error. A webhook URL is never logged; logs carry the subscriber id only.
- Outbound: every webhook request (handshake and events) goes through `outbound::public_client()` after `outbound::check_url_for_send(url, UrlRule::Https, Reach::PublicOnly, state.config.outbound_policy)`. Never `outbound::client()`, never the monitors' client. No operator setting widens it (R-SP2-2). The response body is read only up to a cap and never stored.
- Every new limit is a `pub const` next to the type it bounds and has a test at the bound and one past it.
- nami-ui: Part 1's rules (`callEngine` / `callPublicEngine`, `enginePath`, `[...=uuid]` matchers and `isUuid`, `engineFailure`, two-step deletes with `confirm=yes`, times in UTC, design tokens and `#lib/components/ui` components, no raw hex in pages). The widget is not a page: it renders on foreign sites where Nami's tokens do not exist (R-SP2-16).
- Writing style in code and messages: no long dashes in user-facing text (a plain hyphen or a colon).

## Review Focus

- **A webhook must never reach an internal service.** Example: the public form with `https://localhost/hook`, `https://10.0.0.5/hook`, `https://[::ffff:127.0.0.1]/`, `https://169.254.169.254/latest/meta-data`, a name that resolves to `127.0.0.1`, or a name that resolves to a public address at subscribe time and to `10.0.0.5` at delivery (rebinding). Each is refused at subscribe time (literal or resolved) or at connect time (`PublicOnlyResolver`), and nothing about the internal service reaches the caller → T17 (`a_webhook_url_must_resolve_to_public_addresses_only`), T19 (`a_name_that_rebinds_to_loopback_is_refused_at_send_time`).
- **A redirect must never be followed.** Example: the endpoint answers `302 Location: http://127.0.0.1/`. The attempt fails, the location is never requested → T19 (`a_redirect_is_a_failed_delivery_and_is_not_followed`).
- **Nothing before the challenge.** Example: a report update is posted while a subscriber is still unverified, then the subscriber verifies. No row is queued for it, and a row queued by a race is cancelled at send time → T20 (`an_unverified_subscriber_receives_no_event`), T19 (`an_event_row_for_an_unverified_subscriber_is_cancelled`).
- **The secret is shown once.** Example: create a subscriber, then list, read and re-verify it. Only the create response has `secret`; the database holds only the sealed bytes → T17 (`the_secret_is_returned_once_and_stored_sealed`).
- **Payloads leak nothing.** Example: Part 1's leak fixture (monitor URL with a token, keyword, basic-auth user, proxy, incident cause and postmortem) with an automatic report and a component change. Neither the `report.*` nor the `component.status_changed` body contains any of them → T20 (`report_events_leak_nothing_private`), T21 (`component_events_leak_nothing_private`).
- **A visitor must not hijack a verified subscription.** Example: a second public subscribe of a URL that is already verified. It answers 409 and never rotates or reveals the secret → T18 (`a_verified_url_cannot_be_subscribed_again`).
- **The widget renders report titles as text.** Example: an active report titled `<img src=x onerror=alert(1)>`. The banner shows the literal text, no element is created from it, no cookie or storage is touched → T22 (`report_titles_render_as_text`, `the_widget_sets_no_cookie_and_uses_no_storage`).

## Rulings (decisions the spec leaves open)

- **R-SP2-1 Scope and numbering.** This part is exactly what Part 1 lists under "Part 2 follows": webhook subscribers, `component.status_changed`, the widget and its snippet, the UI for them, and the phase security review. Part 1 took NAMI-SP-16 for its own security review and fix wave, so Part 2 starts at NAMI-SP-17. No other business feature is added.
- **R-SP2-2 Strict outbound, always.** Webhook subscribers are created by anonymous visitors, so they get the strictest reach Incidents Part 1 has: `Reach::PublicOnly` with `UrlRule::Https`, sent with `outbound::public_client()`. The spec's "whatever the operator's outbound setting" means that no operator configuration (today none exists; Platform may add one) is ever consulted for them: the call sites pass the fixed `Reach::PublicOnly` and never a configurable reach. The one seam is Incidents Part 1's test-only `OutboundPolicy::Relaxed` (set only by `Config::from_env_for_test`, never by an environment variable), under which the send-time check skips the https and public-only rules so tests can deliver to wiremock on `127.0.0.1`. API validation (admin add, public form) is always strict whatever the policy; delivery tests insert subscribers through the model (`common::seed_webhook_subscriber`), and one test per refusal sets `state.config.outbound_policy = Strict`.
- **R-SP2-3 Checked at subscribe time and at every delivery.** At subscribe time the URL must parse, be `https:`, have a host, carry no userinfo and no fragment, be at most `MAX_URL_LEN = 2048` bytes, and pass `outbound::check_url(.., UrlRule::Https, Reach::PublicOnly)` (an IP literal must be public). A name is then resolved once (`tokio::net::lookup_host`, `RESOLVE_TIMEOUT = 5 s`, at most `RESOLVE_SLOTS = 8` resolutions in flight per process, more → 429). Every resolved address must pass `!outbound::is_non_public` (one non-public address refuses the URL: `400 webhook URLs must resolve to public addresses only`); no address or a timeout → `400 the webhook host could not be resolved`. At every attempt `check_url_for_send` runs again and `PublicOnlyResolver` drops every non-public address at connect time. Because the idle pool is off, each attempt resolves and connects in one step to an address the resolver just vetted, so a DNS rebind between subscribe and delivery is refused at connect time.
- **R-SP2-4 The handshake is a queued delivery.** Creating a subscriber (admin or public) inserts it unverified with a fresh `challenge` (32 random bytes, base64url, 43 chars) and queues one `sp_wh_verify` row in the same transaction. The request never waits for the endpoint, so the public form is not an outbound probe with a visible result. The drain POSTs `{"type": "verification", "challenge": "<43 chars>", "page": {"slug", "title", "url"}}`, signed like every event, within `outbound::OUTBOUND_TIMEOUT` (10 s, the spec's limit). The echo is accepted when the status is 2xx and the body (read up to `MAX_ECHO_BYTES = 4096`) is either exactly the challenge after trimming ASCII whitespace or a JSON object whose `challenge` equals it. Accepted → `verified_at = now()`, `challenge = null`. A 2xx without the echo → `Retry::Never` ("challenge not echoed"). Transport errors, 408, 429 and 5xx retry under the queue's rules. Unverified subscribers that were never disabled and are older than `PENDING_TTL = 48 h` are deleted by Part 1's hourly prune. An admin can re-send the handshake (new challenge, same secret) for a pending or a disabled subscriber. A disabled subscriber keeps `verified_at` while its new challenge is out, so the check allows a challenge on a verified row only while `disabled_at` is set, and the prune never deletes it. The handshake row of a disabled subscriber is the one row `send` does not cancel for being disabled; its success re-enables the subscriber (R-SP2-9).
- **R-SP2-5 The secret.** One per subscriber (format and sealing in Global Constraints, "Secrets"), created with the row and returned once. Re-verification keeps it. A replaced row (R-SP2-7) gets a new one.
- **R-SP2-6 The signature is over the body only.** The spec fixes `X-Nami-Signature: sha256=<HMAC(secret, body)>`, so the header is `"sha256=" + format::hmac_hex(secret, body)` over the exact bytes sent. This differs from Incidents Part 1's channel webhooks (`format::signature`, which signs `{timestamp}.{body}` and adds `X-Nami-Timestamp`): status-page receivers get replay protection from the body instead, through `event_id` (the delivery row id, stable across retries, so receivers drop repeats) and `sent_at` (the attempt time). Other headers: `content-type: application/json`, `x-nami-event: <event or "verification">`, and the client's `user-agent`.
- **R-SP2-7 One subscription per URL per page.** `unique (status_page_id, url)`. A subscribe (admin or public) for a URL that is verified and not disabled → `409 this URL is already subscribed to this page`; the secret is never rotated or revealed. A URL whose row is unverified or disabled is **replaced** (the old row is deleted with its queued rows, a new row with a new secret and challenge is inserted). So a squatter's pending row blocks the URL's owner for at most the time until the owner subscribes again, and a verified subscription can only be removed by its unsubscribe link or an admin. Knowing whether a URL is subscribed requires knowing the full URL, which usually carries the receiver's own credential, so the 409 is accepted.
- **R-SP2-8 Caps and rate limits.** Per page at most `MAX_ACTIVE = 500` active (verified, not disabled) and `MAX_PENDING = 50` unverified webhook subscribers. When either is reached, every webhook subscribe to that page answers `429 this page is not accepting new webhook subscriptions right now`, checked before the URL is looked up (as Part 1's R15). The public form shares Part 1's buckets `subscribe-ip:{client_key}` (5 / h) and `subscribe-page:{page}` (100 / h) with email subscribe, so a visitor gets 5 subscriptions an hour across both kinds. Admin adds are not rate limited (admin role) but count against the caps.
- **R-SP2-9 Counting failures.** A *delivery* is one event row (not the handshake). It is failed when it ends `failed`: `Retry::Never`, the last attempt (`attempts = MAX_ATTEMPTS`), or Incidents Part 1's poison-row step (b). Each failed delivery adds 1 to `consecutive_failures`; a sent one resets it to 0. At `MAX_CONSECUTIVE_FAILURES = 20` the subscriber gets `disabled_at = now()` and its pending rows are cancelled, in the same transaction. Every outcome also stores `last_error` (the fixed texts of `outbound::describe_error`, `"redirects are not followed"`, `"challenge not echoed"` or `"destination not allowed"`, never a body or URL) and `last_attempt_at` for the admin list. A re-verification that succeeds clears `disabled_at` and the counter.
- **R-SP2-10 Report events.** `notify::enqueue_update` (Part 1) queues one `sp_wh_report` row per active subscriber the update reaches, in the same statement batch as the email rows, under the same conditions: the update has `notify_subscribers = true`, and the subscriber follows all components, or the report has no affected components, or one of the report's components is in the subscriber's subset. So the per-update toggle, `automatic_reports_notify` and the 20-notified-updates cap apply unchanged. The event name is decided at send time: the report's first update (oldest by `created_at, id`) is `report.created`, every later one `report.updated`.
- **R-SP2-11 Component changes come from a worker tick, not page views.** Part 1's note says "the snapshot builder records each component's last published status", but the snapshot is built only when someone views the page, so a change on an unviewed page would never be sent. Instead a tick in the incidents worker loop (`COMPONENT_TICK = 30 s`) claims pages that have at least one active webhook subscriber (`for update skip locked`, at most `PAGES_PER_TICK = 50` pages, oldest `component_states_at` first), computes every component's status with the snapshot's status step (factored into `statuspages::status::current`, so both use one function), and compares it with `status_page_components.published_status`.
  - Changed → one `status_page_component_changes` row and one `sp_wh_component` row per reached subscriber (all components, or the component in the subset), set by `update ... where published_status is distinct from $new returning`, so two instances never record one change twice.
  - First observation (`published_status` null) or a stale page (`component_states_at` null or older than `STALE_AFTER = 5 min`, for example after every subscriber was disabled) → the statuses are stored without an event, so turning the feature on never sends a burst.
  - The tick updates `status_pages.component_states_at` only, never `updated_at`, so it does not invalidate the snapshot cache.
  - A flapping monitor can produce at most one event per component per tick (2 per minute); the page bucket cap `STATUS_PAGE_PER_MINUTE` bounds the rest. Change rows older than `CHANGE_RETENTION = 7 days` are deleted by the hourly prune (their queued rows cascade).
  - Part 1's `INCIDENTS_WORKER_ENABLED=false` stops the tick too.
- **R-SP2-12 Payloads hold public DTO fields only.**
  - `page`: `{slug, title, url}` (`snapshot::page_url`).
  - `report`: `{id, title, kind, status, impact, starts_at, ends_at, published_at, updated_at, url, components: [{id, name, impact}]}` with `url = {page url}#report-{id}`.
  - `update`: `{id, status, message, created_at}`.
  - `component`: `{id, name, section, status, previous_status, changed_at}`.
  - `unsubscribe_url`: `{page url}/unsubscribe?token=<WebhookUnsubscribe token>`.
  - `sent_at`: the attempt time. Optional keys are omitted, never `null`, when the event has no such object.
  - Every value is read when the row is sent (a renamed component sends its new name; a deleted report or component cancels the row through the cascade).
- **R-SP2-13 Unsubscribing a webhook.** Part 1's subscriber tokens get one more purpose, `WebhookUnsubscribe = 4`, whose id is the webhook subscriber's and whose nonce is its `token_nonce`. Part 1's `inspect` and `unsubscribe` routes accept it (inspect answers `{"purpose": "webhook_unsubscribe", "page": {...}, "host": "<url host>"}`). A token of one kind never opens a row of the other table, because the purpose byte is under the MAC and selects the table. The nami-ui unsubscribe page shows "Stop sending webhooks for {title} to {host}?" with one POST button; GET never mutates. The host is shown in full (the link holder is the receiver). There is no manage page for webhooks: to change components, unsubscribe and subscribe again (or an admin removes and re-adds).
- **R-SP2-14 The public form.** `POST .../webhook-subscribe` exists for slug and verified domain, needs page access exactly like email subscribe (a private page's form needs the cookie or the allowlist), and answers the fixed 404 when `allow_webhook_subscribers` is off (the DTO already says `subscriptions.webhook: false`, so the 404 reveals nothing). Admins can add webhooks whatever the toggle. `subscriptions.webhook` in the DTO becomes `page.allow_webhook_subscribers`.
- **R-SP2-15 Admin view.** The list shows `host` (never the full URL after creation: it often embeds the receiver's credential), components, `state` (`pending` | `active` | `disabled`), `consecutive_failures`, `last_error`, `last_attempt_at`, `created_at`, and whether an admin or the public form created it. Admins can remove and re-verify. No edit route.
- **R-SP2-16 The widget.**
  - One static file, `nami-ui/src/lib/statuspages/widget.js`, served unchanged for every page, so its ETag is fixed and it is cacheable (`public, max-age=300`).
  - It derives `status.json` from its own URL (`new URL('status.json', document.currentScript.src)`), so it works on the app host (`/status/{slug}/widget.js`) and on a custom domain (`/widget.js`). No `currentScript` (a module context) → it does nothing.
  - It fetches once on load and every `WIDGET_POLL = 5 min` while `document.visibilityState === 'visible'`, with `credentials: 'omit'`, `referrerPolicy: 'no-referrer'`.
  - It shows the first active report (incidents before maintenance): its title, its status label and a "View status" link to `page.url` (only an `https:` or `http:` URL, checked with `new URL`), plus a dismiss button that hides it for this page view. Nothing is stored.
  - It renders into a closed Shadow DOM on a `div` appended to `document.body`, styled by one `CSSStyleSheet` (`replaceSync`) in `adoptedStyleSheets`. Without constructable stylesheet support it renders nothing (a `<style>` element or `style` attribute would break under a strict `style-src`).
  - Its colours are literals copied from the `layout.css` tokens (`--color-danger`, `--color-warning`, `--color-info`, `--color-surface`, `--color-ink`), with a comment naming each token, because Nami's tokens do not exist on the host page.
  - The route answers 404 for an unknown or private page (R25 of Part 1: no widget on private pages).
- **R-SP2-17 DOM tests need a DOM.** nami-ui has no DOM library; the widget tests need `attachShadow`, `CSSStyleSheet.replaceSync` and `adoptedStyleSheets`. Add `happy-dom` as a dev dependency (`npm install -D --save-exact happy-dom@<release current at phase start>`; record the version in the decisions doc), used only by the widget test file through `// @vitest-environment happy-dom`, so every other suite keeps the node environment. The test runs the widget source with `window.Function` after stubbing `document.currentScript`; that is test-only, the widget itself never evaluates code.
- **R-SP2-18 Snippet.** The editor shows `<script src="{publicUrl(page, origin)}/widget.js" async></script>` with `CopyButton` for public pages only. A private page shows "Private pages have no widget." instead.
- **R-SP2-19 The secret on the public page.** The public form is a form action on a `csr = false` page. Its response carries the secret, so the page's `load` sets `cache-control: private, no-store` (not Part 1's `public, max-age=30`) whenever `request.method !== 'GET'`, and the secret is shown once in a `CopyButton` box with "Shown once. Store it now.", never placed in a URL or a redirect.
- **R-SP2-20 Queue rows.** Events `sp_wh_verify`, `sp_wh_report`, `sp_wh_component`, priority 1, bucket `p:<status_page_id>` (shared with email, cap `STATUS_PAGE_PER_MINUTE = 600`). `target_key = 'wh:' || webhook_subscriber_id`. `round`: the update id in simple hex for `sp_wh_report` (the same round as the email rows of that update; the `target_key` differs), the change id in simple hex for `sp_wh_component`, and `v-{verify_sends}` for `sp_wh_verify`, all within the 1-32 chars `round` check.

## File Structure

```
nami-engine/
  migrations/
    NNNN_statuspages_webhooks.sql               T17 subscribers + components; T21 component changes
    NNNN_incidents_status_page_webhooks.sql     T19 queue extension; T21 change column
  src/statuspages/
    mod.rs                                      T17 pub mod webhook; T21 pub mod changes
    webhook/mod.rs                              T17 URL rules, secret, caps; T19 send, handshake; T20/T21 payloads
    webhook/model.rs                            T17 rows and SQL; T19 failure counter
    webhook/routes.rs                           T17 admin routes; T18 public subscribe
    changes.rs                                  T21 component tick
    status.rs                                   T21 `current` (factored out of snapshot step 2)
    snapshot.rs                                 T18 subscriptions.webhook; T21 uses status::current
    tokens.rs                                   T18 Purpose::WebhookUnsubscribe
    deliver.rs                                  T19 dispatch of webhook rows
    notify.rs                                   T20 webhook rows in enqueue_update
    subscriber/routes.rs                        T18 inspect/unsubscribe accept the webhook purpose
  src/incidents/deliveries.rs                   T19 claimed-row field, failure hook in (b) and (2f)
  src/incidents/worker.rs                       T19 prune of unverified rows; T21 component tick
  src/core/router.rs                            T17 admin routes; T18 public routes
  tests/
    common/mod.rs                               T17 seed_webhook_subscriber
    statuspages/main.rs                         T17 mod webhook_subscribers; T19 mod webhook_delivery; T21 mod component_changes
    statuspages/webhook_subscribers.rs          T17, T18
    statuspages/webhook_delivery.rs             T19, T20
    statuspages/component_changes.rs            T21
    core/migrations.rs, core/json_bodies.rs     T17, T18, T19, T21

nami-ui/
  package.json                                  T22 happy-dom (dev, exact version)
  src/lib/statuspages/widget.js                 T22 the widget
  src/lib/statuspages/widget.test.ts            T22 DOM, size and CSP tests
  src/lib/statuspages/status-pages.ts (+ test)  T23 widgetSnippet, webhook types
  src/lib/server/statuspages/webhook-form.ts (+ test)   T23, T24 admin and public form parsing
  src/routes/status/[slug]/widget.js/+server.ts T22
  src/routes/status/[slug]/widget-route.test.ts T22 route tests
  src/routes/(app)/projects/[id=uuid]/status-pages/[pageId=uuid]/
    +page.server.ts, +page.svelte               T23 webhook toggle, widget snippet
    subscribers/+page.server.ts, +page.svelte   T23 webhook list, add (secret once), re-verify, remove
    subscribers/webhooks.test.ts                T23
  src/routes/status/[slug]/
    +page.server.ts, +page.svelte               T24 webhook subscribe action and form
    unsubscribe/+page.server.ts, +page.svelte   T24 webhook purpose
    webhook-subscribe.test.ts                   T24
```

The Part 1 module, test and route names above are the ones its plan gives (planned in NAMI-SP-1 .. NAMI-SP-15); use the names it merges with.

---

### Task 17 (NAMI-SP-17): Webhook subscriber schema, URL rules, sealed secret and admin API

**Covers:** R5.10 (admin half: add, list, re-verify, remove).  **Depends on:** Status Pages Part 1, NAMI-INC-3.

**Files:**
- Create: `nami-engine/migrations/NNNN_statuspages_webhooks.sql`, `nami-engine/src/statuspages/webhook/{mod,model,routes}.rs`, `nami-engine/tests/statuspages/webhook_subscribers.rs`.
- Modify: `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/core/router.rs`, `nami-engine/tests/statuspages/main.rs`, `nami-engine/tests/common/mod.rs` (`seed_webhook_subscriber(pool, cipher, page, url, verified: bool) -> (Uuid, String)`, inserting through the model without validation, so tests can point at wiremock), `nami-engine/tests/core/migrations.rs`, `nami-engine/tests/core/json_bodies.rs`.

**Data shape:**

```sql
-- Webhook subscribers of status pages (NAMI-SP-17). The secret is sealed
-- (context status_page_webhook_subscribers.secret:{id}) and shown once.
create table status_page_webhook_subscribers (
    id uuid primary key,
    status_page_id uuid not null,
    url text not null,
    host text not null,
    secret_enc bytea not null,
    all_components boolean not null default true,
    token_nonce bytea not null,
    challenge text,
    verify_sends int not null default 0,
    verified_at timestamptz,
    consecutive_failures int not null default 0,
    disabled_at timestamptz,
    last_error text,
    last_attempt_at timestamptz,
    created_by uuid,
    created_at timestamptz not null default now(),
    constraint status_page_webhook_subscribers_page_fk foreign key (status_page_id)
        references status_pages(id) on delete cascade,
    constraint status_page_webhook_subscribers_created_by_fk foreign key (created_by)
        references users(id) on delete set null,
    constraint status_page_webhook_subscribers_url_check
        check (char_length(url) between 9 and 2048 and url like 'https://%'),
    constraint status_page_webhook_subscribers_host_check check (char_length(host) between 1 and 253),
    constraint status_page_webhook_subscribers_nonce_check check (octet_length(token_nonce) = 16),
    constraint status_page_webhook_subscribers_challenge_check
        check (challenge is null or char_length(challenge) = 43),
    -- A challenge is out only while unverified, or while a disabled subscriber is re-verified (R-SP2-4).
    constraint status_page_webhook_subscribers_verified_check
        check (verified_at is null or challenge is null or disabled_at is not null),
    constraint status_page_webhook_subscribers_failures_check check (consecutive_failures >= 0),
    constraint status_page_webhook_subscribers_error_check
        check (last_error is null or char_length(last_error) <= 200),
    constraint status_page_webhook_subscribers_url_unique unique (status_page_id, url)
);
create index status_page_webhook_subscribers_active_idx on status_page_webhook_subscribers (status_page_id)
    where verified_at is not null and disabled_at is null;
create index status_page_webhook_subscribers_pending_idx on status_page_webhook_subscribers (created_at)
    where verified_at is null;

-- The components a subset webhook subscriber follows (all_components = false).
create table status_page_webhook_subscriber_components (
    webhook_subscriber_id uuid not null,
    component_id uuid not null,
    constraint status_page_webhook_subscriber_components_pk primary key (webhook_subscriber_id, component_id),
    constraint status_page_webhook_subscriber_components_subscriber_fk foreign key (webhook_subscriber_id)
        references status_page_webhook_subscribers(id) on delete cascade,
    constraint status_page_webhook_subscriber_components_component_fk foreign key (component_id)
        references status_page_components(id) on delete cascade
);
create index status_page_webhook_subscriber_components_component_idx
    on status_page_webhook_subscriber_components (component_id);
```

The `id` has no database default: it is generated in Rust before the insert, because the secret's seal context names it.

```rust
// statuspages::webhook
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_COMPONENTS: usize = 100;
pub const MAX_ACTIVE: i64 = 500;
pub const MAX_PENDING: i64 = 50;
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
pub const RESOLVE_SLOTS: usize = 8;
pub const PENDING_TTL: TimeDelta = TimeDelta::hours(48);
pub fn secret_context(id: Uuid) -> String;                 // "status_page_webhook_subscribers.secret:{id}"
/// R-SP2-3 static rules; Ok carries the parsed URL and its lowercased host.
pub fn validate_url(raw: &str) -> Result<(reqwest::Url, String), &'static str>;
/// R-SP2-3 resolution of a name (an IP literal passes through after `validate_url`).
pub async fn vet_destination(url: &reqwest::Url) -> Result<(), Vetting>;
pub enum Vetting { Refused(&'static str), Busy }
/// Derived from the row, never stored as text: `disabled_at` set → Disabled, else `verified_at` set → Active, else Pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)] #[serde(rename_all = "snake_case")]
pub enum State { Pending, Active, Disabled }
/// The secret on its way to the one response that shows it. `Debug` by hand, printing nothing.
pub struct NewSecret(zeroize::Zeroizing<String>);

// statuspages::webhook::model
#[derive(Debug, Clone, Serialize)]
pub struct WebhookSubscriberView { pub id: Uuid, pub host: String, pub state: State, pub all_components: bool,
    pub components: Vec<Uuid>, pub consecutive_failures: i32, pub last_error: Option<String>,
    pub last_attempt_at: Option<DateTime<Utc>>, pub created_at: DateTime<Utc>, pub created_by_admin: bool }
pub enum Created { Created { view: WebhookSubscriberView, secret: NewSecret }, AlreadySubscribed, Full }
/// One transaction: lock the page row (`select ... from status_pages where id = $1 for update`), count against
/// the caps (R-SP2-8), replace-or-insert (R-SP2-7), then the `sp_wh_verify` row (T19). The lock makes two
/// concurrent subscribes at `MAX_ACTIVE - 1` admit one.
pub async fn create(conn: &mut PgConnection, cipher: &Cipher, page_id: Uuid, url: &str, host: &str,
    components: Option<&[Uuid]>, created_by: Option<Uuid>) -> Result<Created, sqlx::Error>;
pub async fn list(pool: &PgPool, page_id: Uuid) -> Result<Vec<WebhookSubscriberView>, sqlx::Error>;
pub async fn reverify(conn: &mut PgConnection, page_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>;
pub async fn remove(pool: &PgPool, page_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>;
```

Request body (admin and public): `#[serde(deny_unknown_fields)] struct WebhookSubscribeBody { url: String, components: Option<Vec<Uuid>> }`. `components` null means all; otherwise 1 to `MAX_COMPONENTS` distinct ids of components on this page.

**API** (admin only, `require_project_role(.., Role::Admin)` first; a page or subscriber of another project → 404):

| Method and path | Body | Answer |
|---|---|---|
| `GET /projects/{id}/status-pages/{page_id}/webhook-subscribers` | | 200 `[WebhookSubscriberView]` |
| `POST /projects/{id}/status-pages/{page_id}/webhook-subscribers` | `WebhookSubscribeBody` | 201 view plus `"secret": "<43 chars>"`; 400 URL or component; 409 verified duplicate; 429 cap or `Busy` |
| `POST /projects/{id}/status-pages/{page_id}/webhook-subscribers/{sub_id}/verify` | `{}` | 202 `{"ok": true}`; 404 foreign id |
| `DELETE /projects/{id}/status-pages/{page_id}/webhook-subscribers/{sub_id}` | | 204; 404 foreign id |

**Behaviour:**
1. Order in `POST`: role, page, body (`validate_url`; components on this page, else `400 component not found on this page`), caps (`Full` → `429 this page is not accepting new webhook subscriptions right now`), `vet_destination` (`Refused(msg)` → 400, `Busy` → `429 too many subscriptions, try again later`), `create`, then (from T19) `kick` after commit.
2. `validate_url` refuses, with a fixed message each: not `https:` (`webhook URLs must use https`), userinfo or a fragment (`webhook URLs must not contain credentials or a fragment`), no host, more than `MAX_URL_LEN` bytes (`webhook URL is too long`), control or invisible characters (`core::validate` rules), and a non-public IP literal through `outbound::check_url(.., UrlRule::Https, Reach::PublicOnly)` (`webhook URLs must resolve to public addresses only`). The stored URL is the parsed URL's serialisation; `host` is its lowercased host without brackets.
3. `vet_destination` follows R-SP2-3 with `RESOLVE_TIMEOUT` and a process-wide `static RESOLVE: Semaphore = Semaphore::const_new(RESOLVE_SLOTS)` taken with `try_acquire`. The lookup is a private function argument (`tokio::net::lookup_host` in production), so a unit test can pass one that never answers.
4. The secret and `token_nonce` come from `OsRng`. The secret is sealed under `secret_context(id)` and returned only in the create response. `list` never selects `secret_enc` or `url`.
5. `reverify` on an active subscriber changes nothing and still answers 202. On a pending or disabled one it sets a new challenge and increments `verify_sends`, leaving `verified_at` and `disabled_at` as they are (the check allows it, R-SP2-4); the `sp_wh_verify` row is added in T19 (this task leaves a marked seam, "planned in NAMI-SP-19").
6. `remove` deletes the row; its queued rows cascade once T19 adds the column.

**Tests** (`tests/statuspages/webhook_subscribers.rs`; unit tests in `webhook/mod.rs`):
- `an_admin_adds_a_webhook_and_sees_the_secret_once` (201 with `secret`; `GET` then has no `secret` and no `url` key, only `host`).
- `the_secret_is_returned_once_and_stored_sealed` (the column differs from the secret; `Cipher::decrypt` under `secret_context(id)` returns it; under another id's context it fails).
- `members_and_viewers_cannot_list_add_verify_or_remove_webhooks` (403 for a member, the highest role that fails; an admin passes; a global admin without membership passes).
- `a_foreign_page_or_subscriber_id_is_a_404`.
- `webhook_urls_must_be_https_without_credentials` (`http://`, `https://u:p@x.test/`, `ftp://`, `javascript:alert(1)`, a fragment; a URL of `MAX_URL_LEN` bytes passes, `MAX_URL_LEN + 1` fails).
- `a_webhook_url_must_resolve_to_public_addresses_only` (literals `127.0.0.1`, `10.0.0.5`, `172.16.0.1`, `192.168.1.1`, `0.0.0.0`, `100.64.0.1`, `198.18.0.1`, `192.0.2.1`, `169.254.169.254`, `224.0.0.1`, `255.255.255.255`, `[::1]`, `[fe80::1]`, `[fd00::1]`, `[ff02::1]`, `[2001:db8::1]`, `[::ffff:127.0.0.1]`, `[64:ff9b::a00:5]`, and the name `localhost`: each 400 under the default test policy, because API validation is always strict; one public literal such as `1.1.1.1` passes).
- `resolution_gives_up_after_the_timeout` (unit, through a resolver seam that never answers: `Refused("the webhook host could not be resolved")` after `RESOLVE_TIMEOUT`, under paused tokio time).
- `an_unresolvable_host_is_a_400` (a `.invalid` name).
- `a_verified_url_cannot_be_added_twice` (409, secret unchanged) and `a_pending_or_disabled_url_is_replaced_with_a_new_secret`.
- `the_page_caps_refuse_every_new_webhook` (`MAX_ACTIVE` active rows: the next add is 429 for a new and for a known URL; the same at `MAX_PENDING`; one below each passes).
- `concurrent_subscribes_at_the_cap_admit_one` (`MAX_ACTIVE - 1` active rows, two creates at once: one 201, one 429).
- `resolution_slots_are_bounded` (unit: with `RESOLVE_SLOTS` permits held, `vet_destination` answers `Busy`).
- `components_must_belong_to_the_page` and `an_empty_or_oversized_component_list_is_a_400` (0 ids, `MAX_COMPONENTS + 1` ids; `MAX_COMPONENTS` passes).
- `removing_a_page_or_component_cascades` (rows and subset rows go).
- Migrations: both tables in `all_tables_exist`; `a_webhook_row_needs_https_and_a_16_byte_nonce` (`23514`). The two state-changing routes in `json_bodies.rs`.

**Acceptance:** an admin can add, list, re-verify and remove webhook subscribers of a page; the secret appears once; every refused URL above is a 400; `cargo test --test statuspages webhook_subscribers::` is green.

---

### Task 18 (NAMI-SP-18): Public webhook subscribe, DTO flag and webhook unsubscribe tokens

**Covers:** R5.10 (public half: form endpoint and unsubscribe).  **Depends on:** NAMI-SP-17.

**Files:**
- Modify: `nami-engine/src/statuspages/webhook/routes.rs` (public routes), `nami-engine/src/statuspages/public/routes.rs` (mount on the public status router), `nami-engine/src/statuspages/snapshot.rs` (`subscriptions.webhook`), `nami-engine/src/statuspages/tokens.rs` (`Purpose::WebhookUnsubscribe = 4`), `nami-engine/src/statuspages/subscriber/routes.rs` (inspect, unsubscribe), `nami-engine/src/core/router.rs`, `nami-engine/tests/statuspages/webhook_subscribers.rs`, `nami-engine/tests/core/json_bodies.rs`.

**API** (private API, no session, behind Part 1's public status router and its governor):
- `POST /public/status-pages/{slug}/webhook-subscribe` and `POST /public/status-domains/{host}/webhook-subscribe`, body `WebhookSubscribeBody`:
  - 202 `{"ok": true, "secret": "<43 chars>", "state": "pending"}`;
  - 400 the URL or component message; 404 the fixed body (unknown, locked, or `allow_webhook_subscribers` off); 409 `this URL is already subscribed to this page`; 429 `too many subscriptions, try again later` (buckets or `Busy`) or the cap text (R-SP2-8).
- `POST /public/status-subscriptions/inspect {token}` with a `WebhookUnsubscribe` token → 200 `{"purpose": "webhook_unsubscribe", "page": {"title", "url"}, "host": "hooks.example.test"}`.
- `POST /public/status-subscriptions/unsubscribe {token}` with a `WebhookUnsubscribe` token → deletes the webhook subscriber → 200 `{"ok": true}`. Idempotent (a MAC-valid token whose row is gone → 200); a bad MAC → 404.

**Behaviour:**
1. Order: page from the fresh row plus `access::granted` (no access → 404), the toggle (off → 404), the body, Part 1's buckets `hit("subscribe-ip:{client_key}", 5, 1 h)` and `hit("subscribe-page:{page}", 100, 1 h)`, the caps, `vet_destination`, `create(.., created_by: None)`, `kick` after commit. Every message is a fixed literal; none echoes input.
2. `snapshot::build` sets `subscriptions.webhook = page.allow_webhook_subscribers` (R-SP2-14). Toggling bumps `updated_at` through Part 1's page PATCH, so the DTO follows at once. If Part 1's PATCH body does not accept `allow_webhook_subscribers`, this task adds it (admin, bool).
3. `tokens::open` with `Purpose::WebhookUnsubscribe` reads `status_page_webhook_subscribers` and compares `token_nonce` with Part 1's constant-time `ct_eq`. The purpose byte is under the MAC and selects the table, so an email token never reads the webhook table and this purpose never reads the email table.
4. `webhook::unsubscribe_url(state, page, row) -> String` builds `{page url}/unsubscribe?token=<sign(WebhookUnsubscribe, token_nonce)>` (used by T20 and T21).

**Tests** (`tests/statuspages/webhook_subscribers.rs`):
- `the_public_form_needs_the_toggle` (off → 404 byte-identical to an unknown slug; on → 202 with a secret).
- `a_private_page_needs_access_to_subscribe_a_webhook` (no token → 404; a valid access token → 202).
- `public_webhook_subscribe_shares_the_email_buckets` (4 email subscribes and 1 webhook subscribe from one address pass; the next of either kind is 429; the page bucket likewise at 100 and 101).
- `a_verified_url_cannot_be_subscribed_again` (409; the stored sealed secret is unchanged).
- `the_public_form_refuses_internal_destinations` (the R-SP2-3 list, each 400, nothing inserted or queued).
- `the_dto_reports_webhook_subscriptions` (false, then true after the PATCH).
- `a_webhook_unsubscribe_token_removes_only_its_webhook` (and a second POST still answers 200).
- `an_email_token_cannot_remove_a_webhook_subscriber_and_vice_versa` (purpose byte swapped, ids swapped: 404, nothing deleted).
- `inspect_shows_the_host_of_a_webhook_token` (no URL path, no secret).
- The two public subscribe routes in `json_bodies.rs`.

**Acceptance:** a visitor of a page with the toggle on can subscribe a webhook and gets the secret once; the subscription can be removed through its token; tokens of the two kinds never cross.

---

### Task 19 (NAMI-SP-19): Queue extension, verification handshake, signed sender and auto-disable

**Covers:** R5.10 (delivery half).  **Depends on:** NAMI-SP-18, NAMI-INC-4, NAMI-INC-5.

**Files:**
- Create: `nami-engine/migrations/NNNN_incidents_status_page_webhooks.sql`, `nami-engine/tests/statuspages/webhook_delivery.rs`.
- Modify: `nami-engine/src/statuspages/webhook/mod.rs` (`send`, verification payload, signature), `nami-engine/src/statuspages/webhook/model.rs` (`record_outcome`), `nami-engine/src/statuspages/webhook/routes.rs` (fill T17's seam), `nami-engine/src/statuspages/deliver.rs` (dispatch), `nami-engine/src/incidents/deliveries.rs` (claimed-row field, the hooks in claim step (b) and per-row step (2f)), `nami-engine/src/incidents/worker.rs` (prune), `nami-engine/tests/statuspages/main.rs`, `nami-engine/tests/core/migrations.rs`.

**Data shape** (`NNNN_incidents_status_page_webhooks.sql`, numbered after the statuspages file it references):

```sql
-- Webhook subscribers ride the status-page branch of the queue (NAMI-SP-19).
alter table notification_deliveries
    add column webhook_subscriber_id uuid,
    add constraint notification_deliveries_webhook_subscriber_fk foreign key (webhook_subscriber_id)
        references status_page_webhook_subscribers(id) on delete cascade;

-- Part 1 (NAMI-SP-9) allowed status-page rows for email subscribers only.
alter table notification_deliveries drop constraint notification_deliveries_row_kind;
alter table notification_deliveries add constraint notification_deliveries_row_kind check (
    (incident_id is not null and status_page_id is null and subscriber_id is null
        and webhook_subscriber_id is null and status_report_update_id is null and priority = 0
        and event in ('triggered', 'resolved') and (user_id is not null or channel_id is not null))
    or (incident_id is null and status_page_id is not null and subscriber_id is not null
        and webhook_subscriber_id is null and user_id is null and channel_id is null and priority = 1
        and event in ('sp_confirm', 'sp_subscribed', 'sp_update')
        and ((event = 'sp_update') = (status_report_update_id is not null)))
    or (incident_id is null and status_page_id is not null and subscriber_id is null
        and webhook_subscriber_id is not null and user_id is null and channel_id is null and priority = 1
        and event in ('sp_wh_verify', 'sp_wh_report')
        and ((event = 'sp_wh_report') = (status_report_update_id is not null)))
);
create index notification_deliveries_webhook_subscriber_idx on notification_deliveries (webhook_subscriber_id)
    where webhook_subscriber_id is not null;
```

T21 edits this file in place (it has not merged) to add the change column and the `sp_wh_component` event to the rule.

```rust
// statuspages::webhook
pub const MAX_ECHO_BYTES: usize = 4096;
pub const MAX_RESPONSE_DRAIN: usize = 64 * 1024;
pub const MAX_CONSECUTIVE_FAILURES: i32 = 20;
#[derive(Serialize)] pub struct Verification<'a> { #[serde(rename = "type")] pub kind: &'static str,   // "verification"
    pub challenge: &'a str, pub page: EventPage<'a> }
#[derive(Serialize)] pub struct EventPage<'a> { pub slug: &'a str, pub title: &'a str, pub url: &'a str }
/// Sends one webhook row (R-SP2-4, R-SP2-6). Missing, disabled, or (for an event) unverified subscriber → Cancelled.
pub async fn send(state: &AppState, row: &StatusPageRow) -> Outcome;
/// "sha256=" + format::hmac_hex(secret, body).
pub fn signature_header(secret: &[u8], body: &[u8]) -> String;
/// R-SP2-4: the exact trimmed text, or a JSON object whose `challenge` equals it.
pub fn echoes(body: &[u8], challenge: &str) -> bool;
// statuspages::webhook::model
/// R-SP2-9. Called in the transaction that records a webhook row's final state.
pub async fn record_outcome(conn: &mut PgConnection, subscriber_id: Uuid, event: &str, sent: bool,
    error: Option<&str>) -> Result<(), sqlx::Error>;
```

Part 1's `StatusPageRow` gains `webhook_subscriber_id: Option<Uuid>`, and its `subscriber_id` becomes `Option<Uuid>`. The claim's `returning` list gains `d.webhook_subscriber_id`.

**Behaviour:**
1. Dispatch: a status-page row with `webhook_subscriber_id` goes to `webhook::send`; one with `subscriber_id` goes to Part 1's mail path, unchanged.
2. `send`: load the subscriber with its page (one query). Missing → `Cancelled`. An event row whose subscriber is disabled → `Cancelled`; a verify row of a disabled subscriber is sent (it is the re-enable path, R-SP2-4). An event row whose subscriber is not verified → `Cancelled` (the second guard of "nothing before the challenge"; the first is the enqueue filter of T20 and T21). Open the secret (`DecryptError` → `Failed`, `Retry::Never`, "secret unreadable"). Build the body once (`serde_json::to_vec`), sign it, run `check_url_for_send(url, UrlRule::Https, Reach::PublicOnly, state.config.outbound_policy)` (refused → `Retry::Never`, `DESTINATION_NOT_ALLOWED`), then POST with `outbound::public_client()`, whose 10 s total timeout (`OUTBOUND_TIMEOUT`) is the handshake limit.
3. Responses: 2xx success (a verify row also needs `echoes`, else `Never`, "challenge not echoed"); 3xx `Never` ("redirects are not followed"; the client never follows one); 429 `After(Retry-After capped at 1 h)`; 408 and 5xx `After(None)`; other 4xx `Never` (`HTTP <code>`); transport errors `After(None)` with `describe_error`. The body is read up to `MAX_ECHO_BYTES` for a verify row and drained up to `MAX_RESPONSE_DRAIN` otherwise. It is never stored or logged.
4. Recording: after the drain records the row in step (2f), a webhook row also calls `record_outcome` in that transaction. A verify row that succeeded sets `verified_at = now()`, `challenge = null`, `disabled_at = null`, `consecutive_failures = 0`. An event row that ended `sent` sets `consecutive_failures = 0`. An event row that ended `failed` adds 1 and, on reaching `MAX_CONSECUTIVE_FAILURES`, sets `disabled_at = now()` and cancels the subscriber's pending rows. Every call sets `last_attempt_at` and `last_error` (fixed text, at most 200 chars). A row that will be retried does not count.
5. Poison rows: claim step (b) gains `returning id, webhook_subscriber_id, event`, and each webhook event row it fails calls `record_outcome(.., sent: false, Some("delivery interrupted"))` inside the claim transaction.
6. Queueing the handshake: `create` and `reverify` insert `sp_wh_verify` with `round = 'v-' || verify_sends`, `target_key = 'wh:' || id`, priority 1, `status_page_id`; the routes call `incidents::worker::kick` after commit, as Part 1's subscribe does.
7. Retention: Part 1's hourly prune also deletes webhook subscribers with `verified_at is null and disabled_at is null and created_at < now() - PENDING_TTL`.

**Tests** (`tests/statuspages/webhook_delivery.rs`, wiremock under the test policy unless stated; unit tests in `webhook/mod.rs`):
- `the_handshake_posts_the_challenge_and_verifies_on_echo` (both echo forms; the request's `x-nami-signature` verifies with the returned secret; `x-nami-event: verification`).
- `a_wrong_echo_leaves_the_subscriber_unverified` (no retry: `failed` after one attempt).
- `an_unanswered_handshake_fails_after_the_outbound_timeout` (wiremock delays past `outbound::OUTBOUND_TIMEOUT`; the attempt fails with "timeout" and is rescheduled).
- `an_event_row_for_an_unverified_subscriber_is_cancelled` (a row inserted directly; wiremock sees nothing).
- `a_redirect_is_a_failed_delivery_and_is_not_followed` (302 to a second wiremock path that records hits: zero hits; `last_error` is "redirects are not followed").
- `a_name_that_rebinds_to_loopback_is_refused_at_send_time` (`state.config.outbound_policy = Strict`, a subscriber seeded with `https://localhost:<port>/hook`: the row fails with `destination not allowed`, wiremock sees nothing).
- `a_private_literal_is_refused_at_send_time_under_the_strict_policy` (`https://127.0.0.1:<port>/hook`).
- `signatures_cover_the_exact_body` (unit: a known secret and body give a known header; one changed byte changes it).
- `echo_matching_accepts_only_the_challenge` (unit: trimmed text, JSON object, a wrong value, other JSON types; a matching body of exactly `MAX_ECHO_BYTES` (challenge plus whitespace padding) passes, `MAX_ECHO_BYTES + 1` fails).
- `a_large_response_is_drained_only_up_to_the_cap` (wiremock answers 2xx with `MAX_RESPONSE_DRAIN + 1` bytes: the row is `sent` and the read stops at `MAX_RESPONSE_DRAIN`; a body of exactly the cap is read whole).
- `twenty_failed_deliveries_disable_the_subscriber` (19 → still active; the 20th → `disabled_at` set and pending rows cancelled; a success at 19 resets the count to 0).
- `a_poisoned_webhook_row_counts_as_a_failed_delivery`.
- `a_handshake_failure_never_counts_toward_disabling`.
- `a_successful_reverification_reenables_a_disabled_subscriber` (the verify row is sent although `disabled_at` is set; afterwards `disabled_at` and the count are cleared and `challenge` is null).
- `a_disabled_subscriber_gets_no_event_while_its_reverification_is_pending`.
- `removing_a_subscriber_cascades_its_queued_rows`.
- `unverified_subscribers_are_pruned_after_the_pending_ttl` (created exactly `PENDING_TTL` ago is kept, one second older is deleted; a disabled subscriber with a pending re-verification is never deleted).
- Incidents Part 1's `notification_deliveries` tests and Part 1's subscriber tests run unchanged and green.
- Migration: `a_delivery_row_is_exactly_one_kind` (an email row with `webhook_subscriber_id`, a webhook row with `subscriber_id`, a webhook row with `user_id`: each `23514`).

**Acceptance:** a subscriber becomes active only after echoing its challenge; every request is signed and refuses internal destinations at send time; a redirect is never followed; failures disable it at 20.

---

### Task 20 (NAMI-SP-20): Report events to webhook subscribers

**Covers:** R5.10 (report events), R5.06 (webhook half), R5.07 (webhook half).  **Depends on:** NAMI-SP-19.

**Files:**
- Modify: `nami-engine/src/statuspages/notify.rs` (`enqueue_update`), `nami-engine/src/statuspages/webhook/mod.rs` (report payload), `nami-engine/tests/statuspages/webhook_delivery.rs`.

**Data shape:** a second statement in `enqueue_update`, in the same transaction, for the same `update_id`:

```sql
insert into notification_deliveries (status_page_id, webhook_subscriber_id, status_report_update_id, event, round, target_key, priority)
select w.status_page_id, w.id, u.id, 'sp_wh_report', replace(u.id::text, '-', ''), 'wh:' || w.id::text, 1
from status_report_updates u
join status_reports r on r.id = u.report_id
join status_page_webhook_subscribers w on w.status_page_id = r.status_page_id
     and w.verified_at is not null and w.disabled_at is null
where u.id = $1 and u.notify_subscribers
  and (w.all_components
       or not exists (select 1 from status_report_components rc where rc.report_id = r.id)
       or exists (select 1 from status_report_components rc
                  join status_page_webhook_subscriber_components wc
                    on wc.component_id = rc.component_id and wc.webhook_subscriber_id = w.id
                  where rc.report_id = r.id))
on conflict (status_page_id, round, target_key) where status_page_id is not null do nothing
```

`enqueue_update` returns the sum of both inserts.

```rust
#[derive(Serialize)] pub struct ReportEvent<'a> { pub event_id: Uuid, pub event: &'static str,   // "report.created" | "report.updated"
    pub page: EventPage<'a>, pub report: EventReport, pub update: EventUpdate, pub sent_at: DateTime<Utc>,
    pub unsubscribe_url: String }
#[derive(Serialize)] pub struct EventReport { pub id: Uuid, pub title: String, pub kind: String, pub status: String,
    pub impact: Option<ComponentStatus>, pub starts_at: Option<DateTime<Utc>>, pub ends_at: Option<DateTime<Utc>>,
    pub published_at: DateTime<Utc>, pub updated_at: DateTime<Utc>, pub url: String, pub components: Vec<PublicAffected> }
#[derive(Serialize)] pub struct EventUpdate { pub id: Uuid, pub status: String, pub message: String, pub created_at: DateTime<Utc> }
```

**Behaviour:**
1. Every caller of `enqueue_update` (report create, posted updates, and the automatic-report hooks `automatic::on_incident_opened` / `on_incident_resolved`) now reaches webhook subscribers too, with no change at the call sites.
2. `send` for `sp_wh_report` loads the update, its report, the affected components and the page. `event` is `report.created` when the update is the report's first (oldest by `created_at, id`), else `report.updated`. `event_id` is the row id. The body has no `component` key (R-SP2-12).
3. Report and update text are the stored text (markdown source); the receiver escapes it.

**Tests** (`tests/statuspages/webhook_delivery.rs`):
- `a_new_report_sends_report_created_then_updates_send_report_updated`.
- `an_update_with_notify_off_sends_no_webhook`.
- `the_twenty_notified_updates_cap_covers_webhooks` (the 21st notified update is refused by Part 1's 400; nothing queued).
- `an_unverified_subscriber_receives_no_event` and `a_disabled_subscriber_receives_no_event`.
- `subset_subscribers_get_only_reports_of_their_components` (and every report without components).
- `automatic_reports_reach_webhooks_when_notify_is_on` and `automatic_reports_send_no_webhook_when_notify_is_off` (Part 1's automatic-report fixture).
- `one_update_queues_one_row_per_subscriber` (calling `enqueue_update` twice queues once).
- `report_events_leak_nothing_private` (Part 1's leak fixture with an automatic report: the body contains none of its secrets, no project, monitor, heartbeat or incident id, no monitor name, no subscriber email).
- `the_report_event_has_exactly_the_spec_keys` (top-level keys `event_id, event, page, report, update, sent_at, unsubscribe_url`).
- `the_unsubscribe_url_in_an_event_removes_the_webhook` (POST its token to `/public/status-subscriptions/unsubscribe`).

**Acceptance:** posting a report update reaches exactly the email and webhook subscribers it should, under one rule, with signed bodies that pass the leak test.

---

### Task 21 (NAMI-SP-21): The `component.status_changed` event

**Covers:** R5.10 (component events).  **Depends on:** NAMI-SP-20.

**Files:**
- Create: `nami-engine/src/statuspages/changes.rs`, `nami-engine/tests/statuspages/component_changes.rs`.
- Modify: `nami-engine/migrations/NNNN_statuspages_webhooks.sql` and `nami-engine/migrations/NNNN_incidents_status_page_webhooks.sql` (both unmerged, edited in place), `nami-engine/src/statuspages/status.rs` (`current`), `nami-engine/src/statuspages/snapshot.rs` (step 2 calls `status::current`), `nami-engine/src/statuspages/webhook/mod.rs` (component payload), `nami-engine/src/statuspages/deliver.rs` (`StatusPageRow` field, dispatch of `sp_wh_component`), `nami-engine/src/incidents/deliveries.rs` (the claim's `returning` list), `nami-engine/src/statuspages/mod.rs`, `nami-engine/src/incidents/worker.rs` (tick and prune), `nami-engine/tests/statuspages/main.rs`, `nami-engine/tests/core/migrations.rs`.

**Data shape** (appended to `NNNN_statuspages_webhooks.sql`):

```sql
-- component.status_changed (NAMI-SP-21, R-SP2-11). The tick writes these
-- columns only; it never bumps status_pages.updated_at.
alter table status_pages add column component_states_at timestamptz;
alter table status_page_components
    add column published_status text,
    add column published_status_at timestamptz,
    add constraint status_page_components_published_status_check check (published_status is null
        or published_status in ('operational', 'degraded', 'partial_outage', 'major_outage', 'maintenance', 'no_data'));

create table status_page_component_changes (
    id uuid primary key default gen_random_uuid(),
    status_page_id uuid not null,
    component_id uuid not null,
    from_status text not null,
    to_status text not null,
    changed_at timestamptz not null default now(),
    constraint status_page_component_changes_page_fk foreign key (status_page_id)
        references status_pages(id) on delete cascade,
    constraint status_page_component_changes_component_fk foreign key (component_id)
        references status_page_components(id) on delete cascade,
    constraint status_page_component_changes_status_check check (from_status <> to_status
        and from_status in ('operational', 'degraded', 'partial_outage', 'major_outage', 'maintenance', 'no_data')
        and to_status in ('operational', 'degraded', 'partial_outage', 'major_outage', 'maintenance', 'no_data'))
);
create index status_page_component_changes_changed_idx on status_page_component_changes (changed_at);
create index status_page_component_changes_component_idx on status_page_component_changes (component_id);
```

In `NNNN_incidents_status_page_webhooks.sql`: add `status_page_component_change_id uuid` with `notification_deliveries_component_change_fk ... references status_page_component_changes(id) on delete cascade`; in `notification_deliveries_row_kind`, the first two branches also require `status_page_component_change_id is null`, and the webhook branch becomes `event in ('sp_wh_verify', 'sp_wh_report', 'sp_wh_component') and ((event = 'sp_wh_report') = (status_report_update_id is not null)) and ((event = 'sp_wh_component') = (status_page_component_change_id is not null))`.

```rust
// statuspages::status
/// The status of every component of `page` at `now` (R6 of Part 1: derive, then the active-report override).
/// The snapshot's step 2 and the change tick both call it, so they never disagree.
pub async fn current(conn: &mut PgConnection, page: &StatusPage, now: DateTime<Utc>)
    -> Result<Vec<(Uuid, ComponentStatus)>, sqlx::Error>;
// statuspages::changes
pub const COMPONENT_TICK: Duration = Duration::from_secs(30);
pub const PAGES_PER_TICK: i64 = 50;
pub const STALE_AFTER: TimeDelta = TimeDelta::minutes(5);
pub const CHANGE_RETENTION: TimeDelta = TimeDelta::days(7);
/// One pass (R-SP2-11). Returns the number of change rows recorded.
pub async fn tick(state: &AppState, now: DateTime<Utc>) -> Result<u64, sqlx::Error>;
// statuspages::webhook
#[derive(Serialize)] pub struct ComponentEvent<'a> { pub event_id: Uuid, pub event: &'static str,   // "component.status_changed"
    pub page: EventPage<'a>, pub component: EventComponent, pub sent_at: DateTime<Utc>, pub unsubscribe_url: String }
#[derive(Serialize)] pub struct EventComponent { pub id: Uuid, pub name: String, pub section: String,
    pub status: ComponentStatus, pub previous_status: ComponentStatus, pub changed_at: DateTime<Utc> }
```

**Behaviour:**
1. Claim, one transaction per pass: pages that have an active webhook subscriber and whose `component_states_at` is null or older than `COMPONENT_TICK` minus 5 s, oldest first (nulls first), at most `PAGES_PER_TICK`, `for update of p skip locked`; set `component_states_at = now()` and return each page's previous value.
2. Per claimed page, in one transaction: `status::current`. The page is **stale** when its previous `component_states_at` was null or older than `STALE_AFTER`. Per component, a compare-and-set returns the old value: `update status_page_components ... set published_status = $2, published_status_at = $3 where id = $1 and published_status is distinct from $2`, with the old value read in the same statement (a CTE with `for update`).
   - Old value null, or the page stale → nothing else (silent).
   - Otherwise → insert a `status_page_component_changes` row and fan it out to every active subscriber of the page that follows all components or has the component in its subset: `sp_wh_component` rows, `round` = the change id in simple hex, `target_key = 'wh:' || id`, priority 1, with the dedupe `on conflict ... do nothing`.
3. After the pass, `kick` if any row was queued. The incidents worker loop runs `tick` every `COMPONENT_TICK` (skipped with `INCIDENTS_WORKER_ENABLED=false`); the hourly prune deletes change rows older than `CHANGE_RETENTION`.
4. Queue plumbing: the claim's `returning` list gains `d.status_page_component_change_id`, `StatusPageRow` gains `status_page_component_change_id: Option<Uuid>`, and the dispatch sends `sp_wh_component` rows to `webhook::send` like the other webhook rows (failure counting of T19 applies: it is an event row).
5. `send` for `sp_wh_component` builds `ComponentEvent` from the change row (`from_status`, `to_status`, `changed_at`) and the component's current display name and section name. The body has no `report` or `update` key.
6. The tick writes no ClickHouse query, never touches `status_pages.updated_at`, and never reads the snapshot cache.

**Tests** (`tests/statuspages/component_changes.rs`):
- `a_monitor_going_down_sends_one_component_event` (up and a first silent tick, then down: one event `operational` → `major_outage`, signed, with the component's display name).
- `the_first_observation_sends_nothing`.
- `a_stale_page_is_refreshed_silently` (previous `component_states_at` older than `STALE_AFTER`; one second younger than `STALE_AFTER` is not stale).
- `an_unchanged_status_sends_nothing`.
- `two_ticks_at_once_record_one_change` (two `AppState`s on one pool, `tokio::join!`).
- `pages_without_active_webhook_subscribers_are_not_evaluated` (`published_status` stays null; a pending-only page is skipped too).
- `an_active_report_impact_is_a_change` (a report setting `degraded` on an `operational` component).
- `a_maintenance_window_is_a_change`.
- `subset_subscribers_get_changes_of_their_components_only`.
- `the_tick_does_not_invalidate_the_snapshot` (`status_pages.updated_at` unchanged).
- `snapshot_and_tick_agree_on_status` (the DTO's component statuses equal `status::current`).
- `component_events_leak_nothing_private` (Part 1's leak fixture).
- `the_component_event_has_exactly_the_spec_keys` (`event_id, event, page, component, sent_at, unsubscribe_url`).
- `the_tick_is_bounded_per_pass` (`PAGES_PER_TICK + 1` pages: `PAGES_PER_TICK` evaluated, the last on the next pass).
- `old_changes_are_pruned` (at `CHANGE_RETENTION` kept, past it deleted, its queued rows cascade).
- `a_disabled_worker_runs_no_tick`.
- Migration: `a_component_change_has_two_different_known_statuses` (`23514`), `status_page_component_changes` in `all_tables_exist`, and `a_delivery_row_is_exactly_one_kind` extended to the change column.

**Acceptance:** with a verified webhook subscriber, a component's status change produces exactly one signed `component.status_changed` event, whether or not anyone views the page, and enabling the feature sends no burst.

---

### Task 22 (NAMI-SP-22): The embeddable announcement widget

**Covers:** R5.15.  **Depends on:** Status Pages Part 1 (`status.json`, `pageRef`, `enginePrefix`, `callPublicEngine`). Independent of T17-T21.

**Files:**
- Create: `nami-ui/src/lib/statuspages/widget.js`, `nami-ui/src/lib/statuspages/widget.test.ts`, `nami-ui/src/routes/status/[slug]/widget.js/+server.ts`, `nami-ui/src/routes/status/[slug]/widget-route.test.ts`.
- Modify: `nami-ui/package.json` and the lock file (`happy-dom`, dev, exact version, R-SP2-17).

**Data shape:**
- `widget.js` is a classic script, one IIFE, ES2019, no imports, no globals left behind. It reads only these `status.json` fields: `page.url`, `active_reports[].{title, kind, status}`.
- Constants inside the file: `POLL_MS = 300000` (`WIDGET_POLL`), the status labels (the same text as Part 1's `STATUS_LABELS` for report statuses), the colours (R-SP2-16).
- `widget.test.ts` exports nothing; it holds `WIDGET_GZIP_BUDGET = 5120`.

**API** (nami-ui): `GET /status/{slug}/widget.js`, and `GET /widget.js` on a verified custom domain through Part 1's reroute (`/status/_domain/widget.js`).
- 200: the exact bytes of `widget.js` (imported with `?raw`), `content-type: text/javascript; charset=utf-8`, `cache-control: public, max-age=300`, `x-content-type-options: nosniff`, `cross-origin-resource-policy: cross-origin`, `etag: "w-<first 16 hex of SHA-256 of the source>"`.
- 304 when `If-None-Match` equals the ETag, checked only after the page check.
- 404 `text/plain` `not found` for an unknown, unverified or private page. The route does not use `fetchPage` (it forwards the visitor's access cookie): it calls `callPublicEngine(event, enginePrefix(ref))` with no `access`, parses the DTO and requires `dto.page.visibility === 'public'`. It never reads or sets a cookie, so its answer is the same with and without one.

**Behaviour** (R-SP2-16):
1. On run: no `document.currentScript` or no `CSSStyleSheet.prototype.replaceSync` or no `adoptedStyleSheets` support → return. Otherwise compute `new URL('status.json', currentScript.src)`.
2. Fetch with `{ credentials: 'omit', referrerPolicy: 'no-referrer' }`; a network error, a non-2xx answer or an unexpected shape renders nothing and throws nothing.
3. No active report → remove the banner if shown. Otherwise show the first active report with kind `incident`, else the first with kind `maintenance`: a host `div` appended to `document.body` with `attachShadow({ mode: 'closed' })`, one `CSSStyleSheet` set with `replaceSync` and assigned to `adoptedStyleSheets`, and inside `div[role=status][aria-live=polite]` with a status dot, the title and status label (each through `textContent`), a "View status" `a` (`href` set only when `new URL(page.url).protocol` is `https:` or `http:`, `rel="noopener"`, `target="_blank"`), and a dismiss `button` (`aria-label="Dismiss"`) that removes the host for this page view.
4. Poll every `POLL_MS` with `setInterval(fn, POLL_MS)` (a function, never a string), skipping while `document.visibilityState !== 'visible'`; a dismissed banner stays dismissed.
5. No `eval`, `Function`, `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `style` attributes or properties, `<style>` elements, `document.cookie`, `localStorage`, `sessionStorage`, `indexedDB`.

**Tests:**
- `widget.test.ts` (`// @vitest-environment happy-dom`; runs the source with `window.Function` after stubbing `document.currentScript` and `fetch`):
  - `shows_a_banner_for_an_active_incident` (title and status label in the shadow root, link to `page.url`).
  - `prefers_an_incident_over_maintenance` and `shows_maintenance_when_only_maintenance_is_active`.
  - `shows_nothing_when_no_report_is_active`.
  - `report_titles_render_as_text` (title `<img src=x onerror=alert(1)>`: no `img` element exists anywhere, the text equals the title).
  - `fetches_status_json_next_to_the_script_without_credentials` (`https://nami.acme.test/status/acme/widget.js` → `https://nami.acme.test/status/acme/status.json`, `credentials: 'omit'`).
  - `works_on_a_custom_domain` (`https://status.acme.test/widget.js` → `https://status.acme.test/status.json`).
  - `a_page_url_that_is_not_http_gets_no_link` (`javascript:alert(1)`, `data:text/html,x`).
  - `the_widget_sets_no_cookie_and_uses_no_storage` (`document.cookie` unchanged; `Storage.prototype.setItem` and `indexedDB.open` spies never called).
  - `styles_come_only_from_a_constructable_stylesheet` (one adopted sheet, no `style` element, no element with a `style` attribute).
  - `renders_nothing_without_constructable_stylesheets`.
  - `a_failed_or_malformed_fetch_renders_nothing_and_throws_nothing`.
  - `dismiss_hides_the_banner_for_good`.
  - `polls_only_while_visible` (fake timers).
  - `the_source_is_csp_safe` (a static scan of the source for every construct banned in Behaviour 5 and for a string first argument to `setTimeout`/`setInterval`).
  - `the_widget_fits_the_size_budget` (`gzipSync(source).length <= WIDGET_GZIP_BUDGET`).
- `widget-route.test.ts` (node environment, `fakeEvent`):
  - `serves_the_widget_for_a_public_page` (exact bytes, every header above).
  - `a_private_page_has_no_widget` (404 even when the visitor sends a valid access cookie; the engine call carries no `x-nami-page-access`).
  - `an_unknown_page_is_a_404`.
  - `the_widget_route_sets_no_cookie_and_ignores_cookies` (no `set-cookie`; status, headers and body identical with and without a session cookie and an access cookie).
  - `answers_304_only_after_the_page_check`.
  - `a_custom_domain_looks_the_page_up_by_host` (calls `/public/status-domains/<host>`).

**Acceptance:** `<script src=".../widget.js" async></script>` on a page with a strict CSP that allows only the status host in `script-src` and `connect-src` shows the banner during an active report and nothing otherwise; the gzip size is within 5 KiB; the vitest suite and `npm run check` pass.

---

### Task 23 (NAMI-SP-23): UI, editor toggle, widget snippet and admin webhook subscribers

**Covers:** R5.16 (UI half), R5.10 (admin UI), R5.15 (snippet).  **Depends on:** NAMI-SP-18, NAMI-SP-22.

**Files:**
- Create: `nami-ui/src/lib/server/statuspages/webhook-form.ts` (+ `webhook-form.test.ts`), `nami-ui/src/routes/(app)/projects/[id=uuid]/status-pages/[pageId=uuid]/subscribers/webhooks.test.ts`.
- Modify: `nami-ui/src/lib/statuspages/status-pages.ts` (+ test), `nami-ui/src/lib/server/statuspages/status-page-form.ts` (+ test), the editor `.../status-pages/[pageId=uuid]/+page.server.ts`, `+page.svelte`, `editor.test.ts`, the subscribers page `.../subscribers/+page.server.ts`, `+page.svelte`.

**Data shape:**

```ts
// lib/statuspages/status-pages.ts
export type WebhookSubscriber = { id: string; host: string; state: 'pending' | 'active' | 'disabled'; all_components: boolean;
  components: string[]; consecutive_failures: number; last_error: string | null; last_attempt_at: string | null;
  created_at: string; created_by_admin: boolean };
/** `<script src="<publicUrl>/widget.js" async></script>` for a public page; null for a private one (R-SP2-18). */
export function widgetSnippet(page: PageView, origin: string): string | null;
// lib/server/statuspages/webhook-form.ts
/** url trimmed and required to start with https://; scope=all → components null; scope=some → the
 *  `component` values that pass isUuid (at least one). */
export function parseWebhookForm(form: FormData): Parsed<{ url: string; components: string[] | null }>;
```

**Behaviour:**
1. Editor settings: the "Webhook subscribers" checkbox is enabled ("Let visitors subscribe a webhook on the public page. Admins can always add webhooks.") and `parsePageSettings` sends `allow_webhook_subscribers`.
2. Editor "Embed" card: for a public page, the snippet in a `<pre>` (Svelte text, so escaped) with `CopyButton` and the line "Shows a banner on your site while an incident or maintenance is active. Allow this host in your CSP `script-src` and `connect-src`."; for a private page, "Private pages have no widget."
3. Subscribers page (admin; others see Part 1's "Only admins can see subscribers."): a second `Card` "Webhooks" with a `.table` (host, state `Badge`, components or "All components", failures, last error, last attempt, created, "public form" or "admin"), an add form (URL, "All components" or "Only these" with component checkboxes), and per row "Re-send verification" (pending and disabled) and a two-step remove.
4. `addWebhook` returns `{ webhookSecret, host }` once, shown in a copy box: "Shown once. Store it now." plus the receiver guidance: "Nami first POSTs `{\"type\": \"verification\", \"challenge\": ...}`; answer 2xx with the challenge within 10 seconds. Every request has `X-Nami-Signature: sha256=<hex>`, the HMAC-SHA256 of the raw body with this secret. Drop repeats of `event_id`." The secret never goes into a URL or a redirect.
5. The load fetches `.../subscribers` and `.../webhook-subscribers` in parallel and sets `cache-control: private, no-store`.

**Tests:**
- `webhook-form.test.ts`: `requires_an_https_url`, `all_components_is_null`, `some_components_keeps_only_uuids` (`../x` dropped), `some_with_no_component_is_refused`.
- `status-pages.test.ts`: `widgetSnippet` for an app-host page, a verified custom domain (`https://status.acme.test/widget.js`) and a private page (null).
- `status-page-form.test.ts`: `allow_webhook_subscribers` maps to the PATCH body.
- `editor.test.ts`: the snippet renders for a public page and is absent for a private one; a slug or domain with markup renders escaped.
- `webhooks.test.ts`: the load calls both endpoints and sets `private, no-store`; `addWebhook` posts the parsed body and returns the secret only in the action result (never in `redirect`); a 409 and a 400 show the engine message; `verifyWebhook` posts `{}`; `removeWebhook` without `confirm=yes` calls nothing; a 403 gives the forbidden view; a host `<img src=x onerror=alert(1)>` renders escaped; a viewer sees no form.

**Acceptance:** an admin can turn on public webhook subscriptions, copy the widget snippet of a public page, add a webhook and see its secret once, and re-verify or remove webhooks; `npm run check` and the listed vitest files pass.

---

### Task 24 (NAMI-SP-24): UI, public webhook subscribe form and webhook unsubscribe

**Covers:** R5.16 (UI half), R5.10 (public UI).  **Depends on:** NAMI-SP-23.

**Files:**
- Create: `nami-ui/src/routes/status/[slug]/webhook-subscribe.test.ts`.
- Modify: `nami-ui/src/routes/status/[slug]/+page.server.ts` (action `subscribeWebhook`, the `load` cache rule), `+page.svelte` (form), `nami-ui/src/routes/status/[slug]/unsubscribe/+page.server.ts`, `+page.svelte` (the webhook purpose).

**Behaviour:**
1. The public page shows a "Get webhook notifications" form under the email form when `dto.subscriptions.webhook` is true: URL, "All components" or "Only these" (checkboxes), submit. No client script (`csr = false`).
2. `subscribeWebhook`: `parseWebhookForm`, then `callPublicEngine(event, enginePrefix(ref) + '/webhook-subscribe', { method: 'POST', access })`. 202 → `{ webhookSecret, message: 'Your endpoint will receive a verification request. Answer it with the challenge to start receiving events.' }` with the same receiver guidance as T23; 400 and 409 → the engine message; 404 → "Webhook subscriptions are not available on this page."; 429 → "Too many requests. Try again later." or the engine's cap message.
3. The page `load` sets `cache-control: private, no-store` whenever `request.method !== 'GET'` (R-SP2-19), so the action response that carries the secret is never cached; GET keeps Part 1's header.
4. Unsubscribe page: when `inspect` answers `purpose: "webhook_unsubscribe"`, it shows "Stop sending webhooks for {title} to {host}?" with one POST button; the action posts `{token}` to `/public/status-subscriptions/unsubscribe` and shows "Webhooks to {host} are stopped." GET only inspects.

**Tests** (`webhook-subscribe.test.ts`):
- `the_form_is_shown_only_when_webhooks_are_enabled` (server render with `subscriptions.webhook` true and false).
- `subscribe_webhook_posts_the_url_and_components_with_the_access_cookie` (the engine call path, body and `x-nami-page-access`).
- `the_secret_is_returned_once_in_the_action_result` (never in a redirect or URL).
- `a_post_response_is_never_cacheable` (`private, no-store` on the action's page render; GET keeps `public, max-age=30`).
- `engine_errors_map_to_fixed_messages` (400, 404, 409, 429).
- `the_unsubscribe_page_handles_a_webhook_token` (load calls only `inspect`; the host renders escaped; the action posts the token in the body).

**Acceptance:** a visitor can subscribe a webhook from the public page when the page allows it, sees the secret once, and can stop webhooks through the link in any event; `npm run check` and the full vitest run pass.

---

### Task 25 (NAMI-SP-25): Phase security review and fix wave

**Covers:** the Part 2 security review (phase).  **Depends on:** NAMI-SP-17 .. NAMI-SP-24.

**Review** (whole-phase diff of `nami-engine/` and `nami-ui/`, against the spec's "Security requirements (tested)"). At least:
1. **Outbound.** Every webhook request goes through `outbound::public_client()` after `check_url_for_send(.., UrlRule::Https, Reach::PublicOnly, ..)`; grep for any other client or reach in `statuspages/`. No redirect is followed (3xx is a failure). The idle pool is off, so each attempt is resolved and judged again. Subscribe-time checks cover literals (v4, v6, mapped, NAT64, CGNAT, documentation ranges, metadata) and resolved names. `OutboundPolicy::Relaxed` is reachable only through `Config::from_env_for_test`. Resolution is bounded (`RESOLVE_SLOTS`, `RESOLVE_TIMEOUT`), response reads are capped.
2. **Handshake.** No event row is queued for, or sent to, an unverified or disabled subscriber. The echo is compared exactly. The public form never waits for or reports on the endpoint.
3. **Secrets and signatures.** The secret is random, sealed under its per-row context, returned once, never logged, never in `Debug`, never in a list response. The signature covers the exact sent bytes. `event_id` is stable across retries.
4. **Hijack and enumeration.** A verified URL is never replaced or re-keyed by a public request. The 409 reveals only what the caller already knows. Caps answer the same for known and new URLs. The public form shares the email buckets.
5. **Tokens.** `WebhookUnsubscribe` tokens are single-purpose, MAC-checked before any database read, and never open an email subscriber (and the reverse). GET never mutates.
6. **Leaks.** Re-run Part 1's leak fixture against `report.*` and `component.status_changed` bodies and the widget's data path. The admin list never returns the URL or secret.
7. **Queue.** Incident rows keep every Incidents Part 1 behaviour and test. `notification_deliveries_row_kind` admits exactly the three row kinds. Failure counting covers step (2f) and poison rows, resets on success, disables at 20. Cascades remove rows of deleted subscribers, reports, components and changes.
8. **Component tick.** Atomic across instances, bounded per pass, never bumps `updated_at`, no ClickHouse, silent on first observation and on stale pages.
9. **Widget.** CSP-safe (no eval, no inline style or script, constructable stylesheets only), text through `textContent`, links only `http(s)`, no cookies or storage, `credentials: "omit"`, size within 5 KiB gzip, 404 for private pages, no access cookie forwarded by its route.
10. **UI.** No `{@html}` on any webhook host, URL, error or title. Secrets only in action results with `no-store`. Two-step removes. `isUuid` on every id. Every new state-changing engine route is in `tests/core/json_bodies.rs`, every new table in `all_tables_exist`.

**Then:** a fix wave on the phase branch for every Important finding and the Minor findings marked fix-now, a scoped re-review, the decisions doc `docs/superpowers/decisions/<date>-status-pages-part2.md` (R-SP2-1 .. R-SP2-20, the limits table, the `happy-dom` version, deferred items), the roadmap ticks (R5.10, R5.15 and R5.16, which Part 1 leaves unticked as partial; Part 1 ticks R5.06 and R5.07 itself), push, and the one phase PR (purpose and scope, the task list with commit ranges, implementation summary, the engine and UI commands run with their results, breaking changes, risks and follow-ups, and "No CI is configured on this repo yet").

**Acceptance:** the review is clean after the fix wave, the decisions doc and roadmap ticks are committed, and the phase PR is merged.

## Limits (named constants)

| Constant | Value | Where |
|---|---|---|
| `MAX_URL_LEN` | 2048 bytes | `statuspages::webhook` |
| `MAX_COMPONENTS` | 100 per subscriber | `statuspages::webhook` |
| `MAX_ACTIVE` / `MAX_PENDING` | 500 / 50 per page | `statuspages::webhook` |
| `RESOLVE_TIMEOUT` / `RESOLVE_SLOTS` | 5 s / 8 per process | `statuspages::webhook` |
| `PENDING_TTL` | 48 h | `statuspages::webhook` |
| handshake and request timeout | 10 s (`outbound::OUTBOUND_TIMEOUT`) | Incidents Part 1 |
| `MAX_ECHO_BYTES` / `MAX_RESPONSE_DRAIN` | 4 KiB / 64 KiB | `statuspages::webhook` |
| `MAX_CONSECUTIVE_FAILURES` | 20 | `statuspages::webhook` |
| `COMPONENT_TICK` / `PAGES_PER_TICK` | 30 s / 50 | `statuspages::changes` |
| `STALE_AFTER` / `CHANGE_RETENTION` | 5 min / 7 days | `statuspages::changes` |
| subscribe buckets | 5 / h / IP, 100 / h / page (shared with email) | Part 1 `statuspages::rate_limit` |
| page delivery cap | 600 / min (`STATUS_PAGE_PER_MINUTE`) | Part 1 |
| `WIDGET_POLL` | 5 min | `widget.js` |
| `WIDGET_GZIP_BUDGET` | 5120 bytes | `widget.test.ts` |

## Deferred (not in this phase)

- A manage page for webhook subscribers (change components without re-subscribing): not in the spec.
- An operator outbound allowlist: Platform; webhook subscribers will never use it (R-SP2-2).
- Widget options (position, colours, language): the spec lists custom CSS/JS and multi-language pages as non-goals.
