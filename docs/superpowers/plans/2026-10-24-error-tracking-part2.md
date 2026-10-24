# Error Tracking (Sentry-compatible), Part 2 Implementation Plan

> **Status:** outline. Nothing is merged; NAMI-ERR-12 is next, once Error Tracking Part 1 has merged.
> **Branch:** `feat/errors-part2` in `.worktrees/errors-part2/`.
> **Depends on:** Error Tracking Part 1 (`2026-10-24-error-tracking-part1.md`, NAMI-ERR-1..11), Incident Management Part 1 (`incidents::lifecycle`, escalation policies, severities, `incidents::worker::kick`) and Traces, all merged.

## Roadmap coverage

| Roadmap | Item | Tasks |
|---|---|---|
| R8.03 | Grouping, Part 2 half: symbolication runs before grouping for JavaScript and Node events | NAMI-ERR-14 |
| R8.05 | Release tracking | NAMI-ERR-12, NAMI-ERR-17 |
| R8.07 | JS source map upload (upload token) + server-side symbolication | NAMI-ERR-13, NAMI-ERR-14, NAMI-ERR-17 |
| R8.08 | Alert rules (new issue, regression, frequency) to incidents | NAMI-ERR-15, NAMI-ERR-16, NAMI-ERR-17 |
| R8.10 | UI, Part 2 half: releases, alert rules (plus the upload token panel) | NAMI-ERR-17 |

The phase security review and fix wave is NAMI-ERR-18. Part 1 covers the
other halves of R8.03 and R8.10. Why the work is split is set out in Part
1, "Scope and split".

**Goal:** Releases are tracked per app (first and last seen, events, new
issues, regressions). CI uploads minified JavaScript and its source maps
with an upload token, and JavaScript and Node stack traces are
symbolicated at ingest before grouping, with nothing ever fetched from the
network. Alert rules (new issue, regression, frequency) open incidents
that page through escalation policies, and resolving or ignoring the issue
resolves them.

**Architecture:**
- Releases: `error_releases` rows are upserted by the Part 1 counter flush
  (release deltas next to the issue deltas) and by the upload API.
- Artifacts: `error_artifacts` holds zstd-compressed bytes in Postgres per
  (release, name), uploaded through a multipart endpoint on the public
  listener that authenticates with `nami_upl_` bearer tokens.
- Symbolication: `errors::symbolicate` runs between normalization and
  grouping in `errors::ingest::accept_event`. It runs on a blocking thread
  under a 2 s budget and a parse semaphore, with a weighted two-generation
  cache of parsed maps keyed by SHA-256.
- Alert rules: `errors::alert` turns the Part 1 state machine's effects
  (`Regressed`, `Closed`) and new issues into incident opens and resolves
  through `incidents::lifecycle`, with a new
  `IncidentSource::ErrorAlert { rule_id, issue_id }`. A minute tick in
  `errors::worker` evaluates frequency rules with one ClickHouse aggregate
  per rule.

**Tech Stack:** as Part 1, plus axum's `multipart` feature, `sourcemap`
and `zstd` (R-ERR33).

**Spec:** `docs/superpowers/specs/2026-10-13-error-tracking-design.md`
(Releases, Source maps, Alert rules, UI "Releases, Alert rules", RBAC,
Security requirements).

**When to start:** after the Part 1 phase PR has merged. Create
`.worktrees/errors-part2/` on `feat/errors-part2` from the latest
`origin/main`, re-read this plan against the merged Part 1 and Incidents
code, write the step-level code task by task, and pick the migration
numbers.

## Global Constraints

- Everything in Part 1's Global Constraints applies, including the
  `tests/errors/` target and its run command
  (`cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test errors <file>:: -- --test-threads=8`).
- Migrations, in this order because of foreign keys:
  1. `NNNN_errors_<change>.sql` (suggested
     `NNNN_errors_releases_alerts.sql`): `error_releases`,
     `error_artifacts`, `error_apps.upload_token_hash`, `error_alert_rules`.
     NAMI-ERR-12 creates it, and NAMI-ERR-13 and NAMI-ERR-15 append to it.
  2. `NNNN_incidents_<change>.sql` (suggested
     `NNNN_incidents_error_alerts.sql`, numbered after the errors file):
     the incidents columns, constraint and trigger of NAMI-ERR-15.
- RBAC: viewer reads releases, artifacts and alert rules. Member deletes
  releases, rotates the upload token and creates, edits and deletes alert
  rules. The upload endpoint takes the upload token, never a session or a
  DSN key.
- An id of another project stored in a rule body (app, escalation policy,
  severity) is a 400. An id outside the path's project is a 404.
- The symbolication path makes no outbound request. It has no HTTP
  client, and a test listens on the referenced address and asserts zero
  connections.
- Large or CPU-heavy work (zstd, map parsing, token lookups over large
  maps) runs in `spawn_blocking` under the semaphores named below.

## Review Focus

1. The upload token is hashed at rest, shown once and rotatable. A DSN key
   cannot upload. Uploads to another app are 403. Size limits (50 MiB per
   file, 1 GiB per release, 5 GiB per app) are 413 and are checked under a
   lock, so concurrent uploads cannot pass a quota together.
2. No SSRF. An `http(s)` `sourceMappingURL` resolves only against uploaded
   artifacts.
3. Malformed, huge or slow maps leave frames as they were. The ingest path
   never panics and never waits past the 2 s budget.
4. Alert rules open at most one unresolved incident per (rule, issue).
   Ignored issues never alert. Resolving or ignoring an issue resolves its
   incidents in the same transaction as the status change.
