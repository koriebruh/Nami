# Log Management Part 1 — decisions and carried-forward items

Part 1 (NAMI-LOG-1..8, PRs #29, #32, #36, #38, #39, #42, #43, #45; plan PR #22) and its final security review fix wave (PR #50) were executed with subagent-driven development. During execution the controller made the rulings below on the owner's behalf (directive: "jangan tunggu konfirmasi"). Each line states what was decided, why, and what it costs if wrong. Undo any of them by opening an issue or telling the agent.

Plan: `docs/superpowers/plans/2026-10-13-log-management-part1.md`. Spec: `docs/superpowers/specs/2026-10-13-log-management-design.md`.

## Rulings (in the order made)

- Ruling: Logs split into two plans. Part 1 is sources, table and normalization, HTTP ingest, syslog, search, tail, sources UI and explorer UI. Part 2 is pipelines, patterns tab, saved views, export, log alerts and usage chart. One plan writer cannot hold about 14 verified tasks. Cost if wrong: one extra plan cycle.
- R1: LOG-6..8 UI follows the merged conventions (`[id=uuid]` route dirs and matchers, `#lib/uuid`, `enginePath`, 401 to /login, `engineFailure`, two-step rotate and delete, submit guards, page titles, no-store on token pages). Cost if wrong: none.
- R2: redacted `Debug` for `SourceWithToken` (LOG-1) and `SdElement`/`SyslogMessage` (LOG-4). Tokens must never reach logs. Cost if wrong: none.
- R3: `sources.token` is sealed with a per-row AAD context `sources.token:{id}`. A column-only context would allow a sealed value to be swapped between rows. UP-10 gets the same rule. Cost if wrong: none.
- R4: remove ties to the unmerged Uptime Phase 2 (private `nullable` helper in the logs module, local `test_cipher` helper in logs tests). Logs must not depend on unmerged work. Cost if wrong: small duplication to dedupe later.
- R5: shared-file protocol for the parallel streams: non-tail insertion points in router/state/config/main/models/routes/tests-common/UI nav, `cargo add` only for deps, rebase onto latest main before the PR and keep both sides on conflict, controller ticks the roadmap. Cost if wrong: occasional rebase work.
- R6: validate `PUBLIC_SYSLOG_ADDRESS` like `publicEngineUrl`, shell-quote the curl and Vector snippets, document the env var. Cost if wrong: none.
- R7: minimal fake-event page tests for the Logs UI (token flow, 401/403). Cost if wrong: none.
- R8: public ingest (`POST /api/v1/logs`) mounts on the public listener (`PUBLIC_PORT`) added by the Uptime Phase 1 fix wave, never on the private API port. LOG-7 snippets use `publicEngineUrl`. Cost if wrong: one route move.
- R9: LOG-1 and LOG-2 start in parallel with the Uptime Phase 1 fix wave. LOG-3 waits for it to merge (needs the public router). Cost if wrong: idle wait.
- LOG-2 ruling: ids are stored UUIDv7 with their 64-bit halves swapped. ClickHouse compares the second half of a UUID first, so the swap makes `ORDER BY id` follow generation order and keeps equal-`dt` ties in arrival order for keyset paging. Ids are opaque. Cost if wrong: ids are not in the RFC v7 byte layout.
- LOG-2 ruling: `max_partitions_per_insert_block=400` (guard on, about 366 daily partitions fit one flush) and `dt` clamped in `build_row` for every path (TTL base would wrap outside 1970-2106). Cost if wrong: none.
- LOG-2 ride-alongs: char-boundary truncation test, `RESERVED_SETTINGS` loop test (client-owned settings pinned against real requests), `query_rows_with` wiremock test, caps re-applied in `build_row`, retention clamp, unknown raw level kept in `fields`, pattern quality (decimals and units to `<num>`, ISO timestamps to `<ts>`, strict IP), `wait_for_async_insert_timeout=25`, trace_id/span_id strict W3C hex else moved to `fields`. Cost if wrong: larger fix diff.
- LOG-2 rebase: the Uptime fix wave added `max_execution_time` and `cancel_http_readonly_queries_on_client_close` to client requests, so `RESERVED_SETTINGS` grew 9 to 11. LOG-5 then dropped its own `max_execution_time`; the client default of 25 s applies. Part 2 export needs an explicit long-query API with a separate timeout. Cost if wrong: search capped at 25 s.
- LOG-3 ruling: fix all three Important findings. (1) a 51 KB msgpack bomb cost about 1.8 GB RSS: value budget in both decoders plus a decode semaphore. (2) the permit was freed on client disconnect while the blocking decode continued, so an insert could land without `record_accepted` (quota escape): owned permits move into the spawned work. (3) one valid token could hold all 32 places for 60 s: per-source in-flight cap, paced body deadline, counted timeouts. Ride-alongs: DB-phase cap below the pool, JSONEachRow body built off the runtime, ordered usage upsert, multiple `Content-Encoding` gets 415, real gzip-bomb test. Cost if wrong: larger fix diff.
- LOG-3 round 2: a 60 KB gzipped msgpack still blew up after decode (about 322 MiB encoded, up to about 11 GiB across 32 places). Ruling: a stored/encoded cap of 100 MiB gives 413; bodies over 8 MiB hold their decode place through the ClickHouse insert; capped writer for non-string messages; per-source decode share 2. Cost if wrong: dense legitimate batches near 50 MiB may get 413 (send smaller batches).
- LOG-4 ruling: syslog has its own per-source event limiter (2000 events/s, burst 4000). HTTP's 50 req/s bucket counts batches and would cap syslog at 50 events/s. Drops count in `requests_rate_limited`. Cost if wrong: tune constants.
- LOG-4 ruling: close a TCP connection on the first bad or tokenless frame, restart the idle timer only for accepted frames, per-IP connection cap. Fixes tokenless slot exhaustion. Ride-alongs: repeated and colliding SD-ID merge (`sd_<id>`), tighter fuzz asserts, token-free log capture. Cost if wrong: none.
- LOG-6 ruling: fix both Important findings. (I-1) a client that stops reading kept its slot past the 30-minute close and session re-check: every send is bounded and ends the task. (I-2) with no upper `dt` bound, future-dated rows starved the tail: `dt <= now() + 5 s`. Cost if wrong: none.
- Ruling: shared `CARGO_TARGET_DIR` for the worktrees from LOG-4 on, `CARGO_PROFILE_DEV_DEBUG=0`, at most 3 targeted mutants per check, never restart Docker. Cost if wrong: weaker mutation evidence.
- Final review ruling: one opus fix wave (7 items, PR #50), including a shared event-to-row step and a single-entry full-row endpoint. Cost if wrong: larger wave.
- Wave ruling: list and tail rows are cut server-side (message 4096 characters, fields over 8 KiB replaced by a `{"_nami_omitted": ...}` marker, `truncated` flag per row). Full content comes from `GET /projects/{id}/logs/{log_id}?dt=`. Tighter than the reviewer's suggestion (8192 and 16-32 KiB): worst case about 34 KB per row, about 17 MB per 500-row page. Cost if wrong: an extra request on expand.
- Wave ruling: overflow of the read budget returns a generic 400, not 500, with a fixed message that leaks nothing. Cost if wrong: none.
- Wave ruling: the per-user cap of 4 concurrent queries also counts entry reads (the item named search and histogram only). Cost if wrong: fast expands can hit 429.
- Wave ruling: listeners are HTTP/1.1 only (h2c no longer accepted). The hyper `header_read_timeout` has no h2c equivalent. No known client (nami-ui/undici, Vector, Fluent Bit, curl) uses h2c over plain http. Cost if wrong: an h2c client breaks.
- Wave ruling: syslog splits an over-cap batch into several inserts instead of failing it. Only a frame whose row alone exceeds the cap is dropped (`too_large`). HTTP keeps whole-request 413. Cost if wrong: none.
- Wave ruling: project decode share is 2 (same as the per-source value, half of the engine's 4). Cost if wrong: tune a constant.

## Limits and env vars

Env vars (engine unless stated):

- `PUBLIC_PORT` (default 8081): public listener; serves heartbeat pings and `POST /api/v1/logs`. Private API port returns 404 for ingest.
- `SYSLOG_ENABLED` (default off): syslog listener on 5140 TCP and UDP.
- `MAX_SOURCES_PER_PROJECT` (default 50, positive whole number; anything else stops startup): the 51st create returns 409.
- `PUBLIC_SYSLOG_ADDRESS` (nami-ui, optional, validated): shown in setup snippets.

HTTP ingest (`POST /api/v1/logs`, `Authorization: Bearer <source token>`):

- Formats: JSON object, JSON array, NDJSON, MessagePack; `Content-Encoding: gzip` (more than one encoding gets 415).
- 10 MiB compressed, 50 MiB decompressed (incremental inflate), 10,000 events, 192 MiB decoded-value budget, 100 MiB stored/encoded cap. Over any limit gets 413.
- 32 request slots in total (full gets 503 + `Retry-After: 1`, no DB access). 4 concurrent decodes, at most 2 per source and 2 per project.
- Per source: 50 req/s, burst 100, at most 4 in flight. Per project: at most 8 in flight (429 `too_many_in_flight`). Postgres phase capped at 8.
- Body read deadline 5 s plus 64 KiB/s, at most 60 s, else 408.
- Daily byte quota per source (at least 1 MiB), over quota gets 429 `quota_exceeded`.
- Malformed token gets 401 before a slot or DB access (43-char base64url pre-check). Unknown, rotated and deleted tokens give the same 401.

Normalization and storage:

- `dt` from `dt`/`timestamp`/`time` (RFC 3339 or unix s/ms), else receive time; outside [now-365 d, now+1 d] falls back. Message from `message`/`msg`/`log`, 64 KiB cap with marker. `fields` 256 KiB cap.
- Sources: `retention_days` 1-365 (default 30), per-row TTL. Daily partitions, `ORDER BY (project_id, dt, id)`.

Syslog:

- RFC 5424, newline and octet-counting framing on TCP, one frame per UDP datagram, 64 KiB frame cap. Token in the `[nami token="..."]` SD element.
- 256 TCP connections in total, 16 per IP (IPv6 per /64). 5 minute idle timer. Queue 10,000 frames and 32 MiB.
- Per source 2000 events/s, burst 4000.
- Unknown-token cache: 10,000 hashes, 5 minute TTL; a hit closes the TCP connection or drops the UDP frame.

Search and tail:

- Window default 1 hour, maximum 31 days. `limit` up to 500. Up to 10 field filters, 10 operators (`exists`, `regex`, `not_regex`, `gt`, `gte`, `lt`, `lte`, plus eq/neq/contains), path depth at most 5. Keyset cursor `(dt, id)`.
- Read budget: `max_result_bytes` 32 MiB with overflow throw, 32 MiB cap on any ClickHouse response body, 25 s query timeout.
- At most 4 concurrent searches, histograms and entry reads per user (429).
- Tail: 1 s poll, 60 s look-back up to `now + 5 s`, `TAIL_BATCH` 200, id-first polling, sent-id set capped at 20k. 5 streams per user, 200 total (429). Session re-checked every 60 s, closes after 30 minutes.

Listeners: `header_read_timeout` 10 s, idle keep-alive 10 s, 512 connections per listener, HTTP/1.1 only.

## Security measures

- Ingest tokens: 32 random bytes, stored as SHA-256 hash (lookup) and AES-GCM ciphertext sealed with a per-row context. Rotation replaces both at once. Only members and above ever receive a token. `Cache-Control: no-store` on create, reveal and rotate. Redacted `Debug`; tests assert no token text in logs.
- Search SQL uses static fragments with typed numbered placeholders. No `LIKE`. Values, path segments, regexes, cursor parts and LIMIT are bound. Regexes are capped and pre-validated. Queries are confined to the path's project; authorization failure is 403.
- Memory and DoS bounds on public paths: decode budgets, owned permits, per-source and per-project caps, paced body deadline, listener header timeout and connection cap, syslog caps and unknown-token cache.
- Log content is attacker-controlled. UI renders it as text only (no `{@html}`, no auto-linking). SSE data is `event: log` JSON, so message text cannot inject SSE fields. Explorer query parameters pass through strict parsing and an allow-list.

## Behaviour changes (from PR #50)

- Engine listeners speak HTTP/1.1 only. Idle keep-alive connections close after 10 s (the BFF's undici keep-alive is 4 s). More than 512 open connections per listener are closed at accept.
- Search and tail rows are truncated; full row from the new entry endpoint (404 if absent or from another project, 400 on missing or bad `dt`).
- New responses: 429 for the per-user query cap and for per-project ingest in flight, 409 for the source cap, 400 for a read result over budget.
- Syslog: a TCP connection sending a token recently found unknown is closed.
- `ingest::prepare` gained a `project_shares` parameter; `store` gained `RowBatch`/`chunk_rows`/`encode_run`.

## Deferred items (carried forward, with their target)

- LOG-1 minors (done in wave): no-store on token responses. Still open: explicit column lists instead of `select *`, rate limit and audit on reveal/rotate, unique source names (-> Platform).
- LOG-2: pattern `5::`/`12::` over-match to `<ip>` (-> Part 2, before the Patterns tab); OTLP and syslog paths should map empty or all-zero trace ids to `""` (-> Traces); MAC/CLF timestamps not normalized; plan file has stale lines (old partition setting, v7 nibble test) that later briefs must not copy.
- LOG-3: per-IP limits on the public listener (-> Platform); large-body decode-place hold is 30 s, add a 2-permit large-bodies semaphore; `shrink_to_fit` on the prefix result; memory-test baseline under-reports peak; encoded bodies awaiting ClickHouse unbounded in aggregate beyond the 32 places.
- LOG-4: TLS syslog (-> Platform); graceful shutdown, queued frames lost on exit (-> Platform); UDP oversize branch unreachable on IPv4.
- LOG-5: numeric `eq` compares text (200 vs 200.0); Rust regex pre-check stricter than RE2; `user_error` substring-matches codes; paging window not carried in the cursor (drift without a fixed window); no result row cap or search rate limit beyond the wave's budgets; 31-day window gives about 32 histogram buckets; empty `contains` matches all.
- LOG-6: Last-Event-ID resume (rows in a reconnect gap are lost) -> Part 2; per-instance global tail cap -> Platform; no test for the `MAX_UNKNOWN_CHECKS` path.
- LOG-7: viewers see Reveal/Rotate buttons (engine 403 message); token panel a11y (`role=status` announces token), snippet tab a11y; rsyslog template token unescaped (hex today); no component tests.
- LOG-8: explorer live tail restarts on any navigation, stale Load-more response race, live rows unvalidated and not deduped, histogram/sources failures look empty, `<pre>` fields do not wrap, a11y gaps, no live-tail lifecycle test (-> Part 2).
- Final review, moved to Part 2 plan: `chunk_rows` on the async syslog batcher should run off-thread; the tail test should assert that a poll fetches only new rows; entry reads count against the per-user cap, so fast expands can hit 429.
- Final review, moved to Platform (NAMI-PLT): per-IP HTTP limits (the 512-connection cap is global), configurable connection caps and fd limits (Linux needs a limit above 1024), graceful shutdown.
- Verification note: no CI exists. Final state was engine 854 passed, UI 412 passed, clippy and svelte-check clean, all run locally and verified in review.
