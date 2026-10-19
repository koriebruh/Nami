# Uptime Monitoring Phase 3 — decisions and carried-forward items

Phase 3 (NAMI-UP-20..32, PRs #53, #55, #57, #60, #61, #64, #65, #68-#70, #72-#74) and its final security review fix wave (PR #77) were executed with subagent-driven development. The plan itself landed as PR #49. During execution the controller made the rulings below on the owner's behalf (directive: "jangan tunggu konfirmasi"). Each line states what was decided, why, and what it costs if wrong. Undo any of them by opening an issue or telling the agent.

Plan: `docs/superpowers/plans/2026-10-13-uptime-phase3.md`. Spec: `docs/superpowers/specs/2026-10-13-uptime-phase3-design.md`. The plan was reviewed and drift-fixed before dispatch, but its code was not scratch-verified: implementers fixed compile drift against main and reported it.

Task to PR map: UP-20 #53 (heartbeat `/fail` and exit codes), UP-21 #55 (output capture, `heartbeat_pings`, incident details), UP-22 #57 (heartbeat groups), UP-23 #64 (IP version, cookies, encrypted proxy settings), UP-24 #68 (checks honour them), UP-25 #60 (import parsers), UP-26 #73 (import endpoint), UP-27 #61 (availability report), UP-28 #65 (check-history CSV export), UP-29 #69 (heartbeat UI), UP-30 #72 (advanced options UI), UP-31 #74 (import UI), UP-32 #70 (availability card and export UI), final fix wave #77.

File paths below are the post-restructure ones (see `docs/superpowers/decisions/2026-10-19-domain-restructure.md`).

## Rulings (in the order made)

### Process and scope

- Ruling: Phase 3 heartbeat tasks (UP-20..22) may start while the Phase 2 fix wave (#58) is in flight; the monitor-model tasks (UP-23+) wait until it merges. Migrations 0020/0021 were reserved for that wave (it used none) — avoids conflicts in `monitor.rs` — cost if wrong: idle wait.
- Ruling: the shared `CARGO_TARGET_DIR` is dropped after parallel worktrees overwrote each other's test binaries (phantom failures); every task and review uses its own `.worktrees/.cargo-target-<TASKID>`, deleted on merge, at most ~4 at once — cost if wrong: disk use.
- Ruling (owner directive 2026-10-19): every UI follows `docs/superpowers/design/ui-reference-atom.png` through the shared design system (`src/lib/components/ui` + tokens, PR #66). UP-29..32 started only after it merged — cost if wrong: none.
- Ruling: briefs name the Postgres container `nami-engine-postgres-1` explicitly — cost if wrong: none.

### Heartbeats: signals, output and flapping

- Ruling: `/{token}/fail` records a failure and `/{token}/{exit_code}` either (`0` = success); the rate limit (1/s, burst 10) is keyed by `heartbeat_id`, so rotating the token does not reset it. The path order is token shape (404), suffix (400), declared length (413), lookup (404), rate limit (429), then body: nobody without a token makes the endpoint buffer a byte.
- Ruling: success pings store at most 1 KiB of output, fail and non-zero pings at most 10 KiB (plan OQ4), sanitized (control characters and the shared invisible/bidi set dropped, U+FFFD for bad UTF-8) and capped after filtering; the 10 KiB output is copied to `incidents.details` (DB check `<= 10240 + 16`) — cost if wrong: one constant.
- Ruling: the exit-code snippet reads stdin (`--data-binary @-`); after the fix wave it is `tail -c 10240 | curl -fsS -m 10 --retry 3 --data-binary @- '<url>'/$code`, so the end of the output (where errors are) is kept and a large output can no longer lose the exit code to a 413.
- Ruling: heartbeat groups reuse monitor groups (`heartbeats.group_id`, composite FK `(group_id, project_id)`, deleting a group ungroups) — one group list per project, spec §3 — cost if wrong: none.
- Ruling (fix wave #1, flap control): within 5 minutes of its resolution a heartbeat incident reopens instead of a new one being inserted (covers `/fail` and the sweep; monitors never reopen, since that would count their up-gaps as downtime). An in-memory `AlertThrottle` in `AppState`, shared by monitors and heartbeats, sends at most one DOWN and one recovery email per subject per `ALERT_FLAP_WINDOW` (5 min). A held DOWN is cancelled by the next recovery, otherwise `run_deferred_alerts` (every instance, every 5 s) sends it once the window ends, skipping subjects that are deleted, paused or in maintenance. Live: a 33 s flap loop gives 1 incident and 2 emails per member (was 12 incidents and 23 emails) — cost if wrong: alerts delayed up to 5 min during a flap.
- Ruling (fix wave #2, pings never lose their signal): when the body cannot be read (no per-heartbeat place, no global or per-project body slot, the 10 s body timeout, a broken body) the ping is still recorded, with a fixed marker as output; 413 only for a declared length over 1 MiB. Body reads are capped at 2 per project (16 in all), and the 4 database places are waited for up to 2 s (`PING_SLOT_WAIT`) instead of `try_acquire`. Live: 24 held bodies across 8 projects, another tenant's `/0` and `/fail` still answer 200 — cost if wrong: a busy marker where output was expected.
- Ruling (fix wave #4): each heartbeat has an output budget of 1 MiB per hour (byte token bucket); past it rows store `[output not stored: output budget used]` — bounds ClickHouse growth from a token holder looping `/fail` — cost if wrong: lost output under heavy legitimate use.
- Ruling (fix wave #5): one invisible-character predicate (`core::validate::is_invisible_or_bidi_char`) for names, URLs and ping output, extended with the tag characters U+E0000-E007F and U+FFF9-FFFB. Emoji subdivision flags (U+1F3F4 + tags + U+E007F) stay allowed, and a PATCH checks only the text fields it changes (`with_unchanged_values`), so rows stored before the rule can still be paused or edited.

### Advanced request options and the proxy

- Ruling: `ip_version` (any/ipv4/ipv6), `remember_cookies` (a cookie jar per check, inside a per-check client) and an HTTP(S) proxy per monitor. `proxy_password_enc` uses the Phase 2 sealed format with AAD `monitors.proxy_password:<id>`; the API returns `has_proxy_password` only.
- Ruling: a proxy-origin change forces re-entry of the target secrets and the proxy password; a target-only URL change keeps the proxy password; removing the proxy (`proxy_url: null`) drops its stored username and password; an IP-literal proxy host goes through the same destination guard; proxy URLs with userinfo are refused.
- Ruling: the proxy host resolves through `GuardedResolver`, and an https proxy is always verified against exactly its own name by `ProxyOnlyVerifier` (a custom rustls verifier, one shared `OnceLock`), whatever the monitor's `verify_ssl` says. If any resolved proxy address is forbidden the check is refused.
- Ruling (accepted residual, T24): behind a proxy the target name is only pre-resolved locally (a name that does not resolve on the engine goes ahead) and redirect hops are not resolved at all; a proxy sharing the engine's network could fetch metadata as a slow status/keyword oracle → NAMI-PLT (`MONITOR_PROXY_ALLOWLIST`, with Phase 2's `MONITOR_DENY_CIDRS`).

### Import

- Ruling: formats are a URL list, Uptime Kuma JSON backup and UptimeRobot CSV, at most 500 entries, per-entry results; an unknown format is 422; one import per project at a time (`pg_try_advisory_xact_lock`, 409, never waits); the project-exists check (404) runs before parsing.
- Ruling: every invalid entry is a per-entry error, never a 500: blank, duplicate (case-insensitive) or reserved headers, more than 20 headers, i32 overflow (generic `UNREADABLE_ENTRY` text, no serde message), a body with GET, keyword with HEAD, a password without a username, timeout >= interval, a DNS IP literal. Entries are never logged with `{:?}`; error rows show header names, never values (live: no planted secret echoed).
- Ruling: Kuma's numeric flags (`upsideDown`, `invertKeyword` as `1`) keep their meaning; `accepted_statuscodes` keeps only the first range, with a per-entry warning; `notificationList` is never read.
- Ruling: the skip-existing key is URL + check type, plus the DNS record type and keyword (+ mode) where they apply.
- Ruling (fix wave #3): UptimeRobot's keyword type is the alert condition: `exists` / `1` / `alert_exists` import as `not_contains`, `not exists` / `2` / `alert_not_exists` as `contains`, missing or unknown as `contains` with a warning (source: UptimeRobot help article 11358441 and the `getMonitors` docs). The fixture was corrected; line numbers are file lines (pinned by a multi-line test).
- Ruling: the import body limit is 2 x 2 MiB + 64 KiB (escaped JSON), so the BFF `BODY_SIZE_LIMIT` must be at least 4.1 MB (`.env.example` sets `5M`).

### Availability and export

- Ruling: the availability report covers any range up to 3 years (1096 days), for monitors and heartbeats; maintenance time is subtracted (sort-and-merge, not the O(n x m) per-incident re-sort the first version had). A malformed query is 400 before the role check (leaks nothing; accepted). Reopened heartbeat incidents count the up-gap inside them as downtime (documented).
- Ruling: the check-history CSV export streams up to 180 days with a 30 s send-stall timeout and a 600 s total deadline; an abort ends the body with an error, never a clean close, and the BFF `relayStream` propagates both upstream errors and client cancellation. Slots: 2 per user, 2 per project, 8 per instance (fix wave #9; was 4 global).
- Ruling (fix wave #6): both CSV exports (uptime and logs) quote every cell (`QuoteStyle::Always`) through `core::download::push_csv_field`, keeping the leading-`'` formula guard, so a `;`-splitting spreadsheet locale cannot turn error text into formulas.

### UI

- Ruling: heartbeat pages show failure pings, recent pings with output (as text), groups, the exit-code snippet with the URL single-quoted; the incidents page renders job output and is served `private, no-store` (fix wave #7), like every page that shows output. The monitor form gains IP version, cookies and proxy, with "Keep" taken away when re-entry is required. The import page previews a dry run first. No `{@html}` anywhere.

## Limits and environment

- Pings: 1/s per heartbeat, burst 10; body <= 1 MiB (declared), body read timeout 10 s; 16 concurrent body reads, 2 per project; 4 database places, 2 s wait; output 10 KiB (fail) / 1 KiB (success), 1 MiB per hour per heartbeat; recent pings list default 50.
- Flap control: `ALERT_FLAP_WINDOW` 5 min, deferred-alert loop every 5 s on every instance. A held DOWN is lost on restart (see Deferred).
- Import: 500 entries, 2 MiB file, 4.1 MB request (`BODY_SIZE_LIMIT` >= 4.1M on the BFF).
- Availability: 3-year range. Export: 180 days, 64 KiB chunks, 30 s stall, 600 s total, 2 per user / 2 per project / 8 per instance.
- Proxy URL <= 2048 B. ClickHouse: `heartbeat_pings` table (180-day TTL).
- Migrations used: 0022-0024 (0020-0021 reserved for the Phase 2 wave, unused). Since 2026-10-19 they are folded into the `0003_uptime` / `0004_incidents` baselines.

## Deferred items (carried forward, with their target)

- UP-20: public-router prune scan under pathological load; unmatched public routes answer an empty 404 without `no-store` → NAMI-PLT.
- UP-21: the 2 s ClickHouse timeout arm is untested (needs a stalling stub) → NAMI-PLT.
- UP-25: optionally drop empty-value Kuma headers (today the entry fails whole with "enter a value") → NAMI-PLT (import polish).
- UP-26: pre-lock JSON `Value` amplification by parallel dry runs, and the body parsed before the role check → NAMI-PLT (rate limits, global body budget). Import as an amplifier (all imported monitors due at once; `skip_existing: false` can import one URL 500 times) → NAMI-PLT quotas, with Phase 2's per-project monitor cap; spread the first `next_check_at`.
- UP-31: production adapter choice (`adapter-node` body limit) → NAMI-PLT.
- Design-system chart primitives → NAMI-MET.
- Flap control: persist `last_alerted_kind` so a held DOWN survives a restart, and reset escalation/acknowledgement state when an incident reopens → NAMI-INC.
- Log export per-project slots (`core::download::ProjectSlots` is ready) → Logs Part 2 security wave.
- Per-IP connection limiter on the public listener (well-formed-token floods take the 4 database places; the 2 s wait bounds the cost to latency) → NAMI-PLT.
- Viewers read ping output and incident details while tokens are member-only (job output can hold secrets) → product decision in NAMI-INC or NAMI-PLT.
- Proxied and cookie checks rebuild their TLS state per check → NAMI-PLT (performance).
- Proxy residuals (local pre-resolution only, redirect hops unresolved, sidecar-proxy metadata oracle) → NAMI-PLT (`MONITOR_PROXY_ALLOWLIST` + `MONITOR_DENY_CIDRS`).
- Phase 1 and Phase 2 deferrals keep the destinations listed in `2026-10-13-uptime-phase1.md` and `2026-10-13-uptime-phase2.md`.