5. Cross-project isolation for releases, artifacts and rules (404), and
   escaped rendering of artifact names and versions.

## Rulings (where the spec is silent)

Numbering continues from Part 1 (R-ERR1..25).

- **R-ERR26 Releases start with Part 2.** No backfill. Part 1's events
  keep their `release` in ClickHouse and on issues. A release row appears
  when the next event names it, or when an upload names it. A release
  created by an upload has `first_seen` and `last_seen` null until an event
  arrives.

  When an app first uploads maps, its JavaScript issues split once.
  Symbolicated events group on original frames, so they form new issues
  next to the minified ones. The old issues stop receiving events and are
  resolved by hand. `grouping_version` stays 1, because the algorithm is
  unchanged and only its input is. This is documented on the app page next
  to the upload script.
- **R-ERR27 Release counters.** The Part 1 counter flush also carries
  per-(app, version) deltas: `events`, `new_issues` (an issue created by an
  event of that release) and `regressions` (a `Regressed` effect on an
  event of that release). They are upserted with
  `insert ... on conflict (app_id, version) do update set ... = ... + excluded...`,
  with no per-event write. `first_seen` and `last_seen` take the least and
  greatest.
- **R-ERR28 Version rules.** A version in a path or an event follows
  Part 1's release rule (at most 200 chars; no `/`, `\`, newline or tab;
  not `.`, `..` or blank). The path segment is percent-decoded by axum and
  then validated (400).
- **R-ERR29 The upload endpoint is public.** CI calls it, so it is served
  on `PUBLIC_PORT` (the API port is private to nami-ui), at the spec's path
  `POST /api/v1/error-apps/{app_id}/releases/{version}/artifacts`.
  Credentials and errors:
  - `Authorization: Bearer nami_upl_<43 base64url>` is looked up by its
    SHA-256.
  - Missing, malformed or unknown is a 401. That includes a DSN key in any
    form (`Bearer <public_key>`, `X-Sentry-Auth`).
  - A valid token of another app is a 403, whether or not the path's app
    exists.
- **R-ERR30 Artifact names and parts.**
  - The artifact name is the multipart field name (curl
    `-F "~/static/js/main.js=@main.js"`), because curl replaces the
    part's `filename` with the local file name. It is 1 to 1024 chars with
    no control chars, and either starts with `~/` or is an absolute
    `http`/`https` URL without a fragment.
  - At most `MAX_PARTS_PER_UPLOAD = 100` parts per request. A part with the
    name of an existing artifact replaces it.
  - The whole request has `UPLOAD_TIMEOUT = 300 s`. Axum's
    `DefaultBodyLimit` on this route is raised to `MAX_UPLOAD_REQUEST_BYTES`
    = 1 GiB + 1 MiB (the release quota plus multipart overhead).
- **R-ERR31 Quotas are serialized per app.** An upload locks the app row
  (`select ... for update`) in the transaction that sums the release's and
  the app's artifact sizes (uncompressed `size`) and inserts. Two
  concurrent uploads therefore cannot pass a quota together. Uploads in
  flight: `MAX_UPLOADS_IN_FLIGHT = 2` engine-wide and 1 per app (429). Each
  part is held in memory up to `MAX_ARTIFACT_BYTES`, then compressed (zstd
  level 3) on a blocking thread.
- **R-ERR32 Symbolication bounds.**
  - Maps are parsed only up to `MAX_MAP_BYTES = 50 MiB`, the file limit.
  - One parse at a time engine-wide (`MAX_MAP_PARSES_IN_FLIGHT = 1`).
  - The parsed-map cache is weighed at three times the raw map bytes plus
    the minified file bytes, in a `TwoGen` cache of `MAP_CACHE_BYTES`
    (256 MiB), so the "LRU" is two generations as in R-ERR10.
  - `tokio::time::timeout(SYMBOLICATION_BUDGET = 2 s, ..)` bounds how long
    ingest waits. The blocking task keeps its parse permit until it
    finishes, and its result still fills the cache, so a slow map delays
    only itself and can never pile up threads.
  - The closure also checks the deadline between frames.
- **R-ERR33 Crate pins.** At phase start, re-pin each to the newest release
  at least 14 days old:
  - `sourcemap` 9.3.2 (`cargo add sourcemap@9.3.2`): Sentry's own crate,
    used by its symbolicator. It handles regular and index maps,
    `sourcesContent` and original function names (`SourceView`). BSD-3,
    pure Rust, stable since January 2026.
  - `zstd` 0.13.3 (`cargo add zstd@0.13.3`): the spec's artifact codec,
    with `zstd::bulk::decompress(data, size)` giving an exact output cap
    from the stored size. 0.13.3 is chosen over 0.14.x, which had three
    releases in October 2026. It builds the vendored C library with `cc`.
  - axum's `multipart` feature (multer 3) instead of a new crate.
- **R-ERR34 Map resolution.**
  - A frame's `abs_path` (without query or fragment) is looked up as a
    full name, then as `~` + its URL path (or `~` + its file path for Node).
  - The minified file's last `sourceMappingURL=` comment (`//#` or `//@`,
    found at upload and stored in `source_mapping_url`) is resolved
    relative to the file's name. A `~/a/b.js` name resolves `b.js.map` to
    `~/a/b.js.map`. The result is looked up as a full name, then as
    `~/path`.
  - A `data:application/json;base64,` map is decoded from the file.
  - Anything else is ignored.
  - A source path starting with `webpack:///` or `webpack://<ns>/` loses
    that prefix. A symbolicated frame whose source path contains
    `/node_modules/` becomes `in_app: false`. Other frames keep the SDK's
    `in_app`.
- **R-ERR35 Incident wiring.**
  - `incidents` gains `error_alert_rule_id` and `error_issue_id`. Both are
    set or both are null, and both use `on delete cascade`, like
    `monitor_id` and `log_alert_id`. Deleting a rule, an issue or an app
    is always a member's or admin's action (Part 1 keeps issues, R-ERR18),
    and it deletes the linked incidents. This is recorded in the decisions
    doc.
  - "At most one unresolved incident per (rule, issue)" is a partial unique
    index.
  - The `source` check is recreated with `error_alert` added to the list
    current at phase start, and `incidents_derive_source` gains the
    `error_alert_rule_id` branch.
  - A rule's `app_id` cascades, so deleting an app deletes its rules
    instead of widening them to every app. `escalation_policy_id` and
    `severity_id` are set null, falling back to the project defaults.
- **R-ERR36 Alert evaluation choices.**
  - New issue and regression rules are evaluated in the ingest request,
    after the ClickHouse insert, in their own transaction. A failure is
    logged at `error` and the event is still accepted.
  - Frequency rules are claimed every minute
    (`next_evaluation_at <= now()`, `for update skip locked`, at most
    `FREQUENCY_RULES_PER_TICK = 50`). Each opens at most
    `MAX_FREQUENCY_ISSUES_PER_RULE = 100` incidents per tick, on unresolved
    issues only.
  - An error-alert incident is resolved only by resolving or ignoring its
    issue, or by hand in Incidents. A count dropping below N resolves
    nothing.
  - Disabling a rule leaves its open incidents open.
  - Minimum level order: `debug < info < warning < error < fatal`.
  - At most `MAX_ALERT_RULES_PER_PROJECT = 100` rules (409). Names are
    unique per project (409). A rule has a `name` (1 to 100 chars) so lists
    and incident titles can label it.
- **R-ERR37 Incident content.**
  - Title: `"<app name>: <issue title>"`, at most 200 chars.
  - Cause: the culprit.
  - Metadata: `{"app": <app name>, "environment": <event or rule environment>, "release": <event release or the issue's last release>}`,
    with absent values left out.
  - Severity and escalation policy are the rule's (or the project
    defaults).
- **R-ERR38 Existing apps and the upload token.** Apps created in Part 1
  have no upload token (`upload_token_hash` null) until a member rotates
  one. The UI shows "Generate upload token". Apps created from Part 2 on
  get one at creation, shown once in the create reply.

## Tasks

### Task 12 (NAMI-ERR-12): Track releases

**Covers:** R8.05  **Depends on:** Part 1 merged (NAMI-ERR-11)

**Files:**
- Create: `nami-engine/migrations/NNNN_errors_releases_alerts.sql`,
  `nami-engine/src/errors/release/mod.rs` (version validation reuse,
  deltas), `nami-engine/src/errors/release/model.rs`,
  `nami-engine/src/errors/release/routes.rs`,
  `nami-engine/tests/errors/releases.rs`.
- Modify: `nami-engine/src/errors/issue/counters.rs` (release deltas,
  R-ERR27), `nami-engine/src/errors/ingest/mod.rs` (`accept_event` records
  `new_issues` and `regressions` deltas), `nami-engine/src/errors/mod.rs`,
  `nami-engine/src/core/router.rs`, `nami-engine/tests/core/migrations.rs`.

**Data shape:**

```sql
create table error_releases (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    app_id uuid not null references error_apps(id) on delete cascade,
    version text not null,
    first_seen timestamptz,
    last_seen timestamptz,
    event_count bigint not null default 0,
    new_issues bigint not null default 0,
    regressions bigint not null default 0,
    created_at timestamptz not null default now(),
    constraint error_releases_version_unique unique (app_id, version),
    constraint error_releases_version_length check (char_length(version) between 1 and 200),
    constraint error_releases_counts_check check (event_count >= 0 and new_issues >= 0 and regressions >= 0),
    constraint error_releases_seen_shape check ((first_seen is null) = (last_seen is null))
);
create index error_releases_app_last_seen_idx on error_releases (app_id, last_seen desc nulls last);
```

- JSON: `Release = {version, first_seen, last_seen, event_count, new_issues, regressions, artifact_count, artifact_bytes, created_at}`.
  The detail reply adds `artifacts: [{name, size, sha256, created_at}]`
  (NAMI-ERR-13 fills it).

**API:**

| Method and path | Min role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/errors/apps/{app_id}/releases?page=&limit=` | viewer | `200 {"releases", "page", "limit"}`, newest `last_seen` first | 400, 403, 404 |
| `GET /projects/{id}/errors/apps/{app_id}/releases/{version}` | viewer | `200 Release + artifacts` | 400 version, 403, 404 |
| `DELETE /projects/{id}/errors/apps/{app_id}/releases/{version}` | member | `204` (artifacts cascade) | 400, 403, 404 |

**Behaviour:**
1. The flush upserts one row per (app, version) delta in the same
   iteration as the issue counters (R-ERR27). A failed upsert keeps its
   deltas.
2. An issue created by an event counts one `new_issues` for the event's
   release. A `Regressed` effect counts one `regressions` for the event's
   release. Events without a release touch no release row.
3. Deleting a release removes its row and artifacts. Issues and events
   keep their release strings, and the next event naming it recreates the
   row with fresh counters (R-ERR26).
4. The release list links each version to the issue list with
   `release=<version>`. That is a URL in the UI, not an engine join.

**Tests** (`tests/errors/releases.rs`):
- `the_first_event_naming_a_release_creates_it`
- `release_counters_flush_events_new_issues_and_regressions`
- `events_without_a_release_create_no_release`
- `a_viewer_lists_releases_and_cannot_delete_one`
- `a_member_deletes_a_release_and_its_artifacts`
- `a_release_of_another_project_is_a_404`
- `a_version_with_a_slash_or_over_200_chars_is_a_400`
- `releases_page_by_last_seen`

**Acceptance:** `cargo test --test errors releases::` and the Part 1
suites (`issues::`, `ingest::`) pass.

### Task 13 (NAMI-ERR-13): Add the upload token and the artifact upload endpoint

**Covers:** R8.07 (upload token, artifact storage and limits)  **Depends on:** NAMI-ERR-12

**Files:**
- Create: `nami-engine/src/errors/artifact/mod.rs` (name rules,
  `sourceMappingURL` scan, compression), `nami-engine/src/errors/artifact/model.rs`,
  `nami-engine/src/errors/artifact/routes.rs` (the public upload router),
  `nami-engine/tests/errors/artifacts.rs`.
- Modify: `nami-engine/Cargo.toml` (axum `multipart` feature,
  `zstd = "0.13.3"`, R-ERR33),
  `nami-engine/migrations/NNNN_errors_releases_alerts.sql` (append),
  `nami-engine/src/errors/app/model.rs` and
  `nami-engine/src/errors/app/routes.rs` (token on create,
  `rotate-upload-token`, `has_upload_token`), `nami-engine/src/core/router.rs`
  (merge the upload router into the public router),
  `nami-engine/src/errors/release/model.rs` (artifact list in the detail).

**Data shape:**

```sql
alter table error_apps
    add column upload_token_hash text,
    add constraint error_apps_upload_token_unique unique (upload_token_hash),
    add constraint error_apps_upload_token_format check (upload_token_hash is null or upload_token_hash ~ '^[0-9a-f]{64}$');

create table error_artifacts (
    id uuid primary key default gen_random_uuid(),
    release_id uuid not null references error_releases(id) on delete cascade,
    app_id uuid not null references error_apps(id) on delete cascade,
    name text not null,
    size bigint not null,
    sha256 text not null,
    source_mapping_url text,
    content bytea not null,
    created_at timestamptz not null default now(),
    constraint error_artifacts_name_unique unique (release_id, name),
    constraint error_artifacts_name_length check (char_length(name) between 1 and 1024),
    constraint error_artifacts_size_check check (size between 0 and 52428800),
    constraint error_artifacts_sha_format check (sha256 ~ '^[0-9a-f]{64}$'),
    constraint error_artifacts_map_url_length check (source_mapping_url is null or char_length(source_mapping_url) <= 2048)
);
create index error_artifacts_app_idx on error_artifacts (app_id);
```

- `size` is the uncompressed size. `content` is zstd level 3.
  `source_mapping_url` is the last `sourceMappingURL` of the file, or the
  marker `inline` for a `data:` URL.
- Constants: `UPLOAD_TOKEN_PREFIX = "nami_upl_"`,
  `MAX_ARTIFACT_BYTES = 50 MiB`, `MAX_RELEASE_ARTIFACT_BYTES = 1 GiB`,
  `MAX_APP_ARTIFACT_BYTES = 5 GiB`, `MAX_PARTS_PER_UPLOAD = 100`,
  `MAX_ARTIFACT_NAME_CHARS = 1024`, `MAX_UPLOADS_IN_FLIGHT = 2`,
  `UPLOAD_TIMEOUT = 300 s`, `MAX_UPLOAD_REQUEST_BYTES`, and
  `ZSTD_LEVEL = 3`.
- Token: `nami_upl_` + base64url without padding of 32 `OsRng` bytes.
  `upload_token_hash` is the SHA-256 hex of the whole token
  (`auth::token::hash_token`). The plaintext appears only in the create
  and rotate replies, and the type that carries it redacts the value in
  its `Debug` output (as `SourceWithToken` does).

**API:**

| Method and path | Auth / role | Success | Errors |
|---|---|---|---|
| `POST /api/v1/error-apps/{app_id}/releases/{version}/artifacts` (public listener, `multipart/form-data`) | upload token | `201 {"release": version, "artifacts": [{name, size, sha256}]}` | 400 name, version or multipart, 401 token, 403 other app, 408 timeout, 413 limits, 415 not multipart, 429 uploads in flight |
| `POST /projects/{id}/errors/apps/{app_id}/rotate-upload-token` | member | `200 {"upload_token": ".."}` (once) | 403, 404 |
| `POST /projects/{id}/errors/apps` (changed) | member | `201 ErrorApp + "upload_token"` (once) | as Part 1 |

**Behaviour:**
1. Auth first (R-ERR29), then the in-flight caps (R-ERR31), then the
   version (R-ERR28). The release row is upserted when it is missing.
2. Each part is streamed (`field.chunk()`) into a buffer. Passing
   `MAX_ARTIFACT_BYTES` stops reading and is a 413. The name is validated
   (R-ERR30). On a blocking thread the part is hashed, compressed and
   scanned for `sourceMappingURL`.
3. One transaction per part: lock the app row, sum `size` for the release
   and for the app without the artifact this part replaces, check both
   quotas (413 past either), then upsert on `(release_id, name)`.
4. Parts before a failing part stay stored. The reply of a failed request
   names the failing part. Each part is atomic.
5. A DSN key presented as a bearer token or in `X-Sentry-Auth` is a 401
   (R-ERR29). The session API does not accept artifacts.
6. Rotating the token replaces the hash in one update. The old token is a
   401 from then on.

**Tests** (`tests/errors/artifacts.rs`):
- `an_upload_with_the_token_stores_compressed_artifacts`
- `the_token_is_shown_once_and_stored_hashed` (the row holds only the
  SHA-256)
- `a_rotated_upload_token_is_a_401`
- `no_token_or_a_bad_token_is_a_401`
- `a_dsn_key_cannot_upload`
- `a_token_of_another_app_is_a_403` (also when the path's app is unknown)
- `a_file_of_max_artifact_bytes_is_accepted_and_one_byte_more_is_a_413`
- `the_release_quota_is_enforced_at_and_past_the_bound`: rows seeded in
  SQL with a large `size` and tiny `content` bring the release to
  `MAX_RELEASE_ARTIFACT_BYTES` minus a few bytes, so no test uploads 1 GiB.
  The quota arithmetic is also a pure `check_quota` with unit tests at the
  bounds.
- `the_app_quota_is_enforced_at_and_past_the_bound` (seeded the same way)
- `two_concurrent_uploads_cannot_pass_the_release_quota_together`
- `more_than_max_parts_per_upload_is_a_400`
- `a_name_that_is_neither_tilde_nor_an_absolute_url_is_a_400`
- `reuploading_a_name_replaces_the_artifact`
- `the_source_mapping_url_is_found_at_upload`
- `a_viewer_cannot_rotate_the_upload_token`
- `release_detail_lists_artifacts_without_content`

**Acceptance:** `cargo test --test errors artifacts::` passes. The
documented curl script (in the app page, NAMI-ERR-17) uploads a Vite build
to a local engine.

### Task 14 (NAMI-ERR-14): Symbolicate JavaScript and Node frames at ingest

**Covers:** R8.07 (server-side symbolication), R8.03 (symbolication runs first)  **Depends on:** NAMI-ERR-13

**Files:**
- Create: `nami-engine/src/errors/symbolicate.rs`,
  `nami-engine/tests/errors/symbolicate.rs`,
  `nami-engine/tests/fixtures/errors/sourcemaps/` (a small Vite build: a
  minified file, its map with `sourcesContent`, an index map, an inline
  `data:` map, a malformed map, and a map whose `sources` include
  `node_modules`).
- Modify: `nami-engine/Cargo.toml` (`sourcemap = "9.3.2"`, R-ERR33),
  `nami-engine/src/errors/normalize/mod.rs` (`Frame.minified:
  Option<MinifiedPosition>` with `abs_path`, `lineno`, `colno` and
  `function`; serde default and skip when none, so Part 1 payloads stay
  valid), `nami-engine/src/errors/ingest/mod.rs` (call it between
  `normalize` and `group`), `nami-engine/src/errors/mod.rs`
  (`ErrorsState.maps: TwoGen` and the parse semaphore).

**Data shape:**
- `pub async fn symbolicate(state: &AppState, app: &IngestApp, event: &mut NormalizedEvent)`
  is infallible: every failure leaves the frame as it was and is logged at
  `warn` with ids only.
- Constants: `SYMBOLICATION_BUDGET = 2 s`, `MAX_MAP_BYTES = 50 MiB`,
  `MAX_MAP_PARSES_IN_FLIGHT = 1`, `MAP_CACHE_BYTES = 256 MiB`,
  `MAP_WEIGHT_FACTOR = 3`, `SYMBOLICATED_PLATFORMS = ["javascript", "node"]`.

**Behaviour:**
1. It runs only when the platform is in `SYMBOLICATED_PLATFORMS`, the
   event has a release, and that release has artifacts.
2. One query loads the metadata (`name, sha256, size, source_mapping_url`)
   of every candidate name of every frame (R-ERR34). Content is loaded only
   for files not in the cache, with `zstd::bulk::decompress(content, size)`.
3. On a blocking thread, holding the parse permit, under the deadline:
   - parse with `sourcemap::decode_slice` (regular and index maps;
     anything else leaves the frame);
   - for each frame, `lookup_token(lineno - 1, colno - 1)`, because the
     crate's lines and columns are 0-based;
   - set `filename` and `abs_path` from the token's source (prefix rules of
     R-ERR34), and `lineno` and `colno` from its source position + 1;
   - set `function` from `get_original_function_name` over a `SourceView`
     of the minified file, else the token's name, else unchanged;
   - set the context lines from `get_source_contents`, through Part 1's
     context rules (5 + 1 + 5 lines, 512 chars);
   - move the minified position into `minified`;
   - apply the `node_modules` in-app rule.
4. Parsed maps (with the minified file's `SourceView`) are cached by the
   map's SHA-256, weighed per R-ERR32.
5. A timeout returns the event with whatever frames were done. A late
   parse still fills the cache.
6. Grouping then runs on the symbolicated frames (R8.03, symbolication
   first).

**Tests** (`tests/errors/symbolicate.rs`):
- `a_minified_frame_maps_to_its_original_file_line_column_and_function`
- `context_lines_come_from_sources_content`
- `the_minified_position_stays_on_the_frame`
- `an_index_map_and_an_inline_data_map_are_used`
- `a_tilde_artifact_matches_a_full_url_frame`
- `the_map_is_found_relative_to_the_minified_file`
- `node_modules_sources_are_not_in_app`
- `an_http_source_mapping_url_without_an_artifact_makes_no_request`: a
  `TcpListener` at the referenced address records connections, and there
  are zero after ingest.
- `an_event_before_its_maps_stays_minified`
- `a_malformed_map_leaves_frames_unsymbolicated_and_the_event_accepted`
- `a_map_over_max_map_bytes_is_not_parsed`
- `a_slow_parse_past_the_budget_returns_the_event_unsymbolicated` (with a
  test-sized budget)
- `the_second_event_uses_the_cached_map` (one content load in all)
- `symbolicated_events_group_by_original_frames`: two builds with different
  minified positions and the same sources make one issue.
- `events_of_other_platforms_are_not_touched`

**Acceptance:** `cargo test --test errors symbolicate::` and the Part 1
ingest suites pass. The ingest p50 for a non-JavaScript event is unchanged
(the fast path returns before any query), which a review can check in the
code.

### Task 15 (NAMI-ERR-15): Add alert rules and the error-alert incident source

**Covers:** R8.08 (rules, incident wiring)  **Depends on:** NAMI-ERR-12; Incident Management Part 1 merged (`incidents::lifecycle` from NAMI-INC-1, escalation policies and `OpenIncident.escalation_policy_id` from NAMI-INC-8)

**Files:**
- Create: `nami-engine/migrations/NNNN_incidents_error_alerts.sql`,
  `nami-engine/src/errors/alert/mod.rs` (validation, the level order),
  `nami-engine/src/errors/alert/model.rs`,
  `nami-engine/src/errors/alert/routes.rs`,
  `nami-engine/tests/errors/alert_rules.rs`.
- Modify: `nami-engine/migrations/NNNN_errors_releases_alerts.sql`
  (append `error_alert_rules`), `nami-engine/src/incidents/lifecycle.rs`
  (`IncidentSource::ErrorAlert { rule_id, issue_id }`: `as_str` gives
  `"error_alert"`, its own `OPEN_ERROR_ALERT` statement with the
  `(error_alert_rule_id, error_issue_id) where status <> 'resolved'`
  conflict target, and `resolve_source` by both columns),
  `nami-engine/src/incidents/model.rs` (`error_issue_id` and
  `error_alert_rule_id` on list and detail items, and the `source` filter
  accepts `error_alert`), `nami-engine/src/core/router.rs`,
  `nami-engine/tests/core/migrations.rs`,
  `nami-engine/tests/incidents/` (one lifecycle test for the new source in
  the existing lifecycle test file).

**Data shape:**

```sql
-- errors file (before the incidents file)
create table error_alert_rules (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    app_id uuid references error_apps(id) on delete cascade,
    environment text,
    min_level text,
    condition text not null,
    frequency_count int,
    window_seconds int,
    escalation_policy_id uuid references escalation_policies(id) on delete set null,
    severity_id uuid references incident_severities(id) on delete set null,
    enabled boolean not null default true,
    next_evaluation_at timestamptz not null default now(),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint error_alert_rules_name_unique unique (project_id, name),
    constraint error_alert_rules_condition_check check (condition in ('new_issue', 'regression', 'frequency')),
    constraint error_alert_rules_level_check check (min_level is null
        or min_level in ('fatal', 'error', 'warning', 'info', 'debug')),
    constraint error_alert_rules_environment_length check (environment is null or char_length(environment) between 1 and 64),
    constraint error_alert_rules_frequency_shape check ((condition = 'frequency')
        = (frequency_count is not null and window_seconds is not null)),
    constraint error_alert_rules_frequency_range check (frequency_count is null or frequency_count between 1 and 1000000),
    constraint error_alert_rules_window_range check (window_seconds is null or window_seconds between 60 and 86400)
);
create index error_alert_rules_due_idx on error_alert_rules (next_evaluation_at)
    where enabled and condition = 'frequency';

-- incidents file
alter table incidents
    add column error_alert_rule_id uuid references error_alert_rules(id) on delete cascade,
    add column error_issue_id uuid references error_issues(id) on delete cascade,
    add constraint incidents_error_alert_shape check ((error_alert_rule_id is null) = (error_issue_id is null));
alter table incidents drop constraint incidents_source_check;
alter table incidents add constraint incidents_source_check check (source in
    (<the list current at phase start>, 'error_alert'));
create unique index incidents_one_unresolved_per_error_alert
    on incidents (error_alert_rule_id, error_issue_id) where status <> 'resolved';
create index incidents_error_issue_idx on incidents (error_issue_id) where error_issue_id is not null;
-- incidents_derive_source(): recreated with
-- `when new.error_alert_rule_id is not null then 'error_alert'` added.
```

- Rust:

  ```rust
  pub enum Condition { NewIssue, Regression, Frequency { count: i32, window_seconds: i32 } }
  pub struct ValidRule { name, app_id: Option<Uuid>, environment: Option<String>, min_level: Option<Level>, condition: Condition, escalation_policy_id, severity_id: Option<Uuid>, enabled: bool }
  ```

  `Level` (from Part 1's `errors::normalize`) gets `Ord` in the R-ERR36
  order.
- Constants: `MAX_ALERT_RULES_PER_PROJECT = 100`,
  `MIN_WINDOW_SECONDS = 60`, `MAX_WINDOW_SECONDS = 86_400`,
  `MAX_FREQUENCY_COUNT = 1_000_000`.
- JSON: `{name, app_id?, environment?, min_level?, condition: "new_issue"|"regression"|"frequency", frequency_count?, window_seconds?, escalation_policy_id?, severity_id?, enabled?}`
  (`deny_unknown_fields`). PATCH uses `Option<Option<T>>` for the clearable
  fields.

**API:**

| Method and path | Min role | Success | Errors |
|---|---|---|---|
| `GET /projects/{id}/errors/alert-rules` | viewer | `200 {"rules": [..]}` by name | 403 |
| `POST /projects/{id}/errors/alert-rules` | member | `201 Rule` | 400 validation or foreign app/policy/severity id, 403, 409 name or quota |
| `GET /projects/{id}/errors/alert-rules/{rule_id}` | viewer | `200 Rule` | 403, 404 |
| `PATCH /projects/{id}/errors/alert-rules/{rule_id}` | member | `200 Rule` | 400, 403, 404, 409 |
| `DELETE /projects/{id}/errors/alert-rules/{rule_id}` | member | `204` | 403, 404 |

**Behaviour:**
1. `frequency_count` and `window_seconds` are required for `frequency`
   and refused for the other conditions (400).
2. Foreign ids in the body are checked inside the insert or update
   transaction (`belongs_to` queries) and give a 400.
3. The count check and the insert share one transaction that locks the
   project row.
4. Deleting a rule deletes its incidents (R-ERR35, the precedent of log
   alerts).

**Tests:**
- `tests/errors/alert_rules.rs`:
  - `a_member_creates_each_condition`
  - `a_viewer_reads_rules_but_cannot_create_one`
  - `frequency_needs_count_and_window_and_others_refuse_them`
  - `window_and_count_are_checked_at_and_past_their_bounds`
  - `a_foreign_app_policy_or_severity_is_a_400`
  - `the_rule_after_max_alert_rules_per_project_is_a_409`
  - `a_rule_of_another_project_is_a_404`
  - `deleting_an_app_deletes_its_rules`
  - `patch_clears_the_app_filter_with_null`
- `tests/incidents/` lifecycle file:
  `an_error_alert_source_opens_one_unresolved_incident_per_rule_and_issue`.
- `tests/core/migrations.rs`: the incidents constraint and index exist, and
  the derive-source trigger sets `error_alert`.

**Acceptance:** `cargo test --test errors alert_rules::`,
`cargo test --test incidents` and `cargo test --test core migrations::`
pass.

### Task 16 (NAMI-ERR-16): Evaluate alert rules and resolve incidents with their issue

**Covers:** R8.08 (new issue, regression, frequency to incidents)  **Depends on:** NAMI-ERR-15

**Files:**
- Create: `nami-engine/src/errors/alert/evaluate.rs` (matching, pure; the
  frequency SQL from static fragments), `nami-engine/tests/errors/alert_eval.rs`.
- Modify: `nami-engine/src/errors/ingest/mod.rs` (new issue and
  `Regressed` hooks after the ClickHouse insert),
  `nami-engine/src/errors/issue/routes.rs` (the `Closed` effect resolves
  incidents in the status transaction), `nami-engine/src/errors/worker.rs`
  (the frequency tick).

**Data shape:**
- `pub fn matches(rule: &RuleRow, app_id: Uuid, environment: &str, level: Level) -> bool`
  is pure.
- `pub async fn open_for(conn, rule: &RuleRow, issue: &IssueRow, ctx: &AlertContext) -> Result<Option<Uuid>, sqlx::Error>`
  calls `incidents::lifecycle::open` with
  `IncidentSource::ErrorAlert { rule_id, issue_id }`, the R-ERR37 title,
  cause and metadata, and the rule's `severity_id` and
  `escalation_policy_id`.
- Constants: `FREQUENCY_TICK = 60 s`, `FREQUENCY_RULES_PER_TICK = 50`,
  `MAX_FREQUENCY_ISSUES_PER_RULE = 100`.

**Behaviour:**
1. New issue: when `upsert_issue` reports `created` and the ClickHouse
   insert succeeded, load the project's enabled `new_issue` rules that
   match (app, environment, level of the event). Open one incident per
   rule in one transaction, commit, then `incidents::worker::kick(&state)`
   (from NAMI-INC-5) in a spawned task. A failure is logged and the event
   is still `200` (R-ERR36).
2. Regression: the same for `regression` rules, when this request's
   guarded update reported `Regressed` (once across instances, Part 1).
3. Frequency: the worker claims due rules (R-ERR36). For each, one
   ClickHouse query over `error_events` binds the project id, the window,
   the optional app, environment and level set, and
   `HAVING count() >= frequency_count`, at most
   `MAX_FREQUENCY_ISSUES_PER_RULE` issue ids. Postgres keeps those that are
   `unresolved` and in the rule's project. One incident is opened per
   (rule, issue): the partial unique index makes a repeat a no-op. Then
   kick.
4. Ignored issues never open incidents. A new issue cannot be ignored, a
   regression starts from resolved, and frequency keeps only unresolved
   issues.
5. Closing: a status change whose effect is `Closed` (resolve or ignore)
   selects the issue's unresolved incidents (`error_issue_id = $1`,
   `for update`) and resolves each with
   `incidents::lifecycle::resolve(conn, id, Some(actor), now)` in the same
   transaction. After the commit it kicks the incidents worker.

**Tests** (`tests/errors/alert_eval.rs`):
- `a_new_issue_opens_an_incident_for_each_matching_rule`
- `the_app_environment_and_min_level_filters_are_applied`
- `a_disabled_rule_opens_nothing`
- `a_regression_opens_an_incident_once_for_concurrent_events`
- `frequency_opens_an_incident_at_n_events_in_the_window_and_not_at_n_minus_one`
- `events_outside_the_window_do_not_count`
- `a_second_frequency_tick_does_not_open_a_second_incident`
- `ignored_issues_do_not_alert`
- `resolving_the_issue_resolves_its_incidents`
- `ignoring_the_issue_resolves_its_incidents`
- `the_incident_has_the_source_title_metadata_severity_and_policy`
- `an_alert_failure_does_not_fail_the_ingest_request`
- `a_rule_of_one_project_never_sees_another_projects_events`
- Unit tests in `evaluate.rs`: `matches` over every filter combination and
  the level order.

**Acceptance:** `cargo test --test errors alert_eval::` and
`cargo test --test incidents` pass.

### Task 17 (NAMI-ERR-17): Build the releases page, the upload token panel and the alert rules pages (UI)

**Covers:** R8.10 (Part 2 half: releases, alert rules), R8.05, R8.07, R8.08 (their UI)  **Depends on:** NAMI-ERR-12..16

**Files:**
- Create:
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/releases/+page.server.ts`,
    `+page.svelte` and `releases-page.test.ts`;
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/releases/[appId=uuid]/[version=release]/+page.server.ts`
    and `+page.svelte`. A new `release` matcher in `nami-ui/src/params.ts`
    (built on `isReleaseVersion`) refuses an invalid version before it
    reaches an engine path, and the version is always sent
    percent-encoded;
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/alerts/+page.server.ts`,
    `+page.svelte` and `alerts-page.test.ts`;
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/alerts/[ruleId=uuid]/+page.server.ts`
    and `+page.svelte`;
  - `nami-ui/src/lib/server/errors/rule-form.ts` and
    `nami-ui/src/lib/server/errors/rule-form.test.ts`.
- Modify:
  - `nami-ui/src/params.ts` (the `release` matcher);
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/+layout.svelte`
    (sub-tabs Releases and Alert rules);
  - `nami-ui/src/routes/(app)/projects/[id=uuid]/errors/apps/[appId=uuid]/+page.svelte`
    and its `+page.server.ts` (upload token panel);
  - `nami-ui/src/lib/errors/errors.ts` (`Release`, `Artifact`, `AlertRule`
    types, `isReleaseVersion`, `uploadScript(appId, version)`);
  - the incident detail page under
    `nami-ui/src/routes/(app)/projects/[id=uuid]/incidents/` (a link to the
    issue when `error_issue_id` is set).

**Behaviour:**
1. Releases: an app `Select`, then a table with version, first and last
   seen (UTC), events, new issues, regressions and artifacts. A version
   links to the issue list filtered by `release`. The detail page lists
   artifacts (name, size, SHA-256, uploaded) and a two-step "Delete
   release" (member).
2. Upload token panel on the app page:
   - "Generate upload token" (R-ERR38) or "Rotate upload token" (member);
   - the token shown once in an `.alert-info` with `CopyButton`, and never
     again after navigation;
   - `uploadScript` gives a documented curl loop that uploads every file
     of a build directory with `-F "~/<path>=@<file>"` and the bearer
     header.
3. Alert rules: a list (name, condition, filters, enabled `Badge`), and a
   create and edit form:
   - condition `Select`, where `frequency` shows count and window;
   - app, environment and minimum level filters;
   - escalation policy and severity `Select`s loaded from the incidents
     API;
   - enabled toggle;
   - two-step delete.

   Viewers see the forms disabled.
4. Every name, version and artifact name is text interpolation.

**Tests** (vitest):
- `rule-form.test.ts`: frequency fields required only for `frequency`,
  bounds, empty filters become `null`;
- `releases-page.test.ts`: a version with `%` and `#` is encoded in the
  engine path, and a viewer sees no delete;
- `alerts-page.test.ts`: the policy and severity options come from the
  incidents API, and a 400 shows the engine message;
- `uploadScript` quotes the path and never embeds a token value;
- a server render with `<script>` in an artifact name and a version shows
  escaped text.

**Acceptance:** `npm run check` (0 errors, 0 warnings) and
`npx vitest run` pass.

### Task 18 (NAMI-ERR-18): Run the Part 2 security review and fix wave

**Covers:** the phase security review (no roadmap item of its own)  **Depends on:** NAMI-ERR-12..17

**Files:** fixes on the branch as found, plus
`docs/superpowers/decisions/<date>-errors-part2.md` and the roadmap ticks
for R8.03, R8.05, R8.07, R8.08 and R8.10 (with Part 1 merged, all of §8).

**Behaviour:** a review of the whole phase diff against the spec's
security requirements, with one test each where it is missing:
1. Upload token: required, hashed at rest, shown once, rotatable. A DSN
   key cannot upload. Size limits. Another app is 403.
2. No SSRF from events or maps (zero connections). There is no HTTP client
   in `errors::symbolicate`.
3. Source map robustness: malformed, huge, deeply nested and slow maps,
   the budget and the parse permit, and no panic on the ingest path. A
   seeded mutation loop over the map fixtures runs through `symbolicate`.
4. Alerting: one incident per (rule, issue), ignored issues silent,
   resolve and ignore resolving incidents. A rule cannot reach another
   project's events or incidents.
5. Isolation: releases, artifacts and rules of another project are 404.
   Artifact content is never returned by any route.
6. Escaping: the render tests of NAMI-ERR-17.
7. Logging: no token, artifact content or map text in `tracing` fields.

A fix wave follows on the same branch, then a re-review until clean.

**Acceptance:** every finding is fixed or recorded as deferred in the
decisions doc. `cargo test`, `cargo clippy --all-targets`, `npm run check`
and `npx vitest run` pass. The phase PR lists the local runs (no CI yet)
and is auto-merged once the review is clean.
