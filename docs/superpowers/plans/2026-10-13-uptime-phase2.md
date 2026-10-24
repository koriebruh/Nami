# Uptime Monitoring — Phase 2 Implementation Plan

> **Status:** done. Historical record of a merged phase. Paths, module names and migration numbers below predate the 2026-10-19 restructure; translate them with `docs/superpowers/decisions/2026-10-19-domain-restructure.md`. Do not execute this plan again.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining BetterStack uptime gaps in Nami — request configuration with encrypted write-only secrets, eight more monitor types (ping, DNS, SSL and domain expiry, SMTP/POP3/IMAP, UDP), a recovery threshold, one-off and weekly maintenance windows, and monitor groups — with the nami-ui screens to manage them.

**Architecture:** nami-engine gains `src/crypto.rs` (AES-256-GCM, key from `NAMI_ENCRYPTION_KEY`) that seals custom header values and basic-auth passwords before they reach Postgres; API responses go through `MonitorView`, which reduces secrets to header names and `has_auth_password`. The checker takes a per-pass `CheckContext` (prebuilt reqwest clients per redirect/TLS setting, the cipher, a rustls client config with the trust roots, the RDAP base URL) and dispatches each new type to its own small probe module (`uptime/{ping,dns,expiry,mail,udp,tls}.rs`). The worker's state machine learns `recovery_threshold` and freezes entirely while a maintenance window (a pure, DST-aware `Schedule::covers`) applies; the heartbeat sweep and pings honour project-wide windows. Monitor groups are a flat `group_id` on monitors plus their own CRUD; the UI groups the list client-side.

**Tech Stack:** Rust 2024 (Axum 0.8, SQLx 0.9 runtime queries, reqwest 0.13, tokio), new crates `aes-gcm 0.11.1`, `base64 0.22.1`, `surge-ping 0.9.1`, `hickory-resolver 0.26.3`, `rustls 0.23.45`, `tokio-rustls 0.26.6`, `webpki-roots 1.0.9`, `x509-parser 0.18.1`, `chrono-tz 0.10.4` (dev: `rcgen 0.14.10`), Postgres 17, ClickHouse 26.9, Mailpit, SvelteKit 3 + Svelte 5 runes + Tailwind 4, vitest.

**Spec:** `docs/superpowers/specs/2026-10-13-uptime-phase2-design.md` (with its "Amendments (2026-10-13)" section: controller rulings A-D). Phase 1 context: `docs/superpowers/specs/2026-10-13-uptime-monitoring-design.md`, plan `docs/superpowers/plans/2026-10-13-uptime-monitoring.md`.

**When to start:** only after every Phase 1 task (NAMI-UP-1 .. NAMI-UP-8) is merged to `main`. This plan was written against the complete Phase 1 end state; Phase 1 review fixes landing in the meantime may shift a line, a name or a test count. Implementers adapt such drift minimally (same behaviour, same tests) and note it in the PR; they never redesign around it.

**Verification status (2026-10-13):** every task below was applied, in order, to a scratch copy of the Phase 1 end state (plus the NAMI-UP-1 review fixes already on `main`), one commit per task: after each engine task the full engine suite was green against Postgres 17, ClickHouse 26.9 and Mailpit (216 tests after Task 17; Phase 1 alone had 125), and after each UI task `npm run check` reported 0 errors / 0 warnings with vitest green (63 tests after Task 19). A live walkthrough (engine + UI + browser) created every new monitor type, edited secrets (keep/replace/clear), grouped monitors, and scheduled one-off and weekly maintenance windows; it found one bug (the monitor form wiped typed values when the check type changed), which Task 18 fixes and pins with a test. Code blocks below are copied from those commits. Still run every step — the scratch copy is not the repo.

**Execution:** per CLAUDE.md — one worktree per task under `.worktrees/NAMI-UP-<n>/`, branch from the latest `origin/main`, the branch named under each task heading, Conventional Commits, task review, PR, and auto-merge on a clean review (pre-authorized for sub-projects 2-6). Tasks are sequential: each consumes the interfaces of the ones before it. How changes are written down: new files are given in full; a modified file is given as a unified diff against the previous task's state (apply it with `git apply` from the repo root, or by hand — the hunks are small), except files a task mostly rewrites, which are given in full. `Cargo.lock` changes come from the `cargo add` commands shown and are committed with the task.

## Global Constraints

- All infrastructure runs as containers from `nami-engine/docker-compose.yaml` (Postgres on host port **5433**, ClickHouse 8123, Mailpit 1025/8025), never native installs. Engine tests run with `DATABASE_URL=postgres://nami:nami@localhost:5433/nami`.
- SQLx runtime queries only (`query`/`query_as`/`query_scalar`), no compile-time macros, no ORM; SQL strings are literals (SQLx 0.9 takes `&'static str`) — never `format!` SQL.
- ClickHouse values are always bound query parameters (Phase 1 rule; this plan adds no ClickHouse queries).
- axum 0.8 path syntax `{param}`. Authorization failure is 403, never 404; a resource id from another project inside an accessible project path is 404 (spec: "Maintenance window ids/monitor ids from another project → 404").
- Project-scoped RBAC like `routes/projects.rs`: viewer reads, member creates/edits, admin deletes — for maintenance windows and monitor groups too (spec §4, §5).
- Request config (spec §1): methods `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS` (keyword monitors still forbid `HEAD`); request body max 64 KiB and only for `POST`/`PUT`/`PATCH`; up to 20 custom headers, names RFC 7230 tokens, values max 1024 bytes, no CR/LF anywhere, `Host`/`Content-Length`/`Transfer-Encoding`/`Connection` rejected; `follow_redirects` default true, max 10 hops; `verify_ssl` default true.
- Secrets at rest (spec §1): header values and the basic-auth password are encrypted with AES-256-GCM, random 96-bit nonce per value, stored as nonce‖ciphertext, key `NAMI_ENCRYPTION_KEY` (32 bytes, base64); the engine refuses to start without it outside tests. Secrets are write-only: PATCH replaces when provided, keeps when omitted, clears on explicit `null`. Responses carry header names with `"value_set": true` and `has_auth_password`, never values. Secrets never appear in an API response, log line or alert email.
- New types (spec §2 + ruling A): `ping` (unprivileged datagram ICMP, response time = RTT), `dns` (`A`, `AAAA`, `CNAME`, `MX`, `TXT`, `NS`, optional nameserver and expected value; down on NXDOMAIN, SERVFAIL, timeout, empty answer, missing value), `ssl_expiry` (TLS with SNI, default port 443, threshold default 14 days), `domain_expiry` (RDAP via `https://rdap.org/domain/<domain>` following its redirect, threshold default 30 days), `smtp`/`pop3`/`imap` (greeting `220` / `+OK` / `* OK` or `* PREAUTH`, optional implicit TLS via `use_tls`), `udp` (required `udp_payload`, up when a datagram arrives and contains `keyword` if set). `ssl_expiry` and `domain_expiry` intervals: minimum 3600 s, default 86400 s.
- `recovery_threshold` 1-10, default 1 (spec §3). Maintenance windows (spec §4 + ruling B): `once` or `weekly` (ISO weekdays 1-7, local times that may wrap past midnight, IANA zone validated with chrono-tz); while covered, checks run and are recorded but no incident opens or resolves, no alert is sent, counters do not move; an incident open before the window stays open and can resolve after it.
- Groups (spec §5 + ruling C): the monitors list stays a flat array (each monitor carries `group_id`); groups come from `GET /projects/{id}/monitor-groups`, ordered by position then name; the UI groups client-side.
- Monitor targets are not SSRF-restricted (ruling D, as in Phase 1: self-hosted, internal hosts are legitimate).
- nami-ui: every engine call goes through `callEngine`; SvelteKit 3 conventions from Phase 1 (`#lib` alias, `process.env` in server code, runes, `page` from `$app/state`, `refreshAll`, named actions never mixed with `default`, `use:enhance` where client state matters); dark theme classes copied from the existing pages; times shown in UTC.
- Tests: Mailpit assertions filter by their own recipient and never delete messages; test binaries run one after another (`cargo test`), never a cross-binary parallel runner.
- Commits: Conventional Commits whose message ends with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- Switching the check type in the monitor form must not wipe what the user already typed (seen live: picking "SSL certificate expiry" cleared the name and timeout, because Svelte re-applies `value={...}` when a block re-renders) → Task 18 (`form-inputs.test.ts`: "never sets value={...} or checked={...} on an input or textarea").
- Editing a monitor without touching its secrets (rename, pause, regroup) must leave the stored ciphertext byte for byte — a rename must never wipe an API key or password → Task 10 (`patch_keeps_omitted_secrets_replaces_given_ones_and_null_clears`).
- A custom header that carries a secret must never be forwarded to another host when the target redirects (reqwest strips `Authorization`/`Cookie` on a cross-host hop, but not `X-Api-Key`) → Task 11 (`custom_headers_never_follow_a_redirect_to_another_host`).
- An expiry monitor created without an interval must be checked daily, and never more often than hourly — otherwise every domain monitor queries rdap.org every 60 s and gets rate-limited → Task 14 (`expiry_monitors_get_daily_checks_and_type_specific_thresholds`).
- A weekly maintenance window that wraps midnight or spans a DST change must cover exactly the wall-clock hours it names — no window that silently starts an hour late or covers the wrong day → Task 16 (`a_window_ending_at_or_before_its_start_runs_past_midnight`, `daylight_saving_spring_forward_shortens_or_skips_a_window`, `daylight_saving_fall_back_covers_the_repeated_hour_twice`).

## Decisions the spec leaves open

- **Header names are stored in the clear** in `request_header_names text[]`, beside `request_headers_enc` (the sealed JSON `[{"name","value"}]` the spec describes). Reads list names without the key, so a changed or lost key never breaks a monitor list.
- **Headers are replaced as a whole:** PATCH `request_headers: [...]` replaces every header (`[]` or `null` clears them); there is no per-header keep. The UI offers keep / replace all / remove all.
- **Each sealed value is bound to its column** (`monitors.request_headers` / `monitors.auth_password` as AES-GCM associated data), so a blob from one column cannot be opened as the other.
- **Basic auth:** removing the username removes the stored password; a password without a username is a 400; basic auth together with a custom `Authorization` header is a 400 (reqwest would send both); the username may not contain `:` (RFC 7617).
- **The request body is not a secret** (the spec encrypts only header values and the password): it is stored and returned in the clear. Credentials belong in headers or basic auth.
- **Redirects with custom headers:** such monitors only follow redirects that stay on the monitored host name (scheme and port may change, e.g. http→https); a redirect elsewhere is reported as down, naming the host. Monitors without custom headers follow up to 10 hops anywhere.
- **Undecryptable secrets** (key changed, tampering) make the check down with the generic error `request configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)`; nothing is sent to the target, and the pass carries on with the other monitors.
- **Recovery threshold:** successes are only counted while a monitor is `down`; any failure resets the count; heartbeats still recover on their first ping (spec: heartbeats unchanged).
- **DNS checks** build a fresh resolver per check (no cache, hosts file ignored), query the name as fully qualified (no search domains), keep only answers of the requested type, and match `expected_value` case-insensitively as a substring of an answer's readable form (names without the trailing dot, MX as `preference exchange`, TXT strings joined). `dns_nameserver` is an IP with an optional port, stored as `ip:port`; empty means the system resolver.
- **Ping** sends one echo request (56 zero bytes) to the host's first address, preferring IPv4; the check's own timeout bounds it.
- **SSL expiry** verifies against the Mozilla roots in `webpki-roots` (not the OS store); "within the threshold" means less than `expiry_threshold_days` × 24 h left; days are rounded down; an expired or untrusted certificate fails the handshake and the error says why (`certificate expired 3 days ago (2026-10-10)`, `certificate is not trusted (unknown issuer)`).
- **Domain expiry** reads `RDAP_BASE_URL` (default `https://rdap.org`), sends `Accept: application/rdap+json, application/json`, follows redirects on the default client, reads at most 1 MiB, and uses the first `expiration` event.
- **Expiry intervals:** omitted on create → 86400 s for the expiry types (60 s for the others); below 3600 s is a 400, and a table check constraint enforces the same.
- **Mail checks** support implicit TLS only (no STARTTLS) and always verify the certificate (`verify_ssl` is an http/keyword setting); the greeting is the first line, at most 512 bytes read, quoted in errors with control characters removed and at most 100 characters.
- **UDP payloads** are text; the UI writes them on one line with `\n`, `\r`, `\t`, `\\` escapes. `keyword_mode` applies to the reply (`contains`/`not_contains`). An ICMP port-unreachable is reported as `connection refused (port unreachable)`.
- **Maintenance freezes the whole state machine:** status and both counters stay as they are and no incident changes; only `last_checked_at` (and the ClickHouse history) moves. A heartbeat ping during a window only updates `last_ping_at`. Windows scoped to a monitor never cover heartbeats. Reads expose `in_maintenance` (monitors, heartbeats) and `active` (windows).
- **Project consistency in the database:** windows reference `(monitor_id, project_id)` and monitors reference `(group_id, project_id)` through composite foreign keys, so another project's monitor or group cannot be referenced even by a bug; deleting a group ungroups its monitors (`on delete set null (group_id)`, Postgres 15+); deleting a monitor deletes its scoped windows.
- **Group positions** are 0..1,000,000 (default 0). The list page hides empty groups and shows ungrouped monitors last; with no groups it looks exactly like Phase 1.
- **One-off windows are entered and shown in UTC** (Phase 1 shows every time in UTC); weekly windows carry their own zone, defaulting to the browser's.
- **BFF ids:** ids taken from form fields or route params are checked to be UUIDs (`isUuid`) before they go into an engine path — `fetch` would normalise `../` segments into a different engine route.
- **Test fixtures:** `Monitor` derives `Default` so checker tests can build in-memory monitors with `..Monitor::default()`; real monitors always come from the database.

## File Structure

```
nami-engine/
  Cargo.toml                          T9 aes-gcm, base64; T11 dev rcgen (+ rustls, tokio-rustls); T13 surge-ping,
                                      hickory-resolver; T14 rustls, tokio-rustls, webpki-roots, x509-parser; T16 chrono-tz
  .env.example                        T9 NAMI_ENCRYPTION_KEY; T13 ping_group_range note; T14 RDAP_BASE_URL
  migrations/
    0012_monitor_request_config.sql   T10 http_method widened; request config + sealed secret columns
    0013_monitor_recovery_threshold.sql T12 recovery_threshold, consecutive_successes
    0014_monitor_ping_dns.sql         T13 check_type + ping, dns; dns_record_type, dns_nameserver, expected_value
    0015_monitor_expiry_checks.sql    T14 check_type + ssl_expiry, domain_expiry; expiry_threshold_days; interval check
    0016_monitor_mail_udp.sql         T15 check_type + smtp, pop3, imap, udp; use_tls, udp_payload
    0017_create_maintenance_windows.sql T16 maintenance_windows (+ monitors (id, project_id) unique)
    0018_create_monitor_groups.sql    T17 monitor_groups; monitors.group_id (composite FK)
  src/
    crypto.rs                         T9  Cipher (AES-256-GCM), parse_key, SecretString, TEST_KEY
    config.rs                         T9  encryption_key; T14 rdap_base_url
    state.rs, main.rs, lib.rs         T9  AppState.cipher
    models/
      mod.rs                          T10 nullable(); T13 mod target; T16 validate_label, mod maintenance; T17 mod monitor_group
      monitor.rs                      T10 request config, MonitorView, write_config; T12-T15 new fields/types; T16 in_maintenance; T17 group_id
      monitor_request.rs              T10 headers/password validation, HeadersChange/PasswordChange, seal/open
      target.rs                       T13 validate_host, parse_nameserver
      maintenance.rs                  T16 MaintenanceWindow, Schedule::covers, validation, queries
      monitor_group.rs                T17 MonitorGroup + queries
      heartbeat.rs                    T16 HeartbeatView
    routes/
      monitors.rs                     T10 MonitorView responses; T16 in_maintenance; T17 group check
      heartbeats.rs                   T16 HeartbeatView responses
      maintenance.rs                  T16 /projects/{id}/maintenance-windows CRUD
      monitor_groups.rs               T17 /projects/{id}/monitor-groups CRUD
    router.rs                         T16, T17 merge the new routers
    uptime/
      checker.rs                      T11 CheckContext, HttpClients, request config; T13 Observation shared; T14 tls/rdap context
      worker.rs                       T11 per-pass CheckContext; T12 recovery threshold; T16 maintenance freeze
      heartbeat.rs                    T16 maintenance in sweep and pings
      ping.rs, dns.rs                 T13
      tls.rs, expiry.rs               T14
      mail.rs, udp.rs                 T15
  tests/
    common/mod.rs                     T11 unsaved_monitor, check_context, test_cipher
    common/tls.rs                     T11 self_signed, serve_https_ok; T14 TestCa; T15 serve_tls_banner
    crypto.rs, startup.rs             T9
    monitor_request_config.rs         T10
    checker_http_config.rs            T11
    recovery_threshold.rs             T12
    checker_ping_dns.rs               T13
    monitor_types_api.rs              T13 (extended in T14, T15)
    checker_expiry.rs                 T14
    checker_mail_udp.rs               T15
    maintenance_windows.rs            T16
    monitor_groups.rs                 T17
    monitors_api.rs, uptime_checker.rs, migrations.rs   adjusted in T10, T11, T16, T17

nami-ui/src/
  lib/uptime.ts (+ .test.ts)          T18 types for every field/type, groupMonitors, monitorTarget, payload escapes
  lib/maintenance.ts (+ .test.ts)     T19 window types and display helpers
  lib/server/monitor-form.ts (+ .test) T18 every type, request config, secrets keep/replace/clear
  lib/server/maintenance-form.ts (+ .test) T19
  lib/server/engine.ts (+ .test)      T19 isUuid
  lib/components/MonitorForm.svelte   T18 rewritten (defaultValue convention)
  lib/components/MaintenanceBadge.svelte T18
  lib/components/MaintenanceWindowForm.svelte T19
  lib/components/form-inputs.test.ts  T18 (extended in T19)
  routes/(app)/projects/[id]/
    +layout.svelte                    T19 Maintenance tab
    monitors/**                       T18 groups, badges, new form
    heartbeats/**                     T18 badges
    maintenance/+page.*, maintenance/[windowId]/+page.*   T19
    settings/+page.*                  T19 monitor groups card
```

---

### Task 9 (NAMI-UP-9): Encryption at rest — `crypto.rs` and `NAMI_ENCRYPTION_KEY`

Branch `feat/NAMI-UP-9-secret-encryption`, worktree `.worktrees/NAMI-UP-9/`.

**Files:**
- Create: `nami-engine/src/crypto.rs`
- Modify: `nami-engine/Cargo.toml` (+ `Cargo.lock`), `nami-engine/src/lib.rs`, `nami-engine/src/config.rs`, `nami-engine/src/state.rs`, `nami-engine/src/main.rs`, `nami-engine/.env.example`
- Test: `nami-engine/tests/crypto.rs`, `nami-engine/tests/startup.rs`

**Interfaces:**
- Consumes: `Config::from_env()`, `Config::from_env_defaults_only()` (private), `Config::from_env_for_test()`, `AppState::for_test(PgPool) -> AppState` (Phase 1).
- Produces:
  - `crypto::KEY_LEN: usize = 32`; `crypto::TEST_KEY: [u8; KEY_LEN]` (the fixed test key).
  - `crypto::Cipher` (`Clone`, `Debug` prints `Cipher(<redacted>)`): `Cipher::new(key: &[u8; KEY_LEN]) -> Cipher`; `fn encrypt(&self, context: &str, plaintext: &[u8]) -> Vec<u8>` (nonce‖ciphertext‖tag); `fn decrypt(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, DecryptError>`.
  - `crypto::DecryptError` (unit struct, `Display`: `stored secret could not be decrypted`).
  - `crypto::parse_key(raw: &str) -> Result<[u8; KEY_LEN], String>` (the error is a complete startup message starting with `NAMI_ENCRYPTION_KEY`).
  - `crypto::SecretString` (`Deserialize` transparent, `Clone`, `PartialEq`, `Eq`, no `Serialize`, `Debug` prints `SecretString(<redacted>)`): `SecretString::new(impl Into<String>)`, `fn expose(&self) -> &str`.
  - `Config.encryption_key: [u8; KEY_LEN]` (from `NAMI_ENCRYPTION_KEY` in `from_env`, `TEST_KEY` in `from_env_for_test`).
  - `AppState.cipher: Cipher` (built from `config.encryption_key`; `AppState::for_test` uses the test key).

- [ ] **Step 1: Add the crates**

```bash
cd nami-engine
cargo add aes-gcm@0.11.1 base64@0.22.1
```

- [ ] **Step 2: Write the failing tests**

`nami-engine/tests/crypto.rs`:

```rust
use std::collections::HashSet;

use base64::Engine;
use nami_engine::crypto::{parse_key, Cipher, DecryptError, SecretString, KEY_LEN, TEST_KEY};

const CONTEXT: &str = "monitors.auth_password";

fn cipher() -> Cipher {
    Cipher::new(&TEST_KEY)
}

#[test]
fn encrypt_then_decrypt_round_trips() {
    let sealed = cipher().encrypt(CONTEXT, b"s3cret password");
    assert_eq!(cipher().decrypt(CONTEXT, &sealed).unwrap(), b"s3cret password");
}

#[test]
fn the_sealed_value_is_nonce_then_ciphertext_and_tag_and_hides_the_plaintext() {
    let plaintext = b"Bearer abc.def.ghi";
    let sealed = cipher().encrypt(CONTEXT, plaintext);
    // 12-byte nonce + ciphertext (same length as the plaintext) + 16-byte tag.
    assert_eq!(sealed.len(), 12 + plaintext.len() + 16);
    assert!(
        !sealed.windows(plaintext.len()).any(|w| w == plaintext),
        "the plaintext must not appear in the sealed bytes"
    );
}

#[test]
fn every_encryption_uses_a_fresh_random_nonce() {
    let c = cipher();
    let mut nonces = HashSet::new();
    for _ in 0..1000 {
        let sealed = c.encrypt(CONTEXT, b"same plaintext");
        assert!(nonces.insert(sealed[..12].to_vec()), "a nonce was reused");
    }
    assert_ne!(c.encrypt(CONTEXT, b"x"), c.encrypt(CONTEXT, b"x"), "same input, different output");
}

#[test]
fn any_tampering_fails_cleanly() {
    let c = cipher();
    let sealed = c.encrypt(CONTEXT, b"s3cret password");
    for index in [0, 11, 12, sealed.len() / 2, sealed.len() - 1] {
        let mut tampered = sealed.clone();
        tampered[index] ^= 0x01;
        assert_eq!(c.decrypt(CONTEXT, &tampered), Err(DecryptError), "flipped byte {index}");
    }
    for len in [0, 1, 11, 12, 27] {
        assert_eq!(c.decrypt(CONTEXT, &sealed[..len]), Err(DecryptError), "truncated to {len} bytes");
    }
}

#[test]
fn a_different_key_or_context_cannot_decrypt() {
    let sealed = cipher().encrypt(CONTEXT, b"s3cret password");
    let other = Cipher::new(&[9u8; KEY_LEN]);
    assert_eq!(other.decrypt(CONTEXT, &sealed), Err(DecryptError));
    assert_eq!(
        cipher().decrypt("monitors.request_headers", &sealed),
        Err(DecryptError),
        "a value sealed for one column cannot be opened as another"
    );
}

#[test]
fn the_decrypt_error_says_nothing_about_the_secret() {
    assert_eq!(DecryptError.to_string(), "stored secret could not be decrypted");
}

#[test]
fn parse_key_accepts_exactly_32_base64_bytes() {
    let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    assert_eq!(parse_key(&encoded).unwrap(), [7u8; 32]);
    assert_eq!(parse_key(&format!("  {encoded}\n")).unwrap(), [7u8; 32], "surrounding whitespace is ignored");
}

#[test]
fn parse_key_rejects_missing_malformed_and_wrong_length_keys() {
    let b64 = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![1u8; n]);
    let cases = [
        (String::new(), "must be set"),
        ("   ".to_string(), "must be set"),
        ("not base64!".to_string(), "valid base64"),
        (b64(16), "exactly 32 bytes, got 16"),
        (b64(33), "exactly 32 bytes, got 33"),
    ];
    for (raw, fragment) in cases {
        let message = parse_key(&raw).unwrap_err();
        assert!(message.starts_with("NAMI_ENCRYPTION_KEY"), "{message}");
        assert!(message.contains(fragment), "{raw:?}: {message:?} should mention {fragment:?}");
    }
}

#[test]
fn secret_strings_never_print_their_value() {
    let secret: SecretString = serde_json::from_str("\"hunter2\"").unwrap();
    assert_eq!(secret.expose(), "hunter2");
    assert_eq!(format!("{secret:?}"), "SecretString(<redacted>)");
    assert_eq!(SecretString::new("hunter2"), secret);
}
```

`nami-engine/tests/startup.rs` runs the real binary with an empty environment:

```rust
//! Runs the real `nami-engine` binary to prove it refuses to start without a
//! usable `NAMI_ENCRYPTION_KEY`. The environment is cleared and the working
//! directory is the system temp dir, so neither the developer's shell nor a
//! local `.env` file can supply a key.
use std::process::{Command, Output};

use base64::Engine;

fn start_engine(encryption_key: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nami-engine"));
    command
        .env_clear()
        .current_dir(std::env::temp_dir())
        // Never reached: the key is validated before the database is dialed.
        .env("DATABASE_URL", "postgres://nami:nami@127.0.0.1:1/nami");
    if let Some(key) = encryption_key {
        command.env("NAMI_ENCRYPTION_KEY", key);
    }
    command.output().expect("the nami-engine binary runs")
}

#[test]
fn the_engine_refuses_to_start_without_an_encryption_key() {
    let output = start_engine(None);
    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("NAMI_ENCRYPTION_KEY must be set"), "{stderr}");
}

#[test]
fn the_engine_refuses_to_start_with_a_key_of_the_wrong_length() {
    let short_key = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
    let output = start_engine(Some(&short_key));
    assert!(!output.status.success(), "the engine must not start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exactly 32 bytes, got 16"), "{stderr}");
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test crypto --test startup`
Expected: FAIL to compile — `error[E0432]: unresolved import nami_engine::crypto`.

- [ ] **Step 4: Implement `src/crypto.rs`**

```rust
//! Encryption of secrets at rest (monitor header values and basic-auth
//! passwords): AES-256-GCM with a random 96-bit nonce per value, stored as
//! `nonce || ciphertext || tag`. The key comes from `NAMI_ENCRYPTION_KEY`
//! (see `Config::from_env`).
//!
//! Every value is sealed for a `context` (e.g. `"monitors.auth_password"`),
//! passed as AES-GCM associated data: a value sealed for one column cannot be
//! opened as another.

use std::fmt;

use aes_gcm::{
    aead::{Aead, Nonce, Payload},
    Aes256Gcm, KeyInit,
};
use base64::Engine;
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;

/// AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// The fixed key `Config::from_env_for_test` (and so `AppState::for_test`)
/// uses. Production always reads `NAMI_ENCRYPTION_KEY` instead.
pub const TEST_KEY: [u8; KEY_LEN] = *b"nami-test-encryption-key-32bytes";

/// Decryption failed: wrong key, wrong context, truncated or tampered data.
/// Deliberately carries no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("stored secret could not be decrypted")]
pub struct DecryptError;

#[derive(Clone)]
pub struct Cipher {
    aead: Aes256Gcm,
}

/// Never prints key material.
impl fmt::Debug for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cipher(<redacted>)")
    }
}

impl Cipher {
    pub fn new(key: &[u8; KEY_LEN]) -> Cipher {
        Cipher { aead: Aes256Gcm::new(&(*key).into()) }
    }

    /// Seals `plaintext` under a fresh random nonce from the OS RNG.
    pub fn encrypt(&self, context: &str, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
        let ciphertext = self
            .aead
            .encrypt(&nonce, Payload { msg: plaintext, aad: context.as_bytes() })
            .expect("AES-GCM encryption of an in-memory buffer cannot fail");
        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        sealed
    }

    pub fn decrypt(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, DecryptError> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(DecryptError);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| DecryptError)?;
        self.aead
            .decrypt(&nonce, Payload { msg: ciphertext, aad: context.as_bytes() })
            .map_err(|_| DecryptError)
    }
}

/// Parses `NAMI_ENCRYPTION_KEY`: standard base64 (surrounding whitespace
/// ignored) that decodes to exactly 32 bytes. The error is a complete
/// startup message.
pub fn parse_key(raw: &str) -> Result<[u8; KEY_LEN], String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("NAMI_ENCRYPTION_KEY must be set to 32 random bytes, base64-encoded \
                    (generate one with `openssl rand -base64 32`)"
            .to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "NAMI_ENCRYPTION_KEY must be valid base64".to_string())?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| format!("NAMI_ENCRYPTION_KEY must decode to exactly 32 bytes, got {len}"))
}

/// A write-only secret from a request body (a header value, a password).
/// `Debug` never shows it and it cannot be serialized, so it cannot leak
/// through a log line or an API response by accident.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> SecretString {
        SecretString(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}
```

Register it in `src/lib.rs`:

```diff
diff --git a/nami-engine/src/lib.rs b/nami-engine/src/lib.rs
--- a/nami-engine/src/lib.rs
+++ b/nami-engine/src/lib.rs
@@ -1,6 +1,7 @@
 pub mod auth;
 pub mod clickhouse;
 pub mod config;
+pub mod crypto;
 pub mod db;
 pub mod email;
 pub mod error;
```

- [ ] **Step 5: Wire the key through `Config`, `AppState` and `main.rs`**

`src/config.rs` — `from_env` requires the key (a panic at startup is the refusal the spec asks for); the shared test defaults carry `TEST_KEY`:

```diff
diff --git a/nami-engine/src/config.rs b/nami-engine/src/config.rs
--- a/nami-engine/src/config.rs
+++ b/nami-engine/src/config.rs
@@ -27,6 +27,9 @@ pub struct Config {
     /// `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`, case-insensitively; any
     /// other value stops startup (see `env_flag`).
     pub uptime_worker_enabled: bool,
+    /// AES-256 key for secrets at rest (`NAMI_ENCRYPTION_KEY`, base64). Never
+    /// logged: `Config` deliberately does not implement `Debug`.
+    pub encryption_key: [u8; crate::crypto::KEY_LEN],
 }
 
 impl Config {
@@ -47,6 +50,7 @@ impl Config {
             cookie_secure: std::env::var("COOKIE_SECURE")
                 .map(|v| v == "true" || v == "1")
                 .unwrap_or(false),
+            encryption_key: encryption_key_from_env(),
             ..Self::from_env_defaults_only()
         }
     }
@@ -98,10 +102,20 @@ impl Config {
             ui_base_url: std::env::var("UI_BASE_URL")
                 .unwrap_or_else(|_| "http://localhost:5173".to_string()),
             uptime_worker_enabled: env_flag("UPTIME_WORKER_ENABLED", true),
+            // Tests only: `from_env` always replaces it with the real key.
+            encryption_key: crate::crypto::TEST_KEY,
         }
     }
 }
 
+/// `NAMI_ENCRYPTION_KEY` is required outside tests: a missing or malformed
+/// key stops startup instead of running with secrets nobody can decrypt
+/// later (or encrypted under a key nobody kept).
+fn encryption_key_from_env() -> [u8; crate::crypto::KEY_LEN] {
+    let raw = std::env::var("NAMI_ENCRYPTION_KEY").unwrap_or_default();
+    crate::crypto::parse_key(&raw).unwrap_or_else(|message| panic!("{message}"))
+}
+
 /// Reads a boolean env flag, `default` when it is unset. A value that is not
 /// a recognised spelling (see `parse_bool_flag`) panics at startup instead of
 /// falling back to a guess: for `UPTIME_WORKER_ENABLED`, an "API-only" replica
```

`src/state.rs`:

```diff
diff --git a/nami-engine/src/state.rs b/nami-engine/src/state.rs
--- a/nami-engine/src/state.rs
+++ b/nami-engine/src/state.rs
@@ -1,6 +1,6 @@
 use sqlx::PgPool;
 
-use crate::{clickhouse::ClickHouse, config::Config, email};
+use crate::{clickhouse::ClickHouse, config::Config, crypto::Cipher, email};
 
 /// A ClickHouse database name that is never created. `AppState::for_test`
 /// binds its client to it, so a test that touches ClickHouse without opting
@@ -13,6 +13,8 @@ pub struct AppState {
     pub db: PgPool,
     pub mailer: email::Mailer,
     pub clickhouse: ClickHouse,
+    /// Seals and opens monitor secrets; built from `config.encryption_key`.
+    pub cipher: Cipher,
     pub config: Config,
 }
 
@@ -20,8 +22,9 @@ impl AppState {
     /// Test-only constructor (same precedent as
     /// `router::with_fallback_connect_info`): every field gets the
     /// defaults tests expect — `Config::from_env_for_test()`, the Mailpit
-    /// mailer, and a ClickHouse client bound to a database that does not
-    /// exist. Tests override single fields with struct-update syntax, e.g.
+    /// mailer, a ClickHouse client bound to a database that does not
+    /// exist, and a cipher over the fixed `crypto::TEST_KEY`. Tests override
+    /// single fields with struct-update syntax, e.g.
     /// `AppState { mailer: Mailer::new("localhost", 1), ..AppState::for_test(pool) }`,
     /// so adding a field to `AppState` never touches test files again.
     pub fn for_test(pool: PgPool) -> AppState {
@@ -35,6 +38,7 @@ impl AppState {
                 &config.clickhouse_password,
                 UNCONFIGURED_TEST_CLICKHOUSE_DATABASE,
             ),
+            cipher: Cipher::new(&config.encryption_key),
             config,
         }
     }
```

`src/main.rs`:

```diff
diff --git a/nami-engine/src/main.rs b/nami-engine/src/main.rs
--- a/nami-engine/src/main.rs
+++ b/nami-engine/src/main.rs
@@ -1,4 +1,4 @@
-use nami_engine::{clickhouse::ClickHouse, config::Config, db, email, router, state::AppState};
+use nami_engine::{clickhouse::ClickHouse, config::Config, crypto::Cipher, db, email, router, state::AppState};
 use std::net::SocketAddr;
 use tower_sessions_sqlx_store::PostgresStore;
 
@@ -52,6 +52,7 @@ async fn main() {
         db: pool,
         mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
         clickhouse,
+        cipher: Cipher::new(&config.encryption_key),
         config,
     };
 
```

`nami-engine/.env.example` (append):

```diff
diff --git a/nami-engine/.env.example b/nami-engine/.env.example
--- a/nami-engine/.env.example
+++ b/nami-engine/.env.example
@@ -27,3 +27,8 @@ UI_BASE_URL=http://localhost:5173
 # Accepts true/false/1/0/yes/no/on/off (case-insensitive); anything else
 # stops startup rather than guessing.
 UPTIME_WORKER_ENABLED=true
+# 32 random bytes, base64-encoded. Encrypts monitor secrets at rest (custom
+# request header values, basic-auth passwords). Required: the engine refuses
+# to start without it. Generate one with `openssl rand -base64 32` and back it
+# up — secrets encrypted under a lost key cannot be recovered.
+NAMI_ENCRYPTION_KEY=
```

- [ ] **Step 6: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/crypto.rs` 9 passed, `tests/startup.rs` 2 passed, every Phase 1 binary still green (no other test file changes: `AppState::for_test` supplies the cipher).

- [ ] **Step 7: Put a real key in your local `.env`**

The engine no longer starts without one. Run `echo "NAMI_ENCRYPTION_KEY=$(openssl rand -base64 32)" >> nami-engine/.env` (the file is gitignored) and keep a copy: monitor secrets encrypted under a lost key cannot be recovered. Mention this breaking change in the PR description.

- [ ] **Step 8: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/.env.example nami-engine/src/crypto.rs \
  nami-engine/src/lib.rs nami-engine/src/config.rs nami-engine/src/state.rs nami-engine/src/main.rs \
  nami-engine/tests/crypto.rs nami-engine/tests/startup.rs
git commit -m "feat(engine): AES-256-GCM encryption of secrets at rest (NAMI-UP-9)

NAMI_ENCRYPTION_KEY (32 bytes, base64) is required outside tests; the
engine refuses to start without it. AppState carries the cipher.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10 (NAMI-UP-10): Request configuration model + API with write-only secrets

Branch `feat/NAMI-UP-10-request-config`, worktree `.worktrees/NAMI-UP-10/`.

**Files:**
- Create: `nami-engine/migrations/0012_monitor_request_config.sql`, `nami-engine/src/models/monitor_request.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/models/monitor.rs` (rewritten), `nami-engine/src/routes/monitors.rs`
- Modify (tests): `nami-engine/tests/monitors_api.rs` (POST is now a valid method), `nami-engine/tests/uptime_checker.rs` (its in-memory `Monitor` uses `..Monitor::default()`)
- Test: `nami-engine/tests/monitor_request_config.rs`

**Interfaces:**
- Consumes: `crypto::{Cipher, SecretString, DecryptError}`, `AppState.cipher` (Task 9); Phase 1 `models::validate_name`, `monitor::{Monitor, MonitorConfig, CreateMonitorRequest, PatchMonitorRequest, validate, find_in_project, list_for_project, delete_in_project}`.
- Produces:
  - Columns: `request_body text`, `request_headers_enc bytea`, `request_header_names text[] not null default '{}'`, `auth_username text`, `auth_password_enc bytea`, `follow_redirects boolean not null default true`, `verify_ssl boolean not null default true`; `monitors_http_method_check` widened to the seven methods.
  - `models::nullable` — `#[serde(default, deserialize_with = "nullable")]` on `Option<Option<T>>`: absent = keep, `null` = clear, value = set.
  - `models::monitor_request`: `RequestHeader { name: String, value: SecretString }` (`Deserialize`); `HeadersChange::{Keep { names: Vec<String> }, Clear, Replace(Vec<RequestHeader>)}` with `fn names(&self) -> Vec<&str>`; `PasswordChange::{Keep { present: bool }, Clear, Replace(SecretString)}` with `fn is_set(&self) -> bool`; `HeaderView { name: String, value_set: bool }`; consts `HTTP_METHODS`, `BODY_METHODS`, `MAX_BODY_BYTES` (65536), `MAX_HEADERS` (20), `MAX_HEADER_VALUE_BYTES` (1024), `RESERVED_HEADERS`, `HEADERS_CONTEXT`, `PASSWORD_CONTEXT`; `validate_header`, `validate_headers`, `validate_username`, `validate_password`; `seal_headers(&Cipher, &[RequestHeader]) -> Vec<u8>`, `open_headers(&Cipher, &[u8]) -> Result<Vec<RequestHeader>, DecryptError>`, `seal_password(&Cipher, &SecretString) -> Vec<u8>`, `open_password(&Cipher, &[u8]) -> Result<SecretString, DecryptError>`.
  - `Monitor` (now `Default`) gains `request_body`, `request_headers_enc` (never serialized), `request_header_names` (never serialized), `auth_username`, `auth_password_enc` (never serialized), `follow_redirects`, `verify_ssl`.
  - `MonitorView { monitor: Monitor (flattened), request_headers: Vec<HeaderView>, has_auth_password: bool }` with `impl From<Monitor> for MonitorView` — every monitor response uses it (Task 16 replaces `From` with `MonitorView::new(monitor, in_maintenance)`).
  - `MonitorConfig` gains `request_body`, `request_headers: HeadersChange`, `auth_username`, `auth_password: PasswordChange`, `follow_redirects`, `verify_ssl`.
  - `insert_monitor(pool: &PgPool, cipher: &Cipher, project_id: Uuid, c: &MonitorConfig) -> Result<Monitor, sqlx::Error>` and `update_in_project(pool: &PgPool, cipher: &Cipher, project_id: Uuid, monitor_id: Uuid, patch: PatchMonitorRequest) -> Result<Option<Monitor>, AppError>`; both write through one private `write_config(conn, cipher, id, &MonitorConfig, reschedule)` whose single `update` statement later tasks extend by appending columns.

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/monitor_request_config.rs`:

```rust
// Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use nami_engine::{
    crypto::{Cipher, TEST_KEY},
    models::monitor_request::{open_headers, open_password},
};
use serde_json::{json, Value};
use uuid::Uuid;

const HEADER_SECRET: &str = "header-secret-4f9a";
const PASSWORD_SECRET: &str = "password-secret-7c21";

fn configured_monitor() -> Value {
    json!({
        "name": "Orders API",
        "check_type": "http",
        "url": "https://example.com/orders",
        "http_method": "POST",
        "request_body": "{\"probe\":true}",
        "request_headers": [
            { "name": "X-Api-Key", "value": HEADER_SECRET },
            { "name": "Content-Type", "value": "application/json" }
        ],
        "auth_username": "monitor",
        "auth_password": PASSWORD_SECRET,
        "follow_redirects": false,
        "verify_ssl": false
    })
}

/// Fails if a secret, or any `*_enc` column, appears anywhere in `body`.
fn assert_redacted(body: &Value) {
    let text = body.to_string();
    for secret in [HEADER_SECRET, PASSWORD_SECRET] {
        assert!(!text.contains(secret), "secret leaked in {text}");
    }
    assert!(!text.contains("_enc"), "ciphertext column leaked in {text}");
    assert!(!text.contains("request_header_names"), "internal column leaked in {text}");
}

async fn stored_secrets(pool: &sqlx::PgPool, id: &str) -> (Option<Vec<u8>>, Vec<String>, Option<Vec<u8>>) {
    sqlx::query_as("select request_headers_enc, request_header_names, auth_password_enc from monitors where id = $1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn request_config_round_trips_and_every_response_redacts_secrets(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(&app, "POST", &collection, Some(&member), Some(configured_monitor())).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_redacted(&created);
    assert_eq!(created["http_method"], "POST");
    assert_eq!(created["request_body"], "{\"probe\":true}");
    assert_eq!(
        created["request_headers"],
        json!([{ "name": "X-Api-Key", "value_set": true }, { "name": "Content-Type", "value_set": true }])
    );
    assert_eq!(created["auth_username"], "monitor");
    assert_eq!(created["has_auth_password"], true);
    assert_eq!((created["follow_redirects"].clone(), created["verify_ssl"].clone()), (json!(false), json!(false)));
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    assert_redacted(&detail);
    assert_eq!(detail["request_headers"], created["request_headers"]);
    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_redacted(&list);
    assert_eq!(list[0]["has_auth_password"], true);
    let (status, patched) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "name": "Orders" }))).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_redacted(&patched);
}

#[sqlx::test]
async fn defaults_follow_redirects_and_verify_ssl(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let body = json!({ "name": "Home", "check_type": "http", "url": "https://example.com" });

    let (status, created) = call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(body)).await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["follow_redirects"], true);
    assert_eq!(created["verify_ssl"], true);
    assert_eq!(created["request_headers"], json!([]));
    assert_eq!(created["has_auth_password"], false);
    assert_eq!(created["request_body"], Value::Null);
}

#[sqlx::test]
async fn secrets_are_stored_as_ciphertext_never_plaintext(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) =
        call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(configured_monitor())).await;

    let (headers_enc, names, password_enc) = stored_secrets(&pool, created["id"].as_str().unwrap()).await;
    let (headers_enc, password_enc) = (headers_enc.unwrap(), password_enc.unwrap());
    for (sealed, secret) in [(&headers_enc, HEADER_SECRET), (&password_enc, PASSWORD_SECRET)] {
        assert!(!sealed.windows(secret.len()).any(|w| w == secret.as_bytes()), "plaintext {secret} stored");
    }
    assert_eq!(names, vec!["X-Api-Key", "Content-Type"], "only the names are kept in the clear");

    // The ciphertext opens with the configured key: it is encrypted, not lost.
    let cipher = Cipher::new(&TEST_KEY);
    let headers = open_headers(&cipher, &headers_enc).unwrap();
    assert_eq!(headers[0].name, "X-Api-Key");
    assert_eq!(headers[0].value.expose(), HEADER_SECRET);
    assert_eq!(open_password(&cipher, &password_enc).unwrap().expose(), PASSWORD_SECRET);
}

#[sqlx::test]
async fn patch_keeps_omitted_secrets_replaces_given_ones_and_null_clears(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) =
        call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(configured_monitor())).await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/monitors/{id}");
    let before = stored_secrets(&pool, &id).await;

    let (status, _) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "name": "Renamed", "paused": true }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored_secrets(&pool, &id).await, before, "omitted secrets are kept byte for byte");

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "auth_password": "rotated-pass", "request_headers": [{ "name": "X-Other", "value": "v2" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["request_headers"], json!([{ "name": "X-Other", "value_set": true }]));
    let (headers_enc, names, password_enc) = stored_secrets(&pool, &id).await;
    let cipher = Cipher::new(&TEST_KEY);
    assert_eq!(open_password(&cipher, &password_enc.unwrap()).unwrap().expose(), "rotated-pass");
    assert_eq!(open_headers(&cipher, &headers_enc.unwrap()).unwrap()[0].value.expose(), "v2");
    assert_eq!(names, vec!["X-Other"]);

    let (status, cleared) =
        call(&app, "PATCH", &item, Some(&member), Some(json!({ "request_headers": null, "auth_password": null }))).await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["request_headers"], json!([]));
    assert_eq!(cleared["has_auth_password"], false);
    assert_eq!(cleared["auth_username"], "monitor", "the username is not a secret and stays");
    assert_eq!(stored_secrets(&pool, &id).await, (None, Vec::new(), None));
}

#[sqlx::test]
async fn every_http_method_is_accepted_and_keyword_monitors_still_refuse_head(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        let body = json!({ "name": method, "check_type": "http", "url": "https://example.com", "http_method": method });
        let (status, created) = call(&app, "POST", &collection, Some(&member), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{method}: {created}");
        assert_eq!(created["http_method"], method);
    }
    let keyword_post = json!({
        "name": "search", "check_type": "keyword", "url": "https://example.com/search",
        "keyword": "results", "http_method": "POST", "request_body": "q=nami"
    });
    assert_eq!(call(&app, "POST", &collection, Some(&member), Some(keyword_post)).await.0, StatusCode::CREATED);
}

/// Security: header injection and every other invalid request config is a
/// 400 that writes nothing.
#[sqlx::test]
async fn header_injection_and_invalid_request_config_are_rejected_and_nothing_is_stored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let base = |extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "http", "url": "https://example.com", "http_method": "POST" });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        body
    };
    let header = |name: &str, value: &str| json!({ "request_headers": [{ "name": name, "value": value }] });
    let too_many: Vec<Value> = (0..21).map(|i| json!({ "name": format!("X-H{i}"), "value": "v" })).collect();
    let cases = [
        (header("X-Api-Key", "abc\r\nX-Injected: 1"), "CR or LF"),
        (header("X-Api-Key", "abc\nX-Injected: 1"), "CR or LF"),
        (header("X-Api-Key", "abc\rdef"), "CR or LF"),
        (header("X-Api-Key", "nul\u{0}byte"), "control characters"),
        (header("X-Bad\r\nName", "v"), "not a valid HTTP header name"),
        (header("X Bad", "v"), "not a valid HTTP header name"),
        (header("X-Bad:", "v"), "not a valid HTTP header name"),
        (header("", "v"), "not a valid HTTP header name"),
        (header("Host", "evil.test"), "cannot be set"),
        (header("content-length", "0"), "cannot be set"),
        (header("Transfer-Encoding", "chunked"), "cannot be set"),
        (header("CONNECTION", "close"), "cannot be set"),
        (header("X-Long", &"a".repeat(1025)), "at most 1024 bytes"),
        (json!({ "request_headers": too_many }), "at most 20"),
        (json!({ "http_method": "TRACE" }), "http_method"),
        (json!({ "http_method": "GET", "request_body": "x" }), "only allowed with POST, PUT or PATCH"),
        (json!({ "request_body": "a".repeat(64 * 1024 + 1) }), "at most 65536 bytes"),
        (json!({ "auth_username": "a:b", "auth_password": "p" }), "auth_username"),
        (json!({ "auth_username": "a\nb", "auth_password": "p" }), "auth_username"),
        (json!({ "auth_username": "a", "auth_password": "p\r\nq" }), "auth_password"),
        (json!({ "auth_password": "orphan" }), "auth_password requires auth_username"),
        (
            json!({ "auth_username": "a", "auth_password": "p", "request_headers": [{ "name": "authorization", "value": "Bearer t" }] }),
            "either basic auth or an Authorization header",
        ),
    ];
    for (extra, fragment) in cases {
        let body = base(extra);
        let (status, json) = call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(message.contains(fragment), "error {message:?} should mention {fragment:?}");
        assert!(!message.contains("abc"), "the error must not echo the header value: {message}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0, "no invalid monitor was written");

    // The same checks guard PATCH: the stored monitor stays untouched.
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/projects/{project}/monitors/{monitor}"),
        Some(&member),
        Some(header("X-Api-Key", "abc\r\nX-Injected: 1")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let names: Vec<String> = sqlx::query_scalar("select request_header_names from monitors where id = $1")
        .bind(monitor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(names.is_empty());
}

#[sqlx::test]
async fn switching_to_a_non_http_type_drops_the_request_config(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) =
        call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(configured_monitor())).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, tcp) = call(
        &app,
        "PATCH",
        &format!("/projects/{project}/monitors/{id}"),
        Some(&member),
        Some(json!({ "check_type": "tcp", "host": "db.internal", "port": 5432 })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{tcp}");
    assert_eq!(tcp["http_method"], "GET");
    assert_eq!(tcp["request_body"], Value::Null);
    assert_eq!(tcp["auth_username"], Value::Null);
    assert_eq!(tcp["has_auth_password"], false);
    assert_eq!(tcp["request_headers"], json!([]));
    assert_eq!(stored_secrets(&pool, &id).await, (None, Vec::new(), None));
}

#[test]
fn request_types_never_print_secrets() {
    let headers: Vec<nami_engine::models::monitor_request::RequestHeader> =
        serde_json::from_value(json!([{ "name": "X-Api-Key", "value": HEADER_SECRET }])).unwrap();
    let printed = format!("{headers:?}");
    assert!(printed.contains("X-Api-Key"));
    assert!(!printed.contains(HEADER_SECRET), "{printed}");
}
```

POST is a valid method from now on, so the Phase 1 rejection case switches to an invalid one (`tests/monitors_api.rs`):

```diff
diff --git a/nami-engine/tests/monitors_api.rs b/nami-engine/tests/monitors_api.rs
--- a/nami-engine/tests/monitors_api.rs
+++ b/nami-engine/tests/monitors_api.rs
@@ -141,7 +141,7 @@ async fn invalid_configs_are_rejected_with_400_and_nothing_is_written(pool: sqlx
         (json!({ "name": "x", "check_type": "http" }), "url"),
         (json!({ "name": "x", "check_type": "http", "url": "ftp://example.com" }), "url"),
         (json!({ "name": "x", "check_type": "http", "url": "not a url" }), "url"),
-        (json!({ "name": "x", "check_type": "http", "url": url, "http_method": "POST" }), "http_method"),
+        (json!({ "name": "x", "check_type": "http", "url": url, "http_method": "TRACE" }), "http_method"),
         (json!({ "name": "x", "check_type": "keyword", "url": url }), "keyword"),
         (json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "   " }), "keyword"),
         (
```

`Monitor` gains columns in this and later tasks; the checker tests' in-memory monitor names only what checks read and takes the rest from `Default` (`tests/uptime_checker.rs`):

```diff
diff --git a/nami-engine/tests/uptime_checker.rs b/nami-engine/tests/uptime_checker.rs
--- a/nami-engine/tests/uptime_checker.rs
+++ b/nami-engine/tests/uptime_checker.rs
@@ -17,23 +17,19 @@ fn monitor(check_type: &str) -> Monitor {
         project_id: Uuid::new_v4(),
         name: "test".to_string(),
         check_type: check_type.to_string(),
-        url: None,
-        host: None,
-        port: None,
         http_method: "GET".to_string(),
-        keyword: None,
         keyword_mode: "contains".to_string(),
         expected_status_min: 200,
         expected_status_max: 399,
         interval_seconds: 60,
         timeout_seconds: 5,
         confirmation_threshold: 2,
-        paused: false,
         current_status: "pending".to_string(),
-        consecutive_failures: 0,
-        last_checked_at: None,
+        follow_redirects: true,
+        verify_ssl: true,
         next_check_at: Utc::now(),
         created_at: Utc::now(),
+        ..Monitor::default()
     }
 }
 
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && cargo test --test monitor_request_config`
Expected: FAIL to compile — `unresolved import nami_engine::models::monitor_request`.

- [ ] **Step 3: Add the migration**

The constraint name comes from Phase 1's inline check (`monitors_http_method_check`, verified in `pg_constraint`). `nami-engine/migrations/0012_monitor_request_config.sql`:

```sql
-- Request configuration for http/keyword monitors (Phase 2).
alter table monitors drop constraint monitors_http_method_check;
alter table monitors add constraint monitors_http_method_check
    check (http_method in ('GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'));

alter table monitors
    add column request_body text,
    -- AES-256-GCM (src/crypto.rs) of the JSON list [{"name","value"}]. Only
    -- the names are kept in the clear, so reads never need the key.
    add column request_headers_enc bytea,
    add column request_header_names text[] not null default '{}',
    add column auth_username text,
    add column auth_password_enc bytea,
    add column follow_redirects boolean not null default true,
    add column verify_ssl boolean not null default true;
```

- [ ] **Step 4: Add `models/monitor_request.rs` and the `nullable` helper**

```rust
//! Request configuration of http/keyword monitors: method, body, custom
//! headers and basic auth. Header values and the basic-auth password are
//! write-only secrets: they are sealed with `crypto::Cipher` before they
//! reach the database and no API response ever contains them.

use serde::{Deserialize, Serialize};

use crate::crypto::{Cipher, DecryptError, SecretString};

pub const HTTP_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
/// The only methods that may carry `request_body`.
pub const BODY_METHODS: [&str; 3] = ["POST", "PUT", "PATCH"];
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS: usize = 20;
pub const MAX_HEADER_NAME_BYTES: usize = 256;
pub const MAX_HEADER_VALUE_BYTES: usize = 1024;
pub const MAX_USERNAME_CHARS: usize = 256;
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Framing headers the HTTP client owns; letting users set them would
/// enable request smuggling or break the request.
pub const RESERVED_HEADERS: [&str; 4] = ["host", "content-length", "transfer-encoding", "connection"];

/// AES-GCM associated data per column (see `crypto`).
pub const HEADERS_CONTEXT: &str = "monitors.request_headers";
pub const PASSWORD_CONTEXT: &str = "monitors.auth_password";

/// One custom request header. `Debug` shows the name only.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestHeader {
    pub name: String,
    pub value: SecretString,
}

/// The plaintext inside `request_headers_enc`.
#[derive(Serialize, Deserialize)]
struct StoredHeader {
    name: String,
    value: String,
}

/// What a create/PATCH does to the stored headers.
#[derive(Debug, Clone, PartialEq)]
pub enum HeadersChange {
    /// Leave them as they are; `names` are the stored names, for validation.
    Keep { names: Vec<String> },
    Clear,
    Replace(Vec<RequestHeader>),
}

impl HeadersChange {
    /// The header names the monitor has once this change is applied.
    pub fn names(&self) -> Vec<&str> {
        match self {
            HeadersChange::Keep { names } => names.iter().map(String::as_str).collect(),
            HeadersChange::Clear => Vec::new(),
            HeadersChange::Replace(headers) => headers.iter().map(|h| h.name.as_str()).collect(),
        }
    }
}

/// What a create/PATCH does to the stored basic-auth password.
#[derive(Debug, Clone, PartialEq)]
pub enum PasswordChange {
    /// Leave it as it is; `present` says whether one is stored.
    Keep { present: bool },
    Clear,
    Replace(SecretString),
}

impl PasswordChange {
    pub fn is_set(&self) -> bool {
        match self {
            PasswordChange::Keep { present } => *present,
            PasswordChange::Clear => false,
            PasswordChange::Replace(_) => true,
        }
    }
}

/// A header as API responses show it: the value is never returned.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeaderView {
    pub name: String,
    pub value_set: bool,
}

/// RFC 7230 `tchar`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Checks one header and returns it with its name trimmed. Error messages
/// name the header but never echo its value.
pub fn validate_header(header: RequestHeader) -> Result<RequestHeader, String> {
    let name = header.name.trim().to_string();
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        return Err(format!("header name {name:?} is not a valid HTTP header name"));
    }
    if name.len() > MAX_HEADER_NAME_BYTES {
        return Err(format!("header names must be at most {MAX_HEADER_NAME_BYTES} bytes"));
    }
    if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "header {name} cannot be set: Host, Content-Length, Transfer-Encoding and Connection are managed by the checker"
        ));
    }
    let value = header.value.expose();
    if value.contains(['\r', '\n']) {
        return Err(format!("header {name}: values must not contain CR or LF"));
    }
    if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
        return Err(format!("header {name}: values must not contain control characters"));
    }
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(format!("header {name}: values must be at most {MAX_HEADER_VALUE_BYTES} bytes"));
    }
    Ok(RequestHeader { name, value: header.value })
}

pub fn validate_headers(headers: Vec<RequestHeader>) -> Result<Vec<RequestHeader>, String> {
    if headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} request headers are allowed"));
    }
    headers.into_iter().map(validate_header).collect()
}

/// Basic-auth user id (RFC 7617): no `:` and no control characters.
pub fn validate_username(raw: &str) -> Result<String, String> {
    if raw.contains(':') || raw.chars().any(char::is_control) {
        return Err("auth_username must not contain ':' or control characters".into());
    }
    if raw.chars().count() > MAX_USERNAME_CHARS {
        return Err(format!("auth_username must be at most {MAX_USERNAME_CHARS} characters"));
    }
    Ok(raw.to_string())
}

pub fn validate_password(password: &SecretString) -> Result<(), String> {
    let value = password.expose();
    if value.chars().any(char::is_control) {
        return Err("auth_password must not contain control characters".into());
    }
    if value.len() > MAX_PASSWORD_BYTES {
        return Err(format!("auth_password must be at most {MAX_PASSWORD_BYTES} bytes"));
    }
    Ok(())
}

pub fn seal_headers(cipher: &Cipher, headers: &[RequestHeader]) -> Vec<u8> {
    let stored: Vec<StoredHeader> = headers
        .iter()
        .map(|h| StoredHeader { name: h.name.clone(), value: h.value.expose().to_string() })
        .collect();
    let json = serde_json::to_vec(&stored).expect("a list of strings always serializes");
    cipher.encrypt(HEADERS_CONTEXT, &json)
}

/// Any failure (wrong key, tampering, malformed plaintext) is a
/// [`DecryptError`].
pub fn open_headers(cipher: &Cipher, sealed: &[u8]) -> Result<Vec<RequestHeader>, DecryptError> {
    let json = cipher.decrypt(HEADERS_CONTEXT, sealed)?;
    let stored: Vec<StoredHeader> = serde_json::from_slice(&json).map_err(|_| DecryptError)?;
    Ok(stored
        .into_iter()
        .map(|h| RequestHeader { name: h.name, value: SecretString::new(h.value) })
        .collect())
}

pub fn seal_password(cipher: &Cipher, password: &SecretString) -> Vec<u8> {
    cipher.encrypt(PASSWORD_CONTEXT, password.expose().as_bytes())
}

pub fn open_password(cipher: &Cipher, sealed: &[u8]) -> Result<SecretString, DecryptError> {
    let bytes = cipher.decrypt(PASSWORD_CONTEXT, sealed)?;
    String::from_utf8(bytes).map(SecretString::new).map_err(|_| DecryptError)
}
```

`src/models/mod.rs`:

```diff
diff --git a/nami-engine/src/models/mod.rs b/nami-engine/src/models/mod.rs
--- a/nami-engine/src/models/mod.rs
+++ b/nami-engine/src/models/mod.rs
@@ -2,9 +2,24 @@ pub mod user;
 pub mod token;
 pub mod project;
 pub mod monitor;
+pub mod monitor_request;
 pub mod incident;
 pub mod heartbeat;
 
+use serde::{Deserialize, Deserializer};
+
+/// Deserializer for a PATCH field that can be cleared. With
+/// `#[serde(default, deserialize_with = "nullable")]` on an
+/// `Option<Option<T>>`: absent = `None` (keep), `null` = `Some(None)`
+/// (clear), a value = `Some(Some(v))` (set).
+pub fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
+where
+    D: Deserializer<'de>,
+    T: Deserialize<'de>,
+{
+    Option::<T>::deserialize(deserializer).map(Some)
+}
+
 /// Name rule shared by monitors and heartbeats: trimmed, 1-200 characters,
 /// and no control characters (names end up in alert email subjects).
 pub fn validate_name(raw: &str) -> Result<String, String> {
```

- [ ] **Step 5: Rewrite `models/monitor.rs`**

Changes from Phase 1: the new columns and `MonitorView`; tri-state PATCH fields; method/body/header/basic-auth validation for http and keyword (cleared for tcp); and one `write_config` statement shared by insert and update, which seals replaced secrets and leaves kept ones untouched. The full file:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    monitor_request::{
        self, seal_headers, seal_password, HeaderView, HeadersChange, PasswordChange, RequestHeader, BODY_METHODS,
        HTTP_METHODS, MAX_BODY_BYTES,
    },
    nullable, validate_name,
};
use crate::{
    crypto::{Cipher, SecretString},
    error::AppError,
};

/// One `monitors` row. The `*_enc` columns and `request_header_names` are
/// never serialized: API responses go through [`MonitorView`]. `Default`
/// exists for tests that build monitors in memory (`..Monitor::default()`);
/// real monitors always come from the database.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow, Serialize)]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
    pub current_status: String,
    pub consecutive_failures: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub request_body: Option<String>,
    /// AES-GCM sealed JSON `[{"name","value"}]`, see
    /// `monitor_request::open_headers`.
    #[serde(skip_serializing)]
    pub request_headers_enc: Option<Vec<u8>>,
    /// The same headers' names in the clear, for display.
    #[serde(skip_serializing)]
    pub request_header_names: Vec<String>,
    pub auth_username: Option<String>,
    /// AES-GCM sealed basic-auth password, see `monitor_request::open_password`.
    #[serde(skip_serializing)]
    pub auth_password_enc: Option<Vec<u8>>,
    pub follow_redirects: bool,
    pub verify_ssl: bool,
}

/// A monitor as every API response shows it: the secrets are reduced to
/// `request_headers` (names, `value_set: true`) and `has_auth_password`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorView {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub request_headers: Vec<HeaderView>,
    pub has_auth_password: bool,
}

impl From<Monitor> for MonitorView {
    fn from(monitor: Monitor) -> Self {
        let request_headers = monitor
            .request_header_names
            .iter()
            .map(|name| HeaderView { name: name.clone(), value_set: true })
            .collect();
        let has_auth_password = monitor.auth_password_enc.is_some();
        MonitorView { monitor, request_headers, has_auth_password }
    }
}

/// The user-editable fields of a monitor. Create and PATCH both build one,
/// run it through [`validate`], and only then write it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorConfig {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
    pub request_body: Option<String>,
    pub request_headers: HeadersChange,
    pub auth_username: Option<String>,
    pub auth_password: PasswordChange,
    pub follow_redirects: bool,
    pub verify_ssl: bool,
}

impl From<&Monitor> for MonitorConfig {
    fn from(m: &Monitor) -> Self {
        MonitorConfig {
            name: m.name.clone(),
            check_type: m.check_type.clone(),
            url: m.url.clone(),
            host: m.host.clone(),
            port: m.port,
            http_method: m.http_method.clone(),
            keyword: m.keyword.clone(),
            keyword_mode: m.keyword_mode.clone(),
            expected_status_min: m.expected_status_min,
            expected_status_max: m.expected_status_max,
            interval_seconds: m.interval_seconds,
            timeout_seconds: m.timeout_seconds,
            confirmation_threshold: m.confirmation_threshold,
            paused: m.paused,
            request_body: m.request_body.clone(),
            request_headers: HeadersChange::Keep { names: m.request_header_names.clone() },
            auth_username: m.auth_username.clone(),
            auth_password: PasswordChange::Keep { present: m.auth_password_enc.is_some() },
            follow_redirects: m.follow_redirects,
            verify_ssl: m.verify_ssl,
        }
    }
}

impl MonitorConfig {
    /// True when a field that changes how or when the monitor is checked
    /// differs from `current`. Renaming, pausing and the confirmation
    /// threshold do not count; replacing or clearing a secret does.
    pub fn changes_check_of(&self, current: &Monitor) -> bool {
        self.check_type != current.check_type
            || self.url != current.url
            || self.host != current.host
            || self.port != current.port
            || self.http_method != current.http_method
            || self.keyword != current.keyword
            || self.keyword_mode != current.keyword_mode
            || self.expected_status_min != current.expected_status_min
            || self.expected_status_max != current.expected_status_max
            || self.interval_seconds != current.interval_seconds
            || self.timeout_seconds != current.timeout_seconds
            || self.request_body != current.request_body
            || !matches!(self.request_headers, HeadersChange::Keep { .. })
            || self.auth_username != current.auth_username
            || !matches!(self.auth_password, PasswordChange::Keep { .. })
            || self.follow_redirects != current.follow_redirects
            || self.verify_ssl != current.verify_ssl
    }
}

/// `POST /projects/{id}/monitors` body. Server-owned fields (`id`,
/// `project_id`, `current_status`, ...) are not part of it, so a client that
/// sends them is ignored rather than obeyed.
#[derive(Debug, Deserialize)]
pub struct CreateMonitorRequest {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    #[serde(default = "default_http_method")]
    pub http_method: String,
    pub keyword: Option<String>,
    #[serde(default = "default_keyword_mode")]
    pub keyword_mode: String,
    #[serde(default = "default_expected_status_min")]
    pub expected_status_min: i32,
    #[serde(default = "default_expected_status_max")]
    pub expected_status_max: i32,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: i32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: i32,
    #[serde(default = "default_confirmation_threshold")]
    pub confirmation_threshold: i32,
    #[serde(default)]
    pub paused: bool,
    pub request_body: Option<String>,
    pub request_headers: Option<Vec<RequestHeader>>,
    pub auth_username: Option<String>,
    pub auth_password: Option<SecretString>,
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_true")]
    pub verify_ssl: bool,
}

fn default_http_method() -> String {
    "GET".to_string()
}
fn default_keyword_mode() -> String {
    "contains".to_string()
}
fn default_expected_status_min() -> i32 {
    200
}
fn default_expected_status_max() -> i32 {
    399
}
fn default_interval_seconds() -> i32 {
    60
}
fn default_timeout_seconds() -> i32 {
    10
}
fn default_confirmation_threshold() -> i32 {
    2
}
fn default_true() -> bool {
    true
}

impl CreateMonitorRequest {
    pub fn into_config(self) -> MonitorConfig {
        MonitorConfig {
            name: self.name,
            check_type: self.check_type,
            url: self.url,
            host: self.host,
            port: self.port,
            http_method: self.http_method,
            keyword: self.keyword,
            keyword_mode: self.keyword_mode,
            expected_status_min: self.expected_status_min,
            expected_status_max: self.expected_status_max,
            interval_seconds: self.interval_seconds,
            timeout_seconds: self.timeout_seconds,
            confirmation_threshold: self.confirmation_threshold,
            paused: self.paused,
            request_body: self.request_body,
            request_headers: match self.request_headers {
                Some(headers) => HeadersChange::Replace(headers),
                None => HeadersChange::Keep { names: Vec::new() },
            },
            auth_username: self.auth_username,
            auth_password: match self.auth_password {
                Some(password) => PasswordChange::Replace(password),
                None => PasswordChange::Keep { present: false },
            },
            follow_redirects: self.follow_redirects,
            verify_ssl: self.verify_ssl,
        }
    }
}

/// `PATCH` body: any subset of the editable fields. Absent fields keep
/// their current value; unknown and server-owned fields are ignored. The
/// Phase 2 nullable fields distinguish absent (keep) from `null` (clear),
/// see [`nullable`] — that is how write-only secrets are cleared.
#[derive(Debug, Default, Deserialize)]
pub struct PatchMonitorRequest {
    pub name: Option<String>,
    pub check_type: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: Option<String>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    pub expected_status_min: Option<i32>,
    pub expected_status_max: Option<i32>,
    pub interval_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub confirmation_threshold: Option<i32>,
    pub paused: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub request_body: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub request_headers: Option<Option<Vec<RequestHeader>>>,
    #[serde(default, deserialize_with = "nullable")]
    pub auth_username: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub auth_password: Option<Option<SecretString>>,
    pub follow_redirects: Option<bool>,
    pub verify_ssl: Option<bool>,
}

/// A nullable PATCH field applied to its current value.
fn patched<T>(patch: Option<Option<T>>, current: Option<T>) -> Option<T> {
    match patch {
        None => current,
        Some(value) => value,
    }
}

impl PatchMonitorRequest {
    pub fn apply_to(self, base: MonitorConfig) -> MonitorConfig {
        MonitorConfig {
            name: self.name.unwrap_or(base.name),
            check_type: self.check_type.unwrap_or(base.check_type),
            url: self.url.or(base.url),
            host: self.host.or(base.host),
            port: self.port.or(base.port),
            http_method: self.http_method.unwrap_or(base.http_method),
            keyword: self.keyword.or(base.keyword),
            keyword_mode: self.keyword_mode.unwrap_or(base.keyword_mode),
            expected_status_min: self.expected_status_min.unwrap_or(base.expected_status_min),
            expected_status_max: self.expected_status_max.unwrap_or(base.expected_status_max),
            interval_seconds: self.interval_seconds.unwrap_or(base.interval_seconds),
            timeout_seconds: self.timeout_seconds.unwrap_or(base.timeout_seconds),
            confirmation_threshold: self
                .confirmation_threshold
                .unwrap_or(base.confirmation_threshold),
            paused: self.paused.unwrap_or(base.paused),
            request_body: patched(self.request_body, base.request_body),
            request_headers: match self.request_headers {
                None => base.request_headers,
                Some(None) => HeadersChange::Clear,
                Some(Some(headers)) => HeadersChange::Replace(headers),
            },
            auth_username: patched(self.auth_username, base.auth_username),
            auth_password: match self.auth_password {
                None => base.auth_password,
                Some(None) => PasswordChange::Clear,
                Some(Some(password)) => PasswordChange::Replace(password),
            },
            follow_redirects: self.follow_redirects.unwrap_or(base.follow_redirects),
            verify_ssl: self.verify_ssl.unwrap_or(base.verify_ssl),
        }
    }
}

/// Fail-closed validation of a complete config. Returns the normalized
/// config (trimmed name/url/host; fields irrelevant to the check type
/// cleared) or a message for a 400.
pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
    c.name = validate_name(&c.name)?;
    if !matches!(c.keyword_mode.as_str(), "contains" | "not_contains") {
        return Err("keyword_mode must be contains or not_contains".into());
    }
    match c.check_type.as_str() {
        "http" | "keyword" => validate_http(&mut c)?,
        "tcp" => validate_tcp(&mut c)?,
        _ => return Err("check_type must be http, keyword or tcp".into()),
    }
    if !matches!(c.check_type.as_str(), "http" | "keyword") {
        clear_request_config(&mut c);
    }
    if c.interval_seconds < 30 {
        return Err("interval_seconds must be at least 30".into());
    }
    if !(1..=60).contains(&c.timeout_seconds) {
        return Err("timeout_seconds must be between 1 and 60".into());
    }
    if !(100..=599).contains(&c.expected_status_min) || !(100..=599).contains(&c.expected_status_max)
    {
        return Err("expected status codes must be between 100 and 599".into());
    }
    if c.expected_status_min > c.expected_status_max {
        return Err("expected_status_min must not exceed expected_status_max".into());
    }
    if !(1..=10).contains(&c.confirmation_threshold) {
        return Err("confirmation_threshold must be between 1 and 10".into());
    }
    Ok(c)
}

fn validate_http(c: &mut MonitorConfig) -> Result<(), String> {
    let url = c.url.as_deref().map(str::trim).unwrap_or_default().to_string();
    let parsed = reqwest::Url::parse(&url).map_err(|_| "url must be a valid http(s) URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("url must be a valid http(s) URL".into());
    }
    c.url = Some(url);
    c.host = None;
    c.port = None;
    if !HTTP_METHODS.contains(&c.http_method.as_str()) {
        return Err("http_method must be one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS".into());
    }
    if c.check_type == "keyword" {
        if c.keyword.as_deref().is_none_or(|k| k.trim().is_empty()) {
            return Err("keyword is required for keyword monitors".into());
        }
        if c.http_method == "HEAD" {
            return Err("keyword monitors cannot use HEAD: a HEAD response has no body to search (use GET)".into());
        }
    } else {
        c.keyword = None;
    }
    validate_request_config(c)
}

/// Method/body/headers/basic auth of an http or keyword monitor.
fn validate_request_config(c: &mut MonitorConfig) -> Result<(), String> {
    c.request_body = c.request_body.take().filter(|body| !body.is_empty());
    if let Some(body) = &c.request_body {
        if !BODY_METHODS.contains(&c.http_method.as_str()) {
            return Err("request_body is only allowed with POST, PUT or PATCH".into());
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(format!("request_body must be at most {MAX_BODY_BYTES} bytes"));
        }
    }
    if let HeadersChange::Replace(headers) = &mut c.request_headers {
        let headers = monitor_request::validate_headers(std::mem::take(headers))?;
        c.request_headers =
            if headers.is_empty() { HeadersChange::Clear } else { HeadersChange::Replace(headers) };
    }
    c.auth_username = match c.auth_username.take().filter(|username| !username.is_empty()) {
        Some(username) => Some(monitor_request::validate_username(&username)?),
        None => None,
    };
    if let PasswordChange::Replace(password) = &c.auth_password {
        if c.auth_username.is_none() {
            return Err("auth_password requires auth_username".into());
        }
        monitor_request::validate_password(password)?;
    }
    if c.auth_username.is_none() && c.auth_password.is_set() {
        // Removing the username removes basic auth, password included.
        c.auth_password = PasswordChange::Clear;
    }
    let has_authorization_header =
        c.request_headers.names().iter().any(|name| name.eq_ignore_ascii_case("authorization"));
    if c.auth_username.is_some() && has_authorization_header {
        return Err("use either basic auth or an Authorization header, not both".into());
    }
    Ok(())
}

/// Non-http types carry no request config. Secrets are only marked for
/// clearing when one is stored or supplied, so editing such a monitor does
/// not count as a check change (see `changes_check_of`).
fn clear_request_config(c: &mut MonitorConfig) {
    c.http_method = default_http_method();
    c.request_body = None;
    c.auth_username = None;
    c.follow_redirects = true;
    c.verify_ssl = true;
    if !matches!(&c.request_headers, HeadersChange::Keep { names } if names.is_empty()) {
        c.request_headers = HeadersChange::Clear;
    }
    if !matches!(c.auth_password, PasswordChange::Keep { present: false }) {
        c.auth_password = PasswordChange::Clear;
    }
}

fn validate_tcp(c: &mut MonitorConfig) -> Result<(), String> {
    let host = c.host.as_deref().map(str::trim).unwrap_or_default().to_string();
    if host.is_empty() {
        return Err("host is required for tcp monitors".into());
    }
    if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
        return Err("port must be between 1 and 65535".into());
    }
    c.host = Some(host);
    c.url = None;
    c.keyword = None;
    Ok(())
}

pub async fn insert_monitor(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    c: &MonitorConfig,
) -> Result<Monitor, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // The row starts from the column defaults; `write_config` then sets
    // every configurable column, so each column is written in one place.
    let id: Uuid = sqlx::query_scalar(
        "insert into monitors (project_id, name, check_type, interval_seconds)
         values ($1, $2, $3, $4)
         returning id",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.check_type)
    .bind(c.interval_seconds)
    .fetch_one(&mut *tx)
    .await?;
    let monitor = write_config(&mut tx, cipher, id, c, false).await?;
    tx.commit().await?;
    Ok(monitor)
}

/// Writes every configurable column of monitor `id` from a validated
/// config, sealing new secrets. `HeadersChange::Keep`/`PasswordChange::Keep`
/// leave the stored ciphertext untouched; `reschedule` makes the monitor due
/// now. New columns are appended at the end of the statement.
async fn write_config(
    conn: &mut PgConnection,
    cipher: &Cipher,
    id: Uuid,
    c: &MonitorConfig,
    reschedule: bool,
) -> Result<Monitor, sqlx::Error> {
    let (keep_headers, header_names, sealed_headers) = match &c.request_headers {
        HeadersChange::Keep { .. } => (true, Vec::new(), None),
        HeadersChange::Clear => (false, Vec::new(), None),
        HeadersChange::Replace(headers) => (
            false,
            headers.iter().map(|h| h.name.clone()).collect::<Vec<String>>(),
            Some(seal_headers(cipher, headers)),
        ),
    };
    let (keep_password, sealed_password) = match &c.auth_password {
        PasswordChange::Keep { .. } => (true, None),
        PasswordChange::Clear => (false, None),
        PasswordChange::Replace(password) => (false, Some(seal_password(cipher, password))),
    };
    sqlx::query_as(
        "update monitors set
             next_check_at = case when $2 then now() else next_check_at end,
             name = $3, check_type = $4, url = $5, host = $6, port = $7, http_method = $8,
             keyword = $9, keyword_mode = $10, expected_status_min = $11, expected_status_max = $12,
             interval_seconds = $13, timeout_seconds = $14, confirmation_threshold = $15, paused = $16,
             request_body = $17, auth_username = $18, follow_redirects = $19, verify_ssl = $20,
             request_header_names = case when $21 then request_header_names else $22 end,
             request_headers_enc = case when $21 then request_headers_enc else $23 end,
             auth_password_enc = case when $24 then auth_password_enc else $25 end
         where id = $1
         returning *",
    )
    .bind(id)
    .bind(reschedule)
    .bind(&c.name)
    .bind(&c.check_type)
    .bind(&c.url)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.http_method)
    .bind(&c.keyword)
    .bind(&c.keyword_mode)
    .bind(c.expected_status_min)
    .bind(c.expected_status_max)
    .bind(c.interval_seconds)
    .bind(c.timeout_seconds)
    .bind(c.confirmation_threshold)
    .bind(c.paused)
    .bind(&c.request_body)
    .bind(&c.auth_username)
    .bind(c.follow_redirects)
    .bind(c.verify_ssl)
    .bind(keep_headers)
    .bind(header_names)
    .bind(sealed_headers)
    .bind(keep_password)
    .bind(sealed_password)
    .fetch_one(&mut *conn)
    .await
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Every lookup is scoped by BOTH ids: a monitor id from another project is
/// simply not found here (404), even for a caller who can see that project.
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<Option<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write in one transaction. The `for update` row lock makes
/// concurrent PATCHes apply one after the other instead of the second one
/// overwriting the first with values it read before the first committed.
/// Editing any check-config field reschedules the monitor to run now.
pub async fn update_in_project(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    monitor_id: Uuid,
    patch: PatchMonitorRequest,
) -> Result<Option<Monitor>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Monitor> =
        sqlx::query_as("select * from monitors where id = $1 and project_id = $2 for update")
            .bind(monitor_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let config = validate(patch.apply_to(MonitorConfig::from(&current))).map_err(AppError::Validation)?;
    let reschedule = config.changes_check_of(&current);
    let updated = write_config(&mut tx, cipher, monitor_id, &config, reschedule).await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Returns false when no monitor with this id exists in this project.
pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
```

- [ ] **Step 6: Return `MonitorView` from every monitor route**

`src/routes/monitors.rs`:

```diff
diff --git a/nami-engine/src/routes/monitors.rs b/nami-engine/src/routes/monitors.rs
--- a/nami-engine/src/routes/monitors.rs
+++ b/nami-engine/src/routes/monitors.rs
@@ -13,7 +13,7 @@ use uuid::Uuid;
 use crate::{
     error::AppError,
     middleware::rbac::{require_project_role, CurrentUser},
-    models::monitor::{self, CreateMonitorRequest, Monitor, PatchMonitorRequest},
+    models::monitor::{self, CreateMonitorRequest, MonitorView, PatchMonitorRequest},
     state::AppState,
     uptime::store::{self, MonitorStats, UptimeCheckRow},
 };
@@ -37,7 +37,7 @@ pub fn router() -> Router<AppState> {
 #[derive(Serialize)]
 struct MonitorSummary {
     #[serde(flatten)]
-    monitor: Monitor,
+    monitor: MonitorView,
     uptime_24h: Option<f64>,
 }
 
@@ -62,7 +62,7 @@ async fn list_monitors(
             .into_iter()
             .map(|monitor| {
                 let uptime_24h = uptime.get(&monitor.id).copied();
-                MonitorSummary { monitor, uptime_24h }
+                MonitorSummary { monitor: MonitorView::from(monitor), uptime_24h }
             })
             .collect(),
     ))
@@ -73,10 +73,10 @@ async fn create_monitor(
     user: CurrentUser,
     Path(project_id): Path<Uuid>,
     Json(payload): Json<CreateMonitorRequest>,
-) -> Result<(StatusCode, Json<Monitor>), AppError> {
+) -> Result<(StatusCode, Json<MonitorView>), AppError> {
     require_project_role(&state.db, &user, project_id, "member").await?;
     let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
-    let created = monitor::insert_monitor(&state.db, project_id, &config)
+    let created = monitor::insert_monitor(&state.db, &state.cipher, project_id, &config)
         .await
         .map_err(|e| match &e {
             // A global admin bypasses membership, so the project itself may
@@ -86,19 +86,19 @@ async fn create_monitor(
             }
             _ => AppError::Database(e),
         })?;
-    Ok((StatusCode::CREATED, Json(created)))
+    Ok((StatusCode::CREATED, Json(MonitorView::from(created))))
 }
 
 async fn get_monitor(
     State(state): State<AppState>,
     user: CurrentUser,
     Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
-) -> Result<Json<Monitor>, AppError> {
+) -> Result<Json<MonitorView>, AppError> {
     require_project_role(&state.db, &user, project_id, "viewer").await?;
     let found = monitor::find_in_project(&state.db, project_id, monitor_id)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(found))
+    Ok(Json(MonitorView::from(found)))
 }
 
 async fn patch_monitor(
@@ -106,12 +106,12 @@ async fn patch_monitor(
     user: CurrentUser,
     Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
     Json(payload): Json<PatchMonitorRequest>,
-) -> Result<Json<Monitor>, AppError> {
+) -> Result<Json<MonitorView>, AppError> {
     require_project_role(&state.db, &user, project_id, "member").await?;
-    let updated = monitor::update_in_project(&state.db, project_id, monitor_id, payload)
+    let updated = monitor::update_in_project(&state.db, &state.cipher, project_id, monitor_id, payload)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(updated))
+    Ok(Json(MonitorView::from(updated)))
 }
 
 async fn delete_monitor(
```

- [ ] **Step 7: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/monitor_request_config.rs` 8 passed; `monitors_api.rs` (including the Phase 1 Review Focus case: keyword + HEAD is still a 400 mentioning `GET`), `uptime_checker.rs` and the rest green.

- [ ] **Step 8: Commit**

```bash
git add nami-engine/migrations/0012_monitor_request_config.sql nami-engine/src/models/monitor_request.rs \
  nami-engine/src/models/mod.rs nami-engine/src/models/monitor.rs nami-engine/src/routes/monitors.rs \
  nami-engine/tests/monitor_request_config.rs nami-engine/tests/monitors_api.rs nami-engine/tests/uptime_checker.rs
git commit -m "feat(engine): monitor request config with encrypted write-only secrets (NAMI-UP-10)

Methods GET..OPTIONS, request body, custom headers and basic auth.
Header values and the password are sealed with AES-256-GCM; responses
show header names (value_set) and has_auth_password only.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11 (NAMI-UP-11): Checker sends the request config

Branch `feat/NAMI-UP-11-checker-request-config`, worktree `.worktrees/NAMI-UP-11/`.

**Files:**
- Modify: `nami-engine/Cargo.toml` (dev-dependencies), `nami-engine/src/uptime/checker.rs`, `nami-engine/src/uptime/worker.rs`
- Create (tests): `nami-engine/tests/common/tls.rs`
- Modify (tests): `nami-engine/tests/common/mod.rs`, `nami-engine/tests/uptime_checker.rs`
- Test: `nami-engine/tests/checker_http_config.rs`

**Interfaces:**
- Consumes: `models::monitor_request::{open_headers, open_password, seal_headers, seal_password, RequestHeader, BODY_METHODS}`, `Monitor` request fields (Task 10); `crypto::{Cipher, SecretString, TEST_KEY}`, `AppState.cipher` (Task 9).
- Produces:
  - `checker::CheckContext { pub http: HttpClients, pub cipher: Cipher }` (`Clone`): `CheckContext::new(cipher: Cipher) -> CheckContext`, `CheckContext::for_state(state: &AppState) -> CheckContext`. Task 14 adds fields; tests extend it with struct-update syntax.
  - `checker::HttpClients` (`Clone`, `Default`): `HttpClients::new()`, `fn select(&self, follow_redirects: bool, verify_ssl: bool, has_custom_headers: bool) -> &reqwest::Client`.
  - `checker::MAX_REDIRECTS: usize = 10`; `checker::CONFIG_DECRYPT_ERROR: &str`.
  - `checker::perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcome` — replaces Phase 1's `perform_check(&reqwest::Client, &Monitor)`; `build_http_client()` is removed.
  - Test helpers: `common::unsaved_monitor(check_type: &str) -> Monitor`, `common::check_context() -> CheckContext`, `common::test_cipher() -> Cipher`; `common::tls::{CertAndKey, provider(), self_signed() -> CertAndKey, serve_https_ok(CertAndKey) -> u16 (port)}`.

- [ ] **Step 1: Add the TLS test crates**

```bash
cd nami-engine
cargo add --dev rcgen@0.14.10 rustls@0.23.45 tokio-rustls@0.26.6
```

(Task 14 moves `rustls` and `tokio-rustls` to the normal dependencies.)

- [ ] **Step 2: Write the failing tests**

Shared helpers — `tests/common/mod.rs` gains the in-memory monitor and the checker context:

```diff
diff --git a/nami-engine/tests/common/mod.rs b/nami-engine/tests/common/mod.rs
--- a/nami-engine/tests/common/mod.rs
+++ b/nami-engine/tests/common/mod.rs
@@ -3,6 +3,8 @@
 //! it, hence the blanket `dead_code` allowance.
 #![allow(dead_code)]
 
+pub mod tls;
+
 use std::sync::OnceLock;
 
 use axum::{
@@ -10,7 +12,13 @@ use axum::{
     http::{header, HeaderValue, Request, StatusCode},
     Router,
 };
-use nami_engine::state::AppState;
+use chrono::Utc;
+use nami_engine::{
+    crypto::{Cipher, TEST_KEY},
+    models::monitor::Monitor,
+    state::AppState,
+    uptime::checker::CheckContext,
+};
 use serde_json::{json, Value};
 use tower::ServiceExt;
 use uuid::Uuid;
@@ -224,3 +232,36 @@ pub async fn make_due(pool: &sqlx::PgPool, monitor_id: Uuid) {
         .await
         .unwrap();
 }
+
+/// An in-memory monitor for checker tests (never saved). Every field a check
+/// reads has a working default; override fields with struct update syntax.
+pub fn unsaved_monitor(check_type: &str) -> Monitor {
+    Monitor {
+        id: Uuid::new_v4(),
+        project_id: Uuid::new_v4(),
+        name: "test".to_string(),
+        check_type: check_type.to_string(),
+        http_method: "GET".to_string(),
+        keyword_mode: "contains".to_string(),
+        expected_status_min: 200,
+        expected_status_max: 399,
+        interval_seconds: 60,
+        timeout_seconds: 5,
+        confirmation_threshold: 2,
+        current_status: "pending".to_string(),
+        follow_redirects: true,
+        verify_ssl: true,
+        next_check_at: Utc::now(),
+        created_at: Utc::now(),
+        ..Monitor::default()
+    }
+}
+
+/// The checker context tests use: real HTTP clients and the test cipher.
+pub fn check_context() -> CheckContext {
+    CheckContext::new(test_cipher())
+}
+
+pub fn test_cipher() -> Cipher {
+    Cipher::new(&TEST_KEY)
+}
```

`nami-engine/tests/common/tls.rs` (new):

```rust
//! TLS fixtures for checker tests: certificates made on the fly and local
//! TLS servers on 127.0.0.1.
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type CertAndKey = (CertificateDer<'static>, PrivateKeyDer<'static>);

/// rustls is built with both the ring and aws-lc-rs providers in this
/// workspace, so every config names its provider explicitly.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A self-signed `localhost` certificate that no root store trusts.
pub fn self_signed() -> CertAndKey {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()));
    (certified.cert.der().clone(), key)
}

fn acceptor((cert, key): CertAndKey) -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

/// An HTTPS server answering every request with `200 ok`. Returns its port.
pub async fn serve_https_ok(cert_and_key: CertAndKey) -> u16 {
    let acceptor = acceptor(cert_and_key);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // A failed handshake (the client rejected the certificate)
                // just ends this connection.
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut request = [0u8; 4096];
                    let _ = tls.read(&mut request).await;
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                        .await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    port
}
```

`tests/uptime_checker.rs` switches to the shared helpers; the diff also turns every `perform_check(&build_http_client(), …)` into `perform_check(&check_context(), …)`:

```diff
diff --git a/nami-engine/tests/uptime_checker.rs b/nami-engine/tests/uptime_checker.rs
--- a/nami-engine/tests/uptime_checker.rs
+++ b/nami-engine/tests/uptime_checker.rs
@@ -1,38 +1,17 @@
+mod common;
+
 use std::time::{Duration, Instant};
 
-use chrono::Utc;
+use common::{check_context, unsaved_monitor as monitor};
 use nami_engine::{
     models::monitor::Monitor,
-    uptime::checker::{build_http_client, perform_check, CheckStatus, MAX_BODY_BYTES},
+    uptime::checker::{perform_check, CheckStatus, MAX_BODY_BYTES},
 };
-use uuid::Uuid;
 use wiremock::{
     matchers::{method, path},
     Mock, MockServer, ResponseTemplate,
 };
 
-fn monitor(check_type: &str) -> Monitor {
-    Monitor {
-        id: Uuid::new_v4(),
-        project_id: Uuid::new_v4(),
-        name: "test".to_string(),
-        check_type: check_type.to_string(),
-        http_method: "GET".to_string(),
-        keyword_mode: "contains".to_string(),
-        expected_status_min: 200,
-        expected_status_max: 399,
-        interval_seconds: 60,
-        timeout_seconds: 5,
-        confirmation_threshold: 2,
-        current_status: "pending".to_string(),
-        follow_redirects: true,
-        verify_ssl: true,
-        next_check_at: Utc::now(),
-        created_at: Utc::now(),
-        ..Monitor::default()
-    }
-}
-
 fn http_monitor(url: String) -> Monitor {
     Monitor { url: Some(url), ..monitor("http") }
 }
@@ -61,7 +40,7 @@ fn closed_port() -> u16 {
 #[tokio::test]
 async fn status_inside_the_expected_range_is_up() {
     let server = server_returning(ResponseTemplate::new(204)).await;
-    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/", server.uri()))).await;
+    let outcome = perform_check(&check_context(), &http_monitor(format!("{}/", server.uri()))).await;
     assert_eq!(outcome.status, CheckStatus::Up);
     assert_eq!(outcome.status_code, Some(204));
     assert!(outcome.response_time_ms.is_some());
@@ -71,7 +50,7 @@ async fn status_inside_the_expected_range_is_up() {
 #[tokio::test]
 async fn status_outside_the_expected_range_is_down_with_the_reason() {
     let server = server_returning(ResponseTemplate::new(503)).await;
-    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/", server.uri()))).await;
+    let outcome = perform_check(&check_context(), &http_monitor(format!("{}/", server.uri()))).await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.status_code, Some(503));
     assert_eq!(outcome.error.as_deref(), Some("expected status 200-399, got 503"));
@@ -85,7 +64,7 @@ async fn a_custom_expected_range_is_honoured() {
         expected_status_max: 200,
         ..http_monitor(format!("{}/", server.uri()))
     };
-    let outcome = perform_check(&build_http_client(), &only_200).await;
+    let outcome = perform_check(&check_context(), &only_200).await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("expected status 200-200, got 204"));
 }
@@ -99,7 +78,7 @@ async fn redirects_are_followed_to_the_final_status() {
         .mount(&server)
         .await;
     Mock::given(method("GET")).and(path("/new")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
-    let outcome = perform_check(&build_http_client(), &http_monitor(format!("{}/old", server.uri()))).await;
+    let outcome = perform_check(&check_context(), &http_monitor(format!("{}/old", server.uri()))).await;
     assert_eq!(outcome.status, CheckStatus::Up);
     assert_eq!(outcome.status_code, Some(200));
 }
@@ -110,7 +89,7 @@ async fn head_monitors_send_head_requests() {
     // Only HEAD is mocked: a GET would get wiremock's default 404.
     Mock::given(method("HEAD")).and(path("/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
     let head = Monitor { http_method: "HEAD".to_string(), ..http_monitor(format!("{}/", server.uri())) };
-    let outcome = perform_check(&build_http_client(), &head).await;
+    let outcome = perform_check(&check_context(), &head).await;
     assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
 }
 
@@ -119,10 +98,10 @@ async fn keyword_present_is_up_and_absent_is_down() {
     let server = server_returning(ResponseTemplate::new(200).set_body_string("status: all systems go")).await;
     let url = format!("{}/", server.uri());
 
-    let present = perform_check(&build_http_client(), &keyword_monitor(url.clone(), "systems go", "contains")).await;
+    let present = perform_check(&check_context(), &keyword_monitor(url.clone(), "systems go", "contains")).await;
     assert_eq!(present.status, CheckStatus::Up, "{:?}", present.error);
 
-    let absent = perform_check(&build_http_client(), &keyword_monitor(url, "maintenance", "contains")).await;
+    let absent = perform_check(&check_context(), &keyword_monitor(url, "maintenance", "contains")).await;
     assert_eq!(absent.status, CheckStatus::Down);
     assert_eq!(absent.error.as_deref(), Some("keyword \"maintenance\" not found"));
     assert_eq!(absent.status_code, Some(200));
@@ -132,7 +111,7 @@ async fn keyword_present_is_up_and_absent_is_down() {
 async fn keyword_matching_is_case_sensitive() {
     let server = server_returning(ResponseTemplate::new(200).set_body_string("Status: OK")).await;
     let outcome =
-        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "ok", "contains")).await;
+        perform_check(&check_context(), &keyword_monitor(format!("{}/", server.uri()), "ok", "contains")).await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("keyword \"ok\" not found"));
 }
@@ -142,11 +121,11 @@ async fn not_contains_mode_inverts_the_keyword_check() {
     let server = server_returning(ResponseTemplate::new(200).set_body_string("database error")).await;
     let url = format!("{}/", server.uri());
 
-    let found = perform_check(&build_http_client(), &keyword_monitor(url.clone(), "error", "not_contains")).await;
+    let found = perform_check(&check_context(), &keyword_monitor(url.clone(), "error", "not_contains")).await;
     assert_eq!(found.status, CheckStatus::Down);
     assert_eq!(found.error.as_deref(), Some("keyword \"error\" found"));
 
-    let missing = perform_check(&build_http_client(), &keyword_monitor(url, "panic", "not_contains")).await;
+    let missing = perform_check(&check_context(), &keyword_monitor(url, "panic", "not_contains")).await;
     assert_eq!(missing.status, CheckStatus::Up, "{:?}", missing.error);
 }
 
@@ -155,7 +134,7 @@ async fn a_keyword_beyond_the_body_cap_is_not_found() {
     let body = format!("{}NEEDLE", "a".repeat(MAX_BODY_BYTES));
     let server = server_returning(ResponseTemplate::new(200).set_body_string(body)).await;
     let outcome =
-        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains")).await;
+        perform_check(&check_context(), &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains")).await;
     assert_eq!(outcome.status, CheckStatus::Down);
 }
 
@@ -163,7 +142,7 @@ async fn a_keyword_beyond_the_body_cap_is_not_found() {
 async fn a_keyword_match_still_needs_an_expected_status() {
     let server = server_returning(ResponseTemplate::new(500).set_body_string("all systems go")).await;
     let outcome =
-        perform_check(&build_http_client(), &keyword_monitor(format!("{}/", server.uri()), "systems go", "contains"))
+        perform_check(&check_context(), &keyword_monitor(format!("{}/", server.uri()), "systems go", "contains"))
             .await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("expected status 200-399, got 500"));
@@ -174,7 +153,7 @@ async fn a_slow_response_times_out_within_the_monitor_timeout() {
     let server = server_returning(ResponseTemplate::new(200).set_delay(Duration::from_secs(5))).await;
     let slow = Monitor { timeout_seconds: 1, ..http_monitor(format!("{}/", server.uri())) };
     let started = Instant::now();
-    let outcome = perform_check(&build_http_client(), &slow).await;
+    let outcome = perform_check(&check_context(), &slow).await;
     assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
@@ -184,7 +163,7 @@ async fn a_slow_response_times_out_within_the_monitor_timeout() {
 #[tokio::test]
 async fn http_connection_refused_is_down() {
     let outcome =
-        perform_check(&build_http_client(), &http_monitor(format!("http://127.0.0.1:{}/", closed_port()))).await;
+        perform_check(&check_context(), &http_monitor(format!("http://127.0.0.1:{}/", closed_port()))).await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("connection refused"));
     assert_eq!(outcome.status_code, None);
@@ -195,7 +174,7 @@ async fn tcp_open_port_is_up() {
     let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
     let port = listener.local_addr().unwrap().port();
     let tcp = Monitor { host: Some("127.0.0.1".to_string()), port: Some(i32::from(port)), ..monitor("tcp") };
-    let outcome = perform_check(&build_http_client(), &tcp).await;
+    let outcome = perform_check(&check_context(), &tcp).await;
     assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
     assert!(outcome.response_time_ms.is_some());
     drop(listener);
@@ -204,7 +183,7 @@ async fn tcp_open_port_is_up() {
 #[tokio::test]
 async fn tcp_closed_port_is_down_with_connection_refused() {
     let tcp = Monitor { host: Some("127.0.0.1".to_string()), port: Some(i32::from(closed_port())), ..monitor("tcp") };
-    let outcome = perform_check(&build_http_client(), &tcp).await;
+    let outcome = perform_check(&check_context(), &tcp).await;
     assert_eq!(outcome.status, CheckStatus::Down);
     assert_eq!(outcome.error.as_deref(), Some("connection refused"));
 }
```

`nami-engine/tests/checker_http_config.rs`:

```rust
// HTTP request configuration in the checker. The worker test at the end
// requires `docker compose up -d postgres mailpit`.
mod common;

use common::{
    add_member, check_context, seed_project, seed_user, settled_mail, test_cipher, tls, unique_email,
    unsaved_monitor,
};
use nami_engine::{
    crypto::{Cipher, SecretString},
    models::{
        monitor::Monitor,
        monitor_request::{seal_headers, seal_password, RequestHeader},
    },
    state::AppState,
    uptime::{
        checker::{perform_check, CheckStatus, CONFIG_DECRYPT_ERROR},
        worker::run_due_checks,
    },
};
use wiremock::{
    matchers::{body_string, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

const API_KEY: &str = "key-1f2e3d";
const PASSWORD: &str = "pass-9a8b7c";

fn http_monitor(url: String) -> Monitor {
    Monitor { url: Some(url), ..unsaved_monitor("http") }
}

fn sealed_headers(cipher: &Cipher, headers: &[(&str, &str)]) -> (Option<Vec<u8>>, Vec<String>) {
    let headers: Vec<RequestHeader> = headers
        .iter()
        .map(|(name, value)| RequestHeader { name: name.to_string(), value: SecretString::new(*value) })
        .collect();
    (Some(seal_headers(cipher, &headers)), headers.into_iter().map(|h| h.name).collect())
}

/// A monitor carrying an API-key header, basic auth and a JSON body.
fn configured_monitor(url: String) -> Monitor {
    let cipher = test_cipher();
    let (request_headers_enc, request_header_names) =
        sealed_headers(&cipher, &[("X-Api-Key", API_KEY), ("Content-Type", "application/json")]);
    Monitor {
        http_method: "POST".to_string(),
        request_body: Some("{\"probe\":true}".to_string()),
        request_headers_enc,
        request_header_names,
        auth_username: Some("monitor".to_string()),
        auth_password_enc: Some(seal_password(&cipher, &SecretString::new(PASSWORD))),
        ..http_monitor(url)
    }
}

#[tokio::test]
async fn method_body_custom_headers_and_basic_auth_are_sent() {
    let server = MockServer::start().await;
    // base64("monitor:pass-9a8b7c")
    let basic = "Basic bW9uaXRvcjpwYXNzLTlhOGI3Yw==";
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("x-api-key", API_KEY))
        .and(header("content-type", "application/json"))
        .and(header("authorization", basic))
        .and(body_string("{\"probe\":true}"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = perform_check(&check_context(), &configured_monitor(format!("{}/orders", server.uri()))).await;

    // Anything that did not match every matcher gets wiremock's 404.
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(201));
}

#[tokio::test]
async fn each_configured_method_is_used() {
    for verb in ["PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"] {
        let server = MockServer::start().await;
        Mock::given(method(verb)).respond_with(ResponseTemplate::new(204)).mount(&server).await;
        let monitor = Monitor { http_method: verb.to_string(), ..http_monitor(format!("{}/", server.uri())) };
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Up, "{verb}: {:?}", outcome.error);
    }
}

async fn redirect(server: &MockServer, from: &str, to: String) {
    Mock::given(path(from))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", to))
        .mount(server)
        .await;
}

#[tokio::test]
async fn with_follow_redirects_off_the_redirect_itself_is_the_result() {
    let server = MockServer::start().await;
    redirect(&server, "/old", format!("{}/new", server.uri())).await;
    let monitor = Monitor { follow_redirects: false, ..http_monitor(format!("{}/old", server.uri())) };

    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "302 is inside the default 200-399 range");
    assert_eq!(outcome.status_code, Some(302));

    let strict = Monitor { expected_status_max: 299, ..monitor };
    let outcome = perform_check(&check_context(), &strict).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("expected status 200-299, got 302"));
}

/// A chain /{prefix}/0 -> /{prefix}/1 -> ... -> /{prefix}/{hops} answering 200.
async fn redirect_chain(server: &MockServer, prefix: &str, hops: usize) {
    for i in 0..hops {
        redirect(server, &format!("/{prefix}/{i}"), format!("{}/{prefix}/{}", server.uri(), i + 1)).await;
    }
    Mock::given(path(format!("/{prefix}/{hops}"))).respond_with(ResponseTemplate::new(200)).mount(server).await;
}

#[tokio::test]
async fn at_most_ten_redirects_are_followed() {
    let server = MockServer::start().await;
    redirect_chain(&server, "ten", 10).await;
    redirect_chain(&server, "eleven", 11).await;

    let ten = perform_check(&check_context(), &http_monitor(format!("{}/ten/0", server.uri()))).await;
    assert_eq!(ten.status, CheckStatus::Up, "{:?}", ten.error);

    let eleven = perform_check(&check_context(), &http_monitor(format!("{}/eleven/0", server.uri()))).await;
    assert_eq!(eleven.status, CheckStatus::Down);
    assert_eq!(eleven.error.as_deref(), Some("too many redirects"));
}

/// Security: reqwest drops Authorization/Cookie on a cross-host redirect but
/// would forward a custom header such as an API key; such monitors only
/// follow redirects that stay on the monitored host.
#[tokio::test]
async fn custom_headers_never_follow_a_redirect_to_another_host() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&elsewhere).await;
    // Same machine, different host name: 127.0.0.1 -> localhost.
    let other_host = format!("http://localhost:{}/landing", elsewhere.address().port());
    redirect(&target, "/start", other_host).await;
    let cipher = test_cipher();
    let (request_headers_enc, request_header_names) = sealed_headers(&cipher, &[("X-Api-Key", API_KEY)]);
    let with_key = Monitor {
        request_headers_enc,
        request_header_names,
        ..http_monitor(format!("{}/start", target.uri()))
    };

    let outcome = perform_check(&check_context(), &with_key).await;

    assert_eq!(outcome.status, CheckStatus::Down);
    let error = outcome.error.unwrap();
    assert!(error.contains("redirect to another host (localhost) was not followed"), "{error}");
    assert!(!error.contains(API_KEY));
    assert!(elsewhere.received_requests().await.unwrap().is_empty(), "the API key never left for another host");

    let without_headers = http_monitor(format!("{}/start", target.uri()));
    let outcome = perform_check(&check_context(), &without_headers).await;
    assert_eq!(outcome.status, CheckStatus::Up, "plain monitors still follow it: {:?}", outcome.error);
}

#[tokio::test]
async fn verify_ssl_decides_whether_a_self_signed_certificate_is_accepted() {
    let port = tls::serve_https_ok(tls::self_signed()).await;
    let url = format!("https://localhost:{port}/");

    let verified = perform_check(&check_context(), &http_monitor(url.clone())).await;
    assert_eq!(verified.status, CheckStatus::Down);
    assert!(verified.error.as_deref().unwrap().contains("certificate"), "{:?}", verified.error);

    let unverified = Monitor { verify_ssl: false, ..http_monitor(url) };
    let outcome = perform_check(&check_context(), &unverified).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(200));
}

/// Security: tampered (or wrongly keyed) secrets fail the check with a
/// generic error and no request is sent.
#[tokio::test]
async fn undecryptable_secrets_fail_the_check_with_a_generic_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let mut tampered = configured_monitor(format!("{}/orders", server.uri()));
    tampered.request_headers_enc.as_mut().unwrap()[20] ^= 0x01;
    let other_key = Monitor {
        auth_password_enc: Some(seal_password(&Cipher::new(&[3u8; 32]), &SecretString::new(PASSWORD))),
        ..configured_monitor(format!("{}/orders", server.uri()))
    };

    for monitor in [tampered, other_key] {
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down);
        assert_eq!(outcome.error.as_deref(), Some(CONFIG_DECRYPT_ERROR));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Security: a monitor whose secrets cannot be decrypted goes down with the
/// generic error, the pass still checks every other monitor, and the alert
/// email carries no secret.
#[sqlx::test]
async fn the_worker_survives_an_undecryptable_monitor_and_its_alert_has_no_secret(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let recipient = unique_email("oncall");
    let user = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, user, "viewer").await;
    let healthy_server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&healthy_server).await;
    let cipher = test_cipher();
    let (mut sealed, names) = sealed_headers(&cipher, &[("X-Api-Key", API_KEY)]);
    sealed.as_mut().unwrap()[15] ^= 0x01;
    let broken: uuid::Uuid = sqlx::query_scalar(
        "insert into monitors (project_id, name, check_type, url, confirmation_threshold, request_headers_enc, request_header_names)
         values ($1, 'Broken secrets', 'http', 'http://127.0.0.1:9/', 1, $2, $3) returning id",
    )
    .bind(project)
    .bind(sealed)
    .bind(names)
    .fetch_one(&pool)
    .await
    .unwrap();
    let healthy = common::seed_http_monitor(&pool, project, "Healthy", &healthy_server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 2, "both monitors were checked");

    let status = |id| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("select current_status from monitors where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(status(broken).await, "down");
    assert_eq!(status(healthy).await, "up");
    let cause: Option<String> = sqlx::query_scalar("select cause from incidents where monitor_id = $1")
        .bind(broken)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cause.as_deref(), Some(CONFIG_DECRYPT_ERROR));
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].1.contains(CONFIG_DECRYPT_ERROR), "{}", mail[0].1);
    assert!(!mail[0].1.contains(API_KEY) && !mail[0].0.contains(API_KEY));
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test checker_http_config`
Expected: FAIL to compile — `unresolved import nami_engine::uptime::checker::CheckContext` (from `tests/common/mod.rs`).

- [ ] **Step 4: Implement the request config in the checker**

`src/uptime/checker.rs` — the context and its six prebuilt clients (redirect mode × `verify_ssl`; the same-host policy for monitors with custom headers), `build_request` (decrypts per check, generic error on failure), and redirect errors described by their cause:

```diff
diff --git a/nami-engine/src/uptime/checker.rs b/nami-engine/src/uptime/checker.rs
--- a/nami-engine/src/uptime/checker.rs
+++ b/nami-engine/src/uptime/checker.rs
@@ -5,9 +5,17 @@
 use std::time::{Duration, Instant};
 
 use chrono::{DateTime, Utc};
+use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
 use serde::{Deserialize, Serialize};
 
-use crate::models::monitor::Monitor;
+use crate::{
+    crypto::{Cipher, SecretString},
+    models::{
+        monitor::Monitor,
+        monitor_request::{open_headers, open_password, BODY_METHODS},
+    },
+    state::AppState,
+};
 
 /// Keyword checks read at most this much of the body; a keyword that only
 /// appears after the first MiB counts as not found.
@@ -29,31 +37,153 @@ pub struct CheckOutcome {
     pub checked_at: DateTime<Utc>,
 }
 
-/// The client used for checks. Redirects are followed (reqwest's default
-/// policy, up to 10 hops). Idle connections are never kept, so every check
-/// opens a fresh connection: a pooled keep-alive socket would hide a server
-/// that stopped accepting new connections, and would leave connection setup
-/// out of the measured response time.
-pub fn build_http_client() -> reqwest::Client {
+/// Redirect hops a check follows before giving up (`follow_redirects`).
+pub const MAX_REDIRECTS: usize = 10;
+
+/// The down reason when a monitor's stored secrets cannot be decrypted. It
+/// is deliberately generic: it ends up in check history and alert emails.
+pub const CONFIG_DECRYPT_ERROR: &str =
+    "request configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)";
+
+/// What every check of one worker pass shares. Built once per pass: cheap
+/// next to the checks themselves, and its connections never outlive the
+/// runtime that drives them.
+#[derive(Clone)]
+pub struct CheckContext {
+    pub http: HttpClients,
+    pub cipher: Cipher,
+}
+
+impl CheckContext {
+    pub fn new(cipher: Cipher) -> CheckContext {
+        CheckContext { http: HttpClients::new(), cipher }
+    }
+
+    pub fn for_state(state: &AppState) -> CheckContext {
+        CheckContext::new(state.cipher.clone())
+    }
+}
+
+/// One prebuilt reqwest client per combination of the per-monitor transport
+/// settings (`follow_redirects`, `verify_ssl`, custom headers present).
+/// Idle connections are never kept, so every check opens a fresh
+/// connection: a pooled keep-alive socket would hide a server that stopped
+/// accepting new connections, and would leave connection setup out of the
+/// measured response time.
+#[derive(Clone)]
+pub struct HttpClients {
+    /// Indexed as `redirect mode * 2 + (verify_ssl ? 0 : 1)`, redirect mode
+    /// 0 = none, 1 = follow, 2 = follow on the same host only.
+    clients: [reqwest::Client; 6],
+}
+
+impl Default for HttpClients {
+    fn default() -> Self {
+        HttpClients::new()
+    }
+}
+
+impl HttpClients {
+    pub fn new() -> HttpClients {
+        HttpClients {
+            clients: [
+                build_client(RedirectMode::None, true),
+                build_client(RedirectMode::None, false),
+                build_client(RedirectMode::Follow, true),
+                build_client(RedirectMode::Follow, false),
+                build_client(RedirectMode::SameHost, true),
+                build_client(RedirectMode::SameHost, false),
+            ],
+        }
+    }
+
+    /// The client for one monitor. Monitors with custom headers only follow
+    /// redirects that stay on the monitored host: reqwest drops
+    /// `Authorization` and `Cookie` on a cross-host hop, but would forward
+    /// every custom header (an API key, say) to wherever the target points.
+    pub fn select(&self, follow_redirects: bool, verify_ssl: bool, has_custom_headers: bool) -> &reqwest::Client {
+        let mode = match (follow_redirects, has_custom_headers) {
+            (false, _) => 0,
+            (true, false) => 1,
+            (true, true) => 2,
+        };
+        &self.clients[mode * 2 + usize::from(!verify_ssl)]
+    }
+}
+
+enum RedirectMode {
+    None,
+    Follow,
+    SameHost,
+}
+
+#[derive(Debug)]
+struct TooManyRedirects;
+
+impl std::fmt::Display for TooManyRedirects {
+    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
+        f.write_str("too many redirects")
+    }
+}
+
+impl std::error::Error for TooManyRedirects {}
+
+#[derive(Debug)]
+struct CrossHostRedirect(String);
+
+impl std::fmt::Display for CrossHostRedirect {
+    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
+        write!(
+            f,
+            "redirect to another host ({}) was not followed: custom headers are only sent to the monitored host",
+            self.0
+        )
+    }
+}
+
+impl std::error::Error for CrossHostRedirect {}
+
+fn build_client(redirects: RedirectMode, verify_ssl: bool) -> reqwest::Client {
+    let policy = match redirects {
+        RedirectMode::None => reqwest::redirect::Policy::none(),
+        RedirectMode::Follow => reqwest::redirect::Policy::limited(MAX_REDIRECTS),
+        RedirectMode::SameHost => reqwest::redirect::Policy::custom(|attempt| {
+            // `previous` starts with the original URL, as in reqwest's own
+            // `limited` policy.
+            if attempt.previous().len() > MAX_REDIRECTS {
+                return attempt.error(TooManyRedirects);
+            }
+            let original_host = attempt.previous().first().and_then(|url| url.host_str()).map(str::to_owned);
+            match attempt.url().host_str() {
+                Some(host) if Some(host) == original_host.as_deref() => attempt.follow(),
+                host => {
+                    let host = host.unwrap_or("unknown").to_owned();
+                    attempt.error(CrossHostRedirect(host))
+                }
+            }
+        }),
+    };
     reqwest::Client::builder()
         .pool_max_idle_per_host(0)
         .user_agent(concat!("Nami-Uptime/", env!("CARGO_PKG_VERSION")))
+        .redirect(policy)
+        .danger_accept_invalid_certs(!verify_ssl)
         .build()
-        .expect("failed to build the uptime HTTP client")
+        .expect("failed to build an uptime HTTP client")
 }
 
 /// Runs the monitor's check within its timeout. Response time is measured
 /// to the response headers for `http` checks, to the end of the (capped)
 /// body for `keyword` checks, and to connection setup for `tcp` checks.
 /// Keyword matching is case-sensitive.
-pub async fn perform_check(http: &reqwest::Client, monitor: &Monitor) -> CheckOutcome {
+pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcome {
     let checked_at = Utc::now();
     let timeout_secs = u64::try_from(monitor.timeout_seconds.clamp(1, 60)).unwrap_or(10);
     let timeout = Duration::from_secs(timeout_secs);
     let started = Instant::now();
     let probe = async {
         match monitor.check_type.as_str() {
-            "http" | "keyword" => probe_http(http, monitor, timeout, started).await,
+            "http" | "keyword" => probe_http(ctx, monitor, timeout, started).await,
             "tcp" => probe_tcp(monitor, started).await,
             other => Observation::down(None, None, format!("unsupported check type {other:?}")),
         }
@@ -91,17 +221,15 @@ fn elapsed_ms(started: Instant) -> u32 {
     u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
 }
 
-async fn probe_http(
-    http: &reqwest::Client,
-    monitor: &Monitor,
-    timeout: Duration,
-    started: Instant,
-) -> Observation {
+async fn probe_http(ctx: &CheckContext, monitor: &Monitor, timeout: Duration, started: Instant) -> Observation {
     let Some(url) = monitor.url.as_deref() else {
         return Observation::down(None, None, "monitor has no url".to_string());
     };
-    let method = if monitor.http_method == "HEAD" { reqwest::Method::HEAD } else { reqwest::Method::GET };
-    let mut response = match http.request(method, url).timeout(timeout).send().await {
+    let request = match build_request(ctx, monitor, url, timeout) {
+        Ok(request) => request,
+        Err(message) => return Observation::down(None, None, message),
+    };
+    let mut response = match request.send().await {
         Ok(response) => response,
         Err(e) => return Observation::down(None, None, describe_request_error(&e, timeout)),
     };
@@ -140,6 +268,47 @@ async fn probe_http(
     }
 }
 
+/// The request one check sends: method, custom headers, basic auth and
+/// body, on the client matching the monitor's redirect and TLS settings.
+/// Secrets are decrypted here, per check, and never logged or echoed: any
+/// failure becomes a generic message.
+fn build_request(
+    ctx: &CheckContext,
+    monitor: &Monitor,
+    url: &str,
+    timeout: Duration,
+) -> Result<reqwest::RequestBuilder, String> {
+    let headers = match &monitor.request_headers_enc {
+        Some(sealed) => open_headers(&ctx.cipher, sealed).map_err(|_| CONFIG_DECRYPT_ERROR.to_string())?,
+        None => Vec::new(),
+    };
+    let password = match &monitor.auth_password_enc {
+        Some(sealed) => Some(open_password(&ctx.cipher, sealed).map_err(|_| CONFIG_DECRYPT_ERROR.to_string())?),
+        None => None,
+    };
+    let method = reqwest::Method::from_bytes(monitor.http_method.as_bytes())
+        .map_err(|_| format!("unsupported http method {:?}", monitor.http_method))?;
+    let mut header_map = HeaderMap::new();
+    for header in &headers {
+        let invalid = || "invalid request header configuration".to_string();
+        let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| invalid())?;
+        let mut value = HeaderValue::from_str(header.value.expose()).map_err(|_| invalid())?;
+        value.set_sensitive(true);
+        header_map.append(name, value);
+    }
+    let client = ctx.http.select(monitor.follow_redirects, monitor.verify_ssl, !headers.is_empty());
+    let mut request = client.request(method, url).timeout(timeout).headers(header_map);
+    if let Some(username) = &monitor.auth_username {
+        request = request.basic_auth(username, password.as_ref().map(SecretString::expose));
+    }
+    if let Some(body) = &monitor.request_body {
+        if BODY_METHODS.contains(&monitor.http_method.as_str()) {
+            request = request.body(body.clone());
+        }
+    }
+    Ok(request)
+}
+
 async fn read_capped(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, reqwest::Error> {
     let mut body = Vec::new();
     while let Some(chunk) = response.chunk().await? {
@@ -172,7 +341,8 @@ fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
         return format!("timeout after {}s", timeout.as_secs());
     }
     if error.is_redirect() {
-        return "too many redirects".to_string();
+        // "too many redirects", or why a redirect was refused.
+        return root_cause(error);
     }
     match io_error_kind(error) {
         Some(std::io::ErrorKind::ConnectionRefused) => "connection refused".to_string(),
```

`src/uptime/worker.rs` builds the context once per pass:

```diff
diff --git a/nami-engine/src/uptime/worker.rs b/nami-engine/src/uptime/worker.rs
--- a/nami-engine/src/uptime/worker.rs
+++ b/nami-engine/src/uptime/worker.rs
@@ -9,7 +9,7 @@ use uuid::Uuid;
 
 use super::{
     alerts::{self, Subject, Transition, TransitionKind},
-    checker::{self, CheckOutcome, CheckStatus},
+    checker::{self, CheckContext, CheckOutcome, CheckStatus},
     heartbeat,
     store::{self, UptimeCheckRow},
 };
@@ -156,17 +156,15 @@ pub async fn run_due_checks(state: &AppState) -> Result<usize, AppError> {
         return Ok(0);
     }
 
-    // Built per pass: cheap next to the checks themselves, and it never
-    // outlives the runtime that drives its connections.
-    let http = checker::build_http_client();
+    let ctx = Arc::new(CheckContext::for_state(state));
     let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
     let mut checks = JoinSet::new();
     for monitor in monitors {
-        let http = http.clone();
+        let ctx = Arc::clone(&ctx);
         let semaphore = Arc::clone(&semaphore);
         checks.spawn(async move {
             let _permit = semaphore.acquire_owned().await.expect("the check semaphore is never closed");
-            let outcome = checker::perform_check(&http, &monitor).await;
+            let outcome = checker::perform_check(&ctx, &monitor).await;
             (monitor, outcome)
         });
     }
```

- [ ] **Step 5: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/checker_http_config.rs` 8 passed (`the_worker_survives_...` needs Mailpit), `tests/uptime_checker.rs` still 14 passed, the rest green.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/src/uptime/checker.rs nami-engine/src/uptime/worker.rs \
  nami-engine/tests/common/mod.rs nami-engine/tests/common/tls.rs nami-engine/tests/uptime_checker.rs \
  nami-engine/tests/checker_http_config.rs
git commit -m "feat(engine): checks send method, body, headers and basic auth (NAMI-UP-11)

Prebuilt clients per follow_redirects/verify_ssl; monitors with custom
headers never follow a redirect to another host. Undecryptable secrets
fail the check with a generic error and the pass carries on.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12 (NAMI-UP-12): Recovery threshold

Branch `feat/NAMI-UP-12-recovery-threshold`, worktree `.worktrees/NAMI-UP-12/`.

**Files:**
- Create: `nami-engine/migrations/0013_monitor_recovery_threshold.sql`
- Modify: `nami-engine/src/models/monitor.rs`, `nami-engine/src/uptime/worker.rs`
- Test: `nami-engine/tests/recovery_threshold.rs`

**Interfaces:**
- Consumes: `worker::apply_outcome(pool: &PgPool, monitor: &Monitor, outcome: &CheckOutcome) -> Result<Option<Transition>, sqlx::Error>`, `alerts::TransitionKind`, `checker::{CheckOutcome, CheckStatus}` (Phase 1); `write_config` (Task 10).
- Produces:
  - Columns `recovery_threshold int not null default 1 check (between 1 and 10)`, `consecutive_successes int not null default 0`.
  - `Monitor.recovery_threshold: i32`, `Monitor.consecutive_successes: i32`; `MonitorConfig.recovery_threshold: i32` (create default 1, PATCH `Option<i32>`, not a check change).
  - `apply_outcome` state machine: a success while `down` adds to `consecutive_successes` and only reaching `recovery_threshold` sets `up`, resolves the incident and returns `Recovered`; any failure resets `consecutive_successes`. Heartbeats unchanged.

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/recovery_threshold.rs`:

```rust
// Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use chrono::Utc;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use nami_engine::{
    models::monitor::Monitor,
    uptime::{
        alerts::TransitionKind,
        checker::{CheckOutcome, CheckStatus},
        worker::apply_outcome,
    },
};
use serde_json::json;
use uuid::Uuid;

fn outcome(status: CheckStatus) -> CheckOutcome {
    CheckOutcome {
        status,
        status_code: (status == CheckStatus::Up).then_some(200),
        response_time_ms: Some(10),
        error: (status == CheckStatus::Down).then(|| "connection refused".to_string()),
        checked_at: Utc::now(),
    }
}

/// A monitor that goes down on its first failure and needs `recovery`
/// consecutive successes to come back.
async fn monitor_with_recovery(pool: &sqlx::PgPool, recovery: i32) -> Monitor {
    let project = seed_project(pool).await;
    let id = seed_http_monitor(pool, project, "Checkout API", "http://127.0.0.1:9/", 1).await;
    sqlx::query_as("update monitors set recovery_threshold = $2 where id = $1 returning *")
        .bind(id)
        .bind(recovery)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// (status, consecutive_failures, consecutive_successes)
async fn counters(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, i32) {
    sqlx::query_as("select current_status, consecutive_failures, consecutive_successes from monitors where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn incident_statuses(pool: &sqlx::PgPool, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("select status from incidents where monitor_id = $1 order by started_at")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn apply(pool: &sqlx::PgPool, monitor: &Monitor, status: CheckStatus) -> Option<TransitionKind> {
    apply_outcome(pool, monitor, &outcome(status)).await.unwrap().map(|t| t.kind)
}

#[sqlx::test]
async fn the_default_threshold_recovers_on_the_first_success(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 1).await;
    assert_eq!(apply(&pool, &monitor, CheckStatus::Down).await, Some(TransitionKind::Down));

    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, Some(TransitionKind::Recovered));

    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["resolved"]);
}

#[sqlx::test]
async fn a_threshold_of_three_needs_three_consecutive_successes(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;

    for expected_successes in 1..=2 {
        assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None, "no recovery yet");
        assert_eq!(counters(&pool, monitor.id).await, ("down".to_string(), 0, expected_successes));
        assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["open"]);
    }

    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, Some(TransitionKind::Recovered));
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["resolved"]);
}

#[sqlx::test]
async fn a_failure_while_recovering_starts_the_count_again(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    apply(&pool, &monitor, CheckStatus::Up).await;
    apply(&pool, &monitor, CheckStatus::Up).await;

    assert_eq!(apply(&pool, &monitor, CheckStatus::Down).await, None, "already down: no second alert");
    // The successes had reset the failure count, so this is failure #1 again.
    assert_eq!(counters(&pool, monitor.id).await, ("down".to_string(), 1, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["open"], "still exactly one open incident");

    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, Some(TransitionKind::Recovered));
}

#[sqlx::test]
async fn successes_while_up_or_pending_never_build_up_a_count(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    for _ in 0..3 {
        assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    }
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
}

#[sqlx::test]
async fn recovery_threshold_is_part_of_the_api(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let body = |extra: serde_json::Value| {
        let mut body = json!({ "name": "API", "check_type": "http", "url": "https://example.com" });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        body
    };

    let (status, default) = call(&app, "POST", &collection, Some(&member), Some(body(json!({})))).await;
    assert_eq!(status, StatusCode::CREATED, "{default}");
    assert_eq!(default["recovery_threshold"], 1);
    assert_eq!(default["consecutive_successes"], 0);

    let (status, created) =
        call(&app, "POST", &collection, Some(&member), Some(body(json!({ "recovery_threshold": 3 })))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["recovery_threshold"], 3);

    for invalid in [0, 11] {
        let (status, error) =
            call(&app, "POST", &collection, Some(&member), Some(body(json!({ "recovery_threshold": invalid })))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("recovery_threshold"), "{error}");
    }

    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour'").execute(&pool).await.unwrap();
    let (status, patched) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "recovery_threshold": 5 }))).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["recovery_threshold"], 5);
    let due: bool = sqlx::query_scalar("select next_check_at <= now() from monitors where id = $1")
        .bind(Uuid::parse_str(created["id"].as_str().unwrap()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!due, "the recovery threshold is not check config: no reschedule");
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test recovery_threshold`
Expected: FAIL — the tests compile, but every one fails at runtime with `column "recovery_threshold" of relation "monitors" does not exist` (or a missing JSON field).

- [ ] **Step 3: Add the migration**

`nami-engine/migrations/0013_monitor_recovery_threshold.sql`:

```sql
-- Consecutive successes a down monitor needs before it recovers
-- (BetterStack's "recovery period").
alter table monitors
    add column recovery_threshold int not null default 1
        check (recovery_threshold between 1 and 10),
    add column consecutive_successes int not null default 0;
```

- [ ] **Step 4: Add the field to the model and API**

`src/models/monitor.rs`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -56,6 +56,8 @@ pub struct Monitor {
     pub auth_password_enc: Option<Vec<u8>>,
     pub follow_redirects: bool,
     pub verify_ssl: bool,
+    pub recovery_threshold: i32,
+    pub consecutive_successes: i32,
 }
 
 /// A monitor as every API response shows it: the secrets are reduced to
@@ -104,6 +106,7 @@ pub struct MonitorConfig {
     pub auth_password: PasswordChange,
     pub follow_redirects: bool,
     pub verify_ssl: bool,
+    pub recovery_threshold: i32,
 }
 
 impl From<&Monitor> for MonitorConfig {
@@ -129,14 +132,15 @@ impl From<&Monitor> for MonitorConfig {
             auth_password: PasswordChange::Keep { present: m.auth_password_enc.is_some() },
             follow_redirects: m.follow_redirects,
             verify_ssl: m.verify_ssl,
+            recovery_threshold: m.recovery_threshold,
         }
     }
 }
 
 impl MonitorConfig {
     /// True when a field that changes how or when the monitor is checked
-    /// differs from `current`. Renaming, pausing and the confirmation
-    /// threshold do not count; replacing or clearing a secret does.
+    /// differs from `current`. Renaming, pausing and the confirmation and
+    /// recovery thresholds do not count; replacing or clearing a secret does.
     pub fn changes_check_of(&self, current: &Monitor) -> bool {
         self.check_type != current.check_type
             || self.url != current.url
@@ -193,6 +197,8 @@ pub struct CreateMonitorRequest {
     pub follow_redirects: bool,
     #[serde(default = "default_true")]
     pub verify_ssl: bool,
+    #[serde(default = "default_recovery_threshold")]
+    pub recovery_threshold: i32,
 }
 
 fn default_http_method() -> String {
@@ -219,6 +225,9 @@ fn default_confirmation_threshold() -> i32 {
 fn default_true() -> bool {
     true
 }
+fn default_recovery_threshold() -> i32 {
+    1
+}
 
 impl CreateMonitorRequest {
     pub fn into_config(self) -> MonitorConfig {
@@ -249,6 +258,7 @@ impl CreateMonitorRequest {
             },
             follow_redirects: self.follow_redirects,
             verify_ssl: self.verify_ssl,
+            recovery_threshold: self.recovery_threshold,
         }
     }
 }
@@ -283,6 +293,7 @@ pub struct PatchMonitorRequest {
     pub auth_password: Option<Option<SecretString>>,
     pub follow_redirects: Option<bool>,
     pub verify_ssl: Option<bool>,
+    pub recovery_threshold: Option<i32>,
 }
 
 /// A nullable PATCH field applied to its current value.
@@ -326,6 +337,7 @@ impl PatchMonitorRequest {
             },
             follow_redirects: self.follow_redirects.unwrap_or(base.follow_redirects),
             verify_ssl: self.verify_ssl.unwrap_or(base.verify_ssl),
+            recovery_threshold: self.recovery_threshold.unwrap_or(base.recovery_threshold),
         }
     }
 }
@@ -362,6 +374,9 @@ pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
     if !(1..=10).contains(&c.confirmation_threshold) {
         return Err("confirmation_threshold must be between 1 and 10".into());
     }
+    if !(1..=10).contains(&c.recovery_threshold) {
+        return Err("recovery_threshold must be between 1 and 10".into());
+    }
     Ok(c)
 }
 
@@ -518,7 +533,8 @@ async fn write_config(
              request_body = $17, auth_username = $18, follow_redirects = $19, verify_ssl = $20,
              request_header_names = case when $21 then request_header_names else $22 end,
              request_headers_enc = case when $21 then request_headers_enc else $23 end,
-             auth_password_enc = case when $24 then auth_password_enc else $25 end
+             auth_password_enc = case when $24 then auth_password_enc else $25 end,
+             recovery_threshold = $26
          where id = $1
          returning *",
     )
@@ -547,6 +563,7 @@ async fn write_config(
     .bind(sealed_headers)
     .bind(keep_password)
     .bind(sealed_password)
+    .bind(c.recovery_threshold)
     .fetch_one(&mut *conn)
     .await
 }
```

- [ ] **Step 5: Count successes in the state machine**

`src/uptime/worker.rs` — `apply_outcome` now reads its state into a `MonitorState` row:

```diff
diff --git a/nami-engine/src/uptime/worker.rs b/nami-engine/src/uptime/worker.rs
--- a/nami-engine/src/uptime/worker.rs
+++ b/nami-engine/src/uptime/worker.rs
@@ -48,16 +48,31 @@ pub async fn claim_due_monitors(pool: &PgPool, limit: i64) -> Result<Vec<Monitor
     .await
 }
 
+/// The state `apply_outcome` reads under the monitor's row lock.
+#[derive(sqlx::FromRow)]
+struct MonitorState {
+    project_id: Uuid,
+    name: String,
+    current_status: String,
+    consecutive_failures: i32,
+    confirmation_threshold: i32,
+    consecutive_successes: i32,
+    recovery_threshold: i32,
+    paused: bool,
+}
+
 /// Applies one check result to its monitor's state in a single transaction.
 /// The row is locked (`for update`) and the state read from that locked row
 /// rather than from `monitor` (a snapshot taken at claim time), so
 /// concurrent results for one monitor are applied one after the other.
 ///
-/// - success: failures reset; `down` -> `up` resolves the open incident and
-///   returns a `Recovered` transition; `pending` -> `up` is silent.
-/// - failure: failures + 1; reaching the confirmation threshold while not
-///   already `down` (including from `pending`) sets `down`, opens an
-///   incident and returns a `Down` transition.
+/// - success: failures reset. While `down`, successes count up, and
+///   reaching `recovery_threshold` sets `up`, resolves the open incident and
+///   returns a `Recovered` transition; until then the monitor stays `down`.
+///   From `pending` or `up`, a success sets `up` silently.
+/// - failure: successes reset; failures + 1; reaching the confirmation
+///   threshold while not already `down` (including from `pending`) sets
+///   `down`, opens an incident and returns a `Down` transition.
 ///
 /// A monitor deleted or paused while its check was in flight is left alone.
 pub async fn apply_outcome(
@@ -66,37 +81,48 @@ pub async fn apply_outcome(
     outcome: &CheckOutcome,
 ) -> Result<Option<Transition>, sqlx::Error> {
     let mut tx = pool.begin().await?;
-    let row: Option<(Uuid, String, String, i32, i32, bool)> = sqlx::query_as(
-        "select project_id, name, current_status, consecutive_failures, confirmation_threshold, paused
+    let state: Option<MonitorState> = sqlx::query_as(
+        "select project_id, name, current_status, consecutive_failures, confirmation_threshold,
+                consecutive_successes, recovery_threshold, paused
          from monitors where id = $1 for update",
     )
     .bind(monitor.id)
     .fetch_optional(&mut *tx)
     .await?;
-    let Some((project_id, name, status, failures, threshold, paused)) = row else {
+    let Some(state) = state else {
         return Ok(None);
     };
-    if paused {
+    if state.paused {
         return Ok(None);
     }
 
     let transition = match outcome.status {
         CheckStatus::Up => {
+            let is_down = state.current_status == "down";
+            let successes = state.consecutive_successes.saturating_add(1);
+            let recovers = is_down && successes >= state.recovery_threshold;
+            let stays_down = is_down && !recovers;
             sqlx::query(
-                "update monitors set current_status = 'up', consecutive_failures = 0, last_checked_at = $2
+                "update monitors
+                 set current_status = case when $2 then current_status else 'up' end,
+                     consecutive_failures = 0,
+                     consecutive_successes = $3,
+                     last_checked_at = $4
                  where id = $1",
             )
             .bind(monitor.id)
+            .bind(stays_down)
+            .bind(if stays_down { successes } else { 0 })
             .bind(outcome.checked_at)
             .execute(&mut *tx)
             .await?;
-            if status == "down" {
+            if recovers {
                 incident::resolve_for_monitor(&mut tx, monitor.id, outcome.checked_at).await?;
                 Some(Transition {
                     kind: TransitionKind::Recovered,
                     subject: Subject::Monitor(monitor.id),
-                    project_id,
-                    name,
+                    project_id: state.project_id,
+                    name: state.name,
                     cause: None,
                     at: outcome.checked_at,
                 })
@@ -105,11 +131,12 @@ pub async fn apply_outcome(
             }
         }
         CheckStatus::Down => {
-            let failures = failures.saturating_add(1);
-            let goes_down = failures >= threshold && status != "down";
+            let failures = state.consecutive_failures.saturating_add(1);
+            let goes_down = failures >= state.confirmation_threshold && state.current_status != "down";
             sqlx::query(
                 "update monitors
                  set consecutive_failures = $2,
+                     consecutive_successes = 0,
                      current_status = case when $3 then 'down' else current_status end,
                      last_checked_at = $4
                  where id = $1",
@@ -123,9 +150,9 @@ pub async fn apply_outcome(
             if goes_down {
                 let opened = incident::open_for_monitor(
                     &mut tx,
-                    project_id,
+                    state.project_id,
                     monitor.id,
-                    &format!("{name} is down"),
+                    &format!("{} is down", state.name),
                     outcome.error.as_deref(),
                     outcome.checked_at,
                 )
@@ -135,8 +162,8 @@ pub async fn apply_outcome(
                 opened.map(|_| Transition {
                     kind: TransitionKind::Down,
                     subject: Subject::Monitor(monitor.id),
-                    project_id,
-                    name,
+                    project_id: state.project_id,
+                    name: state.name,
                     cause: outcome.error.clone(),
                     at: outcome.checked_at,
                 })
```

- [ ] **Step 6: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `tests/recovery_threshold.rs` 5 passed; `uptime_worker.rs` (threshold 1 = Phase 1 behaviour) and `heartbeats.rs` unchanged and green.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/0013_monitor_recovery_threshold.sql nami-engine/src/models/monitor.rs \
  nami-engine/src/uptime/worker.rs nami-engine/tests/recovery_threshold.rs
git commit -m "feat(engine): recovery threshold for down monitors (NAMI-UP-12)

A down monitor recovers only after recovery_threshold (1-10, default 1)
consecutive successful checks; any failure restarts the count.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13 (NAMI-UP-13): `ping` and `dns` monitors

Branch `feat/NAMI-UP-13-ping-dns`, worktree `.worktrees/NAMI-UP-13/`.

**Files:**
- Create: `nami-engine/migrations/0014_monitor_ping_dns.sql`, `nami-engine/src/models/target.rs`, `nami-engine/src/uptime/ping.rs`, `nami-engine/src/uptime/dns.rs`
- Modify: `nami-engine/Cargo.toml`, `nami-engine/.env.example`, `nami-engine/src/models/mod.rs`, `nami-engine/src/models/monitor.rs`, `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/checker.rs`
- Test: `nami-engine/tests/checker_ping_dns.rs`, `nami-engine/tests/monitor_types_api.rs`, unit tests in `src/models/target.rs`

**Interfaces:**
- Consumes: `checker::{perform_check, CheckContext}`, `common::{check_context, unsaved_monitor}` (Task 11); `write_config` (Task 10).
- Produces:
  - Columns `dns_record_type text check (in A, AAAA, CNAME, MX, TXT, NS)`, `dns_nameserver text`, `expected_value text`; `check_type` accepts `ping`, `dns`.
  - `models::target::validate_host(raw: &str) -> Result<String, String>` (host name or IP literal, normalized) and `parse_nameserver(raw: &str) -> Result<String, String>` (`ip:port`, port 53 by default) — reused by Tasks 14 and 15.
  - `monitor::{CHECK_TYPES, DNS_RECORD_TYPES, MAX_EXPECTED_VALUE_CHARS}`; `Monitor`/`MonitorConfig` gain `dns_record_type`, `dns_nameserver`, `expected_value` (PATCH: `dns_nameserver`/`expected_value` nullable). Validation is split per type (`validate_ping`, `validate_dns`, private `required_host`) with one `clear_unused_fields` that nulls every field the type does not use; later types extend both.
  - `checker::Observation` is now `pub(crate)` with `up(status_code, started)`, `up_after(Duration)`, `down(status_code, response_time_ms, error)`; `pub(crate) fn elapsed_ms(Instant) -> u32`; `pub(crate) fn printable(&str, max_chars) -> String` (control characters dropped, truncated with `…`).
  - `uptime::ping::probe(monitor, timeout) -> Observation`, `uptime::dns::probe(monitor, timeout, started) -> Observation` (both `pub(crate)`).

- [ ] **Step 1: Add the crates and document the ping requirement**

```bash
cd nami-engine
cargo add surge-ping@0.9.1 hickory-resolver@0.26.3
```

Ping uses unprivileged ICMP sockets: on Linux (and in Docker) the engine's — and the test runner's — group must be inside `net.ipv4.ping_group_range` (macOS needs nothing). Append to `nami-engine/.env.example`:

```diff
diff --git a/nami-engine/.env.example b/nami-engine/.env.example
--- a/nami-engine/.env.example
+++ b/nami-engine/.env.example
@@ -32,3 +32,7 @@ UPTIME_WORKER_ENABLED=true
 # to start without it. Generate one with `openssl rand -base64 32` and back it
 # up — secrets encrypted under a lost key cannot be recovered.
 NAMI_ENCRYPTION_KEY=
+# Ping monitors use unprivileged ICMP sockets (no root). On Linux, and so in
+# Docker, the engine's group must be inside net.ipv4.ping_group_range:
+#   sudo sysctl -w net.ipv4.ping_group_range="0 2147483647"
+# or, for a container: sysctls: ["net.ipv4.ping_group_range=0 2147483647"]
```

- [ ] **Step 2: Write the failing tests**

Checker tests (a stub nameserver built from `hickory-resolver`'s re-exported `proto`, so no internet is needed) — `nami-engine/tests/checker_ping_dns.rs`:

```rust
// `ping` and `dns` checks. Ping needs unprivileged ICMP sockets (macOS: always;
// Linux: the test runner's group inside net.ipv4.ping_group_range). DNS checks
// run against an in-test stub nameserver: no internet access needed.
mod common;

use std::{
    net::SocketAddr,
    str::FromStr,
    time::{Duration, Instant},
};

use common::{check_context, unsaved_monitor};
use hickory_resolver::proto::{
    op::{Message, ResponseCode},
    rr::{rdata, Name, RData, Record, RecordType},
};
use nami_engine::{
    models::monitor::Monitor,
    uptime::checker::{perform_check, CheckStatus},
};

fn ping_monitor(host: &str) -> Monitor {
    Monitor { host: Some(host.to_string()), ..unsaved_monitor("ping") }
}

#[tokio::test]
async fn pinging_localhost_is_up_with_a_round_trip_time() {
    let outcome = perform_check(&check_context(), &ping_monitor("127.0.0.1")).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn pinging_an_unroutable_address_is_down_within_the_timeout() {
    // 192.0.2.0/24 is TEST-NET-1 (RFC 5737): nothing ever answers.
    let monitor = Monitor { timeout_seconds: 1, ..ping_monitor("192.0.2.1") };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &monitor).await;
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(outcome.error.is_some());
}

#[tokio::test]
async fn pinging_a_name_that_does_not_resolve_is_down() {
    // .invalid never resolves (RFC 2606).
    let outcome = perform_check(&check_context(), &ping_monitor("no-such-host.invalid")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(outcome.error.as_deref().unwrap().starts_with("could not resolve no-such-host.invalid"), "{:?}", outcome.error);
}

/// A UDP nameserver on 127.0.0.1 answering from a fixed zone:
/// `ok.test` has A/MX/TXT/CNAME/NS records (no AAAA), `broken.test`
/// answers SERVFAIL, `missing.test` NXDOMAIN, and `silent.test` never
/// answers.
async fn stub_nameserver() -> SocketAddr {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((len, peer)) = socket.recv_from(&mut buf).await else { return };
            let Ok(request) = Message::from_vec(&buf[..len]) else { continue };
            let Some(query) = request.queries.first().cloned() else { continue };
            let mut response = Message::response(request.metadata.id, request.metadata.op_code);
            response.metadata.recursion_desired = request.metadata.recursion_desired;
            response.metadata.recursion_available = true;
            response.add_query(query.clone());
            let name = query.name().clone();
            let answer = |data: RData| Record::from_rdata(name.clone(), 60, data);
            let target = |s: &str| Name::from_ascii(s).unwrap();
            match (name.to_ascii().as_str(), query.query_type()) {
                ("ok.test.", RecordType::A) => {
                    response.add_answer(answer(RData::A(rdata::A::from_str("10.0.0.7").unwrap())));
                }
                ("ok.test.", RecordType::MX) => {
                    response.add_answer(answer(RData::MX(rdata::MX::new(10, target("Mail.OK.test.")))));
                }
                ("ok.test.", RecordType::TXT) => {
                    let txt = rdata::TXT::new(vec!["v=spf1 ".to_string(), "include:_spf.ok.test -all".to_string()]);
                    response.add_answer(answer(RData::TXT(txt)));
                }
                ("ok.test.", RecordType::CNAME) => {
                    response.add_answer(answer(RData::CNAME(rdata::CNAME(target("edge.cdn.test.")))));
                }
                ("ok.test.", RecordType::NS) => {
                    response.add_answer(answer(RData::NS(rdata::NS(target("ns1.ok.test.")))));
                }
                ("broken.test.", _) => response.metadata.response_code = ResponseCode::ServFail,
                ("missing.test.", _) => response.metadata.response_code = ResponseCode::NXDomain,
                ("silent.test.", _) => continue,
                _ => {}
            }
            let _ = socket.send_to(&response.to_vec().unwrap(), peer).await;
        }
    });
    addr
}

fn dns_monitor(nameserver: SocketAddr, host: &str, record_type: &str, expected: Option<&str>) -> Monitor {
    Monitor {
        host: Some(host.to_string()),
        dns_record_type: Some(record_type.to_string()),
        dns_nameserver: Some(nameserver.to_string()),
        expected_value: expected.map(str::to_string),
        ..unsaved_monitor("dns")
    }
}

#[tokio::test]
async fn a_resolving_record_is_up_and_an_expected_value_must_be_in_an_answer() {
    let ns = stub_nameserver().await;

    let any = perform_check(&check_context(), &dns_monitor(ns, "ok.test", "A", None)).await;
    assert_eq!(any.status, CheckStatus::Up, "{:?}", any.error);
    assert!(any.response_time_ms.is_some());

    let matching = perform_check(&check_context(), &dns_monitor(ns, "ok.test", "A", Some("10.0.0.7"))).await;
    assert_eq!(matching.status, CheckStatus::Up, "{:?}", matching.error);

    let wrong = perform_check(&check_context(), &dns_monitor(ns, "ok.test", "A", Some("10.0.0.8"))).await;
    assert_eq!(wrong.status, CheckStatus::Down);
    assert_eq!(wrong.error.as_deref(), Some("no A answer contains \"10.0.0.8\" (got: 10.0.0.7)"));
}

#[tokio::test]
async fn every_record_type_is_matched_on_its_readable_form_case_insensitively() {
    let ns = stub_nameserver().await;
    for (record_type, expected) in [
        ("MX", "10 mail.ok.test"),
        ("TXT", "include:_spf.ok.test"),
        ("CNAME", "EDGE.cdn.test"),
        ("NS", "ns1.ok.test"),
    ] {
        let outcome = perform_check(&check_context(), &dns_monitor(ns, "ok.test", record_type, Some(expected))).await;
        assert_eq!(outcome.status, CheckStatus::Up, "{record_type}: {:?}", outcome.error);
    }
}

#[tokio::test]
async fn nxdomain_servfail_and_empty_answers_are_down() {
    let ns = stub_nameserver().await;
    let cases = [
        ("missing.test", "A", "NXDOMAIN: missing.test does not exist"),
        ("broken.test", "A", "DNS server answered SERVFAIL"),
        ("ok.test", "AAAA", "no AAAA records found for ok.test"),
    ];
    for (host, record_type, error) in cases {
        let outcome = perform_check(&check_context(), &dns_monitor(ns, host, record_type, None)).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{host} {record_type}");
        assert_eq!(outcome.error.as_deref(), Some(error));
    }
}

#[tokio::test]
async fn a_nameserver_that_never_answers_times_out() {
    let ns = stub_nameserver().await;
    let monitor = Monitor { timeout_seconds: 1, ..dns_monitor(ns, "silent.test", "A", None) };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &monitor).await;
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
}
```

API validation — `nami-engine/tests/monitor_types_api.rs` (Tasks 14 and 15 append to it):

```rust
// Create/edit validation of the Phase 2 monitor types.
// Requires `docker compose up -d postgres`.
mod common;

use axum::http::{HeaderValue, StatusCode};
use common::{call, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

struct Ctx {
    app: axum::Router,
    pool: sqlx::PgPool,
    project: Uuid,
    member: HeaderValue,
}

async fn ctx(pool: sqlx::PgPool) -> Ctx {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    Ctx { app, pool, project, member }
}

impl Ctx {
    async fn create(&self, body: Value) -> (StatusCode, Value) {
        call(&self.app, "POST", &format!("/projects/{}/monitors", self.project), Some(&self.member), Some(body)).await
    }

    async fn patch(&self, id: &str, body: Value) -> (StatusCode, Value) {
        let uri = format!("/projects/{}/monitors/{id}", self.project);
        call(&self.app, "PATCH", &uri, Some(&self.member), Some(body)).await
    }

    /// Every case must be a 400 whose message mentions its fragment, and
    /// none of them may write a monitor.
    async fn assert_rejected(&self, cases: Vec<(Value, &str)>) {
        for (body, fragment) in cases {
            let (status, json) = self.create(body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
            let message = json["error"].as_str().unwrap_or_default();
            assert!(message.contains(fragment), "{body}: {message:?} should mention {fragment:?}");
        }
        let count: i64 = sqlx::query_scalar("select count(*) from monitors where project_id = $1")
            .bind(self.project)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no invalid monitor was written");
    }
}

#[sqlx::test]
async fn ping_monitors_need_a_valid_host_and_keep_no_port(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, created) =
        c.create(json!({ "name": "Router", "check_type": "ping", "host": " Gateway.LAN ", "port": 22 })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["host"], "gateway.lan", "trimmed and lower-cased");
    assert_eq!(created["port"], Value::Null, "ping has no port");
    let (status, v6) = c.create(json!({ "name": "v6", "check_type": "ping", "host": "[2001:db8::1]" })).await;
    assert_eq!(status, StatusCode::CREATED, "{v6}");
    assert_eq!(v6["host"], "2001:db8::1");

    let c = ctx(c.pool.clone()).await;
    c.assert_rejected(vec![
        (json!({ "name": "x", "check_type": "ping" }), "host is required"),
        (json!({ "name": "x", "check_type": "ping", "host": "https://example.com" }), "host must be"),
        (json!({ "name": "x", "check_type": "ping", "host": "exa mple.com" }), "host must be"),
        (json!({ "name": "x", "check_type": "ping", "host": "example.com/path" }), "host must be"),
        (json!({ "name": "x", "check_type": "ping", "host": "a..b" }), "host must be"),
        (json!({ "name": "x", "check_type": "ping", "host": format!("{}.com", "a".repeat(64)) }), "host must be"),
    ])
    .await;
}

#[sqlx::test]
async fn dns_monitors_validate_record_type_nameserver_and_expected_value(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (status, created) = c
        .create(json!({
            "name": "DMARC", "check_type": "dns", "host": "_dmarc.example.com",
            "dns_record_type": "txt", "dns_nameserver": "1.1.1.1", "expected_value": " v=DMARC1 "
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["dns_record_type"], "TXT");
    assert_eq!(created["dns_nameserver"], "1.1.1.1:53", "the default port is made explicit");
    assert_eq!(created["expected_value"], "v=DMARC1");
    let (status, defaulted) = c.create(json!({ "name": "A", "check_type": "dns", "host": "example.com" })).await;
    assert_eq!(status, StatusCode::CREATED, "{defaulted}");
    assert_eq!(defaulted["dns_record_type"], "A", "A is the default record type");
    assert_eq!(defaulted["dns_nameserver"], Value::Null, "no nameserver = the system resolver");
    let (status, v6) = c
        .create(json!({ "name": "v6", "check_type": "dns", "host": "example.com", "dns_nameserver": "[2606:4700::1111]:5353" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v6}");
    assert_eq!(v6["dns_nameserver"], "[2606:4700::1111]:5353");

    let c = ctx(c.pool.clone()).await;
    let dns = |extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "dns", "host": "example.com" });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        body
    };
    c.assert_rejected(vec![
        (json!({ "name": "x", "check_type": "dns" }), "host is required"),
        (dns(json!({ "dns_record_type": "SOA" })), "dns_record_type must be one of A, AAAA, CNAME, MX, TXT, NS"),
        (dns(json!({ "dns_nameserver": "dns.google" })), "dns_nameserver must be an IP address"),
        (dns(json!({ "dns_nameserver": "1.1.1.1:0" })), "dns_nameserver must be an IP address"),
        (dns(json!({ "expected_value": "a\nb" })), "expected_value"),
        (dns(json!({ "expected_value": "a".repeat(1025) })), "expected_value"),
    ])
    .await;
}

#[sqlx::test]
async fn switching_away_from_dns_clears_its_fields(pool: sqlx::PgPool) {
    let c = ctx(pool).await;
    let (_, created) = c
        .create(json!({
            "name": "MX", "check_type": "dns", "host": "example.com", "dns_record_type": "MX",
            "dns_nameserver": "9.9.9.9", "expected_value": "mail"
        }))
        .await;
    let id = created["id"].as_str().unwrap();

    let (status, http) = c.patch(id, json!({ "check_type": "http", "url": "https://example.com" })).await;

    assert_eq!(status, StatusCode::OK, "{http}");
    for field in ["host", "dns_record_type", "dns_nameserver", "expected_value"] {
        assert_eq!(http[field], Value::Null, "{field} must be cleared");
    }
    let (status, cleared) = c.patch(id, json!({ "check_type": "dns", "host": "example.com" })).await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["dns_record_type"], "A");
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test checker_ping_dns --test monitor_types_api`
Expected: FAIL to compile — `struct Monitor has no field named dns_record_type`.

- [ ] **Step 4: Add the migration**

`nami-engine/migrations/0014_monitor_ping_dns.sql` (Phase 1 named the inline check `monitors_check_type_check`):

```sql
-- ping and dns monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in ('http', 'keyword', 'tcp', 'ping', 'dns'));

alter table monitors
    add column dns_record_type text
        check (dns_record_type in ('A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS')),
    add column dns_nameserver text,   -- "ip:port"; null = the system resolver
    add column expected_value text;   -- an answer must contain it (optional)
```

- [ ] **Step 5: Host and nameserver validation**

`nami-engine/src/models/target.rs` (with its unit tests):

```rust
//! Validation of the targets of non-http monitors.

use std::net::{IpAddr, SocketAddr};

/// A host name or IP literal: trimmed, lower-cased, at most 253 bytes. Names
/// are dot-separated labels of 1-63 letters, digits, `-` or `_` (`_` for
/// service names such as `_dmarc.example.com`) that do not start or end with
/// `-`, with an optional trailing dot. IPv6 literals may be written with or
/// without brackets. Schemes, paths, ports, whitespace and control
/// characters are rejected, so the value is safe to put into a URL path or a
/// DNS query.
pub fn validate_host(raw: &str) -> Result<String, String> {
    let host = raw.trim();
    let unbracketed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }
    let host = host.to_ascii_lowercase();
    let name = host.strip_suffix('.').unwrap_or(&host);
    let valid = !name.is_empty()
        && host.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    if valid {
        Ok(host)
    } else {
        Err("host must be a host name or an IP address (no scheme, path or port)".into())
    }
}

/// `IP`, `IP:port` or `[IPv6]:port` (port 53 when omitted), normalized to
/// the `SocketAddr` form the resolver needs.
pub fn parse_nameserver(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let addr = raw
        .parse::<SocketAddr>()
        .ok()
        .or_else(|| raw.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53)))
        .filter(|addr| addr.port() != 0)
        .ok_or_else(|| "dns_nameserver must be an IP address, optionally with a port (1.1.1.1 or 1.1.1.1:53)".to_string())?;
    Ok(addr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_normalized() {
        assert_eq!(validate_host(" Example.COM ").unwrap(), "example.com");
        assert_eq!(validate_host("example.com.").unwrap(), "example.com.");
        assert_eq!(validate_host("_dmarc.example.com").unwrap(), "_dmarc.example.com");
        assert_eq!(validate_host("localhost").unwrap(), "localhost");
        assert_eq!(validate_host("10.0.0.1").unwrap(), "10.0.0.1");
        assert_eq!(validate_host("[2001:DB8::1]").unwrap(), "2001:db8::1");
        assert_eq!(validate_host("2001:db8::1").unwrap(), "2001:db8::1");
    }

    #[test]
    fn anything_that_is_not_a_bare_host_is_rejected() {
        for raw in [
            "", " ", ".", "a..b", "-", "-a.test", "a-.test", "https://x.test", "x.test/path", "x.test:80", "a b",
            "x\n.test", "ä.test",
        ] {
            assert!(validate_host(raw).is_err(), "{raw:?} must be rejected");
        }
        assert!(validate_host(&format!("{}.test", "a".repeat(64))).is_err(), "labels are at most 63 bytes");
        assert!(validate_host(&"a.".repeat(127)).is_err(), "names are at most 253 bytes");
    }

    #[test]
    fn nameservers_get_an_explicit_port() {
        assert_eq!(parse_nameserver("1.1.1.1").unwrap(), "1.1.1.1:53");
        assert_eq!(parse_nameserver("127.0.0.1:5353").unwrap(), "127.0.0.1:5353");
        assert_eq!(parse_nameserver("::1").unwrap(), "[::1]:53");
        assert_eq!(parse_nameserver("[::1]:5353").unwrap(), "[::1]:5353");
        for raw in ["", "dns.google", "1.1.1.1:0", "1.1.1.1:99999", "1.1.1"] {
            assert!(parse_nameserver(raw).is_err(), "{raw:?} must be rejected");
        }
    }
}
```

`src/models/mod.rs`:

```diff
diff --git a/nami-engine/src/models/mod.rs b/nami-engine/src/models/mod.rs
--- a/nami-engine/src/models/mod.rs
+++ b/nami-engine/src/models/mod.rs
@@ -3,6 +3,7 @@ pub mod token;
 pub mod project;
 pub mod monitor;
 pub mod monitor_request;
+pub mod target;
 pub mod incident;
 pub mod heartbeat;
 
```

- [ ] **Step 6: Model and validation for the new types**

`src/models/monitor.rs`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -8,7 +8,9 @@ use super::{
         self, seal_headers, seal_password, HeaderView, HeadersChange, PasswordChange, RequestHeader, BODY_METHODS,
         HTTP_METHODS, MAX_BODY_BYTES,
     },
-    nullable, validate_name,
+    nullable,
+    target::{parse_nameserver, validate_host},
+    validate_name,
 };
 use crate::{
     crypto::{Cipher, SecretString},
@@ -58,8 +60,15 @@ pub struct Monitor {
     pub verify_ssl: bool,
     pub recovery_threshold: i32,
     pub consecutive_successes: i32,
+    pub dns_record_type: Option<String>,
+    pub dns_nameserver: Option<String>,
+    pub expected_value: Option<String>,
 }
 
+pub const CHECK_TYPES: [&str; 5] = ["http", "keyword", "tcp", "ping", "dns"];
+pub const DNS_RECORD_TYPES: [&str; 6] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"];
+pub const MAX_EXPECTED_VALUE_CHARS: usize = 1024;
+
 /// A monitor as every API response shows it: the secrets are reduced to
 /// `request_headers` (names, `value_set: true`) and `has_auth_password`.
 #[derive(Debug, Clone, PartialEq, Serialize)]
@@ -107,6 +116,9 @@ pub struct MonitorConfig {
     pub follow_redirects: bool,
     pub verify_ssl: bool,
     pub recovery_threshold: i32,
+    pub dns_record_type: Option<String>,
+    pub dns_nameserver: Option<String>,
+    pub expected_value: Option<String>,
 }
 
 impl From<&Monitor> for MonitorConfig {
@@ -133,6 +145,9 @@ impl From<&Monitor> for MonitorConfig {
             follow_redirects: m.follow_redirects,
             verify_ssl: m.verify_ssl,
             recovery_threshold: m.recovery_threshold,
+            dns_record_type: m.dns_record_type.clone(),
+            dns_nameserver: m.dns_nameserver.clone(),
+            expected_value: m.expected_value.clone(),
         }
     }
 }
@@ -159,6 +174,9 @@ impl MonitorConfig {
             || !matches!(self.auth_password, PasswordChange::Keep { .. })
             || self.follow_redirects != current.follow_redirects
             || self.verify_ssl != current.verify_ssl
+            || self.dns_record_type != current.dns_record_type
+            || self.dns_nameserver != current.dns_nameserver
+            || self.expected_value != current.expected_value
     }
 }
 
@@ -199,6 +217,9 @@ pub struct CreateMonitorRequest {
     pub verify_ssl: bool,
     #[serde(default = "default_recovery_threshold")]
     pub recovery_threshold: i32,
+    pub dns_record_type: Option<String>,
+    pub dns_nameserver: Option<String>,
+    pub expected_value: Option<String>,
 }
 
 fn default_http_method() -> String {
@@ -259,6 +280,9 @@ impl CreateMonitorRequest {
             follow_redirects: self.follow_redirects,
             verify_ssl: self.verify_ssl,
             recovery_threshold: self.recovery_threshold,
+            dns_record_type: self.dns_record_type,
+            dns_nameserver: self.dns_nameserver,
+            expected_value: self.expected_value,
         }
     }
 }
@@ -294,6 +318,11 @@ pub struct PatchMonitorRequest {
     pub follow_redirects: Option<bool>,
     pub verify_ssl: Option<bool>,
     pub recovery_threshold: Option<i32>,
+    pub dns_record_type: Option<String>,
+    #[serde(default, deserialize_with = "nullable")]
+    pub dns_nameserver: Option<Option<String>>,
+    #[serde(default, deserialize_with = "nullable")]
+    pub expected_value: Option<Option<String>>,
 }
 
 /// A nullable PATCH field applied to its current value.
@@ -338,6 +367,9 @@ impl PatchMonitorRequest {
             follow_redirects: self.follow_redirects.unwrap_or(base.follow_redirects),
             verify_ssl: self.verify_ssl.unwrap_or(base.verify_ssl),
             recovery_threshold: self.recovery_threshold.unwrap_or(base.recovery_threshold),
+            dns_record_type: self.dns_record_type.or(base.dns_record_type),
+            dns_nameserver: patched(self.dns_nameserver, base.dns_nameserver),
+            expected_value: patched(self.expected_value, base.expected_value),
         }
     }
 }
@@ -353,11 +385,11 @@ pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
     match c.check_type.as_str() {
         "http" | "keyword" => validate_http(&mut c)?,
         "tcp" => validate_tcp(&mut c)?,
-        _ => return Err("check_type must be http, keyword or tcp".into()),
-    }
-    if !matches!(c.check_type.as_str(), "http" | "keyword") {
-        clear_request_config(&mut c);
+        "ping" => validate_ping(&mut c)?,
+        "dns" => validate_dns(&mut c)?,
+        _ => return Err(format!("check_type must be one of {}", CHECK_TYPES.join(", "))),
     }
+    clear_unused_fields(&mut c);
     if c.interval_seconds < 30 {
         return Err("interval_seconds must be at least 30".into());
     }
@@ -387,8 +419,6 @@ fn validate_http(c: &mut MonitorConfig) -> Result<(), String> {
         return Err("url must be a valid http(s) URL".into());
     }
     c.url = Some(url);
-    c.host = None;
-    c.port = None;
     if !HTTP_METHODS.contains(&c.http_method.as_str()) {
         return Err("http_method must be one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS".into());
     }
@@ -399,8 +429,6 @@ fn validate_http(c: &mut MonitorConfig) -> Result<(), String> {
         if c.http_method == "HEAD" {
             return Err("keyword monitors cannot use HEAD: a HEAD response has no body to search (use GET)".into());
         }
-    } else {
-        c.keyword = None;
     }
     validate_request_config(c)
 }
@@ -443,6 +471,31 @@ fn validate_request_config(c: &mut MonitorConfig) -> Result<(), String> {
     Ok(())
 }
 
+/// Clears every type-specific field the check type does not use, so a
+/// monitor never keeps stale config from a type it had before.
+fn clear_unused_fields(c: &mut MonitorConfig) {
+    let check_type = c.check_type.clone();
+    let uses = |types: &[&str]| types.contains(&check_type.as_str());
+    if !uses(&["http", "keyword"]) {
+        c.url = None;
+        clear_request_config(c);
+    }
+    if !uses(&["keyword"]) {
+        c.keyword = None;
+    }
+    if !uses(&["tcp", "ping", "dns"]) {
+        c.host = None;
+    }
+    if !uses(&["tcp"]) {
+        c.port = None;
+    }
+    if !uses(&["dns"]) {
+        c.dns_record_type = None;
+        c.dns_nameserver = None;
+        c.expected_value = None;
+    }
+}
+
 /// Non-http types carry no request config. Secrets are only marked for
 /// clearing when one is stored or supplied, so editing such a monitor does
 /// not count as a check change (see `changes_check_of`).
@@ -469,11 +522,55 @@ fn validate_tcp(c: &mut MonitorConfig) -> Result<(), String> {
         return Err("port must be between 1 and 65535".into());
     }
     c.host = Some(host);
-    c.url = None;
-    c.keyword = None;
     Ok(())
 }
 
+/// The trimmed, validated `host` every host-based type requires.
+fn required_host(c: &MonitorConfig) -> Result<String, String> {
+    match c.host.as_deref().map(str::trim).filter(|host| !host.is_empty()) {
+        Some(host) => validate_host(host),
+        None => Err(format!("host is required for {} monitors", c.check_type)),
+    }
+}
+
+fn validate_ping(c: &mut MonitorConfig) -> Result<(), String> {
+    c.host = Some(required_host(c)?);
+    Ok(())
+}
+
+fn validate_dns(c: &mut MonitorConfig) -> Result<(), String> {
+    c.host = Some(required_host(c)?);
+    let record_type = c
+        .dns_record_type
+        .as_deref()
+        .map(|t| t.trim().to_ascii_uppercase())
+        .filter(|t| !t.is_empty())
+        .unwrap_or_else(|| "A".to_string());
+    if !DNS_RECORD_TYPES.contains(&record_type.as_str()) {
+        return Err(format!("dns_record_type must be one of {}", DNS_RECORD_TYPES.join(", ")));
+    }
+    c.dns_record_type = Some(record_type);
+    c.dns_nameserver = match c.dns_nameserver.as_deref().map(str::trim).filter(|ns| !ns.is_empty()) {
+        Some(raw) => Some(parse_nameserver(raw)?),
+        None => None,
+    };
+    c.expected_value = validate_expected_value(c.expected_value.take())?;
+    Ok(())
+}
+
+/// Optional text an answer must contain: trimmed, no control characters.
+fn validate_expected_value(raw: Option<String>) -> Result<Option<String>, String> {
+    let Some(value) = raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) else {
+        return Ok(None);
+    };
+    if value.chars().any(char::is_control) || value.chars().count() > MAX_EXPECTED_VALUE_CHARS {
+        return Err(format!(
+            "expected_value must be at most {MAX_EXPECTED_VALUE_CHARS} characters without control characters"
+        ));
+    }
+    Ok(Some(value))
+}
+
 pub async fn insert_monitor(
     pool: &PgPool,
     cipher: &Cipher,
@@ -534,7 +631,8 @@ async fn write_config(
              request_header_names = case when $21 then request_header_names else $22 end,
              request_headers_enc = case when $21 then request_headers_enc else $23 end,
              auth_password_enc = case when $24 then auth_password_enc else $25 end,
-             recovery_threshold = $26
+             recovery_threshold = $26,
+             dns_record_type = $27, dns_nameserver = $28, expected_value = $29
          where id = $1
          returning *",
     )
@@ -564,6 +662,9 @@ async fn write_config(
     .bind(keep_password)
     .bind(sealed_password)
     .bind(c.recovery_threshold)
+    .bind(&c.dns_record_type)
+    .bind(&c.dns_nameserver)
+    .bind(&c.expected_value)
     .fetch_one(&mut *conn)
     .await
 }
```

- [ ] **Step 7: The probes**

`nami-engine/src/uptime/ping.rs`:

```rust
//! `ping` checks: one ICMP echo request over an unprivileged datagram
//! socket (`SOCK_DGRAM` + `IPPROTO_ICMP`, no root or `CAP_NET_RAW`). On
//! Linux, and so in Docker, the engine's group must be inside
//! `net.ipv4.ping_group_range`.

use std::{io::ErrorKind, net::IpAddr, time::Duration};

use surge_ping::{Client, Config, PingIdentifier, PingSequence, SurgeError, ICMP};

use super::checker::Observation;
use crate::models::monitor::Monitor;

/// The classic 56 data bytes of `ping`.
const PAYLOAD: [u8; 56] = [0; 56];

pub(crate) async fn probe(monitor: &Monitor, timeout: Duration) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let ip = match resolve(host).await {
        Ok(ip) => ip,
        Err(message) => return Observation::down(None, None, message),
    };
    let kind = if ip.is_ipv4() { ICMP::V4 } else { ICMP::V6 };
    let client = match Client::new(&Config::builder().kind(kind).build()) {
        Ok(client) => client,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Observation::down(
                None,
                None,
                "ICMP sockets are not permitted for the engine (on Linux, add its group to net.ipv4.ping_group_range)"
                    .to_string(),
            );
        }
        Err(e) => return Observation::down(None, None, format!("cannot open an ICMP socket: {e}")),
    };
    let mut pinger = client.pinger(ip, PingIdentifier(rand::random())).await;
    // `perform_check`'s deadline is the one that counts: this only has to
    // be later than it.
    pinger.timeout(timeout + Duration::from_secs(1));
    match pinger.ping(PingSequence(0), &PAYLOAD).await {
        Ok((_reply, round_trip)) => Observation::up_after(round_trip),
        Err(SurgeError::Timeout { .. }) => {
            Observation::down(None, None, format!("timeout after {}s", timeout.as_secs()))
        }
        Err(e) => Observation::down(None, None, format!("ping failed: {e}")),
    }
}

/// `host` as an IP address: a literal as is, a name through the system
/// resolver, preferring IPv4.
async fn resolve(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addresses: Vec<IpAddr> = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .map(|addr| addr.ip())
        .collect();
    addresses
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(addresses.first())
        .copied()
        .ok_or_else(|| format!("could not resolve {host}: no addresses"))
}
```

`nami-engine/src/uptime/dns.rs`:

```rust
//! `dns` checks: resolve `host` for one record type, against
//! `dns_nameserver` or the system resolver, and optionally require an answer
//! that contains `expected_value` (case-insensitive).

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use hickory_resolver::{
    config::{NameServerConfig, ResolveHosts, ResolverConfig},
    net::{runtime::TokioRuntimeProvider, DnsError, NetError},
    proto::{
        op::ResponseCode,
        rr::{RData, RecordType},
    },
    TokioResolver,
};

use super::checker::{elapsed_ms, printable, Observation};
use crate::models::monitor::Monitor;

pub(crate) async fn probe(monitor: &Monitor, timeout: Duration, started: Instant) -> Observation {
    let (Some(host), Some(record_type)) = (monitor.host.as_deref(), monitor.dns_record_type.as_deref()) else {
        return Observation::down(None, None, "monitor has no host or record type".to_string());
    };
    let Ok(record_type) = record_type.parse::<RecordType>() else {
        return Observation::down(None, None, format!("unsupported record type {record_type:?}"));
    };
    let resolver = match build_resolver(monitor.dns_nameserver.as_deref(), timeout) {
        Ok(resolver) => resolver,
        Err(message) => return Observation::down(None, None, message),
    };
    // The trailing dot makes the name fully qualified: no search domains.
    let fqdn = if host.ends_with('.') { host.to_string() } else { format!("{host}.") };
    let host = host.trim_end_matches('.');
    let answers: Vec<String> = match resolver.lookup(fqdn.as_str(), record_type).await {
        Ok(lookup) => lookup
            .answers()
            .iter()
            .filter(|record| record.record_type() == record_type)
            .filter_map(|record| readable(&record.data))
            .collect(),
        Err(e) => {
            return Observation::down(None, Some(elapsed_ms(started)), describe_error(&e, host, record_type));
        }
    };
    if answers.is_empty() {
        return Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("no {record_type} records found for {host}"),
        );
    }
    if let Some(expected) = monitor.expected_value.as_deref() {
        let needle = expected.to_lowercase();
        if !answers.iter().any(|answer| answer.to_lowercase().contains(&needle)) {
            return Observation::down(
                None,
                Some(elapsed_ms(started)),
                format!(
                    "no {record_type} answer contains \"{expected}\" (got: {})",
                    printable(&answers.join(", "), 200)
                ),
            );
        }
    }
    Observation::up(None, started)
}

/// A fresh resolver per check (no cache, no hosts file): every check asks
/// the nameserver again. `perform_check`'s deadline is the one that counts,
/// so the resolver's own timeout only has to be later than it.
fn build_resolver(nameserver: Option<&str>, timeout: Duration) -> Result<TokioResolver, String> {
    let mut builder = match nameserver {
        Some(raw) => {
            let addr: SocketAddr = raw.parse().map_err(|_| format!("invalid nameserver {raw:?}"))?;
            let mut server = NameServerConfig::udp_and_tcp(addr.ip());
            for connection in &mut server.connections {
                connection.port = addr.port();
            }
            TokioResolver::builder_with_config(
                ResolverConfig::from_name_servers(vec![server]),
                TokioRuntimeProvider::default(),
            )
        }
        None => TokioResolver::builder_tokio()
            .map_err(|e| format!("cannot read the system DNS configuration: {e}"))?,
    };
    let options = builder.options_mut();
    options.timeout = timeout + Duration::from_secs(1);
    options.attempts = 1;
    options.cache_size = 0;
    options.use_hosts_file = ResolveHosts::Never;
    builder.build().map_err(|e| format!("cannot build a DNS resolver: {e}"))
}

/// One answer as the text `expected_value` is matched against: addresses as
/// written, names without the trailing dot, MX as "preference exchange",
/// TXT strings concatenated.
fn readable(data: &RData) -> Option<String> {
    let name = |name: &dyn std::fmt::Display| name.to_string().trim_end_matches('.').to_string();
    Some(match data {
        RData::A(a) => a.0.to_string(),
        RData::AAAA(aaaa) => aaaa.0.to_string(),
        RData::CNAME(cname) => name(&cname.0),
        RData::NS(ns) => name(&ns.0),
        RData::MX(mx) => format!("{} {}", mx.preference, name(&mx.exchange)),
        RData::TXT(txt) => txt.txt_data.iter().map(|part| String::from_utf8_lossy(part)).collect(),
        _ => return None,
    })
}

fn describe_error(error: &NetError, host: &str, record_type: RecordType) -> String {
    if error.is_nx_domain() {
        return format!("NXDOMAIN: {host} does not exist");
    }
    if error.is_no_records_found() {
        return format!("no {record_type} records found for {host}");
    }
    match error {
        NetError::Dns(DnsError::ResponseCode(code)) => format!("DNS server answered {}", response_code_name(*code)),
        NetError::Timeout => "DNS query timed out".to_string(),
        other => format!("DNS lookup failed: {}", printable(&other.to_string(), 200)),
    }
}

fn response_code_name(code: ResponseCode) -> String {
    match code {
        ResponseCode::ServFail => "SERVFAIL".to_string(),
        ResponseCode::Refused => "REFUSED".to_string(),
        ResponseCode::FormErr => "FORMERR".to_string(),
        ResponseCode::NotImp => "NOTIMP".to_string(),
        other => other.to_str().to_string(),
    }
}
```

`src/uptime/mod.rs`:

```diff
diff --git a/nami-engine/src/uptime/mod.rs b/nami-engine/src/uptime/mod.rs
--- a/nami-engine/src/uptime/mod.rs
+++ b/nami-engine/src/uptime/mod.rs
@@ -1,9 +1,11 @@
-//! Uptime monitoring: performing checks (`checker`), storing results in
-//! ClickHouse (`store`), the background worker and its state machine
-//! (`worker`), heartbeat pings and the overdue sweep (`heartbeat`), and
-//! alert delivery (`alerts`).
+//! Uptime monitoring: performing checks (`checker`, with one module per
+//! non-http probe), storing results in ClickHouse (`store`), the background
+//! worker and its state machine (`worker`), heartbeat pings and the overdue
+//! sweep (`heartbeat`), and alert delivery (`alerts`).
 pub mod alerts;
 pub mod checker;
+pub mod dns;
 pub mod heartbeat;
+pub mod ping;
 pub mod store;
 pub mod worker;
```

`src/uptime/checker.rs` — dispatch, and `Observation` shared with the probe modules:

```diff
diff --git a/nami-engine/src/uptime/checker.rs b/nami-engine/src/uptime/checker.rs
--- a/nami-engine/src/uptime/checker.rs
+++ b/nami-engine/src/uptime/checker.rs
@@ -185,6 +185,8 @@ pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcom
         match monitor.check_type.as_str() {
             "http" | "keyword" => probe_http(ctx, monitor, timeout, started).await,
             "tcp" => probe_tcp(monitor, started).await,
+            "ping" => super::ping::probe(monitor, timeout).await,
+            "dns" => super::dns::probe(monitor, timeout, started).await,
             other => Observation::down(None, None, format!("unsupported check type {other:?}")),
         }
     };
@@ -201,26 +203,47 @@ pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcom
     }
 }
 
-struct Observation {
-    status_code: Option<u16>,
-    response_time_ms: Option<u32>,
-    error: Option<String>,
+/// What one probe saw; `perform_check` turns it into a [`CheckOutcome`]
+/// (up exactly when `error` is `None`).
+pub(crate) struct Observation {
+    pub(crate) status_code: Option<u16>,
+    pub(crate) response_time_ms: Option<u32>,
+    pub(crate) error: Option<String>,
 }
 
 impl Observation {
-    fn up(status_code: Option<u16>, started: Instant) -> Self {
+    pub(crate) fn up(status_code: Option<u16>, started: Instant) -> Self {
         Self { status_code, response_time_ms: Some(elapsed_ms(started)), error: None }
     }
 
-    fn down(status_code: Option<u16>, response_time_ms: Option<u32>, error: String) -> Self {
+    /// Up with a response time the probe measured itself (ping's RTT).
+    pub(crate) fn up_after(response_time: Duration) -> Self {
+        let ms = u32::try_from(response_time.as_millis()).unwrap_or(u32::MAX);
+        Self { status_code: None, response_time_ms: Some(ms), error: None }
+    }
+
+    pub(crate) fn down(status_code: Option<u16>, response_time_ms: Option<u32>, error: String) -> Self {
         Self { status_code, response_time_ms, error: Some(error) }
     }
 }
 
-fn elapsed_ms(started: Instant) -> u32 {
+pub(crate) fn elapsed_ms(started: Instant) -> u32 {
     u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
 }
 
+/// Text from the monitored service (DNS answers, greetings) made safe for
+/// check history and alert emails: control characters dropped, at most
+/// `max_chars` characters kept.
+pub(crate) fn printable(text: &str, max_chars: usize) -> String {
+    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
+    if clean.chars().count() <= max_chars {
+        return clean;
+    }
+    let mut cut: String = clean.chars().take(max_chars).collect();
+    cut.push('…');
+    cut
+}
+
 async fn probe_http(ctx: &CheckContext, monitor: &Monitor, timeout: Duration, started: Instant) -> Observation {
     let Some(url) = monitor.url.as_deref() else {
         return Observation::down(None, None, "monitor has no url".to_string());
```

- [ ] **Step 8: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `checker_ping_dns.rs` 7 passed, `monitor_types_api.rs` 3 passed, the library's `models::target` unit tests 3 passed, the rest green. A `ping failed: ... permission denied` / `ICMP sockets are not permitted` failure on Linux means the runner's group is outside `net.ipv4.ping_group_range` (Step 1).

- [ ] **Step 9: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/.env.example \
  nami-engine/migrations/0014_monitor_ping_dns.sql nami-engine/src/models/mod.rs nami-engine/src/models/target.rs \
  nami-engine/src/models/monitor.rs nami-engine/src/uptime/mod.rs nami-engine/src/uptime/checker.rs \
  nami-engine/src/uptime/ping.rs nami-engine/src/uptime/dns.rs \
  nami-engine/tests/checker_ping_dns.rs nami-engine/tests/monitor_types_api.rs
git commit -m "feat(engine): ping and dns monitors (NAMI-UP-13)

Ping over an unprivileged ICMP datagram socket (RTT as response time);
DNS A/AAAA/CNAME/MX/TXT/NS with an optional nameserver and expected value.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14 (NAMI-UP-14): `ssl_expiry` and `domain_expiry` monitors

Branch `feat/NAMI-UP-14-expiry-monitors`, worktree `.worktrees/NAMI-UP-14/`.

**Files:**
- Create: `nami-engine/migrations/0015_monitor_expiry_checks.sql`, `nami-engine/src/uptime/tls.rs`, `nami-engine/src/uptime/expiry.rs`
- Modify: `nami-engine/Cargo.toml`, `nami-engine/.env.example`, `nami-engine/src/config.rs`, `nami-engine/src/models/monitor.rs`, `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/checker.rs`
- Modify (tests): `nami-engine/tests/common/tls.rs`, `nami-engine/tests/monitor_types_api.rs`
- Test: `nami-engine/tests/checker_expiry.rs`

**Interfaces:**
- Consumes: `checker::{CheckContext, HttpClients::select, Observation, elapsed_ms}` (Tasks 11, 13); `models::target::validate_host` (Task 13); `common::tls::{serve_https_ok, self_signed, provider}` (Task 11).
- Produces:
  - Column `expiry_threshold_days int check (between 1 and 365)`; constraint `monitors_expiry_interval_check` (expiry types need `interval_seconds >= 3600`); `check_type` accepts `ssl_expiry`, `domain_expiry`.
  - `Config.rdap_base_url: String` (`RDAP_BASE_URL`, default `checker::DEFAULT_RDAP_BASE_URL = "https://rdap.org"`).
  - `CheckContext` gains `pub tls: Arc<rustls::ClientConfig>` (webpki roots by default) and `pub rdap_base_url: String`; `CheckContext::new(cipher)` fills defaults, `for_state` takes the URL from config.
  - `uptime::tls::{provider() -> Arc<CryptoProvider>, client_config(RootCertStore) -> Arc<rustls::ClientConfig>, webpki_client_config() -> Arc<rustls::ClientConfig>}` (public, for tests and Task 15), `pub(crate) connect(config, host, port) -> Result<TlsStream<TcpStream>, String>`, `pub(crate) days(i64) -> String`, `pub(crate) date_of(i64) -> String`.
  - `uptime::expiry::{DEFAULT_SSL_THRESHOLD_DAYS = 14, DEFAULT_DOMAIN_THRESHOLD_DAYS = 30}`, `pub(crate) probe_ssl`, `pub(crate) probe_domain`.
  - `checker::{read_capped, describe_request_error, describe_io_error}` become `pub(crate)`.
  - `Monitor`/`MonitorConfig.expiry_threshold_days: Option<i32>`; `CreateMonitorRequest.interval_seconds` becomes `Option<i32>` (default 86400 for the expiry types, 60 otherwise).
  - Test helper `common::tls::TestCa { cert }` with `TestCa::new()`, `leaf(not_after: OffsetDateTime) -> CertAndKey`, `roots() -> RootCertStore`.

- [ ] **Step 1: Promote the TLS crates and add the parsers**

```bash
cd nami-engine
cargo remove --dev rustls tokio-rustls
cargo add rustls@0.23.45 tokio-rustls@0.26.6 webpki-roots@1.0.9 x509-parser@0.18.1
```

- [ ] **Step 2: Write the failing tests**

A test CA for certificates with chosen lifetimes — `tests/common/tls.rs`:

```diff
diff --git a/nami-engine/tests/common/tls.rs b/nami-engine/tests/common/tls.rs
--- a/nami-engine/tests/common/tls.rs
+++ b/nami-engine/tests/common/tls.rs
@@ -13,6 +13,40 @@ pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
     Arc::new(rustls::crypto::aws_lc_rs::default_provider())
 }
 
+/// A throwaway certificate authority. Its root goes into the checker's
+/// root store (`CheckContext::tls`), so the leaves it issues are trusted.
+pub struct TestCa {
+    pub cert: CertificateDer<'static>,
+    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
+}
+
+impl TestCa {
+    pub fn new() -> TestCa {
+        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
+        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
+        params.distinguished_name.push(rcgen::DnType::CommonName, "Nami Test CA");
+        let key = rcgen::KeyPair::generate().unwrap();
+        let cert = params.self_signed(&key).unwrap();
+        TestCa { cert: cert.der().clone(), issuer: rcgen::Issuer::new(params, key) }
+    }
+
+    /// A `localhost` certificate valid from 30 days ago until `not_after`.
+    pub fn leaf(&self, not_after: time::OffsetDateTime) -> CertAndKey {
+        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
+        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(30);
+        params.not_after = not_after;
+        let key = rcgen::KeyPair::generate().unwrap();
+        let cert = params.signed_by(&key, &self.issuer).unwrap();
+        (cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
+    }
+
+    pub fn roots(&self) -> rustls::RootCertStore {
+        let mut roots = rustls::RootCertStore::empty();
+        roots.add(self.cert.clone()).unwrap();
+        roots
+    }
+}
+
 /// A self-signed `localhost` certificate that no root store trusts.
 pub fn self_signed() -> CertAndKey {
     let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
```

`nami-engine/tests/checker_expiry.rs`:

```rust
// `ssl_expiry` (local TLS servers with certificates from a test CA) and
// `domain_expiry` (wiremock standing in for rdap.org) checks.
mod common;

use chrono::{DateTime, Utc};
use common::{check_context, tls, unsaved_monitor};
use nami_engine::{
    models::monitor::Monitor,
    uptime::{
        checker::{perform_check, CheckContext, CheckOutcome, CheckStatus},
        tls::client_config,
    },
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use wiremock::{
    matchers::{headers, method, path},
    Mock, MockServer, ResponseTemplate,
};

/// The date the error message shows for an instant `days` (+1 h) from now.
fn date_in(days: i64) -> String {
    (Utc::now() + chrono::Duration::days(days) + chrono::Duration::hours(1)).format("%Y-%m-%d").to_string()
}

/// A TLS server whose `localhost` certificate (from `ca`) expires `days`
/// days and one hour from now. Returns its port.
async fn tls_server(ca: &tls::TestCa, days: i64) -> u16 {
    let not_after = OffsetDateTime::now_utc() + Duration::days(days) + Duration::hours(1);
    tls::serve_https_ok(ca.leaf(not_after)).await
}

async fn check_ssl(ca: &tls::TestCa, host: &str, port: u16, threshold: i32) -> CheckOutcome {
    let ctx = CheckContext { tls: client_config(ca.roots()), ..check_context() };
    let monitor = Monitor {
        host: Some(host.to_string()),
        port: Some(i32::from(port)),
        expiry_threshold_days: Some(threshold),
        ..unsaved_monitor("ssl_expiry")
    };
    perform_check(&ctx, &monitor).await
}

#[tokio::test]
async fn a_certificate_far_from_expiry_is_up() {
    let ca = tls::TestCa::new();
    let port = tls_server(&ca, 60).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
}

#[tokio::test]
async fn a_certificate_inside_the_threshold_is_down_and_says_how_long_is_left() {
    let ca = tls::TestCa::new();
    let port = tls_server(&ca, 5).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error, Some(format!("certificate expires in 5 days ({})", date_in(5))));
}

#[tokio::test]
async fn an_expired_certificate_is_down_and_says_since_when() {
    let ca = tls::TestCa::new();
    let not_after = OffsetDateTime::now_utc() - Duration::days(3) - Duration::hours(1);
    let port = tls::serve_https_ok(ca.leaf(not_after)).await;
    let expired_on = (Utc::now() - chrono::Duration::days(3) - chrono::Duration::hours(1)).format("%Y-%m-%d");
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error, Some(format!("certificate expired 3 days ago ({expired_on})")));
}

#[tokio::test]
async fn untrusted_and_mismatched_certificates_are_down() {
    let ca = tls::TestCa::new();
    let untrusted = tls::serve_https_ok(tls::self_signed()).await;
    let outcome = check_ssl(&ca, "localhost", untrusted, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("certificate is not trusted (unknown issuer)"));

    // A valid certificate for `localhost`, reached as 127.0.0.1.
    let port = tls_server(&ca, 60).await;
    let outcome = check_ssl(&ca, "127.0.0.1", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(outcome.error.as_deref().unwrap().contains("not valid for name"), "{:?}", outcome.error);
}

#[tokio::test]
async fn nothing_listening_is_down() {
    let ca = tls::TestCa::new();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let outcome = check_ssl(&ca, "localhost", closed, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

fn rdap_body(expires: Option<DateTime<Utc>>) -> serde_json::Value {
    let mut events = vec![json!({ "eventAction": "registration", "eventDate": "1995-08-14T04:00:00Z" })];
    if let Some(expires) = expires {
        events.push(json!({ "eventAction": "expiration", "eventDate": expires.to_rfc3339() }));
    }
    json!({ "objectClassName": "domain", "ldhName": "EXAMPLE.COM", "events": events })
}

async fn check_domain(rdap: &MockServer, threshold: i32) -> CheckOutcome {
    let ctx = CheckContext { rdap_base_url: rdap.uri(), ..check_context() };
    let monitor = Monitor {
        host: Some("example.com".to_string()),
        expiry_threshold_days: Some(threshold),
        ..unsaved_monitor("domain_expiry")
    };
    perform_check(&ctx, &monitor).await
}

async fn rdap_answering(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domain/example.com"))
        // wiremock splits header values on commas.
        .and(headers("accept", vec!["application/rdap+json", "application/json"]))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_domain_far_from_expiry_is_up() {
    let expires = Utc::now() + chrono::Duration::days(400);
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(expires)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn a_domain_inside_the_threshold_or_expired_is_down() {
    let soon = Utc::now() + chrono::Duration::days(10) + chrono::Duration::hours(1);
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(soon)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error, Some(format!("domain expires in 10 days ({})", date_in(10))));

    let gone = Utc::now() - chrono::Duration::days(2) - chrono::Duration::hours(1);
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(gone)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error, Some(format!("domain expired 2 days ago ({})", gone.format("%Y-%m-%d"))));
}

#[tokio::test]
async fn the_rdap_redirect_to_the_registry_is_followed() {
    let rdap = MockServer::start().await;
    Mock::given(path("/domain/example.com"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", format!("{}/registry/domain/example.com", rdap.uri())),
        )
        .mount(&rdap)
        .await;
    let expires = Utc::now() + chrono::Duration::days(400);
    Mock::given(path("/registry/domain/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(rdap_body(Some(expires))))
        .mount(&rdap)
        .await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn rdap_failures_and_answers_without_an_expiration_are_down() {
    let cases = [
        (ResponseTemplate::new(200).set_body_json(rdap_body(None)), "RDAP response has no expiration date"),
        (ResponseTemplate::new(404), "RDAP lookup failed: HTTP 404"),
        (ResponseTemplate::new(200).set_body_string("<html>"), "RDAP response is not valid JSON"),
    ];
    for (template, error) in cases {
        let rdap = rdap_answering(template).await;
        let outcome = check_domain(&rdap, 30).await;
        assert_eq!(outcome.status, CheckStatus::Down);
        assert_eq!(outcome.error.as_deref(), Some(error));
    }
}
```

Append to `nami-engine/tests/monitor_types_api.rs`:

```diff
diff --git a/nami-engine/tests/monitor_types_api.rs b/nami-engine/tests/monitor_types_api.rs
--- a/nami-engine/tests/monitor_types_api.rs
+++ b/nami-engine/tests/monitor_types_api.rs
@@ -134,3 +134,45 @@ async fn switching_away_from_dns_clears_its_fields(pool: sqlx::PgPool) {
     assert_eq!(status, StatusCode::OK, "{cleared}");
     assert_eq!(cleared["dns_record_type"], "A");
 }
+
+#[sqlx::test]
+async fn expiry_monitors_get_daily_checks_and_type_specific_thresholds(pool: sqlx::PgPool) {
+    let c = ctx(pool).await;
+    let (status, ssl) = c.create(json!({ "name": "TLS", "check_type": "ssl_expiry", "host": "example.com" })).await;
+    assert_eq!(status, StatusCode::CREATED, "{ssl}");
+    assert_eq!(ssl["port"], 443);
+    assert_eq!(ssl["expiry_threshold_days"], 14);
+    assert_eq!(ssl["interval_seconds"], 86400);
+    let (status, domain) =
+        c.create(json!({ "name": "Domain", "check_type": "domain_expiry", "host": "Example.COM." })).await;
+    assert_eq!(status, StatusCode::CREATED, "{domain}");
+    assert_eq!(domain["host"], "example.com", "lower-cased, trailing dot dropped");
+    assert_eq!(domain["expiry_threshold_days"], 30);
+    assert_eq!(domain["interval_seconds"], 86400);
+    assert_eq!(domain["port"], Value::Null);
+    let (status, http) = c.create(json!({ "name": "Web", "check_type": "http", "url": "https://example.com" })).await;
+    assert_eq!(status, StatusCode::CREATED, "{http}");
+    assert_eq!(http["interval_seconds"], 60, "other types keep the 60 s default");
+    assert_eq!(http["expiry_threshold_days"], Value::Null);
+
+    let c = ctx(c.pool.clone()).await;
+    c.assert_rejected(vec![
+        (
+            json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "interval_seconds": 600 }),
+            "interval_seconds must be at least 3600",
+        ),
+        (
+            json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "expiry_threshold_days": 0 }),
+            "expiry_threshold_days must be between 1 and 365",
+        ),
+        (
+            json!({ "name": "x", "check_type": "domain_expiry", "host": "example.com", "expiry_threshold_days": 366 }),
+            "expiry_threshold_days must be between 1 and 365",
+        ),
+        (json!({ "name": "x", "check_type": "ssl_expiry", "host": "example.com", "port": 70000 }), "port"),
+        (json!({ "name": "x", "check_type": "domain_expiry", "host": "93.184.216.34" }), "domain name"),
+        (json!({ "name": "x", "check_type": "domain_expiry", "host": "localhost" }), "domain name"),
+        (json!({ "name": "x", "check_type": "domain_expiry" }), "host is required"),
+    ])
+    .await;
+}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && cargo test --test checker_expiry`
Expected: FAIL to compile — `could not find tls in uptime` / `struct CheckContext has no field named tls`.

- [ ] **Step 4: Add the migration**

`nami-engine/migrations/0015_monitor_expiry_checks.sql`:

```sql
-- ssl_expiry and domain_expiry monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in ('http', 'keyword', 'tcp', 'ping', 'dns', 'ssl_expiry', 'domain_expiry'));

alter table monitors
    add column expiry_threshold_days int
        check (expiry_threshold_days between 1 and 365),
    -- Expiry dates move slowly: these types are checked at most hourly.
    add constraint monitors_expiry_interval_check
        check (check_type not in ('ssl_expiry', 'domain_expiry') or interval_seconds >= 3600);
```

- [ ] **Step 5: TLS helpers and the expiry probes**

`nami-engine/src/uptime/tls.rs` (rustls is built with both the ring and aws-lc-rs providers here, so every config names aws-lc-rs explicitly):

```rust
//! TLS for the non-HTTP checks (`ssl_expiry`, and mail servers with
//! `use_tls`): rustls with SNI, verified against `CheckContext::tls`.

use std::{io, sync::Arc};

use rustls::{pki_types::ServerName, CertificateError, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

use super::checker::describe_io_error;

/// rustls is built with both the ring and aws-lc-rs providers in this
/// workspace (reqwest 0.12, pulled in by oauth2, brings ring), so configs
/// always name their provider instead of relying on a process default.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A client config trusting exactly `roots`.
pub fn client_config(roots: RootCertStore) -> Arc<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("the aws-lc-rs provider supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// The production config: the Mozilla root set from `webpki-roots`.
pub fn webpki_client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    client_config(roots)
}

/// Opens a TCP connection to `host:port` and completes a TLS handshake
/// with `host` as SNI and verification name. The error is ready for check
/// history.
pub(crate) async fn connect(
    config: Arc<rustls::ClientConfig>,
    host: &str,
    port: u16,
) -> Result<TlsStream<TcpStream>, String> {
    let server_name =
        ServerName::try_from(host.to_string()).map_err(|_| format!("{host} is not a valid TLS server name"))?;
    let tcp = TcpStream::connect((host, port)).await.map_err(|e| describe_io_error(&e))?;
    TlsConnector::from(config).connect(server_name, tcp).await.map_err(|e| describe_tls_error(&e))
}

/// A handshake failure in words; certificate problems say what is wrong.
fn describe_tls_error(error: &io::Error) -> String {
    let rustls_error = error.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match rustls_error {
        Some(rustls::Error::InvalidCertificate(CertificateError::ExpiredContext { time, not_after })) => {
            let days_ago = (time.as_secs().saturating_sub(not_after.as_secs()) / 86_400) as i64;
            let date = date_of(not_after.as_secs() as i64);
            format!("certificate expired {} ago ({date})", days(days_ago))
        }
        Some(rustls::Error::InvalidCertificate(CertificateError::Expired)) => "certificate expired".to_string(),
        Some(rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer)) => {
            "certificate is not trusted (unknown issuer)".to_string()
        }
        Some(rustls::Error::InvalidCertificate(other)) => format!("certificate is not valid: {other}"),
        Some(other) => format!("TLS handshake failed: {other}"),
        None => format!("TLS handshake failed: {}", describe_io_error(error)),
    }
}

/// "1 day", "5 days".
pub(crate) fn days(count: i64) -> String {
    if count == 1 { "1 day".to_string() } else { format!("{count} days") }
}

/// `YYYY-MM-DD` (UTC) of a Unix timestamp.
pub(crate) fn date_of(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown date".to_string())
}
```

`nami-engine/src/uptime/expiry.rs`:

```rust
//! `ssl_expiry` (the leaf certificate of a TLS handshake) and
//! `domain_expiry` (the RDAP `expiration` event) checks. Both go down when
//! the expiry is less than `expiry_threshold_days` away, and say how long is
//! left.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{
    checker::{describe_request_error, elapsed_ms, read_capped, CheckContext, Observation, MAX_BODY_BYTES},
    tls::{self, date_of, days},
};
use crate::models::monitor::Monitor;

pub const DEFAULT_SSL_THRESHOLD_DAYS: i32 = 14;
pub const DEFAULT_DOMAIN_THRESHOLD_DAYS: i32 = 30;

pub(crate) async fn probe_ssl(ctx: &CheckContext, monitor: &Monitor, started: Instant) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let Some(port) = monitor.port.and_then(|port| u16::try_from(port).ok()) else {
        return Observation::down(None, None, "monitor has no valid port".to_string());
    };
    let stream = match tls::connect(ctx.tls.clone(), host, port).await {
        Ok(stream) => stream,
        Err(message) => return Observation::down(None, None, message),
    };
    let response_time = elapsed_ms(started);
    let (_, connection) = stream.get_ref();
    let Some(leaf) = connection.peer_certificates().and_then(|chain| chain.first()) else {
        return Observation::down(None, Some(response_time), "the server sent no certificate".to_string());
    };
    let not_after = match x509_parser::parse_x509_certificate(leaf) {
        Ok((_, certificate)) => certificate.validity().not_after.timestamp(),
        Err(_) => {
            return Observation::down(None, Some(response_time), "could not parse the server certificate".to_string());
        }
    };
    let threshold = monitor.expiry_threshold_days.unwrap_or(DEFAULT_SSL_THRESHOLD_DAYS);
    judge("certificate", not_after, threshold, response_time)
}

pub(crate) async fn probe_domain(
    ctx: &CheckContext,
    monitor: &Monitor,
    timeout: Duration,
    started: Instant,
) -> Observation {
    let Some(domain) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no domain".to_string());
    };
    let url = match rdap_url(&ctx.rdap_base_url, domain) {
        Ok(url) => url,
        Err(message) => return Observation::down(None, None, message),
    };
    // rdap.org answers with a redirect to the registry's RDAP server: follow
    // it (no custom headers, certificates verified).
    let client = ctx.http.select(true, true, false);
    let request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/rdap+json, application/json")
        .timeout(timeout);
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            return Observation::down(None, None, format!("RDAP lookup failed: {}", describe_request_error(&e, timeout)));
        }
    };
    let code = response.status().as_u16();
    if code != 200 {
        return Observation::down(Some(code), Some(elapsed_ms(started)), format!("RDAP lookup failed: HTTP {code}"));
    }
    let body = match read_capped(&mut response, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(e) => {
            return Observation::down(
                Some(code),
                None,
                format!("RDAP lookup failed: {}", describe_request_error(&e, timeout)),
            );
        }
    };
    let response_time = elapsed_ms(started);
    let Ok(rdap) = serde_json::from_slice::<RdapDomain>(&body) else {
        return Observation::down(Some(code), Some(response_time), "RDAP response is not valid JSON".to_string());
    };
    let expiration = rdap
        .events
        .iter()
        .filter(|event| event.action.eq_ignore_ascii_case("expiration"))
        .find_map(|event| event.date.as_deref())
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok());
    let Some(expiration) = expiration else {
        return Observation::down(Some(code), Some(response_time), "RDAP response has no expiration date".to_string());
    };
    let threshold = monitor.expiry_threshold_days.unwrap_or(DEFAULT_DOMAIN_THRESHOLD_DAYS);
    let mut observation = judge("domain", expiration.timestamp(), threshold, response_time);
    observation.status_code = Some(code);
    observation
}

/// The RDAP domain object: only its events matter here.
#[derive(Deserialize)]
struct RdapDomain {
    #[serde(default)]
    events: Vec<RdapEvent>,
}

#[derive(Deserialize)]
struct RdapEvent {
    #[serde(rename = "eventAction")]
    action: String,
    #[serde(rename = "eventDate")]
    date: Option<String>,
}

/// `{base}/domain/{domain}`, the domain pushed as one percent-encoded path
/// segment.
fn rdap_url(base: &str, domain: &str) -> Result<reqwest::Url, String> {
    let invalid = || "RDAP_BASE_URL is not a valid http(s) URL".to_string();
    let mut url = reqwest::Url::parse(base).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    url.path_segments_mut().map_err(|_| invalid())?.pop_if_empty().push("domain").push(domain);
    Ok(url)
}

/// Up unless `expires_at` (Unix seconds) is less than `threshold_days`
/// away; the error names the subject, the days left (or since) and the date.
fn judge(subject: &str, expires_at: i64, threshold_days: i32, response_time: u32) -> Observation {
    let seconds_left = expires_at - Utc::now().timestamp();
    if seconds_left >= i64::from(threshold_days) * 86_400 {
        return Observation { status_code: None, response_time_ms: Some(response_time), error: None };
    }
    let date = date_of(expires_at);
    let error = if seconds_left < 0 {
        format!("{subject} expired {} ago ({date})", days((-seconds_left) / 86_400))
    } else {
        format!("{subject} expires in {} ({date})", days(seconds_left / 86_400))
    };
    Observation::down(None, Some(response_time), error)
}
```

`src/uptime/mod.rs`:

```diff
diff --git a/nami-engine/src/uptime/mod.rs b/nami-engine/src/uptime/mod.rs
--- a/nami-engine/src/uptime/mod.rs
+++ b/nami-engine/src/uptime/mod.rs
@@ -5,7 +5,9 @@
 pub mod alerts;
 pub mod checker;
 pub mod dns;
+pub mod expiry;
 pub mod heartbeat;
 pub mod ping;
 pub mod store;
+pub mod tls;
 pub mod worker;
```

`src/uptime/checker.rs` — context fields, dispatch, shared helpers:

```diff
diff --git a/nami-engine/src/uptime/checker.rs b/nami-engine/src/uptime/checker.rs
--- a/nami-engine/src/uptime/checker.rs
+++ b/nami-engine/src/uptime/checker.rs
@@ -2,7 +2,10 @@
 //! refused connection, wrong status, missing keyword) becomes a `Down`
 //! outcome with a short human-readable `error`.
 
-use std::time::{Duration, Instant};
+use std::{
+    sync::Arc,
+    time::{Duration, Instant},
+};
 
 use chrono::{DateTime, Utc};
 use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
@@ -45,6 +48,9 @@ pub const MAX_REDIRECTS: usize = 10;
 pub const CONFIG_DECRYPT_ERROR: &str =
     "request configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)";
 
+/// Where `domain_expiry` checks start their RDAP lookup (`RDAP_BASE_URL`).
+pub const DEFAULT_RDAP_BASE_URL: &str = "https://rdap.org";
+
 /// What every check of one worker pass shares. Built once per pass: cheap
 /// next to the checks themselves, and its connections never outlive the
 /// runtime that drives them.
@@ -52,15 +58,23 @@ pub const CONFIG_DECRYPT_ERROR: &str =
 pub struct CheckContext {
     pub http: HttpClients,
     pub cipher: Cipher,
+    /// Trust roots for `ssl_expiry` and mail TLS (tests inject their own CA).
+    pub tls: Arc<rustls::ClientConfig>,
+    pub rdap_base_url: String,
 }
 
 impl CheckContext {
     pub fn new(cipher: Cipher) -> CheckContext {
-        CheckContext { http: HttpClients::new(), cipher }
+        CheckContext {
+            http: HttpClients::new(),
+            cipher,
+            tls: super::tls::webpki_client_config(),
+            rdap_base_url: DEFAULT_RDAP_BASE_URL.to_string(),
+        }
     }
 
     pub fn for_state(state: &AppState) -> CheckContext {
-        CheckContext::new(state.cipher.clone())
+        CheckContext { rdap_base_url: state.config.rdap_base_url.clone(), ..CheckContext::new(state.cipher.clone()) }
     }
 }
 
@@ -187,6 +201,8 @@ pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcom
             "tcp" => probe_tcp(monitor, started).await,
             "ping" => super::ping::probe(monitor, timeout).await,
             "dns" => super::dns::probe(monitor, timeout, started).await,
+            "ssl_expiry" => super::expiry::probe_ssl(ctx, monitor, started).await,
+            "domain_expiry" => super::expiry::probe_domain(ctx, monitor, timeout, started).await,
             other => Observation::down(None, None, format!("unsupported check type {other:?}")),
         }
     };
@@ -332,7 +348,7 @@ fn build_request(
     Ok(request)
 }
 
-async fn read_capped(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, reqwest::Error> {
+pub(crate) async fn read_capped(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, reqwest::Error> {
     let mut body = Vec::new();
     while let Some(chunk) = response.chunk().await? {
         let remaining = cap - body.len();
@@ -359,7 +375,7 @@ async fn probe_tcp(monitor: &Monitor, started: Instant) -> Observation {
     }
 }
 
-fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
+pub(crate) fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
     if error.is_timeout() {
         return format!("timeout after {}s", timeout.as_secs());
     }
@@ -375,7 +391,7 @@ fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
     }
 }
 
-fn describe_io_error(error: &std::io::Error) -> String {
+pub(crate) fn describe_io_error(error: &std::io::Error) -> String {
     match error.kind() {
         std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
         std::io::ErrorKind::ConnectionReset => "connection reset".to_string(),
```

- [ ] **Step 6: Configuration**

`src/config.rs`:

```diff
diff --git a/nami-engine/src/config.rs b/nami-engine/src/config.rs
--- a/nami-engine/src/config.rs
+++ b/nami-engine/src/config.rs
@@ -30,6 +30,10 @@ pub struct Config {
     /// AES-256 key for secrets at rest (`NAMI_ENCRYPTION_KEY`, base64). Never
     /// logged: `Config` deliberately does not implement `Debug`.
     pub encryption_key: [u8; crate::crypto::KEY_LEN],
+    /// Where `domain_expiry` checks start their RDAP lookups
+    /// (`RDAP_BASE_URL`, default `https://rdap.org`, which redirects to the
+    /// registry's own RDAP server).
+    pub rdap_base_url: String,
 }
 
 impl Config {
@@ -104,6 +108,8 @@ impl Config {
             uptime_worker_enabled: env_flag("UPTIME_WORKER_ENABLED", true),
             // Tests only: `from_env` always replaces it with the real key.
             encryption_key: crate::crypto::TEST_KEY,
+            rdap_base_url: std::env::var("RDAP_BASE_URL")
+                .unwrap_or_else(|_| crate::uptime::checker::DEFAULT_RDAP_BASE_URL.to_string()),
         }
     }
 }
```

`nami-engine/.env.example`:

```diff
diff --git a/nami-engine/.env.example b/nami-engine/.env.example
--- a/nami-engine/.env.example
+++ b/nami-engine/.env.example
@@ -36,3 +36,6 @@ NAMI_ENCRYPTION_KEY=
 # Docker, the engine's group must be inside net.ipv4.ping_group_range:
 #   sudo sysctl -w net.ipv4.ping_group_range="0 2147483647"
 # or, for a container: sysctls: ["net.ipv4.ping_group_range=0 2147483647"]
+# RDAP service for domain-expiry monitors; rdap.org redirects each lookup to
+# the registry's own RDAP server.
+RDAP_BASE_URL=https://rdap.org
```

- [ ] **Step 7: Model and validation**

`src/models/monitor.rs` — type-dependent interval default, the 3600 s floor, threshold defaults, and a domain that must look like `example.com`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -1,3 +1,5 @@
+use std::net::IpAddr;
+
 use chrono::{DateTime, Utc};
 use serde::{Deserialize, Serialize};
 use sqlx::{PgConnection, PgPool};
@@ -15,6 +17,7 @@ use super::{
 use crate::{
     crypto::{Cipher, SecretString},
     error::AppError,
+    uptime::expiry::{DEFAULT_DOMAIN_THRESHOLD_DAYS, DEFAULT_SSL_THRESHOLD_DAYS},
 };
 
 /// One `monitors` row. The `*_enc` columns and `request_header_names` are
@@ -63,9 +66,10 @@ pub struct Monitor {
     pub dns_record_type: Option<String>,
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
+    pub expiry_threshold_days: Option<i32>,
 }
 
-pub const CHECK_TYPES: [&str; 5] = ["http", "keyword", "tcp", "ping", "dns"];
+pub const CHECK_TYPES: [&str; 7] = ["http", "keyword", "tcp", "ping", "dns", "ssl_expiry", "domain_expiry"];
 pub const DNS_RECORD_TYPES: [&str; 6] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"];
 pub const MAX_EXPECTED_VALUE_CHARS: usize = 1024;
 
@@ -119,6 +123,7 @@ pub struct MonitorConfig {
     pub dns_record_type: Option<String>,
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
+    pub expiry_threshold_days: Option<i32>,
 }
 
 impl From<&Monitor> for MonitorConfig {
@@ -148,6 +153,7 @@ impl From<&Monitor> for MonitorConfig {
             dns_record_type: m.dns_record_type.clone(),
             dns_nameserver: m.dns_nameserver.clone(),
             expected_value: m.expected_value.clone(),
+            expiry_threshold_days: m.expiry_threshold_days,
         }
     }
 }
@@ -177,6 +183,7 @@ impl MonitorConfig {
             || self.dns_record_type != current.dns_record_type
             || self.dns_nameserver != current.dns_nameserver
             || self.expected_value != current.expected_value
+            || self.expiry_threshold_days != current.expiry_threshold_days
     }
 }
 
@@ -199,8 +206,8 @@ pub struct CreateMonitorRequest {
     pub expected_status_min: i32,
     #[serde(default = "default_expected_status_max")]
     pub expected_status_max: i32,
-    #[serde(default = "default_interval_seconds")]
-    pub interval_seconds: i32,
+    /// Defaults by type, see [`default_interval_for`].
+    pub interval_seconds: Option<i32>,
     #[serde(default = "default_timeout_seconds")]
     pub timeout_seconds: i32,
     #[serde(default = "default_confirmation_threshold")]
@@ -220,6 +227,7 @@ pub struct CreateMonitorRequest {
     pub dns_record_type: Option<String>,
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
+    pub expiry_threshold_days: Option<i32>,
 }
 
 fn default_http_method() -> String {
@@ -234,8 +242,12 @@ fn default_expected_status_min() -> i32 {
 fn default_expected_status_max() -> i32 {
     399
 }
-fn default_interval_seconds() -> i32 {
-    60
+/// 60 s, or daily for the expiry types: expiry dates move slowly.
+fn default_interval_for(check_type: &str) -> i32 {
+    if is_expiry_type(check_type) { 86_400 } else { 60 }
+}
+fn is_expiry_type(check_type: &str) -> bool {
+    matches!(check_type, "ssl_expiry" | "domain_expiry")
 }
 fn default_timeout_seconds() -> i32 {
     10
@@ -252,6 +264,7 @@ fn default_recovery_threshold() -> i32 {
 
 impl CreateMonitorRequest {
     pub fn into_config(self) -> MonitorConfig {
+        let interval_seconds = self.interval_seconds.unwrap_or_else(|| default_interval_for(&self.check_type));
         MonitorConfig {
             name: self.name,
             check_type: self.check_type,
@@ -263,7 +276,7 @@ impl CreateMonitorRequest {
             keyword_mode: self.keyword_mode,
             expected_status_min: self.expected_status_min,
             expected_status_max: self.expected_status_max,
-            interval_seconds: self.interval_seconds,
+            interval_seconds,
             timeout_seconds: self.timeout_seconds,
             confirmation_threshold: self.confirmation_threshold,
             paused: self.paused,
@@ -283,6 +296,7 @@ impl CreateMonitorRequest {
             dns_record_type: self.dns_record_type,
             dns_nameserver: self.dns_nameserver,
             expected_value: self.expected_value,
+            expiry_threshold_days: self.expiry_threshold_days,
         }
     }
 }
@@ -323,6 +337,7 @@ pub struct PatchMonitorRequest {
     pub dns_nameserver: Option<Option<String>>,
     #[serde(default, deserialize_with = "nullable")]
     pub expected_value: Option<Option<String>>,
+    pub expiry_threshold_days: Option<i32>,
 }
 
 /// A nullable PATCH field applied to its current value.
@@ -370,6 +385,7 @@ impl PatchMonitorRequest {
             dns_record_type: self.dns_record_type.or(base.dns_record_type),
             dns_nameserver: patched(self.dns_nameserver, base.dns_nameserver),
             expected_value: patched(self.expected_value, base.expected_value),
+            expiry_threshold_days: self.expiry_threshold_days.or(base.expiry_threshold_days),
         }
     }
 }
@@ -387,12 +403,17 @@ pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
         "tcp" => validate_tcp(&mut c)?,
         "ping" => validate_ping(&mut c)?,
         "dns" => validate_dns(&mut c)?,
+        "ssl_expiry" => validate_ssl_expiry(&mut c)?,
+        "domain_expiry" => validate_domain_expiry(&mut c)?,
         _ => return Err(format!("check_type must be one of {}", CHECK_TYPES.join(", "))),
     }
     clear_unused_fields(&mut c);
     if c.interval_seconds < 30 {
         return Err("interval_seconds must be at least 30".into());
     }
+    if is_expiry_type(&c.check_type) && c.interval_seconds < 3600 {
+        return Err("interval_seconds must be at least 3600 for ssl_expiry and domain_expiry monitors".into());
+    }
     if !(1..=60).contains(&c.timeout_seconds) {
         return Err("timeout_seconds must be between 1 and 60".into());
     }
@@ -483,10 +504,10 @@ fn clear_unused_fields(c: &mut MonitorConfig) {
     if !uses(&["keyword"]) {
         c.keyword = None;
     }
-    if !uses(&["tcp", "ping", "dns"]) {
+    if !uses(&["tcp", "ping", "dns", "ssl_expiry", "domain_expiry"]) {
         c.host = None;
     }
-    if !uses(&["tcp"]) {
+    if !uses(&["tcp", "ssl_expiry"]) {
         c.port = None;
     }
     if !uses(&["dns"]) {
@@ -494,6 +515,9 @@ fn clear_unused_fields(c: &mut MonitorConfig) {
         c.dns_nameserver = None;
         c.expected_value = None;
     }
+    if !uses(&["ssl_expiry", "domain_expiry"]) {
+        c.expiry_threshold_days = None;
+    }
 }
 
 /// Non-http types carry no request config. Secrets are only marked for
@@ -558,6 +582,36 @@ fn validate_dns(c: &mut MonitorConfig) -> Result<(), String> {
     Ok(())
 }
 
+fn validate_ssl_expiry(c: &mut MonitorConfig) -> Result<(), String> {
+    c.host = Some(required_host(c)?);
+    let port = c.port.unwrap_or(443);
+    if !(1..=65535).contains(&port) {
+        return Err("port must be between 1 and 65535".into());
+    }
+    c.port = Some(port);
+    c.expiry_threshold_days = Some(validate_threshold(c.expiry_threshold_days, DEFAULT_SSL_THRESHOLD_DAYS)?);
+    Ok(())
+}
+
+fn validate_domain_expiry(c: &mut MonitorConfig) -> Result<(), String> {
+    let host = required_host(c)?;
+    let domain = host.trim_end_matches('.').to_string();
+    if domain.parse::<IpAddr>().is_ok() || !domain.contains('.') {
+        return Err("host must be a domain name such as example.com".into());
+    }
+    c.host = Some(domain);
+    c.expiry_threshold_days = Some(validate_threshold(c.expiry_threshold_days, DEFAULT_DOMAIN_THRESHOLD_DAYS)?);
+    Ok(())
+}
+
+fn validate_threshold(days: Option<i32>, default: i32) -> Result<i32, String> {
+    let days = days.unwrap_or(default);
+    if !(1..=365).contains(&days) {
+        return Err("expiry_threshold_days must be between 1 and 365".into());
+    }
+    Ok(days)
+}
+
 /// Optional text an answer must contain: trimmed, no control characters.
 fn validate_expected_value(raw: Option<String>) -> Result<Option<String>, String> {
     let Some(value) = raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) else {
@@ -632,7 +686,8 @@ async fn write_config(
              request_headers_enc = case when $21 then request_headers_enc else $23 end,
              auth_password_enc = case when $24 then auth_password_enc else $25 end,
              recovery_threshold = $26,
-             dns_record_type = $27, dns_nameserver = $28, expected_value = $29
+             dns_record_type = $27, dns_nameserver = $28, expected_value = $29,
+             expiry_threshold_days = $30
          where id = $1
          returning *",
     )
@@ -665,6 +720,7 @@ async fn write_config(
     .bind(&c.dns_record_type)
     .bind(&c.dns_nameserver)
     .bind(&c.expected_value)
+    .bind(c.expiry_threshold_days)
     .fetch_one(&mut *conn)
     .await
 }
```

- [ ] **Step 8: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `checker_expiry.rs` 9 passed, `monitor_types_api.rs` 4 passed, the rest green.

- [ ] **Step 9: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/.env.example \
  nami-engine/migrations/0015_monitor_expiry_checks.sql nami-engine/src/config.rs nami-engine/src/models/monitor.rs \
  nami-engine/src/uptime/mod.rs nami-engine/src/uptime/checker.rs nami-engine/src/uptime/tls.rs \
  nami-engine/src/uptime/expiry.rs nami-engine/tests/common/tls.rs nami-engine/tests/checker_expiry.rs \
  nami-engine/tests/monitor_types_api.rs
git commit -m "feat(engine): ssl and domain expiry monitors (NAMI-UP-14)

ssl_expiry reads the leaf certificate's notAfter after a verified TLS
handshake; domain_expiry reads the RDAP expiration event (RDAP_BASE_URL).
Both default to daily checks and never run more often than hourly.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15 (NAMI-UP-15): `smtp`, `pop3`, `imap` and `udp` monitors

Branch `feat/NAMI-UP-15-mail-udp-monitors`, worktree `.worktrees/NAMI-UP-15/`.

**Files:**
- Create: `nami-engine/migrations/0016_monitor_mail_udp.sql`, `nami-engine/src/uptime/mail.rs`, `nami-engine/src/uptime/udp.rs`
- Modify: `nami-engine/src/models/monitor.rs`, `nami-engine/src/uptime/mod.rs`, `nami-engine/src/uptime/checker.rs`
- Modify (tests): `nami-engine/tests/common/tls.rs`, `nami-engine/tests/monitor_types_api.rs`
- Test: `nami-engine/tests/checker_mail_udp.rs`

**Interfaces:**
- Consumes: `uptime::tls::connect` and `CheckContext.tls` (Task 14); `checker::{Observation, elapsed_ms, printable, describe_io_error}` (Tasks 13-14); `models::target::validate_host` (Task 13); `common::tls::{TestCa, self_signed, provider}`.
- Produces:
  - Columns `use_tls boolean not null default false`, `udp_payload text`; `check_type` accepts `smtp`, `pop3`, `imap`, `udp`.
  - `Monitor`/`MonitorConfig` gain `use_tls: bool`, `udp_payload: Option<String>`; `monitor::MAX_UDP_PAYLOAD_BYTES = 1024`; mail ports default to 25/110/143 (465/995/993 with `use_tls`); `udp` requires host, port and payload, `keyword` optional.
  - `pub(crate) uptime::mail::probe(ctx, monitor, started)`, `pub(crate) uptime::udp::probe(monitor, started)`.
  - Test helper `common::tls::serve_tls_banner(CertAndKey, banner: &'static str) -> u16`.

- [ ] **Step 1: Write the failing tests**

`tests/common/tls.rs` gains a TLS banner server:

```diff
diff --git a/nami-engine/tests/common/tls.rs b/nami-engine/tests/common/tls.rs
--- a/nami-engine/tests/common/tls.rs
+++ b/nami-engine/tests/common/tls.rs
@@ -88,3 +88,24 @@ pub async fn serve_https_ok(cert_and_key: CertAndKey) -> u16 {
     });
     port
 }
+
+/// A TLS server that sends `banner` right after the handshake, like an
+/// IMAPS/POP3S/SMTPS server greeting its client. Returns its port.
+pub async fn serve_tls_banner(cert_and_key: CertAndKey, banner: &'static str) -> u16 {
+    let acceptor = acceptor(cert_and_key);
+    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
+    let port = listener.local_addr().unwrap().port();
+    tokio::spawn(async move {
+        while let Ok((stream, _)) = listener.accept().await {
+            let acceptor = acceptor.clone();
+            tokio::spawn(async move {
+                if let Ok(mut tls) = acceptor.accept(stream).await {
+                    let _ = tls.write_all(banner.as_bytes()).await;
+                    // Hold the connection open while the client reads.
+                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
+                }
+            });
+        }
+    });
+    port
+}
```

`nami-engine/tests/checker_mail_udp.rs`:

```rust
// `smtp`/`pop3`/`imap` banner checks against local TCP (and TLS) servers,
// and `udp` checks against local UDP responders.
mod common;

use std::time::{Duration, Instant};

use common::{check_context, tls, unsaved_monitor};
use nami_engine::{
    models::monitor::Monitor,
    uptime::{
        checker::{perform_check, CheckContext, CheckOutcome, CheckStatus},
        tls::client_config,
    },
};
use tokio::{io::AsyncWriteExt, net::TcpListener};

/// A TCP server that writes `chunks` (pausing 50 ms between them) to every
/// connection, then keeps it open for 2 s. `None` = never write anything.
async fn banner_server(chunks: Option<Vec<&'static str>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let chunks = chunks.clone();
            tokio::spawn(async move {
                for chunk in chunks.unwrap_or_default() {
                    let _ = stream.write_all(chunk.as_bytes()).await;
                    let _ = stream.flush().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            });
        }
    });
    port
}

fn mail_monitor(check_type: &str, port: u16) -> Monitor {
    Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(port)),
        ..unsaved_monitor(check_type)
    }
}

async fn check(monitor: &Monitor) -> CheckOutcome {
    perform_check(&check_context(), monitor).await
}

#[tokio::test]
async fn each_protocol_is_up_on_its_ready_greeting() {
    let cases = [
        ("smtp", "220 mail.test ESMTP Postfix\r\n"),
        ("smtp", "220-mail.test first line of a multi-line greeting\r\n"),
        ("pop3", "+OK POP3 server ready\r\n"),
        ("imap", "* OK [CAPABILITY IMAP4rev1] ready\r\n"),
        ("imap", "* PREAUTH IMAP4rev1 logged in\r\n"),
    ];
    for (check_type, greeting) in cases {
        let port = banner_server(Some(vec![greeting])).await;
        let outcome = check(&mail_monitor(check_type, port)).await;
        assert_eq!(outcome.status, CheckStatus::Up, "{check_type} {greeting:?}: {:?}", outcome.error);
        assert!(outcome.response_time_ms.is_some());
    }
}

#[tokio::test]
async fn a_greeting_split_across_writes_is_read_whole() {
    let port = banner_server(Some(vec!["2", "20 slow.test ESMTP\r\n"])).await;
    let outcome = check(&mail_monitor("smtp", port)).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn a_wrong_greeting_is_down_and_quoted_without_control_characters() {
    let cases = [
        ("smtp", "554 mail.test no service\r\n", "unexpected greeting: 554 mail.test no service"),
        ("pop3", "-ERR too busy\r\n", "unexpected greeting: -ERR too busy"),
        ("imap", "* BYE shutting down\r\n", "unexpected greeting: * BYE shutting down"),
        ("smtp", "421 \u{7}bell\r\n", "unexpected greeting: 421 bell"),
        ("imap", "+OK this is pop3\r\n", "unexpected greeting: +OK this is pop3"),
    ];
    for (check_type, greeting, error) in cases {
        let port = banner_server(Some(vec![greeting])).await;
        let outcome = check(&mail_monitor(check_type, port)).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{check_type} {greeting:?}");
        assert_eq!(outcome.error.as_deref(), Some(error));
    }
}

#[tokio::test]
async fn a_silent_server_times_out_and_a_closing_one_is_down() {
    let silent = banner_server(None).await;
    let monitor = Monitor { timeout_seconds: 1, ..mail_monitor("smtp", silent) };
    let started = Instant::now();
    let outcome = check(&monitor).await;
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closing = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let outcome = check(&mail_monitor("pop3", closing)).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection closed before the server greeting"));

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let outcome = check(&mail_monitor("imap", closed)).await;
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

#[tokio::test]
async fn use_tls_reads_the_greeting_over_implicit_tls() {
    let ca = tls::TestCa::new();
    let not_after = time::OffsetDateTime::now_utc() + time::Duration::days(90);
    let port = tls::serve_tls_banner(ca.leaf(not_after), "* OK IMAPS ready\r\n").await;
    let ctx = CheckContext { tls: client_config(ca.roots()), ..check_context() };
    let monitor = Monitor {
        host: Some("localhost".to_string()),
        port: Some(i32::from(port)),
        use_tls: true,
        ..unsaved_monitor("imap")
    };
    let outcome = perform_check(&ctx, &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);

    let untrusted = tls::serve_tls_banner(tls::self_signed(), "* OK IMAPS ready\r\n").await;
    let monitor = Monitor { port: Some(i32::from(untrusted)), ..monitor };
    let outcome = perform_check(&ctx, &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("certificate is not trusted (unknown issuer)"));
}

/// A UDP responder on 127.0.0.1 that answers every datagram with
/// `PONG <datagram>` (or never answers when `silent`).
async fn udp_responder(silent: bool) -> u16 {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((len, peer)) = socket.recv_from(&mut buf).await {
            if !silent {
                let reply = [b"PONG ".as_slice(), &buf[..len]].concat();
                let _ = socket.send_to(&reply, peer).await;
            }
        }
    });
    port
}

fn udp_monitor(port: u16, keyword: Option<&str>, mode: &str) -> Monitor {
    Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(port)),
        udp_payload: Some("ping\n".to_string()),
        keyword: keyword.map(str::to_string),
        keyword_mode: mode.to_string(),
        ..unsaved_monitor("udp")
    }
}

#[tokio::test]
async fn a_udp_reply_is_up_and_the_keyword_is_matched_against_it() {
    let port = udp_responder(false).await;

    let any = check(&udp_monitor(port, None, "contains")).await;
    assert_eq!(any.status, CheckStatus::Up, "{:?}", any.error);
    assert!(any.response_time_ms.is_some());

    let echoed = check(&udp_monitor(port, Some("PONG ping"), "contains")).await;
    assert_eq!(echoed.status, CheckStatus::Up, "the payload was sent as given: {:?}", echoed.error);

    let missing = check(&udp_monitor(port, Some("NOPE"), "contains")).await;
    assert_eq!(missing.status, CheckStatus::Down);
    assert_eq!(missing.error.as_deref(), Some("keyword \"NOPE\" not found in the reply"));

    let unwanted = check(&udp_monitor(port, Some("PONG"), "not_contains")).await;
    assert_eq!(unwanted.status, CheckStatus::Down);
    assert_eq!(unwanted.error.as_deref(), Some("keyword \"PONG\" found in the reply"));
}

#[tokio::test]
async fn no_udp_reply_within_the_timeout_is_down() {
    let port = udp_responder(true).await;
    let monitor = Monitor { timeout_seconds: 1, ..udp_monitor(port, None, "contains") };
    let started = Instant::now();
    let outcome = check(&monitor).await;
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
}

#[tokio::test]
async fn a_closed_udp_port_is_refused() {
    let closed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let outcome = check(&udp_monitor(closed, None, "contains")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused (port unreachable)"));
}
```

Append to `nami-engine/tests/monitor_types_api.rs`:

```diff
diff --git a/nami-engine/tests/monitor_types_api.rs b/nami-engine/tests/monitor_types_api.rs
--- a/nami-engine/tests/monitor_types_api.rs
+++ b/nami-engine/tests/monitor_types_api.rs
@@ -176,3 +176,75 @@ async fn expiry_monitors_get_daily_checks_and_type_specific_thresholds(pool: sql
     ])
     .await;
 }
+
+#[sqlx::test]
+async fn mail_monitors_default_their_port_from_protocol_and_tls(pool: sqlx::PgPool) {
+    let c = ctx(pool).await;
+    let cases = [
+        ("smtp", false, 25),
+        ("smtp", true, 465),
+        ("pop3", false, 110),
+        ("pop3", true, 995),
+        ("imap", false, 143),
+        ("imap", true, 993),
+    ];
+    for (check_type, use_tls, port) in cases {
+        let (status, created) = c
+            .create(json!({ "name": check_type, "check_type": check_type, "host": "mail.example.com", "use_tls": use_tls }))
+            .await;
+        assert_eq!(status, StatusCode::CREATED, "{created}");
+        assert_eq!(created["port"], port, "{check_type} use_tls={use_tls}");
+        assert_eq!(created["use_tls"], use_tls);
+    }
+    let (status, custom) = c
+        .create(json!({ "name": "submission", "check_type": "smtp", "host": "mail.example.com", "port": 587 }))
+        .await;
+    assert_eq!(status, StatusCode::CREATED, "{custom}");
+    assert_eq!(custom["port"], 587);
+    let (_, http) = c
+        .create(json!({ "name": "web", "check_type": "http", "url": "https://example.com", "use_tls": true }))
+        .await;
+    assert_eq!(http["use_tls"], false, "use_tls only applies to mail monitors");
+
+    let c = ctx(c.pool.clone()).await;
+    c.assert_rejected(vec![
+        (json!({ "name": "x", "check_type": "imap" }), "host is required"),
+        (json!({ "name": "x", "check_type": "pop3", "host": "mail.example.com", "port": 0 }), "port"),
+        (json!({ "name": "x", "check_type": "smtp", "host": "smtp://mail.example.com" }), "host must be"),
+    ])
+    .await;
+}
+
+#[sqlx::test]
+async fn udp_monitors_need_a_port_and_a_payload(pool: sqlx::PgPool) {
+    let c = ctx(pool).await;
+    let (status, created) = c
+        .create(json!({
+            "name": "Game server", "check_type": "udp", "host": "game.example.com", "port": 27015,
+            "udp_payload": "\u{ff}\u{ff}status\r\n", "keyword": "players"
+        }))
+        .await;
+    assert_eq!(status, StatusCode::CREATED, "{created}");
+    assert_eq!(created["udp_payload"], "\u{ff}\u{ff}status\r\n", "payloads are sent verbatim, line breaks included");
+    assert_eq!(created["keyword"], "players");
+    let id = created["id"].as_str().unwrap();
+    let (status, tcp) = c.patch(id, json!({ "check_type": "tcp" })).await;
+    assert_eq!(status, StatusCode::OK, "{tcp}");
+    assert_eq!(tcp["udp_payload"], Value::Null);
+    assert_eq!(tcp["keyword"], Value::Null);
+
+    let c = ctx(c.pool.clone()).await;
+    let udp = |extra: Value| {
+        let mut body = json!({ "name": "x", "check_type": "udp", "host": "game.example.com", "port": 27015, "udp_payload": "hi" });
+        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
+        body
+    };
+    c.assert_rejected(vec![
+        (udp(json!({ "port": null })), "port"),
+        (udp(json!({ "udp_payload": null })), "udp_payload is required"),
+        (udp(json!({ "udp_payload": "" })), "udp_payload is required"),
+        (udp(json!({ "udp_payload": "a\u{0}b" })), "NUL"),
+        (udp(json!({ "udp_payload": "a".repeat(1025) })), "at most 1024 bytes"),
+    ])
+    .await;
+}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && cargo test --test checker_mail_udp`
Expected: FAIL to compile — `struct Monitor has no field named use_tls`.

- [ ] **Step 3: Add the migration**

`nami-engine/migrations/0016_monitor_mail_udp.sql`:

```sql
-- smtp, pop3, imap and udp monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in (
        'http', 'keyword', 'tcp', 'ping', 'dns', 'ssl_expiry', 'domain_expiry', 'smtp', 'pop3', 'imap', 'udp'
    ));

alter table monitors
    add column use_tls boolean not null default false,   -- smtp/pop3/imap: implicit TLS
    add column udp_payload text;                          -- udp: the datagram to send
```

- [ ] **Step 4: Model and validation**

`src/models/monitor.rs`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -67,9 +67,13 @@ pub struct Monitor {
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
     pub expiry_threshold_days: Option<i32>,
+    pub use_tls: bool,
+    pub udp_payload: Option<String>,
 }
 
-pub const CHECK_TYPES: [&str; 7] = ["http", "keyword", "tcp", "ping", "dns", "ssl_expiry", "domain_expiry"];
+pub const CHECK_TYPES: [&str; 11] =
+    ["http", "keyword", "tcp", "ping", "dns", "ssl_expiry", "domain_expiry", "smtp", "pop3", "imap", "udp"];
+pub const MAX_UDP_PAYLOAD_BYTES: usize = 1024;
 pub const DNS_RECORD_TYPES: [&str; 6] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"];
 pub const MAX_EXPECTED_VALUE_CHARS: usize = 1024;
 
@@ -124,6 +128,8 @@ pub struct MonitorConfig {
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
     pub expiry_threshold_days: Option<i32>,
+    pub use_tls: bool,
+    pub udp_payload: Option<String>,
 }
 
 impl From<&Monitor> for MonitorConfig {
@@ -154,6 +160,8 @@ impl From<&Monitor> for MonitorConfig {
             dns_nameserver: m.dns_nameserver.clone(),
             expected_value: m.expected_value.clone(),
             expiry_threshold_days: m.expiry_threshold_days,
+            use_tls: m.use_tls,
+            udp_payload: m.udp_payload.clone(),
         }
     }
 }
@@ -184,6 +192,8 @@ impl MonitorConfig {
             || self.dns_nameserver != current.dns_nameserver
             || self.expected_value != current.expected_value
             || self.expiry_threshold_days != current.expiry_threshold_days
+            || self.use_tls != current.use_tls
+            || self.udp_payload != current.udp_payload
     }
 }
 
@@ -228,6 +238,9 @@ pub struct CreateMonitorRequest {
     pub dns_nameserver: Option<String>,
     pub expected_value: Option<String>,
     pub expiry_threshold_days: Option<i32>,
+    #[serde(default)]
+    pub use_tls: bool,
+    pub udp_payload: Option<String>,
 }
 
 fn default_http_method() -> String {
@@ -297,6 +310,8 @@ impl CreateMonitorRequest {
             dns_nameserver: self.dns_nameserver,
             expected_value: self.expected_value,
             expiry_threshold_days: self.expiry_threshold_days,
+            use_tls: self.use_tls,
+            udp_payload: self.udp_payload,
         }
     }
 }
@@ -338,6 +353,8 @@ pub struct PatchMonitorRequest {
     #[serde(default, deserialize_with = "nullable")]
     pub expected_value: Option<Option<String>>,
     pub expiry_threshold_days: Option<i32>,
+    pub use_tls: Option<bool>,
+    pub udp_payload: Option<String>,
 }
 
 /// A nullable PATCH field applied to its current value.
@@ -386,6 +403,8 @@ impl PatchMonitorRequest {
             dns_nameserver: patched(self.dns_nameserver, base.dns_nameserver),
             expected_value: patched(self.expected_value, base.expected_value),
             expiry_threshold_days: self.expiry_threshold_days.or(base.expiry_threshold_days),
+            use_tls: self.use_tls.unwrap_or(base.use_tls),
+            udp_payload: self.udp_payload.or(base.udp_payload),
         }
     }
 }
@@ -405,6 +424,8 @@ pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
         "dns" => validate_dns(&mut c)?,
         "ssl_expiry" => validate_ssl_expiry(&mut c)?,
         "domain_expiry" => validate_domain_expiry(&mut c)?,
+        "smtp" | "pop3" | "imap" => validate_mail(&mut c)?,
+        "udp" => validate_udp(&mut c)?,
         _ => return Err(format!("check_type must be one of {}", CHECK_TYPES.join(", "))),
     }
     clear_unused_fields(&mut c);
@@ -501,13 +522,13 @@ fn clear_unused_fields(c: &mut MonitorConfig) {
         c.url = None;
         clear_request_config(c);
     }
-    if !uses(&["keyword"]) {
+    if !uses(&["keyword", "udp"]) {
         c.keyword = None;
     }
-    if !uses(&["tcp", "ping", "dns", "ssl_expiry", "domain_expiry"]) {
+    if !uses(&["tcp", "ping", "dns", "ssl_expiry", "domain_expiry", "smtp", "pop3", "imap", "udp"]) {
         c.host = None;
     }
-    if !uses(&["tcp", "ssl_expiry"]) {
+    if !uses(&["tcp", "ssl_expiry", "smtp", "pop3", "imap", "udp"]) {
         c.port = None;
     }
     if !uses(&["dns"]) {
@@ -518,6 +539,12 @@ fn clear_unused_fields(c: &mut MonitorConfig) {
     if !uses(&["ssl_expiry", "domain_expiry"]) {
         c.expiry_threshold_days = None;
     }
+    if !uses(&["smtp", "pop3", "imap"]) {
+        c.use_tls = false;
+    }
+    if !uses(&["udp"]) {
+        c.udp_payload = None;
+    }
 }
 
 /// Non-http types carry no request config. Secrets are only marked for
@@ -604,6 +631,46 @@ fn validate_domain_expiry(c: &mut MonitorConfig) -> Result<(), String> {
     Ok(())
 }
 
+/// smtp/pop3/imap: the port defaults to the protocol's (implicit TLS)
+/// port: 25 (465), 110 (995), 143 (993).
+fn validate_mail(c: &mut MonitorConfig) -> Result<(), String> {
+    c.host = Some(required_host(c)?);
+    let default_port = match (c.check_type.as_str(), c.use_tls) {
+        ("smtp", false) => 25,
+        ("smtp", true) => 465,
+        ("pop3", false) => 110,
+        ("pop3", true) => 995,
+        ("imap", false) => 143,
+        _ => 993,
+    };
+    let port = c.port.unwrap_or(default_port);
+    if !(1..=65535).contains(&port) {
+        return Err("port must be between 1 and 65535".into());
+    }
+    c.port = Some(port);
+    Ok(())
+}
+
+/// udp: host, port and the payload to send are required; `keyword` is an
+/// optional match on the reply.
+fn validate_udp(c: &mut MonitorConfig) -> Result<(), String> {
+    c.host = Some(required_host(c)?);
+    if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
+        return Err("port must be between 1 and 65535".into());
+    }
+    let Some(payload) = c.udp_payload.as_deref().filter(|payload| !payload.is_empty()) else {
+        return Err("udp_payload is required for udp monitors".into());
+    };
+    if payload.contains('\0') {
+        return Err("udp_payload must not contain NUL characters".into());
+    }
+    if payload.len() > MAX_UDP_PAYLOAD_BYTES {
+        return Err(format!("udp_payload must be at most {MAX_UDP_PAYLOAD_BYTES} bytes"));
+    }
+    c.keyword = c.keyword.take().filter(|keyword| !keyword.is_empty());
+    Ok(())
+}
+
 fn validate_threshold(days: Option<i32>, default: i32) -> Result<i32, String> {
     let days = days.unwrap_or(default);
     if !(1..=365).contains(&days) {
@@ -687,7 +754,8 @@ async fn write_config(
              auth_password_enc = case when $24 then auth_password_enc else $25 end,
              recovery_threshold = $26,
              dns_record_type = $27, dns_nameserver = $28, expected_value = $29,
-             expiry_threshold_days = $30
+             expiry_threshold_days = $30,
+             use_tls = $31, udp_payload = $32
          where id = $1
          returning *",
     )
@@ -721,6 +789,8 @@ async fn write_config(
     .bind(&c.dns_nameserver)
     .bind(&c.expected_value)
     .bind(c.expiry_threshold_days)
+    .bind(c.use_tls)
+    .bind(&c.udp_payload)
     .fetch_one(&mut *conn)
     .await
 }
```

- [ ] **Step 5: The probes**

`nami-engine/src/uptime/mail.rs`:

```rust
//! `smtp`, `pop3` and `imap` checks: connect (over implicit TLS when
//! `use_tls`), read the server greeting, and require the protocol's ready
//! reply: SMTP `220`, POP3 `+OK`, IMAP `* OK` or `* PREAUTH`.

use std::time::Instant;

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    net::TcpStream,
};

use super::{
    checker::{describe_io_error, elapsed_ms, printable, CheckContext, Observation},
    tls,
};
use crate::models::monitor::Monitor;

/// A greeting is one line; never read more than this.
const MAX_GREETING_BYTES: usize = 512;

pub(crate) async fn probe(ctx: &CheckContext, monitor: &Monitor, started: Instant) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let Some(port) = monitor.port.and_then(|port| u16::try_from(port).ok()) else {
        return Observation::down(None, None, "monitor has no valid port".to_string());
    };
    let greeting = if monitor.use_tls {
        match tls::connect(ctx.tls.clone(), host, port).await {
            Ok(mut stream) => read_greeting(&mut stream).await,
            Err(message) => return Observation::down(None, None, message),
        }
    } else {
        match TcpStream::connect((host, port)).await {
            Ok(mut stream) => read_greeting(&mut stream).await,
            Err(e) => return Observation::down(None, None, describe_io_error(&e)),
        }
    };
    let greeting = match greeting {
        Ok(greeting) => greeting,
        Err(message) => return Observation::down(None, None, message),
    };
    if is_ready(&monitor.check_type, &greeting) {
        Observation::up(None, started)
    } else {
        Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("unexpected greeting: {}", printable(&greeting, 100)),
        )
    }
}

/// The first line the server sends (without its line break), read until a
/// newline arrives, the server closes, or [`MAX_GREETING_BYTES`] are in.
async fn read_greeting<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, String> {
    let mut greeting = Vec::with_capacity(128);
    let mut chunk = [0u8; 256];
    while !greeting.contains(&b'\n') && greeting.len() < MAX_GREETING_BYTES {
        let read = stream.read(&mut chunk).await.map_err(|e| describe_io_error(&e))?;
        if read == 0 {
            break;
        }
        greeting.extend_from_slice(&chunk[..read]);
    }
    if greeting.is_empty() {
        return Err("connection closed before the server greeting".to_string());
    }
    let end = greeting.iter().position(|&b| b == b'\n').unwrap_or(greeting.len()).min(MAX_GREETING_BYTES);
    Ok(String::from_utf8_lossy(&greeting[..end]).trim_end_matches('\r').to_string())
}

fn is_ready(check_type: &str, greeting: &str) -> bool {
    let upper = greeting.to_ascii_uppercase();
    match check_type {
        "smtp" => greeting.starts_with("220"),
        "pop3" => upper.starts_with("+OK"),
        "imap" => upper.starts_with("* OK") || upper.starts_with("* PREAUTH"),
        _ => false,
    }
}
```

`nami-engine/src/uptime/udp.rs`:

```rust
//! `udp` checks: send `udp_payload` and wait for one datagram back. Up when
//! a reply arrives and, if `keyword` is set, the reply contains it (or, with
//! `keyword_mode = not_contains`, does not).

use std::{io::ErrorKind, net::SocketAddr, time::Instant};

use tokio::net::UdpSocket;

use super::checker::{describe_io_error, elapsed_ms, Observation};
use crate::models::monitor::Monitor;

pub(crate) async fn probe(monitor: &Monitor, started: Instant) -> Observation {
    let (Some(host), Some(port), Some(payload)) =
        (monitor.host.as_deref(), monitor.port, monitor.udp_payload.as_deref())
    else {
        return Observation::down(None, None, "monitor has no host, port or payload".to_string());
    };
    let Ok(port) = u16::try_from(port) else {
        return Observation::down(None, None, format!("invalid port {port}"));
    };
    let target = match resolve(host, port).await {
        Ok(target) => target,
        Err(message) => return Observation::down(None, None, message),
    };
    let local: SocketAddr = if target.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
    let reply = async {
        let socket = UdpSocket::bind(local).await?;
        // A connected socket only accepts datagrams from the target, and an
        // ICMP "port unreachable" surfaces as ECONNREFUSED.
        socket.connect(target).await?;
        socket.send(payload.as_bytes()).await?;
        let mut buffer = vec![0u8; 65_535];
        let len = socket.recv(&mut buffer).await?;
        buffer.truncate(len);
        Ok::<_, std::io::Error>(buffer)
    };
    let reply = match reply.await {
        Ok(reply) => reply,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            return Observation::down(None, None, "connection refused (port unreachable)".to_string());
        }
        Err(e) => return Observation::down(None, None, describe_io_error(&e)),
    };
    let Some(keyword) = monitor.keyword.as_deref() else {
        return Observation::up(None, started);
    };
    let found = String::from_utf8_lossy(&reply).contains(keyword);
    match (monitor.keyword_mode.as_str(), found) {
        ("not_contains", true) => {
            Observation::down(None, Some(elapsed_ms(started)), format!("keyword \"{keyword}\" found in the reply"))
        }
        ("not_contains", false) | (_, true) => Observation::up(None, started),
        (_, false) => Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" not found in the reply"),
        ),
    }
}

/// The first address of `host:port`, preferring IPv4.
async fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .collect();
    addresses
        .iter()
        .find(|addr| addr.is_ipv4())
        .or(addresses.first())
        .copied()
        .ok_or_else(|| format!("could not resolve {host}: no addresses"))
}
```

`src/uptime/mod.rs` and the dispatch in `src/uptime/checker.rs`:

```diff
diff --git a/nami-engine/src/uptime/mod.rs b/nami-engine/src/uptime/mod.rs
--- a/nami-engine/src/uptime/mod.rs
+++ b/nami-engine/src/uptime/mod.rs
@@ -7,7 +7,9 @@ pub mod checker;
 pub mod dns;
 pub mod expiry;
 pub mod heartbeat;
+pub mod mail;
 pub mod ping;
 pub mod store;
 pub mod tls;
+pub mod udp;
 pub mod worker;
```

```diff
diff --git a/nami-engine/src/uptime/checker.rs b/nami-engine/src/uptime/checker.rs
--- a/nami-engine/src/uptime/checker.rs
+++ b/nami-engine/src/uptime/checker.rs
@@ -203,6 +203,8 @@ pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcom
             "dns" => super::dns::probe(monitor, timeout, started).await,
             "ssl_expiry" => super::expiry::probe_ssl(ctx, monitor, started).await,
             "domain_expiry" => super::expiry::probe_domain(ctx, monitor, timeout, started).await,
+            "smtp" | "pop3" | "imap" => super::mail::probe(ctx, monitor, started).await,
+            "udp" => super::udp::probe(monitor, started).await,
             other => Observation::down(None, None, format!("unsupported check type {other:?}")),
         }
     };
```

- [ ] **Step 6: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `checker_mail_udp.rs` 8 passed, `monitor_types_api.rs` 6 passed, the rest green.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/0016_monitor_mail_udp.sql nami-engine/src/models/monitor.rs \
  nami-engine/src/uptime/mod.rs nami-engine/src/uptime/checker.rs nami-engine/src/uptime/mail.rs \
  nami-engine/src/uptime/udp.rs nami-engine/tests/common/tls.rs nami-engine/tests/checker_mail_udp.rs \
  nami-engine/tests/monitor_types_api.rs
git commit -m "feat(engine): smtp, pop3, imap and udp monitors (NAMI-UP-15)

Mail checks read the server greeting (220 / +OK / * OK or * PREAUTH),
optionally over implicit TLS; udp sends a payload and waits for a reply,
optionally matching a keyword.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16 (NAMI-UP-16): Maintenance windows (one-off and weekly)

Branch `feat/NAMI-UP-16-maintenance-windows`, worktree `.worktrees/NAMI-UP-16/`.

**Files:**
- Create: `nami-engine/migrations/0017_create_maintenance_windows.sql`, `nami-engine/src/models/maintenance.rs`, `nami-engine/src/routes/maintenance.rs`
- Modify: `nami-engine/Cargo.toml`, `nami-engine/src/models/mod.rs`, `nami-engine/src/models/monitor.rs`, `nami-engine/src/models/heartbeat.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/src/routes/monitors.rs`, `nami-engine/src/routes/heartbeats.rs`, `nami-engine/src/uptime/worker.rs`, `nami-engine/src/uptime/heartbeat.rs`
- Modify (tests): `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/maintenance_windows.rs`, unit tests in `src/models/maintenance.rs`

**Interfaces:**
- Consumes: `worker::{apply_outcome, sweep_heartbeats}`, `uptime::heartbeat::{record_ping, sweep_overdue}`, `models::{nullable, validate_name}`, `MonitorView` (Tasks 10, 12); Phase 1 heartbeat routes and models.
- Produces:
  - Table `maintenance_windows` (see migration); `monitors_id_project_key unique (id, project_id)` on monitors.
  - `models::validate_label(field: &str, raw: &str) -> Result<String, String>` (`validate_name` now delegates to it).
  - `models::maintenance`: `MaintenanceWindow` (row, `Serialize`); `Schedule::{Once { starts_at, ends_at }, Weekly { days: Vec<Weekday>, start: NaiveTime, end: NaiveTime, timezone: Tz }}` with `fn covers(&self, at: DateTime<Utc>) -> bool`; `weekday_from_iso(i16) -> Option<Weekday>`; `MaintenanceWindow::{schedule() -> Option<Schedule>, covers(at) -> bool, applies_to_monitor(Uuid) -> bool, applies_to_heartbeats() -> bool}`; `WindowView { window (flattened), active: bool }` with `WindowView::new(window, now)`; `CreateWindowRequest`, `PatchWindowRequest`, `WindowConfig`, `ValidWindow`, `validate(WindowConfig) -> Result<ValidWindow, String>`; queries `insert_window`, `list_for_project`, `find_in_project`, `monitor_belongs_to`, `update_in_project`, `delete_in_project`, `monitor_in_maintenance(executor, project_id, monitor_id, at) -> Result<bool, sqlx::Error>`, `heartbeat_in_maintenance(executor, project_id, at) -> Result<bool, sqlx::Error>`, `project_wide_windows(executor, &[Uuid])`.
  - Routes `GET|POST /projects/{id}/maintenance-windows`, `GET|PATCH|DELETE /projects/{id}/maintenance-windows/{window_id}` (viewer / member / admin).
  - `MonitorView::new(monitor: Monitor, in_maintenance: bool) -> MonitorView` (replaces `From<Monitor>`; adds `in_maintenance`); `models::heartbeat::HeartbeatView { heartbeat (flattened), in_maintenance: bool }` returned by every heartbeat route.
  - Worker/heartbeats: inside a window `apply_outcome` only updates `last_checked_at`; the sweep skips heartbeats of projects in a project-wide window; a ping during such a window only updates `last_ping_at`.

- [ ] **Step 1: Add the crate**

```bash
cd nami-engine
cargo add chrono-tz@0.10.4
```

- [ ] **Step 2: Write the failing tests**

`nami-engine/tests/maintenance_windows.rs`:

```rust
// Maintenance windows: CRUD, project isolation, and how they freeze the
// monitor and heartbeat state machines. Requires
// `docker compose up -d postgres mailpit`.
mod common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::{
    add_member, call, seed_http_monitor, seed_project, seed_user, settled_mail, test_app, unique_email,
    user_with_role,
};
use nami_engine::{
    models::monitor::Monitor,
    state::AppState,
    uptime::{
        alerts::TransitionKind,
        checker::{CheckOutcome, CheckStatus},
        worker::{apply_outcome, sweep_heartbeats},
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

/// A one-off window over the next hour (or another `minutes` span).
fn window_now(title: &str, monitor_id: Option<Uuid>) -> Value {
    json!({
        "title": title,
        "monitor_id": monitor_id,
        "kind": "once",
        "starts_at": (Utc::now() - Duration::minutes(5)).to_rfc3339(),
        "ends_at": (Utc::now() + Duration::hours(1)).to_rfc3339()
    })
}

/// Inserts a window covering now directly (bypassing the API).
async fn seed_window_now(pool: &sqlx::PgPool, project: Uuid, monitor_id: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, $2, 'maintenance', 'once', now() - interval '5 minutes', now() + interval '1 hour')
         returning id",
    )
    .bind(project)
    .bind(monitor_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn outcome(status: CheckStatus) -> CheckOutcome {
    CheckOutcome {
        status,
        status_code: (status == CheckStatus::Up).then_some(200),
        response_time_ms: Some(10),
        error: (status == CheckStatus::Down).then(|| "connection refused".to_string()),
        checked_at: Utc::now(),
    }
}

async fn monitor(pool: &sqlx::PgPool, id: Uuid) -> Monitor {
    sqlx::query_as("select * from monitors where id = $1").bind(id).fetch_one(pool).await.unwrap()
}

async fn open_incidents(pool: &sqlx::PgPool, monitor_id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from incidents where monitor_id = $1 and status = 'open'")
        .bind(monitor_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn members_create_and_edit_one_off_and_weekly_windows_admins_delete(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let api_monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/maintenance-windows");

    let (status, once) = call(&app, "POST", &collection, Some(&member), Some(window_now("DB upgrade", None))).await;
    assert_eq!(status, StatusCode::CREATED, "{once}");
    assert_eq!(once["kind"], "once");
    assert_eq!(once["monitor_id"], Value::Null);
    assert_eq!(once["active"], true);
    assert_eq!(once["days_of_week"], Value::Null);

    let weekly = json!({
        "title": "Weekly deploy", "monitor_id": api_monitor, "kind": "weekly",
        "days_of_week": [4, 2, 2], "start_time": "22:00", "end_time": "02:00", "timezone": "Asia/Jakarta",
        "starts_at": "2026-01-01T00:00:00Z"
    });
    let (status, weekly) = call(&app, "POST", &collection, Some(&member), Some(weekly)).await;
    assert_eq!(status, StatusCode::CREATED, "{weekly}");
    assert_eq!(weekly["days_of_week"], json!([2, 4]), "sorted, duplicates dropped");
    assert_eq!(weekly["start_time"], "22:00:00");
    assert_eq!(weekly["end_time"], "02:00:00");
    assert_eq!(weekly["timezone"], "Asia/Jakarta");
    assert_eq!(weekly["starts_at"], Value::Null, "one-off fields are dropped from weekly windows");
    assert_eq!(weekly["monitor_id"], api_monitor.to_string());

    let (status, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);

    let item = format!("{collection}/{}", weekly["id"].as_str().unwrap());
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "title": "Deploys", "monitor_id": null, "days_of_week": [1, 2, 3, 4, 5, 6, 7], "start_time": "00:00", "end_time": "00:00", "timezone": "UTC" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["title"], "Deploys");
    assert_eq!(patched["monitor_id"], Value::Null, "null makes the window project-wide");
    assert_eq!(patched["active"], true, "every day, all day");

    let (status, switched) = call(&app, "PATCH", &item, Some(&member), Some(window_now("Now", None))).await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(switched["kind"], "once");
    assert_eq!(switched["days_of_week"], Value::Null, "weekly fields are dropped when switching kind");

    assert_eq!(call(&app, "DELETE", &item, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    let (status, body) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "deleted": true })));
    assert_eq!(call(&app, "GET", &item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewers_read_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let window = seed_window_now(&pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let item = format!("{collection}/{window}");

    assert_eq!(call(&app, "GET", &collection, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &item, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(
        call(&app, "POST", &collection, Some(&viewer), Some(window_now("x", None))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&app, "PATCH", &item, Some(&viewer), Some(json!({ "title": "x" }))).await.0, StatusCode::FORBIDDEN);
    for (method, uri) in [("GET", &collection), ("GET", &item), ("DELETE", &item)] {
        assert_eq!(call(&app, method, uri, Some(&outsider), None).await.0, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

/// Security: window ids and monitor ids of another project are 404s and
/// nothing is written or changed.
#[sqlx::test]
async fn windows_and_monitors_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign_window = seed_window_now(&pool, other, None).await;
    let foreign_monitor = seed_http_monitor(&pool, other, "Other", "https://example.com", 2).await;
    let own_window = seed_window_now(&pool, project, None).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let foreign_item = format!("{collection}/{foreign_window}");

    assert_eq!(call(&app, "GET", &foreign_item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&app, "PATCH", &foreign_item, Some(&admin), Some(json!({ "title": "hijacked" }))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "DELETE", &foreign_item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    let (status, _) =
        call(&app, "POST", &collection, Some(&admin), Some(window_now("x", Some(foreign_monitor)))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a monitor of another project cannot be targeted");
    let own_item = format!("{collection}/{own_window}");
    let (status, _) =
        call(&app, "PATCH", &own_item, Some(&admin), Some(json!({ "monitor_id": foreign_monitor }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let windows: Vec<(Uuid, Option<Uuid>, String)> =
        sqlx::query_as("select project_id, monitor_id, title from maintenance_windows order by project_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(windows.len(), 2, "nothing was created");
    assert!(windows.iter().all(|(_, monitor_id, title)| monitor_id.is_none() && title == "maintenance"));
    // The database refuses a cross-project reference even without the API.
    let direct = sqlx::query(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, $2, 'x', 'once', now(), now() + interval '1 hour')",
    )
    .bind(project)
    .bind(foreign_monitor)
    .execute(&pool)
    .await;
    assert!(direct.is_err(), "the composite foreign key rejects it");
}

#[sqlx::test]
async fn invalid_windows_are_rejected_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let weekly = |extra: Value| {
        let mut body = json!({
            "title": "w", "kind": "weekly", "days_of_week": [1], "start_time": "01:00", "end_time": "02:00", "timezone": "UTC"
        });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        body
    };
    let now = Utc::now();
    let cases = [
        (json!({ "title": " ", "kind": "once", "starts_at": now, "ends_at": now + Duration::hours(1) }), "title is required"),
        (json!({ "title": "x\r\ny", "kind": "once", "starts_at": now, "ends_at": now + Duration::hours(1) }), "control"),
        (json!({ "title": "x", "kind": "daily" }), "kind must be once or weekly"),
        (json!({ "title": "x", "kind": "once", "starts_at": now }), "starts_at and ends_at are required"),
        (json!({ "title": "x", "kind": "once", "starts_at": now, "ends_at": now }), "ends_at must be after starts_at"),
        (weekly(json!({ "days_of_week": [] })), "days_of_week"),
        (weekly(json!({ "days_of_week": [0] })), "days_of_week"),
        (weekly(json!({ "days_of_week": [8] })), "days_of_week"),
        (weekly(json!({ "start_time": "25:00" })), "start_time must be a time of day"),
        (weekly(json!({ "end_time": "noon" })), "end_time must be a time of day"),
        (weekly(json!({ "timezone": "Mars/Olympus_Mons" })), "timezone must be an IANA time zone"),
        (weekly(json!({ "timezone": null })), "timezone must be an IANA time zone"),
    ];
    for (body, fragment) in cases {
        let (status, json) =
            call(&app, "POST", &format!("/projects/{project}/maintenance-windows"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(message.contains(fragment), "{body}: {message:?} should mention {fragment:?}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from maintenance_windows").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn a_covered_monitor_records_checks_but_its_state_never_moves(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, project, None).await;
    let m = monitor(&pool, id).await;

    for _ in 0..3 {
        assert_eq!(apply_outcome(&pool, &m, &outcome(CheckStatus::Down)).await.unwrap(), None);
    }

    let after = monitor(&pool, id).await;
    assert_eq!((after.current_status.as_str(), after.consecutive_failures), ("pending", 0), "counters frozen");
    assert!(after.last_checked_at.is_some(), "the checks are still recorded");
    assert_eq!(open_incidents(&pool, id).await, 0, "no incident while in maintenance");
}

#[sqlx::test]
async fn a_window_scoped_to_one_monitor_leaves_the_others_alone(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let covered = seed_http_monitor(&pool, project, "Covered", "http://127.0.0.1:9/", 1).await;
    let other = seed_http_monitor(&pool, project, "Other", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, project, Some(covered)).await;

    let covered_result = apply_outcome(&pool, &monitor(&pool, covered).await, &outcome(CheckStatus::Down)).await;
    let other_result = apply_outcome(&pool, &monitor(&pool, other).await, &outcome(CheckStatus::Down)).await;

    assert_eq!(covered_result.unwrap(), None);
    assert_eq!(other_result.unwrap().map(|t| t.kind), Some(TransitionKind::Down));
}

#[sqlx::test]
async fn an_incident_open_before_the_window_stays_open_and_resolves_after_it(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    let m = monitor(&pool, id).await;
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Down)).await.unwrap().map(|t| t.kind),
        Some(TransitionKind::Down)
    );
    let window = seed_window_now(&pool, project, None).await;

    assert_eq!(apply_outcome(&pool, &m, &outcome(CheckStatus::Up)).await.unwrap(), None, "no recovery inside");
    assert_eq!(monitor(&pool, id).await.current_status, "down");
    assert_eq!(open_incidents(&pool, id).await, 1);

    // The window ends.
    sqlx::query("update maintenance_windows set ends_at = now() - interval '1 second', starts_at = now() - interval '1 hour' where id = $1")
        .bind(window)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Up)).await.unwrap().map(|t| t.kind),
        Some(TransitionKind::Recovered)
    );
    assert_eq!(open_incidents(&pool, id).await, 0);
}

#[sqlx::test]
async fn failures_inside_the_window_do_not_count_toward_the_threshold(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 2).await;
    let m = monitor(&pool, id).await;
    apply_outcome(&pool, &m, &outcome(CheckStatus::Down)).await.unwrap();
    let window = seed_window_now(&pool, project, Some(id)).await;
    apply_outcome(&pool, &m, &outcome(CheckStatus::Down)).await.unwrap();
    assert_eq!(monitor(&pool, id).await.consecutive_failures, 1, "the in-window failure did not count");

    sqlx::query("delete from maintenance_windows where id = $1").bind(window).execute(&pool).await.unwrap();
    let transition = apply_outcome(&pool, &m, &outcome(CheckStatus::Down)).await.unwrap();
    assert_eq!(transition.map(|t| t.kind), Some(TransitionKind::Down), "the second counted failure");
}

/// A heartbeat created `seconds_ago` with a 30 s period and no grace, never pinged.
async fn overdue_heartbeat(pool: &sqlx::PgPool, project: Uuid) -> (Uuid, String) {
    let token = Uuid::new_v4().simple().to_string();
    let id = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         values ($1, 'Nightly backup', $2, 30, 0, now() - interval '120 seconds') returning id",
    )
    .bind(project)
    .bind(&token)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, token)
}

async fn heartbeat_status(pool: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select current_status from heartbeats where id = $1").bind(id).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn the_sweep_skips_heartbeats_of_a_project_in_maintenance(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let quiet = seed_project(&pool).await;
    let scoped = seed_project(&pool).await;
    let (covered, _) = overdue_heartbeat(&pool, quiet).await;
    let (uncovered, _) = overdue_heartbeat(&pool, scoped).await;
    seed_window_now(&pool, quiet, None).await;
    // A window scoped to a monitor never covers heartbeats.
    let scoped_monitor = seed_http_monitor(&pool, scoped, "API", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, scoped, Some(scoped_monitor)).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);

    assert_eq!(heartbeat_status(&pool, covered).await, "pending");
    assert_eq!(heartbeat_status(&pool, uncovered).await, "down");
}

#[sqlx::test]
async fn a_ping_inside_a_window_is_recorded_but_resolves_nothing(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let recipient = unique_email("oncall");
    let user = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, user, "viewer").await;
    let (id, token) = overdue_heartbeat(&pool, project).await;
    sweep_heartbeats(&state).await.unwrap();
    seed_window_now(&pool, project, None).await;

    let (status, _) = call(&app, "POST", &format!("/api/v1/heartbeat/{token}"), None, None).await;

    assert_eq!(status, StatusCode::OK);
    let (current, pinged): (String, bool) =
        sqlx::query_as("select current_status, last_ping_at is not null from heartbeats where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((current.as_str(), pinged), ("down", true));
    let open: i64 = sqlx::query_scalar("select count(*) from incidents where heartbeat_id = $1 and status = 'open'")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(open, 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1, "only the DOWN email, no recovery");
}

#[sqlx::test]
async fn reads_say_whether_a_monitor_or_heartbeat_is_in_maintenance(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let covered = seed_http_monitor(&pool, project, "Covered", "https://example.com", 2).await;
    let free = seed_http_monitor(&pool, project, "Free", "https://example.com", 2).await;
    let (heartbeat, _) = overdue_heartbeat(&pool, project).await;
    seed_window_now(&pool, project, Some(covered)).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (_, detail) = call(&app, "GET", &format!("/projects/{project}/monitors/{covered}"), Some(&viewer), None).await;
    assert_eq!(detail["in_maintenance"], true);
    let (_, list) = call(&app, "GET", &format!("/projects/{project}/monitors"), Some(&viewer), None).await;
    let flag = |id: Uuid| list.as_array().unwrap().iter().find(|m| m["id"] == id.to_string()).unwrap()["in_maintenance"].clone();
    assert_eq!((flag(covered), flag(free)), (json!(true), json!(false)));
    let (_, hb) = call(&app, "GET", &format!("/projects/{project}/heartbeats/{heartbeat}"), Some(&viewer), None).await;
    assert_eq!(hb["in_maintenance"], false, "a monitor-scoped window does not cover heartbeats");

    seed_window_now(&pool, project, None).await;
    let (_, hbs) = call(&app, "GET", &format!("/projects/{project}/heartbeats"), Some(&viewer), None).await;
    assert_eq!(hbs[0]["in_maintenance"], true);
    let (_, free_detail) = call(&app, "GET", &format!("/projects/{project}/monitors/{free}"), Some(&viewer), None).await;
    assert_eq!(free_detail["in_maintenance"], true, "a project-wide window covers every monitor");
}
```

`tests/migrations.rs`:

```diff
diff --git a/nami-engine/tests/migrations.rs b/nami-engine/tests/migrations.rs
--- a/nami-engine/tests/migrations.rs
+++ b/nami-engine/tests/migrations.rs
@@ -18,6 +18,7 @@ async fn all_tables_exist(pool: sqlx::PgPool) {
         "monitors",
         "incidents",
         "heartbeats",
+        "maintenance_windows",
     ] {
         assert!(names.contains(&expected.to_string()), "missing table {expected}");
     }
```

- [ ] **Step 3: Run them to see them fail**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test maintenance_windows`
Expected: FAIL — the tests compile; they fail at runtime with `relation "maintenance_windows" does not exist` and 404s from the missing routes.

- [ ] **Step 4: Add the migration**

`nami-engine/migrations/0017_create_maintenance_windows.sql`:

```sql
-- Lets other tables reference a monitor together with its project, so the
-- database itself refuses a reference to another project's monitor.
alter table monitors add constraint monitors_id_project_key unique (id, project_id);

create table maintenance_windows (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    -- null = every monitor and heartbeat of the project
    monitor_id uuid,
    title text not null,
    kind text not null check (kind in ('once', 'weekly')),
    -- kind = 'once'
    starts_at timestamptz,
    ends_at timestamptz,
    -- kind = 'weekly': on each ISO weekday (1 = Monday ... 7 = Sunday) the
    -- window runs from start_time to end_time, wall-clock time in timezone;
    -- an end_time at or before start_time ends on the following day.
    days_of_week smallint[],
    start_time time,
    end_time time,
    timezone text,
    created_at timestamptz not null default now(),
    foreign key (monitor_id, project_id) references monitors (id, project_id) on delete cascade,
    check (
        (kind = 'once'
            and starts_at is not null and ends_at is not null and ends_at > starts_at
            and days_of_week is null and start_time is null and end_time is null and timezone is null)
        or (kind = 'weekly'
            and starts_at is null and ends_at is null
            and cardinality(days_of_week) > 0 and days_of_week <@ array[1, 2, 3, 4, 5, 6, 7]::smallint[]
            and start_time is not null and end_time is not null and timezone is not null)
    )
);

create index maintenance_windows_project_idx on maintenance_windows (project_id);
```

- [ ] **Step 5: The model: schedule coverage, validation and queries**

`nami-engine/src/models/maintenance.rs` (the DST, wrap and day-boundary unit tests sit at the bottom):

```rust
//! Maintenance windows: while one covers a monitor or heartbeat, checks
//! still run and are recorded, but no incident opens or resolves, no alert
//! is sent, and counters do not move (see `uptime::worker::apply_outcome`
//! and `uptime::heartbeat`). A window is either one-off (`starts_at` to
//! `ends_at`) or weekly (wall-clock times on chosen ISO weekdays in an IANA
//! time zone).

use std::str::FromStr;

use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{nullable, validate_label};
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct MaintenanceWindow {
    pub id: Uuid,
    pub project_id: Uuid,
    /// `None`: every monitor and heartbeat of the project.
    pub monitor_id: Option<Uuid>,
    pub title: String,
    /// `once` or `weekly`.
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    /// ISO weekdays, 1 = Monday ... 7 = Sunday.
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub timezone: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// When a window is in effect, independent of how it is stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Schedule {
    Once { starts_at: DateTime<Utc>, ends_at: DateTime<Utc> },
    Weekly { days: Vec<Weekday>, start: NaiveTime, end: NaiveTime, timezone: Tz },
}

impl Schedule {
    /// Whether the window is in effect at `at`.
    ///
    /// - One-off: `starts_at <= at < ends_at`.
    /// - Weekly: compares wall-clock time in `timezone`. On each listed day
    ///   the window runs from `start` (inclusive) to `end` (exclusive); an
    ///   `end` at or before `start` runs past midnight into the next day
    ///   (`start == end` is a full 24 hours). Because it compares what a
    ///   clock on the wall shows, a window inside an hour skipped by a DST
    ///   change does not happen that day, and one across a repeated hour
    ///   lasts an hour longer.
    pub fn covers(&self, at: DateTime<Utc>) -> bool {
        match self {
            Schedule::Once { starts_at, ends_at } => *starts_at <= at && at < *ends_at,
            Schedule::Weekly { days, start, end, timezone } => {
                let local = at.with_timezone(timezone);
                let time = local.time();
                let today = local.weekday();
                let wraps = end <= start;
                let started_today = days.contains(&today) && time >= *start && (wraps || time < *end);
                let carried_over = wraps && days.contains(&today.pred()) && time < *end;
                started_today || carried_over
            }
        }
    }
}

/// ISO weekday number (1 = Monday ... 7 = Sunday).
pub fn weekday_from_iso(day: i16) -> Option<Weekday> {
    u8::try_from(day).ok().filter(|d| (1..=7).contains(d)).and_then(|d| Weekday::try_from(d - 1).ok())
}

impl MaintenanceWindow {
    /// `None` for a row whose fields do not form a schedule. The table's
    /// check constraint rules that out; should it happen anyway, such a
    /// window covers nothing.
    pub fn schedule(&self) -> Option<Schedule> {
        match self.kind.as_str() {
            "once" => Some(Schedule::Once { starts_at: self.starts_at?, ends_at: self.ends_at? }),
            "weekly" => Some(Schedule::Weekly {
                days: self.days_of_week.as_ref()?.iter().map(|d| weekday_from_iso(*d)).collect::<Option<_>>()?,
                start: self.start_time?,
                end: self.end_time?,
                timezone: Tz::from_str(self.timezone.as_deref()?).ok()?,
            }),
            _ => None,
        }
    }

    pub fn covers(&self, at: DateTime<Utc>) -> bool {
        self.schedule().is_some_and(|schedule| schedule.covers(at))
    }

    /// Project-wide windows apply to every monitor; scoped ones to theirs.
    pub fn applies_to_monitor(&self, monitor_id: Uuid) -> bool {
        self.monitor_id.is_none_or(|id| id == monitor_id)
    }

    /// Heartbeats are only covered by project-wide windows.
    pub fn applies_to_heartbeats(&self) -> bool {
        self.monitor_id.is_none()
    }
}

/// A window as API responses show it: `active` = in effect right now.
#[derive(Debug, Serialize)]
pub struct WindowView {
    #[serde(flatten)]
    pub window: MaintenanceWindow,
    pub active: bool,
}

impl WindowView {
    pub fn new(window: MaintenanceWindow, now: DateTime<Utc>) -> WindowView {
        let active = window.covers(now);
        WindowView { window, active }
    }
}

/// `POST /projects/{id}/maintenance-windows` body.
#[derive(Debug, Deserialize)]
pub struct CreateWindowRequest {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

/// `PATCH` body: absent fields keep their value; `monitor_id: null` makes
/// the window project-wide.
#[derive(Debug, Default, Deserialize)]
pub struct PatchWindowRequest {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub monitor_id: Option<Option<Uuid>>,
    pub kind: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

/// The editable fields as received, before [`validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

impl From<CreateWindowRequest> for WindowConfig {
    fn from(r: CreateWindowRequest) -> Self {
        WindowConfig {
            title: r.title,
            monitor_id: r.monitor_id,
            kind: r.kind,
            starts_at: r.starts_at,
            ends_at: r.ends_at,
            days_of_week: r.days_of_week,
            start_time: r.start_time,
            end_time: r.end_time,
            timezone: r.timezone,
        }
    }
}

impl From<&MaintenanceWindow> for WindowConfig {
    fn from(w: &MaintenanceWindow) -> Self {
        WindowConfig {
            title: w.title.clone(),
            monitor_id: w.monitor_id,
            kind: w.kind.clone(),
            starts_at: w.starts_at,
            ends_at: w.ends_at,
            days_of_week: w.days_of_week.clone(),
            start_time: w.start_time.map(|t| t.format("%H:%M:%S").to_string()),
            end_time: w.end_time.map(|t| t.format("%H:%M:%S").to_string()),
            timezone: w.timezone.clone(),
        }
    }
}

impl PatchWindowRequest {
    pub fn apply_to(self, base: WindowConfig) -> WindowConfig {
        WindowConfig {
            title: self.title.unwrap_or(base.title),
            monitor_id: self.monitor_id.unwrap_or(base.monitor_id),
            kind: self.kind.unwrap_or(base.kind),
            starts_at: self.starts_at.or(base.starts_at),
            ends_at: self.ends_at.or(base.ends_at),
            days_of_week: self.days_of_week.or(base.days_of_week),
            start_time: self.start_time.or(base.start_time),
            end_time: self.end_time.or(base.end_time),
            timezone: self.timezone.or(base.timezone),
        }
    }
}

/// A window ready to store: the other kind's fields are `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidWindow {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub timezone: Option<String>,
}

/// "HH:MM" or "HH:MM:SS".
fn parse_time(field: &str, raw: Option<&str>) -> Result<NaiveTime, String> {
    let raw = raw.map(str::trim).unwrap_or_default();
    NaiveTime::parse_from_str(raw, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(raw, "%H:%M:%S"))
        .map_err(|_| format!("{field} must be a time of day as HH:MM"))
}

/// Fail-closed validation; the message is a 400's.
pub fn validate(c: WindowConfig) -> Result<ValidWindow, String> {
    let title = validate_label("title", &c.title)?;
    match c.kind.as_str() {
        "once" => {
            let (Some(starts_at), Some(ends_at)) = (c.starts_at, c.ends_at) else {
                return Err("starts_at and ends_at are required for one-off windows".into());
            };
            if ends_at <= starts_at {
                return Err("ends_at must be after starts_at".into());
            }
            Ok(ValidWindow {
                title,
                monitor_id: c.monitor_id,
                kind: c.kind,
                starts_at: Some(starts_at),
                ends_at: Some(ends_at),
                days_of_week: None,
                start_time: None,
                end_time: None,
                timezone: None,
            })
        }
        "weekly" => {
            let mut days = c.days_of_week.unwrap_or_default();
            if days.is_empty() || days.iter().any(|d| weekday_from_iso(*d).is_none()) {
                return Err("days_of_week must list ISO weekdays, 1 (Monday) to 7 (Sunday)".into());
            }
            days.sort_unstable();
            days.dedup();
            let start_time = parse_time("start_time", c.start_time.as_deref())?;
            let end_time = parse_time("end_time", c.end_time.as_deref())?;
            let timezone = c.timezone.as_deref().map(str::trim).unwrap_or_default();
            let Ok(zone) = Tz::from_str(timezone) else {
                return Err("timezone must be an IANA time zone name such as Europe/Berlin or UTC".into());
            };
            Ok(ValidWindow {
                title,
                monitor_id: c.monitor_id,
                kind: c.kind,
                starts_at: None,
                ends_at: None,
                days_of_week: Some(days),
                start_time: Some(start_time),
                end_time: Some(end_time),
                timezone: Some(zone.name().to_string()),
            })
        }
        _ => Err("kind must be once or weekly".into()),
    }
}

pub async fn insert_window(pool: &PgPool, project_id: Uuid, w: &ValidWindow) -> Result<MaintenanceWindow, sqlx::Error> {
    sqlx::query_as(
        "insert into maintenance_windows
             (project_id, monitor_id, title, kind, starts_at, ends_at, days_of_week, start_time, end_time, timezone)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         returning *",
    )
    .bind(project_id)
    .bind(w.monitor_id)
    .bind(&w.title)
    .bind(&w.kind)
    .bind(w.starts_at)
    .bind(w.ends_at)
    .bind(&w.days_of_week)
    .bind(w.start_time)
    .bind(w.end_time)
    .bind(&w.timezone)
    .fetch_one(pool)
    .await
}

/// Every window of the project (both kinds, active or not), newest first.
pub async fn list_for_project<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
) -> Result<Vec<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as("select * from maintenance_windows where project_id = $1 order by created_at desc, id")
        .bind(project_id)
        .fetch_all(executor)
        .await
}

/// Scoped by both ids: another project's window is not found (404).
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    window_id: Uuid,
) -> Result<Option<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as("select * from maintenance_windows where id = $1 and project_id = $2")
        .bind(window_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// True when `monitor_id` is a monitor of `project_id`.
pub async fn monitor_belongs_to<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from monitors where id = $1 and project_id = $2)")
        .bind(monitor_id)
        .bind(project_id)
        .fetch_one(executor)
        .await
}

/// Merge-validate-write under the row's lock. A `monitor_id` outside the
/// project is a 404, like any other foreign id.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    window_id: Uuid,
    patch: PatchWindowRequest,
) -> Result<Option<MaintenanceWindow>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<MaintenanceWindow> =
        sqlx::query_as("select * from maintenance_windows where id = $1 and project_id = $2 for update")
            .bind(window_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let w = validate(patch.apply_to(WindowConfig::from(&current))).map_err(AppError::Validation)?;
    if let Some(monitor_id) = w.monitor_id {
        if !monitor_belongs_to(&mut *tx, project_id, monitor_id).await? {
            return Err(AppError::NotFound);
        }
    }
    let updated: MaintenanceWindow = sqlx::query_as(
        "update maintenance_windows set
             monitor_id = $3, title = $4, kind = $5, starts_at = $6, ends_at = $7,
             days_of_week = $8, start_time = $9, end_time = $10, timezone = $11
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(window_id)
    .bind(project_id)
    .bind(w.monitor_id)
    .bind(&w.title)
    .bind(&w.kind)
    .bind(w.starts_at)
    .bind(w.ends_at)
    .bind(&w.days_of_week)
    .bind(w.start_time)
    .bind(w.end_time)
    .bind(&w.timezone)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        // The monitor was deleted between the check and the write.
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => AppError::NotFound,
        _ => AppError::Database(e),
    })?;
    tx.commit().await?;
    Ok(Some(updated))
}

pub async fn delete_in_project(pool: &PgPool, project_id: Uuid, window_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from maintenance_windows where id = $1 and project_id = $2")
        .bind(window_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Whether a window covering `monitor_id` (project-wide or scoped to it) is
/// in effect at `at`.
pub async fn monitor_in_maintenance<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    monitor_id: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let windows: Vec<MaintenanceWindow> = sqlx::query_as(
        "select * from maintenance_windows
         where project_id = $1 and (monitor_id is null or monitor_id = $2)",
    )
    .bind(project_id)
    .bind(monitor_id)
    .fetch_all(executor)
    .await?;
    Ok(windows.iter().any(|w| w.covers(at)))
}

/// Whether a project-wide window (the only kind covering heartbeats) of
/// `project_id` is in effect at `at`.
pub async fn heartbeat_in_maintenance<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let windows = project_wide_windows(executor, &[project_id]).await?;
    Ok(windows.iter().any(|w| w.covers(at)))
}

/// The project-wide windows (the ones that cover heartbeats) of `projects`.
pub async fn project_wide_windows<'e>(
    executor: impl PgExecutor<'e>,
    projects: &[Uuid],
) -> Result<Vec<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as("select * from maintenance_windows where monitor_id is null and project_id = any($1)")
        .bind(projects)
        .fetch_all(executor)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn weekly(days: &[i16], start: &str, end: &str, zone: &str) -> Schedule {
        Schedule::Weekly {
            days: days.iter().map(|d| weekday_from_iso(*d).unwrap()).collect(),
            start: NaiveTime::parse_from_str(start, "%H:%M").unwrap(),
            end: NaiveTime::parse_from_str(end, "%H:%M").unwrap(),
            timezone: Tz::from_str(zone).unwrap(),
        }
    }

    #[test]
    fn a_one_off_window_includes_its_start_and_excludes_its_end() {
        let once = Schedule::Once { starts_at: utc(2026, 10, 13, 10, 0, 0), ends_at: utc(2026, 10, 13, 12, 0, 0) };
        assert!(!once.covers(utc(2026, 10, 13, 9, 59, 59)));
        assert!(once.covers(utc(2026, 10, 13, 10, 0, 0)));
        assert!(once.covers(utc(2026, 10, 13, 11, 59, 59)));
        assert!(!once.covers(utc(2026, 10, 13, 12, 0, 0)));
    }

    #[test]
    fn a_weekly_window_uses_wall_clock_time_in_its_zone() {
        // Mondays 09:00-17:00 in Berlin (CEST = UTC+2 on 2026-10-12).
        let office = weekly(&[1], "09:00", "17:00", "Europe/Berlin");
        assert!(!office.covers(utc(2026, 10, 12, 6, 59, 59)), "08:59:59 local");
        assert!(office.covers(utc(2026, 10, 12, 7, 0, 0)), "09:00 local");
        assert!(office.covers(utc(2026, 10, 12, 14, 59, 59)), "16:59:59 local");
        assert!(!office.covers(utc(2026, 10, 12, 15, 0, 0)), "17:00 local");
        assert!(!office.covers(utc(2026, 10, 13, 8, 0, 0)), "Tuesday is not listed");
        // 09:00 UTC on that Monday is 05:00 in New York: not yet.
        assert!(!weekly(&[1], "09:00", "17:00", "America/New_York").covers(utc(2026, 10, 12, 9, 0, 0)));
    }

    #[test]
    fn a_window_ending_at_or_before_its_start_runs_past_midnight() {
        // Fridays 22:00 -> Saturday 02:00, UTC. 2026-10-16 is a Friday.
        let deploys = weekly(&[5], "22:00", "02:00", "UTC");
        assert!(!deploys.covers(utc(2026, 10, 16, 21, 59, 59)));
        assert!(deploys.covers(utc(2026, 10, 16, 22, 0, 0)));
        assert!(deploys.covers(utc(2026, 10, 16, 23, 59, 59)));
        assert!(deploys.covers(utc(2026, 10, 17, 1, 59, 59)), "Saturday morning, carried over");
        assert!(!deploys.covers(utc(2026, 10, 17, 2, 0, 0)));
        assert!(!deploys.covers(utc(2026, 10, 17, 23, 0, 0)), "Saturday itself is not listed");
        assert!(!deploys.covers(utc(2026, 10, 15, 23, 0, 0)), "neither is Thursday");
    }

    #[test]
    fn a_sunday_night_window_carries_over_into_monday() {
        // 2026-10-18 is a Sunday; Monday's weekday().pred() is Sunday.
        let backup = weekly(&[7], "23:00", "01:00", "UTC");
        assert!(backup.covers(utc(2026, 10, 18, 23, 30, 0)));
        assert!(backup.covers(utc(2026, 10, 19, 0, 30, 0)));
        assert!(!backup.covers(utc(2026, 10, 19, 1, 0, 0)));
        assert!(!backup.covers(utc(2026, 10, 19, 23, 30, 0)), "Monday night is not listed");
    }

    #[test]
    fn equal_start_and_end_is_the_whole_day() {
        let wednesday = weekly(&[3], "00:00", "00:00", "UTC"); // 2026-10-14
        assert!(!wednesday.covers(utc(2026, 10, 13, 23, 59, 59)));
        assert!(wednesday.covers(utc(2026, 10, 14, 0, 0, 0)));
        assert!(wednesday.covers(utc(2026, 10, 14, 23, 59, 59)));
        assert!(!wednesday.covers(utc(2026, 10, 15, 0, 0, 0)));
    }

    #[test]
    fn daylight_saving_spring_forward_shortens_or_skips_a_window() {
        // Berlin, Sunday 2026-03-29: 02:00 CET jumps to 03:00 CEST (01:00 UTC).
        let across = weekly(&[7], "01:30", "03:30", "Europe/Berlin");
        assert!(across.covers(utc(2026, 3, 29, 0, 45, 0)), "01:45 CET");
        assert!(across.covers(utc(2026, 3, 29, 1, 15, 0)), "03:15 CEST");
        assert!(!across.covers(utc(2026, 3, 29, 1, 30, 0)), "03:30 CEST: over after one real hour");
        let skipped = weekly(&[7], "02:00", "03:00", "Europe/Berlin");
        assert!(!skipped.covers(utc(2026, 3, 29, 0, 59, 59)), "01:59:59 CET");
        assert!(!skipped.covers(utc(2026, 3, 29, 1, 0, 0)), "03:00 CEST: the window's hour never existed");
    }

    #[test]
    fn daylight_saving_fall_back_covers_the_repeated_hour_twice() {
        // Berlin, Sunday 2026-10-25: 03:00 CEST falls back to 02:00 CET (01:00 UTC).
        let repeated = weekly(&[7], "02:00", "03:00", "Europe/Berlin");
        assert!(!repeated.covers(utc(2026, 10, 24, 23, 59, 59)), "01:59:59 CEST");
        assert!(repeated.covers(utc(2026, 10, 25, 0, 30, 0)), "02:30 CEST, first time");
        assert!(repeated.covers(utc(2026, 10, 25, 1, 30, 0)), "02:30 CET, second time");
        assert!(!repeated.covers(utc(2026, 10, 25, 2, 0, 0)), "03:00 CET");
    }

    #[test]
    fn iso_weekdays_map_monday_to_one_and_sunday_to_seven() {
        assert_eq!(weekday_from_iso(1), Some(Weekday::Mon));
        assert_eq!(weekday_from_iso(7), Some(Weekday::Sun));
        assert_eq!(weekday_from_iso(0), None);
        assert_eq!(weekday_from_iso(8), None);
        assert_eq!(weekday_from_iso(-1), None);
    }

    #[test]
    fn a_row_that_does_not_form_a_schedule_covers_nothing() {
        let broken = MaintenanceWindow {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            monitor_id: None,
            title: "broken".to_string(),
            kind: "weekly".to_string(),
            starts_at: None,
            ends_at: None,
            days_of_week: Some(vec![1, 2, 3, 4, 5, 6, 7]),
            start_time: NaiveTime::from_hms_opt(0, 0, 0),
            end_time: NaiveTime::from_hms_opt(0, 0, 0),
            timezone: Some("Mars/Olympus_Mons".to_string()),
            created_at: Utc::now(),
        };
        assert_eq!(broken.schedule(), None);
        assert!(!broken.covers(Utc::now()));
        let every_day = MaintenanceWindow { timezone: Some("UTC".to_string()), ..broken };
        assert!(every_day.covers(Utc::now()), "the same row with a real zone covers every moment");
    }
}
```

`src/models/mod.rs` (module + `validate_label`):

```diff
diff --git a/nami-engine/src/models/mod.rs b/nami-engine/src/models/mod.rs
--- a/nami-engine/src/models/mod.rs
+++ b/nami-engine/src/models/mod.rs
@@ -6,6 +6,7 @@ pub mod monitor_request;
 pub mod target;
 pub mod incident;
 pub mod heartbeat;
+pub mod maintenance;
 
 use serde::{Deserialize, Deserializer};
 
@@ -24,15 +25,20 @@ where
 /// Name rule shared by monitors and heartbeats: trimmed, 1-200 characters,
 /// and no control characters (names end up in alert email subjects).
 pub fn validate_name(raw: &str) -> Result<String, String> {
-    let name = raw.trim();
-    if name.is_empty() {
-        return Err("name is required".into());
+    validate_label("name", raw)
+}
+
+/// The same rule for any short label (`field` names it in the message).
+pub fn validate_label(field: &str, raw: &str) -> Result<String, String> {
+    let label = raw.trim();
+    if label.is_empty() {
+        return Err(format!("{field} is required"));
     }
-    if name.chars().count() > 200 {
-        return Err("name must be at most 200 characters".into());
+    if label.chars().count() > 200 {
+        return Err(format!("{field} must be at most 200 characters"));
     }
-    if name.chars().any(char::is_control) {
-        return Err("name must not contain control characters".into());
+    if label.chars().any(char::is_control) {
+        return Err(format!("{field} must not contain control characters"));
     }
-    Ok(name.to_string())
+    Ok(label.to_string())
 }
```

- [ ] **Step 6: The CRUD routes**

`nami-engine/src/routes/maintenance.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::maintenance::{self, CreateWindowRequest, PatchWindowRequest, WindowConfig, WindowView},
    state::AppState,
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
// A window or monitor id from another project is a 404.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/maintenance-windows", get(list_windows).post(create_window))
        .route(
            "/projects/{id}/maintenance-windows/{window_id}",
            get(get_window).patch(patch_window).delete(delete_window),
        )
}

async fn list_windows(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<WindowView>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let now = Utc::now();
    let windows = maintenance::list_for_project(&state.db, project_id).await?;
    Ok(Json(windows.into_iter().map(|w| WindowView::new(w, now)).collect()))
}

async fn create_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateWindowRequest>,
) -> Result<(StatusCode, Json<WindowView>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let window = maintenance::validate(WindowConfig::from(payload)).map_err(AppError::Validation)?;
    if let Some(monitor_id) = window.monitor_id {
        if !maintenance::monitor_belongs_to(&state.db, project_id, monitor_id).await? {
            return Err(AppError::NotFound);
        }
    }
    let created = maintenance::insert_window(&state.db, project_id, &window).await.map_err(|e| match &e {
        // The project (global admins skip membership) or the monitor is gone.
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => AppError::NotFound,
        _ => AppError::Database(e),
    })?;
    Ok((StatusCode::CREATED, Json(WindowView::new(created, Utc::now()))))
}

async fn get_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<WindowView>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let window = maintenance::find_in_project(&state.db, project_id, window_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(WindowView::new(window, Utc::now())))
}

async fn patch_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchWindowRequest>,
) -> Result<Json<WindowView>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let window = maintenance::update_in_project(&state.db, project_id, window_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(WindowView::new(window, Utc::now())))
}

async fn delete_window(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, window_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !maintenance::delete_in_project(&state.db, project_id, window_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}
```

```diff
diff --git a/nami-engine/src/routes/mod.rs b/nami-engine/src/routes/mod.rs
--- a/nami-engine/src/routes/mod.rs
+++ b/nami-engine/src/routes/mod.rs
@@ -4,3 +4,4 @@ pub mod projects;
 pub mod monitors;
 pub mod incidents;
 pub mod heartbeats;
+pub mod maintenance;
```

```diff
diff --git a/nami-engine/src/router.rs b/nami-engine/src/router.rs
--- a/nami-engine/src/router.rs
+++ b/nami-engine/src/router.rs
@@ -60,6 +60,7 @@ pub fn build_router(state: AppState) -> Router {
         .merge(routes::incidents::router())
         .merge(routes::heartbeats::router())
         .merge(routes::heartbeats::public_router())
+        .merge(routes::maintenance::router())
         .with_state(state)
 }
 
```

- [ ] **Step 7: Freeze the state machines inside a window**

`src/uptime/worker.rs`:

```diff
diff --git a/nami-engine/src/uptime/worker.rs b/nami-engine/src/uptime/worker.rs
--- a/nami-engine/src/uptime/worker.rs
+++ b/nami-engine/src/uptime/worker.rs
@@ -15,7 +15,7 @@ use super::{
 };
 use crate::{
     error::AppError,
-    models::{incident, monitor::Monitor},
+    models::{incident, maintenance, monitor::Monitor},
     state::AppState,
 };
 
@@ -75,6 +75,8 @@ struct MonitorState {
 ///   `down`, opens an incident and returns a `Down` transition.
 ///
 /// A monitor deleted or paused while its check was in flight is left alone.
+/// Inside a maintenance window only `last_checked_at` moves: no status or
+/// counter change, no incident opened or resolved, no transition.
 pub async fn apply_outcome(
     pool: &PgPool,
     monitor: &Monitor,
@@ -95,6 +97,17 @@ pub async fn apply_outcome(
     if state.paused {
         return Ok(None);
     }
+    if maintenance::monitor_in_maintenance(&mut *tx, state.project_id, monitor.id, outcome.checked_at).await? {
+        // The check itself is recorded (ClickHouse history, last_checked_at);
+        // the state machine stands still until the window ends.
+        sqlx::query("update monitors set last_checked_at = $2 where id = $1")
+            .bind(monitor.id)
+            .bind(outcome.checked_at)
+            .execute(&mut *tx)
+            .await?;
+        tx.commit().await?;
+        return Ok(None);
+    }
 
     let transition = match outcome.status {
         CheckStatus::Up => {
```

`src/uptime/heartbeat.rs` — pings and the sweep (the sweep now locks the overdue rows and decides coverage in Rust, keeping the "two sweeps never flip the same heartbeat" guarantee):

```diff
diff --git a/nami-engine/src/uptime/heartbeat.rs b/nami-engine/src/uptime/heartbeat.rs
--- a/nami-engine/src/uptime/heartbeat.rs
+++ b/nami-engine/src/uptime/heartbeat.rs
@@ -7,7 +7,7 @@ use sqlx::PgPool;
 use uuid::Uuid;
 
 use super::alerts::{Subject, Transition, TransitionKind};
-use crate::models::incident;
+use crate::models::{incident, maintenance};
 
 #[derive(Debug, PartialEq)]
 pub enum PingOutcome {
@@ -18,9 +18,10 @@ pub enum PingOutcome {
 /// Records a ping in one transaction, under the heartbeat's row lock so a
 /// ping and a concurrent sweep apply one after the other.
 ///
-/// Paused heartbeats keep accepting pings — `last_ping_at` stays fresh, so
-/// resuming does not immediately trip the sweep — but their state is frozen:
-/// no status change, no incident change, no alert.
+/// Paused heartbeats, and heartbeats inside a project-wide maintenance
+/// window, keep accepting pings — `last_ping_at` stays fresh, so resuming
+/// (or the window ending) does not immediately trip the sweep — but their
+/// state is frozen: no status change, no incident change, no alert.
 pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx::Error> {
     let mut tx = pool.begin().await?;
     let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
@@ -33,7 +34,7 @@ pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx
         return Ok(PingOutcome::UnknownToken);
     };
 
-    if paused {
+    if paused || maintenance::heartbeat_in_maintenance(&mut *tx, project_id, Utc::now()).await? {
         sqlx::query("update heartbeats set last_ping_at = now() where id = $1")
             .bind(id)
             .execute(&mut *tx)
@@ -67,28 +68,39 @@ pub async fn record_ping(pool: &PgPool, token: &str) -> Result<PingOutcome, sqlx
 
 /// Marks every unpaused heartbeat whose last ping (or creation, if it never
 /// pinged) is older than `period + grace` as down and opens its incident,
-/// in one transaction. The `current_status <> 'down'` guard is re-checked
-/// under each row's lock, so two concurrent sweeps cannot both flip (and
-/// alert on) the same heartbeat. The interval is computed in float8 so a
-/// huge period plus grace cannot overflow int4 and break the sweep for
-/// every heartbeat.
+/// in one transaction — except heartbeats of a project inside a
+/// project-wide maintenance window, which stay as they are until it ends.
+/// The overdue rows are locked (`for update`): a concurrent sweep waits,
+/// then re-checks `current_status <> 'down'` against the committed row, so
+/// two sweeps cannot both flip (and alert on) the same heartbeat. The
+/// interval is computed in float8 so a huge period plus grace cannot
+/// overflow int4 and break the sweep for every heartbeat.
 pub async fn sweep_overdue(pool: &PgPool) -> Result<Vec<Transition>, sqlx::Error> {
     let mut tx = pool.begin().await?;
     let overdue: Vec<(Uuid, Uuid, String, i32, i32)> = sqlx::query_as(
-        "update heartbeats set current_status = 'down'
+        "select id, project_id, name, period_seconds, grace_seconds from heartbeats
          where not paused
            and current_status <> 'down'
            and coalesce(last_ping_at, created_at)
                + make_interval(secs => period_seconds::float8 + grace_seconds::float8) < now()
-         returning id, project_id, name, period_seconds, grace_seconds",
+         for update",
     )
     .fetch_all(&mut *tx)
     .await?;
-    // now() is the transaction's start time: the same instant the UPDATE used.
+    // now() is the transaction's start time: the same instant the query used.
     let now: DateTime<Utc> = sqlx::query_scalar("select now()").fetch_one(&mut *tx).await?;
+    let projects: Vec<Uuid> = overdue.iter().map(|(_, project_id, ..)| *project_id).collect();
+    let windows = maintenance::project_wide_windows(&mut *tx, &projects).await?;
 
     let mut transitions = Vec::new();
     for (id, project_id, name, period, grace) in overdue {
+        if windows.iter().any(|w| w.project_id == project_id && w.covers(now)) {
+            continue;
+        }
+        sqlx::query("update heartbeats set current_status = 'down' where id = $1")
+            .bind(id)
+            .execute(&mut *tx)
+            .await?;
         let cause = format!("no ping received within {period}s (+{grace}s grace)");
         let opened = incident::open_for_heartbeat(
             &mut tx,
```

- [ ] **Step 8: `in_maintenance` on monitor and heartbeat reads**

`src/models/monitor.rs`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -78,24 +78,26 @@ pub const DNS_RECORD_TYPES: [&str; 6] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"
 pub const MAX_EXPECTED_VALUE_CHARS: usize = 1024;
 
 /// A monitor as every API response shows it: the secrets are reduced to
-/// `request_headers` (names, `value_set: true`) and `has_auth_password`.
+/// `request_headers` (names, `value_set: true`) and `has_auth_password`,
+/// and `in_maintenance` says whether a maintenance window covers it now.
 #[derive(Debug, Clone, PartialEq, Serialize)]
 pub struct MonitorView {
     #[serde(flatten)]
     pub monitor: Monitor,
     pub request_headers: Vec<HeaderView>,
     pub has_auth_password: bool,
+    pub in_maintenance: bool,
 }
 
-impl From<Monitor> for MonitorView {
-    fn from(monitor: Monitor) -> Self {
+impl MonitorView {
+    pub fn new(monitor: Monitor, in_maintenance: bool) -> MonitorView {
         let request_headers = monitor
             .request_header_names
             .iter()
             .map(|name| HeaderView { name: name.clone(), value_set: true })
             .collect();
         let has_auth_password = monitor.auth_password_enc.is_some();
-        MonitorView { monitor, request_headers, has_auth_password }
+        MonitorView { monitor, request_headers, has_auth_password, in_maintenance }
     }
 }
 
```

`src/routes/monitors.rs`:

```diff
diff --git a/nami-engine/src/routes/monitors.rs b/nami-engine/src/routes/monitors.rs
--- a/nami-engine/src/routes/monitors.rs
+++ b/nami-engine/src/routes/monitors.rs
@@ -13,7 +13,10 @@ use uuid::Uuid;
 use crate::{
     error::AppError,
     middleware::rbac::{require_project_role, CurrentUser},
-    models::monitor::{self, CreateMonitorRequest, MonitorView, PatchMonitorRequest},
+    models::{
+        maintenance::{self, MaintenanceWindow},
+        monitor::{self, CreateMonitorRequest, Monitor, MonitorView, PatchMonitorRequest},
+    },
     state::AppState,
     uptime::store::{self, MonitorStats, UptimeCheckRow},
 };
@@ -32,6 +35,18 @@ pub fn router() -> Router<AppState> {
         .route("/projects/{id}/monitors/{monitor_id}/stats", get(get_stats))
 }
 
+/// `MonitorView` with `in_maintenance` decided against `windows` (the
+/// monitor's project's) at `now`.
+fn view(monitor: Monitor, windows: &[MaintenanceWindow], now: chrono::DateTime<Utc>) -> MonitorView {
+    let in_maintenance = windows.iter().any(|w| w.applies_to_monitor(monitor.id) && w.covers(now));
+    MonitorView::new(monitor, in_maintenance)
+}
+
+async fn view_one(state: &AppState, monitor: Monitor) -> Result<MonitorView, AppError> {
+    let windows = maintenance::list_for_project(&state.db, monitor.project_id).await?;
+    Ok(view(monitor, &windows, Utc::now()))
+}
+
 /// A monitor plus its 24-hour uptime (`null` when it has no checks in the
 /// window, or when ClickHouse is unavailable).
 #[derive(Serialize)]
@@ -48,6 +63,8 @@ async fn list_monitors(
 ) -> Result<Json<Vec<MonitorSummary>>, AppError> {
     require_project_role(&state.db, &user, project_id, "viewer").await?;
     let monitors = monitor::list_for_project(&state.db, project_id).await?;
+    let windows = maintenance::list_for_project(&state.db, project_id).await?;
+    let now = Utc::now();
     // Uptime decorates this list; current status comes from Postgres. If
     // ClickHouse is down, still list every monitor, with uptime unknown.
     let uptime = match store::uptime_24h_by_monitor(&state.clickhouse, project_id, Utc::now()).await {
@@ -62,7 +79,7 @@ async fn list_monitors(
             .into_iter()
             .map(|monitor| {
                 let uptime_24h = uptime.get(&monitor.id).copied();
-                MonitorSummary { monitor: MonitorView::from(monitor), uptime_24h }
+                MonitorSummary { monitor: view(monitor, &windows, now), uptime_24h }
             })
             .collect(),
     ))
@@ -86,7 +103,7 @@ async fn create_monitor(
             }
             _ => AppError::Database(e),
         })?;
-    Ok((StatusCode::CREATED, Json(MonitorView::from(created))))
+    Ok((StatusCode::CREATED, Json(view_one(&state, created).await?)))
 }
 
 async fn get_monitor(
@@ -98,7 +115,7 @@ async fn get_monitor(
     let found = monitor::find_in_project(&state.db, project_id, monitor_id)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(MonitorView::from(found)))
+    Ok(Json(view_one(&state, found).await?))
 }
 
 async fn patch_monitor(
@@ -111,7 +128,7 @@ async fn patch_monitor(
     let updated = monitor::update_in_project(&state.db, &state.cipher, project_id, monitor_id, payload)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(MonitorView::from(updated)))
+    Ok(Json(view_one(&state, updated).await?))
 }
 
 async fn delete_monitor(
```

`src/models/heartbeat.rs`:

```diff
diff --git a/nami-engine/src/models/heartbeat.rs b/nami-engine/src/models/heartbeat.rs
--- a/nami-engine/src/models/heartbeat.rs
+++ b/nami-engine/src/models/heartbeat.rs
@@ -22,6 +22,15 @@ pub struct Heartbeat {
     pub created_at: DateTime<Utc>,
 }
 
+/// A heartbeat as API responses show it: `in_maintenance` says whether a
+/// project-wide maintenance window covers it now.
+#[derive(Debug, Clone, PartialEq, Serialize)]
+pub struct HeartbeatView {
+    #[serde(flatten)]
+    pub heartbeat: Heartbeat,
+    pub in_maintenance: bool,
+}
+
 /// The user-editable fields of a heartbeat.
 #[derive(Debug, Clone, PartialEq)]
 pub struct HeartbeatConfig {
```

`src/routes/heartbeats.rs`:

```diff
diff --git a/nami-engine/src/routes/heartbeats.rs b/nami-engine/src/routes/heartbeats.rs
--- a/nami-engine/src/routes/heartbeats.rs
+++ b/nami-engine/src/routes/heartbeats.rs
@@ -4,13 +4,17 @@ use axum::{
     routing::get,
     Json, Router,
 };
+use chrono::Utc;
 use serde_json::{json, Value};
 use uuid::Uuid;
 
 use crate::{
     error::AppError,
     middleware::rbac::{require_project_role, CurrentUser},
-    models::heartbeat::{self, CreateHeartbeatRequest, Heartbeat, PatchHeartbeatRequest},
+    models::{
+        heartbeat::{self, CreateHeartbeatRequest, Heartbeat, HeartbeatView, PatchHeartbeatRequest},
+        maintenance::{self, MaintenanceWindow},
+    },
     state::AppState,
     uptime::{alerts, heartbeat::{record_ping, PingOutcome}},
 };
@@ -25,6 +29,19 @@ pub fn router() -> Router<AppState> {
         )
 }
 
+/// `HeartbeatView` with `in_maintenance` decided against `windows` (the
+/// heartbeat's project's).
+fn view(heartbeat: Heartbeat, windows: &[MaintenanceWindow]) -> HeartbeatView {
+    let now = Utc::now();
+    let in_maintenance = windows.iter().any(|w| w.applies_to_heartbeats() && w.covers(now));
+    HeartbeatView { heartbeat, in_maintenance }
+}
+
+async fn view_one(state: &AppState, heartbeat: Heartbeat) -> Result<HeartbeatView, AppError> {
+    let windows = maintenance::list_for_project(&state.db, heartbeat.project_id).await?;
+    Ok(view(heartbeat, &windows))
+}
+
 /// The public ping endpoint. No session: the token in the path is the
 /// secret, and cron jobs call the engine directly.
 pub fn public_router() -> Router<AppState> {
@@ -35,9 +52,11 @@ async fn list_heartbeats(
     State(state): State<AppState>,
     user: CurrentUser,
     Path(project_id): Path<Uuid>,
-) -> Result<Json<Vec<Heartbeat>>, AppError> {
+) -> Result<Json<Vec<HeartbeatView>>, AppError> {
     require_project_role(&state.db, &user, project_id, "viewer").await?;
-    Ok(Json(heartbeat::list_for_project(&state.db, project_id).await?))
+    let heartbeats = heartbeat::list_for_project(&state.db, project_id).await?;
+    let windows = maintenance::list_for_project(&state.db, project_id).await?;
+    Ok(Json(heartbeats.into_iter().map(|h| view(h, &windows)).collect()))
 }
 
 async fn create_heartbeat(
@@ -45,7 +64,7 @@ async fn create_heartbeat(
     user: CurrentUser,
     Path(project_id): Path<Uuid>,
     Json(payload): Json<CreateHeartbeatRequest>,
-) -> Result<(StatusCode, Json<Heartbeat>), AppError> {
+) -> Result<(StatusCode, Json<HeartbeatView>), AppError> {
     require_project_role(&state.db, &user, project_id, "member").await?;
     let config = heartbeat::validate(payload.into_config()).map_err(AppError::Validation)?;
     let created = heartbeat::insert_heartbeat(&state.db, project_id, &config)
@@ -57,19 +76,19 @@ async fn create_heartbeat(
             }
             _ => AppError::Database(e),
         })?;
-    Ok((StatusCode::CREATED, Json(created)))
+    Ok((StatusCode::CREATED, Json(view_one(&state, created).await?)))
 }
 
 async fn get_heartbeat(
     State(state): State<AppState>,
     user: CurrentUser,
     Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
-) -> Result<Json<Heartbeat>, AppError> {
+) -> Result<Json<HeartbeatView>, AppError> {
     require_project_role(&state.db, &user, project_id, "viewer").await?;
     let found = heartbeat::find_in_project(&state.db, project_id, heartbeat_id)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(found))
+    Ok(Json(view_one(&state, found).await?))
 }
 
 async fn patch_heartbeat(
@@ -77,12 +96,12 @@ async fn patch_heartbeat(
     user: CurrentUser,
     Path((project_id, heartbeat_id)): Path<(Uuid, Uuid)>,
     Json(payload): Json<PatchHeartbeatRequest>,
-) -> Result<Json<Heartbeat>, AppError> {
+) -> Result<Json<HeartbeatView>, AppError> {
     require_project_role(&state.db, &user, project_id, "member").await?;
     let updated = heartbeat::update_in_project(&state.db, project_id, heartbeat_id, payload)
         .await?
         .ok_or(AppError::NotFound)?;
-    Ok(Json(updated))
+    Ok(Json(view_one(&state, updated).await?))
 }
 
 async fn delete_heartbeat(
```

- [ ] **Step 9: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `maintenance_windows.rs` 11 passed, the library's `models::maintenance` unit tests 9 passed, `heartbeats.rs` / `uptime_worker.rs` / `migrations.rs` green.

- [ ] **Step 10: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/migrations/0017_create_maintenance_windows.sql \
  nami-engine/src/models/maintenance.rs nami-engine/src/models/mod.rs nami-engine/src/models/monitor.rs \
  nami-engine/src/models/heartbeat.rs nami-engine/src/routes/maintenance.rs nami-engine/src/routes/mod.rs \
  nami-engine/src/router.rs nami-engine/src/routes/monitors.rs nami-engine/src/routes/heartbeats.rs \
  nami-engine/src/uptime/worker.rs nami-engine/src/uptime/heartbeat.rs \
  nami-engine/tests/maintenance_windows.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): one-off and weekly maintenance windows (NAMI-UP-16)

While a window covers a monitor or heartbeat its checks are recorded but
no incident opens or resolves, no alert is sent and counters stay put.
Weekly windows use wall-clock time in an IANA zone (DST-aware).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17 (NAMI-UP-17): Monitor groups

Branch `feat/NAMI-UP-17-monitor-groups`, worktree `.worktrees/NAMI-UP-17/`.

**Files:**
- Create: `nami-engine/migrations/0018_create_monitor_groups.sql`, `nami-engine/src/models/monitor_group.rs`, `nami-engine/src/routes/monitor_groups.rs`
- Modify: `nami-engine/src/models/mod.rs`, `nami-engine/src/models/monitor.rs`, `nami-engine/src/routes/mod.rs`, `nami-engine/src/router.rs`, `nami-engine/src/routes/monitors.rs`
- Modify (tests): `nami-engine/tests/migrations.rs`
- Test: `nami-engine/tests/monitor_groups.rs`

**Interfaces:**
- Consumes: `models::{validate_name, nullable}`, `write_config`, `update_in_project` (Tasks 10, 16).
- Produces:
  - Table `monitor_groups (id, project_id, name, position int default 0, created_at, unique (id, project_id))`; `monitors.group_id` with the composite foreign key `(group_id, project_id) → monitor_groups (id, project_id) on delete set null (group_id)`.
  - `models::monitor_group`: `MonitorGroup` (row, `Serialize`), `CreateGroupRequest { name, position (default 0) }`, `PatchGroupRequest { name?, position? }`, `MAX_POSITION = 1_000_000`, `validate(name: &str, position: i32) -> Result<(String, i32), String>`, `insert_group`, `list_for_project` (ordered by position, name, id), `find_in_project`, `belongs_to(executor, project_id, group_id) -> Result<bool, sqlx::Error>`, `update_in_project`, `delete_in_project`.
  - Routes `GET|POST /projects/{id}/monitor-groups`, `GET|PATCH|DELETE /projects/{id}/monitor-groups/{group_id}` (viewer / member / admin).
  - `Monitor`/`MonitorConfig.group_id: Option<Uuid>` (create field, PATCH nullable; not a check change); a group outside the project is a 404 on create and PATCH. `GET /projects/{id}/monitors` stays a flat array (ruling C).

- [ ] **Step 1: Write the failing tests**

`nami-engine/tests/monitor_groups.rs`:

```rust
// Monitor groups and monitors.group_id. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{json, Value};
use uuid::Uuid;

async fn seed_group(pool: &sqlx::PgPool, project: Uuid, name: &str, position: i32) -> Uuid {
    sqlx::query_scalar("insert into monitor_groups (project_id, name, position) values ($1, $2, $3) returning id")
        .bind(project)
        .bind(name)
        .bind(position)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn group_of(pool: &sqlx::PgPool, monitor: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("select group_id from monitors where id = $1").bind(monitor).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn members_manage_groups_admins_delete_and_lists_are_ordered(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/monitor-groups");

    for (name, position) in [("Websites", 2), ("APIs", 1), ("Databases", 1)] {
        let (status, created) =
            call(&app, "POST", &collection, Some(&member), Some(json!({ "name": name, "position": position }))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
    }
    let (status, defaulted) = call(&app, "POST", &collection, Some(&member), Some(json!({ "name": "Misc" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{defaulted}");
    assert_eq!(defaulted["position"], 0);

    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    let names: Vec<&str> = list.as_array().unwrap().iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Misc", "APIs", "Databases", "Websites"], "position, then name");

    let item = format!("{collection}/{}", defaulted["id"].as_str().unwrap());
    let (status, patched) =
        call(&app, "PATCH", &item, Some(&member), Some(json!({ "name": "Other", "position": 9 }))).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!((patched["name"].clone(), patched["position"].clone()), (json!("Other"), json!(9)));

    assert_eq!(call(&app, "DELETE", &item, Some(&member), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &item, Some(&admin), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewers_read_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitor-groups");
    let item = format!("{collection}/{group}");

    assert_eq!(call(&app, "GET", &collection, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &item, Some(&viewer), None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "POST", &collection, Some(&viewer), Some(json!({ "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PATCH", &item, Some(&viewer), Some(json!({ "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &collection, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &item, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn invalid_groups_are_rejected(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (json!({ "name": " " }), "name is required"),
        (json!({ "name": "a\nb" }), "control characters"),
        (json!({ "name": "x", "position": -1 }), "position must be between 0 and 1000000"),
        (json!({ "name": "x", "position": 1_000_001 }), "position must be between 0 and 1000000"),
    ] {
        let (status, json) =
            call(&app, "POST", &format!("/projects/{project}/monitor-groups"), Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        assert!(json["error"].as_str().unwrap().contains(fragment), "{body}: {json}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitor_groups").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn monitors_join_and_leave_groups_of_their_own_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let body = json!({ "name": "Orders", "check_type": "http", "url": "https://example.com", "group_id": group });

    let (status, created) = call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&member), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["group_id"], group.to_string());
    let (_, list) = call(&app, "GET", &format!("/projects/{project}/monitors"), Some(&member), None).await;
    assert!(list.is_array(), "the list stays a flat array");
    assert_eq!(list[0]["group_id"], group.to_string());

    let item = format!("/projects/{project}/monitors/{}", created["id"].as_str().unwrap());
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour'").execute(&pool).await.unwrap();
    let (status, ungrouped) = call(&app, "PATCH", &item, Some(&member), Some(json!({ "group_id": null }))).await;
    assert_eq!(status, StatusCode::OK, "{ungrouped}");
    assert_eq!(ungrouped["group_id"], Value::Null);
    let due: bool = sqlx::query_scalar("select next_check_at <= now() from monitors").fetch_one(&pool).await.unwrap();
    assert!(!due, "regrouping is not a check change");
}

/// Security: a group of another project (or no group at all) is a 404 and
/// nothing changes; the database refuses the reference on its own too.
#[sqlx::test]
async fn groups_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign_group = seed_group(&pool, other, "Theirs", 0).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;

    let foreign_item = format!("/projects/{project}/monitor-groups/{foreign_group}");
    assert_eq!(call(&app, "GET", &foreign_item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        call(&app, "PATCH", &foreign_item, Some(&admin), Some(json!({ "name": "hijacked" }))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "DELETE", &foreign_item, Some(&admin), None).await.0, StatusCode::NOT_FOUND);

    for group in [foreign_group, Uuid::new_v4()] {
        let body = json!({ "name": "x", "check_type": "http", "url": "https://example.com", "group_id": group });
        let (status, _) = call(&app, "POST", &format!("/projects/{project}/monitors"), Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            &app,
            "PATCH",
            &format!("/projects/{project}/monitors/{monitor}"),
            Some(&admin),
            Some(json!({ "group_id": group })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1, "no monitor was created");
    assert_eq!(group_of(&pool, monitor).await, None, "the monitor was not moved");
    let name: String = sqlx::query_scalar("select name from monitor_groups where id = $1")
        .bind(foreign_group)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Theirs");
    let direct = sqlx::query("update monitors set group_id = $1 where id = $2")
        .bind(foreign_group)
        .bind(monitor)
        .execute(&pool)
        .await;
    assert!(direct.is_err(), "the composite foreign key rejects a cross-project group");
}

#[sqlx::test]
async fn deleting_a_group_ungroups_its_monitors(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    sqlx::query("update monitors set group_id = $1 where id = $2").bind(group).bind(monitor).execute(&pool).await.unwrap();
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;

    let (status, _) = call(&app, "DELETE", &format!("/projects/{project}/monitor-groups/{group}"), Some(&admin), None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(group_of(&pool, monitor).await, None, "the monitor stays, ungrouped");
    let project_kept: Uuid = sqlx::query_scalar("select project_id from monitors where id = $1")
        .bind(monitor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(project_kept, project, "only group_id was cleared");
}
```

`tests/migrations.rs`:

```diff
diff --git a/nami-engine/tests/migrations.rs b/nami-engine/tests/migrations.rs
--- a/nami-engine/tests/migrations.rs
+++ b/nami-engine/tests/migrations.rs
@@ -19,6 +19,7 @@ async fn all_tables_exist(pool: sqlx::PgPool) {
         "incidents",
         "heartbeats",
         "maintenance_windows",
+        "monitor_groups",
     ] {
         assert!(names.contains(&expected.to_string()), "missing table {expected}");
     }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test --test monitor_groups`
Expected: FAIL — the tests compile; they fail at runtime with `relation "monitor_groups" does not exist` and 404s from the missing routes.

- [ ] **Step 3: Add the migration**

`nami-engine/migrations/0018_create_monitor_groups.sql` (`on delete set null (group_id)` needs Postgres 15+; the compose file runs 17):

```sql
create table monitor_groups (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    position int not null default 0,
    created_at timestamptz not null default now(),
    -- Target of monitors' (group_id, project_id) foreign key.
    unique (id, project_id)
);

create index monitor_groups_project_idx on monitor_groups (project_id);

alter table monitors
    add column group_id uuid,
    -- The pair makes a group of another project impossible to reference;
    -- deleting a group ungroups its monitors (only group_id is cleared).
    add constraint monitors_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id);
```

- [ ] **Step 4: Model and routes for groups**

`nami-engine/src/models/monitor_group.rs`:

```rust
//! Monitor groups: named, ordered buckets for a project's monitors
//! (`monitors.group_id`). The monitors list stays flat; clients group it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::validate_name;
use crate::error::AppError;

pub const MAX_POSITION: i32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct MonitorGroup {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// Sort key: groups list by position, then name.
    pub position: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    #[serde(default)]
    pub position: i32,
}

#[derive(Debug, Default, Deserialize)]
pub struct PatchGroupRequest {
    pub name: Option<String>,
    pub position: Option<i32>,
}

/// The validated (name, position) of a group.
pub fn validate(name: &str, position: i32) -> Result<(String, i32), String> {
    let name = validate_name(name)?;
    if !(0..=MAX_POSITION).contains(&position) {
        return Err(format!("position must be between 0 and {MAX_POSITION}"));
    }
    Ok((name, position))
}

pub async fn insert_group(pool: &PgPool, project_id: Uuid, name: &str, position: i32) -> Result<MonitorGroup, sqlx::Error> {
    sqlx::query_as("insert into monitor_groups (project_id, name, position) values ($1, $2, $3) returning *")
        .bind(project_id)
        .bind(name)
        .bind(position)
        .fetch_one(pool)
        .await
}

pub async fn list_for_project(pool: &PgPool, project_id: Uuid) -> Result<Vec<MonitorGroup>, sqlx::Error> {
    sqlx::query_as("select * from monitor_groups where project_id = $1 order by position, name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Scoped by both ids: another project's group is not found (404).
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    group_id: Uuid,
) -> Result<Option<MonitorGroup>, sqlx::Error> {
    sqlx::query_as("select * from monitor_groups where id = $1 and project_id = $2")
        .bind(group_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// True when `group_id` is a group of `project_id`.
pub async fn belongs_to<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    group_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from monitor_groups where id = $1 and project_id = $2)")
        .bind(group_id)
        .bind(project_id)
        .fetch_one(executor)
        .await
}

/// Merge-validate-write under the row's lock.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    group_id: Uuid,
    patch: PatchGroupRequest,
) -> Result<Option<MonitorGroup>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<MonitorGroup> =
        sqlx::query_as("select * from monitor_groups where id = $1 and project_id = $2 for update")
            .bind(group_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let (name, position) = validate(
        patch.name.as_deref().unwrap_or(&current.name),
        patch.position.unwrap_or(current.position),
    )
    .map_err(AppError::Validation)?;
    let updated: MonitorGroup = sqlx::query_as(
        "update monitor_groups set name = $3, position = $4 where id = $1 and project_id = $2 returning *",
    )
    .bind(group_id)
    .bind(project_id)
    .bind(&name)
    .bind(position)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Deleting a group ungroups its monitors (`on delete set null (group_id)`).
pub async fn delete_in_project(pool: &PgPool, project_id: Uuid, group_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from monitor_groups where id = $1 and project_id = $2")
        .bind(group_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
```

`nami-engine/src/routes/monitor_groups.rs`:

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, CurrentUser},
    models::monitor_group::{self, CreateGroupRequest, MonitorGroup, PatchGroupRequest},
    state::AppState,
};

// Same RBAC as monitors: viewer reads, member creates/edits, admin deletes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/monitor-groups", get(list_groups).post(create_group))
        .route(
            "/projects/{id}/monitor-groups/{group_id}",
            get(get_group).patch(patch_group).delete(delete_group),
        )
}

/// Ordered by position, then name.
async fn list_groups(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<MonitorGroup>>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    Ok(Json(monitor_group::list_for_project(&state.db, project_id).await?))
}

async fn create_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(payload): Json<CreateGroupRequest>,
) -> Result<(StatusCode, Json<MonitorGroup>), AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let (name, position) = monitor_group::validate(&payload.name, payload.position).map_err(AppError::Validation)?;
    let created = monitor_group::insert_group(&state.db, project_id, &name, position).await.map_err(|e| match &e {
        // Global admins skip membership: the project may not exist.
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => AppError::NotFound,
        _ => AppError::Database(e),
    })?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MonitorGroup>, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    let group = monitor_group::find_in_project(&state.db, project_id, group_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(group))
}

async fn patch_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<PatchGroupRequest>,
) -> Result<Json<MonitorGroup>, AppError> {
    require_project_role(&state.db, &user, project_id, "member").await?;
    let group = monitor_group::update_in_project(&state.db, project_id, group_id, payload)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(group))
}

async fn delete_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, group_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    require_project_role(&state.db, &user, project_id, "admin").await?;
    if !monitor_group::delete_in_project(&state.db, project_id, group_id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}
```

```diff
diff --git a/nami-engine/src/models/mod.rs b/nami-engine/src/models/mod.rs
--- a/nami-engine/src/models/mod.rs
+++ b/nami-engine/src/models/mod.rs
@@ -2,6 +2,7 @@ pub mod user;
 pub mod token;
 pub mod project;
 pub mod monitor;
+pub mod monitor_group;
 pub mod monitor_request;
 pub mod target;
 pub mod incident;
```

```diff
diff --git a/nami-engine/src/routes/mod.rs b/nami-engine/src/routes/mod.rs
--- a/nami-engine/src/routes/mod.rs
+++ b/nami-engine/src/routes/mod.rs
@@ -5,3 +5,4 @@ pub mod monitors;
 pub mod incidents;
 pub mod heartbeats;
 pub mod maintenance;
+pub mod monitor_groups;
```

```diff
diff --git a/nami-engine/src/router.rs b/nami-engine/src/router.rs
--- a/nami-engine/src/router.rs
+++ b/nami-engine/src/router.rs
@@ -61,6 +61,7 @@ pub fn build_router(state: AppState) -> Router {
         .merge(routes::heartbeats::router())
         .merge(routes::heartbeats::public_router())
         .merge(routes::maintenance::router())
+        .merge(routes::monitor_groups::router())
         .with_state(state)
 }
 
```

- [ ] **Step 5: `group_id` on monitors**

`src/models/monitor.rs`:

```diff
diff --git a/nami-engine/src/models/monitor.rs b/nami-engine/src/models/monitor.rs
--- a/nami-engine/src/models/monitor.rs
+++ b/nami-engine/src/models/monitor.rs
@@ -10,7 +10,7 @@ use super::{
         self, seal_headers, seal_password, HeaderView, HeadersChange, PasswordChange, RequestHeader, BODY_METHODS,
         HTTP_METHODS, MAX_BODY_BYTES,
     },
-    nullable,
+    monitor_group, nullable,
     target::{parse_nameserver, validate_host},
     validate_name,
 };
@@ -69,6 +69,8 @@ pub struct Monitor {
     pub expiry_threshold_days: Option<i32>,
     pub use_tls: bool,
     pub udp_payload: Option<String>,
+    /// A `monitor_groups` row of the same project, or ungrouped.
+    pub group_id: Option<Uuid>,
 }
 
 pub const CHECK_TYPES: [&str; 11] =
@@ -132,6 +134,7 @@ pub struct MonitorConfig {
     pub expiry_threshold_days: Option<i32>,
     pub use_tls: bool,
     pub udp_payload: Option<String>,
+    pub group_id: Option<Uuid>,
 }
 
 impl From<&Monitor> for MonitorConfig {
@@ -164,14 +167,16 @@ impl From<&Monitor> for MonitorConfig {
             expiry_threshold_days: m.expiry_threshold_days,
             use_tls: m.use_tls,
             udp_payload: m.udp_payload.clone(),
+            group_id: m.group_id,
         }
     }
 }
 
 impl MonitorConfig {
     /// True when a field that changes how or when the monitor is checked
-    /// differs from `current`. Renaming, pausing and the confirmation and
-    /// recovery thresholds do not count; replacing or clearing a secret does.
+    /// differs from `current`. Renaming, pausing, regrouping and the
+    /// confirmation and recovery thresholds do not count; replacing or
+    /// clearing a secret does.
     pub fn changes_check_of(&self, current: &Monitor) -> bool {
         self.check_type != current.check_type
             || self.url != current.url
@@ -243,6 +248,7 @@ pub struct CreateMonitorRequest {
     #[serde(default)]
     pub use_tls: bool,
     pub udp_payload: Option<String>,
+    pub group_id: Option<Uuid>,
 }
 
 fn default_http_method() -> String {
@@ -314,6 +320,7 @@ impl CreateMonitorRequest {
             expiry_threshold_days: self.expiry_threshold_days,
             use_tls: self.use_tls,
             udp_payload: self.udp_payload,
+            group_id: self.group_id,
         }
     }
 }
@@ -357,6 +364,8 @@ pub struct PatchMonitorRequest {
     pub expiry_threshold_days: Option<i32>,
     pub use_tls: Option<bool>,
     pub udp_payload: Option<String>,
+    #[serde(default, deserialize_with = "nullable")]
+    pub group_id: Option<Option<Uuid>>,
 }
 
 /// A nullable PATCH field applied to its current value.
@@ -407,6 +416,7 @@ impl PatchMonitorRequest {
             expiry_threshold_days: self.expiry_threshold_days.or(base.expiry_threshold_days),
             use_tls: self.use_tls.unwrap_or(base.use_tls),
             udp_payload: self.udp_payload.or(base.udp_payload),
+            group_id: patched(self.group_id, base.group_id),
         }
     }
 }
@@ -757,7 +767,8 @@ async fn write_config(
              recovery_threshold = $26,
              dns_record_type = $27, dns_nameserver = $28, expected_value = $29,
              expiry_threshold_days = $30,
-             use_tls = $31, udp_payload = $32
+             use_tls = $31, udp_payload = $32,
+             group_id = $33
          where id = $1
          returning *",
     )
@@ -793,6 +804,7 @@ async fn write_config(
     .bind(c.expiry_threshold_days)
     .bind(c.use_tls)
     .bind(&c.udp_payload)
+    .bind(c.group_id)
     .fetch_one(&mut *conn)
     .await
 }
@@ -840,8 +852,18 @@ pub async fn update_in_project(
         return Ok(None);
     };
     let config = validate(patch.apply_to(MonitorConfig::from(&current))).map_err(AppError::Validation)?;
+    if let Some(group_id) = config.group_id.filter(|group| Some(*group) != current.group_id) {
+        // A group of another project is a 404, like any foreign id.
+        if !monitor_group::belongs_to(&mut *tx, project_id, group_id).await? {
+            return Err(AppError::NotFound);
+        }
+    }
     let reschedule = config.changes_check_of(&current);
-    let updated = write_config(&mut tx, cipher, monitor_id, &config, reschedule).await?;
+    let updated = write_config(&mut tx, cipher, monitor_id, &config, reschedule).await.map_err(|e| match &e {
+        // The group was deleted between the check and the write.
+        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => AppError::NotFound,
+        _ => AppError::Database(e),
+    })?;
     tx.commit().await?;
     Ok(Some(updated))
 }
```

`src/routes/monitors.rs`:

```diff
diff --git a/nami-engine/src/routes/monitors.rs b/nami-engine/src/routes/monitors.rs
--- a/nami-engine/src/routes/monitors.rs
+++ b/nami-engine/src/routes/monitors.rs
@@ -16,6 +16,7 @@ use crate::{
     models::{
         maintenance::{self, MaintenanceWindow},
         monitor::{self, CreateMonitorRequest, Monitor, MonitorView, PatchMonitorRequest},
+        monitor_group,
     },
     state::AppState,
     uptime::store::{self, MonitorStats, UptimeCheckRow},
@@ -93,11 +94,18 @@ async fn create_monitor(
 ) -> Result<(StatusCode, Json<MonitorView>), AppError> {
     require_project_role(&state.db, &user, project_id, "member").await?;
     let config = monitor::validate(payload.into_config()).map_err(AppError::Validation)?;
+    if let Some(group_id) = config.group_id {
+        // A group of another project is a 404, like any foreign id.
+        if !monitor_group::belongs_to(&state.db, project_id, group_id).await? {
+            return Err(AppError::NotFound);
+        }
+    }
     let created = monitor::insert_monitor(&state.db, &state.cipher, project_id, &config)
         .await
         .map_err(|e| match &e {
             // A global admin bypasses membership, so the project itself may
-            // not exist: SQLSTATE 23503 (foreign_key_violation) -> 404.
+            // not exist (or the group was just deleted): SQLSTATE 23503
+            // (foreign_key_violation) -> 404.
             sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                 AppError::NotFound
             }
```

- [ ] **Step 6: Run the tests to see them pass**

Run: `cd nami-engine && DATABASE_URL=postgres://nami:nami@localhost:5433/nami cargo test`
Expected: PASS — `monitor_groups.rs` 6 passed, `migrations.rs` green, everything else green (216 tests in the verification run).

- [ ] **Step 7: Commit**

```bash
git add nami-engine/migrations/0018_create_monitor_groups.sql nami-engine/src/models/monitor_group.rs \
  nami-engine/src/routes/monitor_groups.rs nami-engine/src/models/mod.rs nami-engine/src/routes/mod.rs \
  nami-engine/src/router.rs nami-engine/src/models/monitor.rs nami-engine/src/routes/monitors.rs \
  nami-engine/tests/monitor_groups.rs nami-engine/tests/migrations.rs
git commit -m "feat(engine): monitor groups (NAMI-UP-17)

Groups CRUD per project and monitors.group_id, enforced to the same
project by a composite foreign key; the monitors list stays flat.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 18 (NAMI-UP-18): UI — monitor form for every type, grouped list, maintenance badges

Branch `feat/NAMI-UP-18-ui-monitors-phase2`, worktree `.worktrees/NAMI-UP-18/`.

**Files:**
- Create: `nami-ui/src/lib/components/MaintenanceBadge.svelte`, `nami-ui/src/lib/components/form-inputs.test.ts`
- Modify: `nami-ui/src/lib/uptime.ts`, `nami-ui/src/lib/server/monitor-form.ts` (rewritten), `nami-ui/src/lib/components/MonitorForm.svelte` (rewritten)
- Modify (routes, under `nami-ui/src/routes/(app)/projects/[id]/`): `monitors/+page.server.ts`, `monitors/+page.svelte`, `monitors/new/+page.server.ts`, `monitors/new/+page.svelte`, `monitors/[monitorId]/edit/+page.server.ts`, `monitors/[monitorId]/edit/+page.svelte`, `monitors/[monitorId]/+page.svelte`, `heartbeats/+page.svelte`, `heartbeats/[heartbeatId]/+page.svelte`
- Test: `nami-ui/src/lib/uptime.test.ts`, `nami-ui/src/lib/server/monitor-form.test.ts`, `nami-ui/src/lib/components/form-inputs.test.ts`

**Interfaces:**
- Consumes: engine JSON from Tasks 10-17 — `MonitorView` fields (`request_headers: [{name, value_set}]`, `has_auth_password`, `in_maintenance`, every new column), `HeartbeatView.in_maintenance`, `GET /projects/{id}/monitor-groups`; PATCH semantics absent = keep / `null` = clear; Phase 1 `callEngine`, `engineErrorMessage`, `StatusBadge`.
- Produces:
  - `#lib/uptime`: `CheckType` (11 types), `HttpMethod`, `DnsRecordType`, `CHECK_TYPE_LABELS`, `HTTP_METHODS`, `BODY_METHODS`, `DNS_RECORD_TYPES`, `RequestHeaderView`, the extended `Monitor`, `MonitorGroup`, `Heartbeat.in_maintenance`; `monitorTarget(m)` for every type; `groupMonitors<M>(monitors: M[], groups: MonitorGroup[]): MonitorSection<M>[]` with `MonitorSection<M> = { group: MonitorGroup | null; monitors: M[] }`; `unescapePayload(text)`, `escapePayload(text)`.
  - `#lib/server/monitor-form`: `type FormMode = 'create' | 'edit'`; `monitorPayloadFromForm(form: FormData, mode?: FormMode): FormResult`; `parseHeaderLines(text)`; `formText`, `formWholeNumber` unchanged (Task 19 reuses them).
  - `MonitorForm.svelte` props `{ monitor?: Monitor | null; groups?: MonitorGroup[]; submitLabel: string; message?: string | null; cancelHref: string }`; `MaintenanceBadge.svelte` (no props).
  - Convention (pinned by `form-inputs.test.ts`): form inputs take initial values through `defaultValue`/`defaultChecked` or a binding, never `value={...}`/`checked={...}`.

- [ ] **Step 1: Write the failing tests**

`src/lib/uptime.test.ts` — new types, grouping, payload escapes:

```diff
diff --git a/nami-ui/src/lib/uptime.test.ts b/nami-ui/src/lib/uptime.test.ts
--- a/nami-ui/src/lib/uptime.test.ts
+++ b/nami-ui/src/lib/uptime.test.ts
@@ -5,12 +5,15 @@ import {
 	formatMs,
 	formatSeconds,
 	formatUptime,
+	escapePayload,
 	formatUtc,
+	groupMonitors,
 	monitorTarget,
 	niceTicks,
 	pingUrl,
 	statusSince,
 	timeAgo,
+	unescapePayload,
 	type Incident
 } from './uptime';
 
@@ -145,3 +148,63 @@ describe('formatSeconds', () => {
 		expect(formatSeconds(0)).toBe('0s');
 	});
 });
+
+describe('monitorTarget for every check type', () => {
+	const base = { url: null, host: 'example.com', port: 443, dns_record_type: null, dns_nameserver: null };
+	it('picks the field each type checks', () => {
+		expect(monitorTarget({ ...base, check_type: 'ping' })).toBe('example.com');
+		expect(monitorTarget({ ...base, check_type: 'domain_expiry' })).toBe('example.com');
+		expect(monitorTarget({ ...base, check_type: 'ssl_expiry' })).toBe('example.com:443');
+		expect(monitorTarget({ ...base, check_type: 'udp', port: 27015 })).toBe('example.com:27015');
+		expect(monitorTarget({ ...base, check_type: 'smtp', port: 25 })).toBe('example.com:25');
+	});
+	it('shows the record type and nameserver of a DNS monitor', () => {
+		expect(monitorTarget({ ...base, check_type: 'dns', dns_record_type: 'MX' })).toBe('MX example.com');
+		expect(monitorTarget({ ...base, check_type: 'dns', dns_record_type: 'A', dns_nameserver: '1.1.1.1:53' })).toBe(
+			'A example.com @1.1.1.1:53'
+		);
+	});
+});
+
+describe('groupMonitors', () => {
+	const group = (id: string, name: string, position: number) => ({
+		id,
+		project_id: 'p',
+		name,
+		position,
+		created_at: '2026-10-13T00:00:00Z'
+	});
+	const monitor = (id: string, group_id: string | null) => ({ id, group_id });
+
+	it('keeps the engine order of groups and puts ungrouped monitors last', () => {
+		const groups = [group('g1', 'APIs', 0), group('g2', 'Websites', 1)];
+		const sections = groupMonitors(
+			[monitor('a', 'g2'), monitor('b', null), monitor('c', 'g1'), monitor('d', 'g2')],
+			groups
+		);
+		expect(sections.map((s) => [s.group?.name ?? null, s.monitors.map((m) => m.id)])).toEqual([
+			['APIs', ['c']],
+			['Websites', ['a', 'd']],
+			[null, ['b']]
+		]);
+	});
+
+	it('drops empty groups and treats an unknown group as ungrouped', () => {
+		const sections = groupMonitors([monitor('a', 'gone')], [group('g1', 'Empty', 0)]);
+		expect(sections).toEqual([{ group: null, monitors: [monitor('a', 'gone')] }]);
+	});
+
+	it('is a single ungrouped section when the project has no groups', () => {
+		expect(groupMonitors([monitor('a', null)], [])).toEqual([{ group: null, monitors: [monitor('a', null)] }]);
+		expect(groupMonitors([], [])).toEqual([]);
+	});
+});
+
+describe('UDP payload escapes', () => {
+	it('round-trips control characters through one-line text', () => {
+		expect(unescapePayload('PING\\r\\n\\tx\\\\y')).toBe('PING\r\n\tx\\y');
+		expect(escapePayload('PING\r\n\tx\\y')).toBe('PING\\r\\n\\tx\\\\y');
+		expect(unescapePayload(escapePayload('a\\nb\n'))).toBe('a\\nb\n');
+		expect(unescapePayload('no escapes \\q')).toBe('no escapes \\q');
+	});
+});
```

`nami-ui/src/lib/server/monitor-form.test.ts` (replaces the Phase 1 file; its five cases are kept, adjusted for the request-config fields):

```ts
import { describe, expect, it } from 'vitest';
import { monitorPayloadFromForm, parseHeaderLines } from './monitor-form';

function form(fields: Record<string, string>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) data.set(key, value);
	return data;
}

/** The checkboxes the form renders checked by default. */
const defaults = { follow_redirects: 'on', verify_ssl: 'on' };

describe('monitorPayloadFromForm', () => {
	it('sends http fields and parsed numbers for an http monitor', () => {
		const result = monitorPayloadFromForm(
			form({
				...defaults,
				name: ' API ',
				check_type: 'http',
				url: ' https://example.com ',
				http_method: 'HEAD',
				expected_status_min: '200',
				expected_status_max: '299',
				interval_seconds: '30',
				timeout_seconds: '5',
				confirmation_threshold: '3',
				recovery_threshold: '2',
				host: 'ignored',
				keyword: 'ignored'
			})
		);
		expect(result).toEqual({
			ok: true,
			payload: {
				name: 'API',
				check_type: 'http',
				url: 'https://example.com',
				http_method: 'HEAD',
				follow_redirects: true,
				verify_ssl: true,
				expected_status_min: 200,
				expected_status_max: 299,
				interval_seconds: 30,
				timeout_seconds: 5,
				confirmation_threshold: 3,
				recovery_threshold: 2
			}
		});
	});

	it('sends host and port, never a url, for a tcp monitor', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'DB', check_type: 'tcp', host: 'db.internal', port: '5432', url: 'https://stale.test' })
		);
		expect(result).toEqual({ ok: true, payload: { name: 'DB', check_type: 'tcp', host: 'db.internal', port: 5432 } });
	});

	it('keeps the keyword untrimmed and defaults the mode', () => {
		const result = monitorPayloadFromForm(
			form({ name: 'Home', check_type: 'keyword', url: 'https://example.com', keyword: ' Welcome ' })
		);
		expect(result.ok && result.payload.keyword).toBe(' Welcome ');
		expect(result.ok && result.payload.keyword_mode).toBe('contains');
	});

	it('omits blank numbers so the engine defaults apply', () => {
		const result = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', interval_seconds: '' }));
		expect(result.ok && 'interval_seconds' in result.payload).toBe(false);
	});

	it('rejects non-whole numbers', () => {
		expect(monitorPayloadFromForm(form({ name: 'API', check_type: 'http', interval_seconds: '1.5' }))).toEqual({
			ok: false,
			message: 'interval_seconds must be a whole number'
		});
		expect(monitorPayloadFromForm(form({ name: 'DB', check_type: 'tcp', port: 'abc' }))).toEqual({
			ok: false,
			message: 'port must be a whole number'
		});
	});

	it('sends the request body only with POST, PUT or PATCH', () => {
		const post = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'POST', request_body: '{"a":1}' })
		);
		expect(post.ok && post.payload.request_body).toBe('{"a":1}');
		const get = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'GET', request_body: '{"a":1}' })
		);
		expect(get.ok && 'request_body' in get.payload).toBe(false);
	});

	it('on create sends filled-in secrets and skips empty ones', () => {
		const result = monitorPayloadFromForm(
			form({
				name: 'API',
				check_type: 'http',
				url: 'https://x.test',
				request_headers: 'X-Api-Key: abc:def\r\n\r\nX-Tenant:acme',
				auth_username: 'monitor',
				auth_password: 's3cret'
			})
		);
		expect(result.ok && result.payload.request_headers).toEqual([
			{ name: 'X-Api-Key', value: 'abc:def' },
			{ name: 'X-Tenant', value: 'acme' }
		]);
		expect(result.ok && result.payload.auth_username).toBe('monitor');
		expect(result.ok && result.payload.auth_password).toBe('s3cret');

		const empty = monitorPayloadFromForm(form({ name: 'API', check_type: 'http', url: 'https://x.test', auth_password: '' }));
		expect(empty.ok && ['request_headers', 'auth_username', 'auth_password'].some((k) => k in empty.payload)).toBe(false);
	});

	it('on edit keeps, replaces or clears each stored secret as chosen', () => {
		const base = { name: 'API', check_type: 'http', url: 'https://x.test', auth_username: 'monitor' };
		const keep = monitorPayloadFromForm(
			form({ ...base, headers_action: 'keep', password_action: 'keep', request_headers: 'X-Ignored: 1' }),
			'edit'
		);
		expect(keep.ok && 'request_headers' in keep.payload).toBe(false);
		expect(keep.ok && 'auth_password' in keep.payload).toBe(false);

		const replace = monitorPayloadFromForm(
			form({ ...base, headers_action: 'replace', request_headers: 'X-New: 2', password_action: 'replace', auth_password: 'n3w' }),
			'edit'
		);
		expect(replace.ok && replace.payload.request_headers).toEqual([{ name: 'X-New', value: '2' }]);
		expect(replace.ok && replace.payload.auth_password).toBe('n3w');

		const clear = monitorPayloadFromForm(form({ ...base, headers_action: 'clear', password_action: 'clear' }), 'edit');
		expect(clear.ok && clear.payload.request_headers).toBeNull();
		expect(clear.ok && clear.payload.auth_password).toBeNull();
	});

	it('on edit sends null for blank optional fields so they are cleared', () => {
		const http = monitorPayloadFromForm(
			form({ name: 'API', check_type: 'http', url: 'https://x.test', http_method: 'GET', auth_username: '', group_id: '' }),
			'edit'
		);
		expect(http.ok && http.payload.request_body).toBeNull();
		expect(http.ok && http.payload.auth_username).toBeNull();
		expect(http.ok && http.payload.group_id).toBeNull();
		const dns = monitorPayloadFromForm(
			form({ name: 'MX', check_type: 'dns', host: 'example.com', dns_record_type: 'MX', dns_nameserver: '', expected_value: '' }),
			'edit'
		);
		expect(dns.ok && dns.payload).toMatchObject({ dns_nameserver: null, expected_value: null });
	});

	it('refuses a replaced password left empty and a malformed header line', () => {
		expect(
			monitorPayloadFromForm(form({ name: 'A', check_type: 'http', password_action: 'replace', auth_password: '' }), 'edit')
		).toEqual({ ok: false, message: 'Enter the new password, or keep the current one' });
		expect(monitorPayloadFromForm(form({ name: 'A', check_type: 'http', request_headers: 'no colon here' }))).toEqual({
			ok: false,
			message: 'Header line 1 must look like "Name: value"'
		});
	});

	it('shapes every other check type', () => {
		const ok = (fields: Record<string, string>) => {
			const result = monitorPayloadFromForm(form({ name: 'M', ...fields }));
			if (!result.ok) throw new Error(result.message);
			return result.payload;
		};
		expect(ok({ check_type: 'ping', host: ' 10.0.0.1 ', port: '22' })).toEqual({ name: 'M', check_type: 'ping', host: '10.0.0.1' });
		expect(ok({ check_type: 'dns', host: 'example.com', dns_record_type: 'TXT', dns_nameserver: '1.1.1.1', expected_value: 'v=spf1' })).toEqual({
			name: 'M',
			check_type: 'dns',
			host: 'example.com',
			dns_record_type: 'TXT',
			dns_nameserver: '1.1.1.1',
			expected_value: 'v=spf1'
		});
		expect(ok({ check_type: 'ssl_expiry', host: 'example.com', port: '', expiry_threshold_days: '21' })).toEqual({
			name: 'M',
			check_type: 'ssl_expiry',
			host: 'example.com',
			expiry_threshold_days: 21
		});
		expect(ok({ check_type: 'domain_expiry', host: 'example.com', expiry_threshold_days: '45' })).toEqual({
			name: 'M',
			check_type: 'domain_expiry',
			host: 'example.com',
			expiry_threshold_days: 45
		});
		expect(ok({ check_type: 'imap', host: 'mail.example.com', port: '993', use_tls: 'on' })).toEqual({
			name: 'M',
			check_type: 'imap',
			host: 'mail.example.com',
			port: 993,
			use_tls: true
		});
		expect(ok({ check_type: 'smtp', host: 'mail.example.com' })).toEqual({
			name: 'M',
			check_type: 'smtp',
			host: 'mail.example.com',
			use_tls: false
		});
		expect(ok({ check_type: 'udp', host: 'game.example.com', port: '27015', udp_payload: 'PING\\r\\n', keyword: 'PONG' })).toEqual({
			name: 'M',
			check_type: 'udp',
			host: 'game.example.com',
			port: 27015,
			udp_payload: 'PING\r\n',
			keyword: 'PONG',
			keyword_mode: 'contains'
		});
	});

	it('sends the group, or nothing on create when ungrouped', () => {
		const grouped = monitorPayloadFromForm(form({ name: 'A', check_type: 'tcp', host: 'h', port: '1', group_id: 'g-1' }));
		expect(grouped.ok && grouped.payload.group_id).toBe('g-1');
		const ungrouped = monitorPayloadFromForm(form({ name: 'A', check_type: 'tcp', host: 'h', port: '1', group_id: '' }));
		expect(ungrouped.ok && 'group_id' in ungrouped.payload).toBe(false);
	});
});

describe('parseHeaderLines', () => {
	it('splits at the first colon and trims around it', () => {
		expect(parseHeaderLines('Authorization: Bearer a:b\nX-Empty:')).toEqual({
			ok: true,
			headers: [
				{ name: 'Authorization', value: 'Bearer a:b' },
				{ name: 'X-Empty', value: '' }
			]
		});
		expect(parseHeaderLines('  \n')).toEqual({ ok: true, headers: [] });
	});
	it('names the first malformed line', () => {
		expect(parseHeaderLines('X-Ok: 1\n: no name')).toEqual({ ok: false, message: 'Header line 2 must look like "Name: value"' });
	});
});
```

`nami-ui/src/lib/components/form-inputs.test.ts` (Review Focus; Task 19 adds the window form to the list):

```ts
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

// Review Focus: changing the check type re-renders parts of the monitor
// form, and Svelte re-applies a plain value={...} or checked={...} on such
// re-renders, wiping what the user typed — seen live: picking "SSL
// certificate expiry" cleared the monitor name. Initial values must go
// through defaultValue/defaultChecked, or a binding.
describe.each(['MonitorForm.svelte'])('%s', (file) => {
	const source = readFileSync(new URL(`./${file}`, import.meta.url), 'utf8');

	it('never sets value={...} or checked={...} on an input or textarea', () => {
		expect(source.match(/<(input|textarea)\b[^>]*\s(value|checked)=\{[^>]*>/g) ?? []).toEqual([]);
	});

	it('gives textareas their initial text through defaultValue', () => {
		expect(source).not.toMatch(/<textarea[^>]*>[^<]+<\/textarea>/);
	});
});
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-ui && npx vitest run`
Expected: FAIL — `groupMonitors`/`unescapePayload` are not exported, the new monitor-form cases fail, and `form-inputs.test.ts` lists the Phase 1 form's `value={...}` inputs.

- [ ] **Step 3: Types and helpers**

`src/lib/uptime.ts`:

```diff
diff --git a/nami-ui/src/lib/uptime.ts b/nami-ui/src/lib/uptime.ts
--- a/nami-ui/src/lib/uptime.ts
+++ b/nami-ui/src/lib/uptime.ts
@@ -3,7 +3,41 @@
 // client-rendered markup agree (and match the alert emails).
 
 export type Status = 'pending' | 'up' | 'down';
-export type CheckType = 'http' | 'keyword' | 'tcp';
+export type CheckType =
+	| 'http'
+	| 'keyword'
+	| 'tcp'
+	| 'ping'
+	| 'dns'
+	| 'ssl_expiry'
+	| 'domain_expiry'
+	| 'smtp'
+	| 'pop3'
+	| 'imap'
+	| 'udp';
+export type HttpMethod = 'GET' | 'HEAD' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'OPTIONS';
+export type DnsRecordType = 'A' | 'AAAA' | 'CNAME' | 'MX' | 'TXT' | 'NS';
+
+export const CHECK_TYPE_LABELS: Record<CheckType, string> = {
+	http: 'HTTP',
+	keyword: 'Keyword',
+	tcp: 'TCP',
+	ping: 'Ping',
+	dns: 'DNS',
+	ssl_expiry: 'SSL expiry',
+	domain_expiry: 'Domain expiry',
+	smtp: 'SMTP',
+	pop3: 'POP3',
+	imap: 'IMAP',
+	udp: 'UDP'
+};
+
+export const HTTP_METHODS: HttpMethod[] = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'];
+export const BODY_METHODS: HttpMethod[] = ['POST', 'PUT', 'PATCH'];
+export const DNS_RECORD_TYPES: DnsRecordType[] = ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS'];
+
+/** A custom request header as the engine returns it: the value is write-only. */
+export type RequestHeaderView = { name: string; value_set: boolean };
 
 export type Monitor = {
 	id: string;
@@ -13,7 +47,7 @@ export type Monitor = {
 	url: string | null;
 	host: string | null;
 	port: number | null;
-	http_method: 'GET' | 'HEAD';
+	http_method: HttpMethod;
 	keyword: string | null;
 	keyword_mode: 'contains' | 'not_contains';
 	expected_status_min: number;
@@ -27,6 +61,30 @@ export type Monitor = {
 	last_checked_at: string | null;
 	next_check_at: string;
 	created_at: string;
+	request_body: string | null;
+	request_headers: RequestHeaderView[];
+	auth_username: string | null;
+	has_auth_password: boolean;
+	follow_redirects: boolean;
+	verify_ssl: boolean;
+	recovery_threshold: number;
+	consecutive_successes: number;
+	dns_record_type: DnsRecordType | null;
+	dns_nameserver: string | null;
+	expected_value: string | null;
+	expiry_threshold_days: number | null;
+	use_tls: boolean;
+	udp_payload: string | null;
+	group_id: string | null;
+	in_maintenance: boolean;
+};
+
+export type MonitorGroup = {
+	id: string;
+	project_id: string;
+	name: string;
+	position: number;
+	created_at: string;
 };
 
 export type MonitorSummary = Monitor & { uptime_24h: number | null };
@@ -79,6 +137,7 @@ export type Heartbeat = {
 	current_status: Status;
 	last_ping_at: string | null;
 	created_at: string;
+	in_maintenance: boolean;
 };
 
 /** The URL a cron job pings: straight to nami-engine, not through this BFF. */
@@ -159,9 +218,63 @@ export function formatClockUtc(iso: string): string {
 	return new Date(iso).toISOString().slice(11, 16);
 }
 
-/** What the monitor checks: the URL, or host:port for TCP. */
-export function monitorTarget(m: Pick<Monitor, 'check_type' | 'url' | 'host' | 'port'>): string {
-	return m.check_type === 'tcp' ? `${m.host ?? ''}:${m.port ?? ''}` : (m.url ?? '');
+/**
+ * What the monitor checks: the URL for http/keyword, the host for ping and
+ * domain expiry, "TYPE host @nameserver" for DNS, host:port otherwise.
+ */
+export function monitorTarget(
+	m: Pick<Monitor, 'check_type' | 'url' | 'host' | 'port'> &
+		Partial<Pick<Monitor, 'dns_record_type' | 'dns_nameserver'>>
+): string {
+	switch (m.check_type) {
+		case 'http':
+		case 'keyword':
+			return m.url ?? '';
+		case 'ping':
+		case 'domain_expiry':
+			return m.host ?? '';
+		case 'dns':
+			return `${m.dns_record_type ?? 'A'} ${m.host ?? ''}${m.dns_nameserver ? ` @${m.dns_nameserver}` : ''}`;
+		default:
+			return `${m.host ?? ''}:${m.port ?? ''}`;
+	}
+}
+
+const UNESCAPES: Record<string, string> = { n: '\n', r: '\r', t: '\t', '\\': '\\' };
+const ESCAPES: Record<string, string> = { '\n': '\\n', '\r': '\\r', '\t': '\\t', '\\': '\\\\' };
+
+/** UDP payloads are typed on one line: `\n`, `\r`, `\t` and `\\` are escapes. */
+export function unescapePayload(text: string): string {
+	return text.replace(/\\([nrt\\])/g, (_, c: string) => UNESCAPES[c]);
+}
+
+/** The inverse of {@link unescapePayload}, for showing a stored payload. */
+export function escapePayload(text: string): string {
+	return text.replace(/[\\\n\r\t]/g, (c) => ESCAPES[c]);
+}
+
+export type MonitorSection<M> = { group: MonitorGroup | null; monitors: M[] };
+
+/**
+ * Monitors bucketed by group for the list page: groups in the engine's
+ * order (position, then name), then the ungrouped ones (`group: null`).
+ * Empty sections are dropped. A monitor whose group is unknown (e.g. just
+ * deleted) counts as ungrouped.
+ */
+export function groupMonitors<M extends Pick<Monitor, 'group_id'>>(
+	monitors: M[],
+	groups: MonitorGroup[]
+): MonitorSection<M>[] {
+	const known = new Set(groups.map((group) => group.id));
+	const sections: MonitorSection<M>[] = groups.map((group) => ({
+		group,
+		monitors: monitors.filter((monitor) => monitor.group_id === group.id)
+	}));
+	sections.push({
+		group: null,
+		monitors: monitors.filter((monitor) => !monitor.group_id || !known.has(monitor.group_id))
+	});
+	return sections.filter((section) => section.monitors.length > 0);
 }
 
 /** Clean y-axis ticks from 0 to a round value >= max (steps of 1/2/5 x 10^n). */
```

`nami-ui/src/lib/server/monitor-form.ts`:

```ts
// Turns the monitor form's FormData into nami-engine's JSON body. Only the
// fields of the selected check type are sent; numbers must be whole. The
// engine still validates everything — this only shapes the request and
// catches malformed input before the round trip.
//
// Secrets (custom header values, the basic-auth password) are write-only:
// on create they are sent when filled in; on edit the form says what to do
// with the stored ones — keep (field omitted), replace (new value sent) or
// clear (null sent). Blank optional fields are omitted on create (engine
// defaults apply) and sent as null on edit (the stored value is cleared).

import { unescapePayload } from '../uptime';

export type FormResult =
	| { ok: true; payload: Record<string, unknown> }
	| { ok: false; message: string };

export type FormMode = 'create' | 'edit';

const COMMON_NUMBERS = ['interval_seconds', 'timeout_seconds', 'confirmation_threshold', 'recovery_threshold'] as const;
const BODY_METHODS = ['POST', 'PUT', 'PATCH'];

export function formText(form: FormData, field: string): string {
	return String(form.get(field) ?? '').trim();
}

/** `undefined` when blank (the engine applies its default). */
export function formWholeNumber(form: FormData, field: string): number | undefined | 'invalid' {
	const raw = formText(form, field);
	if (raw === '') return undefined;
	return /^-?\d+$/.test(raw) ? Number(raw) : 'invalid';
}

/** "Name: value" per line; blank lines are skipped. */
export function parseHeaderLines(
	text: string
): { ok: true; headers: { name: string; value: string }[] } | { ok: false; message: string } {
	const headers: { name: string; value: string }[] = [];
	for (const [index, line] of text.split(/\r?\n/).entries()) {
		if (line.trim() === '') continue;
		const colon = line.indexOf(':');
		if (colon <= 0 || line.slice(0, colon).trim() === '') {
			return { ok: false, message: `Header line ${index + 1} must look like "Name: value"` };
		}
		headers.push({ name: line.slice(0, colon).trim(), value: line.slice(colon + 1).trim() });
	}
	return { ok: true, headers };
}

export function monitorPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult {
	const checkType = formText(form, 'check_type');
	const payload: Record<string, unknown> = { name: formText(form, 'name'), check_type: checkType };
	const optional = (field: string, value: string) => {
		if (value !== '') payload[field] = value;
		else if (mode === 'edit') payload[field] = null;
	};
	optional('group_id', formText(form, 'group_id'));

	const numbers: string[] = [...COMMON_NUMBERS];
	switch (checkType) {
		case 'http':
		case 'keyword': {
			const error = addRequestConfig(form, mode, checkType, payload, optional);
			if (error) return { ok: false, message: error };
			numbers.push('expected_status_min', 'expected_status_max');
			break;
		}
		case 'tcp':
			payload.host = formText(form, 'host');
			numbers.push('port');
			break;
		case 'udp': {
			payload.host = formText(form, 'host');
			numbers.push('port');
			payload.udp_payload = unescapePayload(String(form.get('udp_payload') ?? ''));
			// Not trimmed: spaces can be part of the expected reply.
			const keyword = String(form.get('keyword') ?? '');
			if (keyword !== '') {
				payload.keyword = keyword;
				payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
			}
			break;
		}
		case 'ping':
			payload.host = formText(form, 'host');
			break;
		case 'dns':
			payload.host = formText(form, 'host');
			payload.dns_record_type = formText(form, 'dns_record_type') || 'A';
			optional('dns_nameserver', formText(form, 'dns_nameserver'));
			optional('expected_value', formText(form, 'expected_value'));
			break;
		case 'ssl_expiry':
			payload.host = formText(form, 'host');
			numbers.push('port', 'expiry_threshold_days');
			break;
		case 'domain_expiry':
			payload.host = formText(form, 'host');
			numbers.push('expiry_threshold_days');
			break;
		case 'smtp':
		case 'pop3':
		case 'imap':
			payload.host = formText(form, 'host');
			payload.use_tls = form.has('use_tls');
			numbers.push('port');
			break;
	}

	for (const field of numbers) {
		const value = formWholeNumber(form, field);
		if (value === 'invalid') return { ok: false, message: `${field} must be a whole number` };
		if (value !== undefined) payload[field] = value;
	}
	return { ok: true, payload };
}

/** URL, method, body, redirects/TLS, basic auth and custom headers. */
function addRequestConfig(
	form: FormData,
	mode: FormMode,
	checkType: string,
	payload: Record<string, unknown>,
	optional: (field: string, value: string) => void
): string | null {
	payload.url = formText(form, 'url');
	const method = formText(form, 'http_method') || 'GET';
	payload.http_method = method;
	if (checkType === 'keyword') {
		// Not trimmed: leading/trailing spaces can be part of the keyword.
		payload.keyword = String(form.get('keyword') ?? '');
		payload.keyword_mode = formText(form, 'keyword_mode') || 'contains';
	}
	// The engine only accepts a body with POST/PUT/PATCH: switching the
	// method away clears a stored one.
	const body = String(form.get('request_body') ?? '');
	optional('request_body', BODY_METHODS.includes(method) ? body : '');
	payload.follow_redirects = form.has('follow_redirects');
	payload.verify_ssl = form.has('verify_ssl');
	optional('auth_username', formText(form, 'auth_username'));

	const headersText = String(form.get('request_headers') ?? '');
	const headersAction = mode === 'create' ? (headersText.trim() ? 'replace' : 'keep') : formText(form, 'headers_action');
	if (headersAction === 'replace') {
		const parsed = parseHeaderLines(headersText);
		if (!parsed.ok) return parsed.message;
		payload.request_headers = parsed.headers;
	} else if (headersAction === 'clear') {
		payload.request_headers = null;
	}

	const password = String(form.get('auth_password') ?? '');
	const passwordAction = mode === 'create' ? (password ? 'replace' : 'keep') : formText(form, 'password_action');
	if (passwordAction === 'replace') {
		if (password === '') return 'Enter the new password, or keep the current one';
		payload.auth_password = password;
	} else if (passwordAction === 'clear') {
		payload.auth_password = null;
	}
	return null;
}
```

- [ ] **Step 4: The components**

`nami-ui/src/lib/components/MaintenanceBadge.svelte`:

```svelte
<!-- Shown next to the status of a monitor or heartbeat that a maintenance
     window covers right now: its checks still run, but it cannot alert. -->
<span
	class="inline-flex items-center gap-1.5 rounded-full border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-xs text-sky-300"
	title="A maintenance window covers this right now: checks still run, but no incidents or alerts."
>
	<span class="h-1.5 w-1.5 shrink-0 rounded-full bg-sky-400" aria-hidden="true"></span>
	Maintenance
</span>
```

`nami-ui/src/lib/components/MonitorForm.svelte` — every type's fields, the "Request options" section, secrets as keep/replace/clear on edit (values never shown), recovery threshold, group picker:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import {
		BODY_METHODS,
		DNS_RECORD_TYPES,
		HTTP_METHODS,
		escapePayload,
		type CheckType,
		type DnsRecordType,
		type HttpMethod,
		type Monitor,
		type MonitorGroup
	} from '#lib/uptime';

	let {
		monitor = null,
		groups = [],
		submitLabel,
		message = null,
		cancelHref
	}: {
		monitor?: Monitor | null;
		groups?: MonitorGroup[];
		submitLabel: string;
		message?: string | null;
		cancelHref: string;
	} = $props();

	// Read once, in functions, on purpose: after mount the selects drive
	// which fields render, independent of later prop updates.
	function initialCheckType(): CheckType {
		return monitor?.check_type ?? 'http';
	}
	function initialMethod(): HttpMethod {
		return monitor?.http_method ?? 'GET';
	}
	function initialUseTls(): boolean {
		return monitor?.use_tls ?? false;
	}
	function initialKeywordMode(): 'contains' | 'not_contains' {
		return monitor?.keyword_mode ?? 'contains';
	}
	function initialRecordType(): DnsRecordType {
		return monitor?.dns_record_type ?? 'A';
	}
	function initialGroup(): string {
		return monitor?.group_id ?? '';
	}
	let checkType = $state<CheckType>(initialCheckType());
	let method = $state<HttpMethod>(initialMethod());
	let useTls = $state(initialUseTls());
	let keywordMode = $state(initialKeywordMode());
	let recordType = $state<DnsRecordType>(initialRecordType());
	let groupId = $state(initialGroup());
	// Edit only: what to do with the stored write-only secrets.
	let headersAction = $state<'keep' | 'replace' | 'clear'>('keep');
	let passwordAction = $state<'keep' | 'replace' | 'clear'>('keep');

	const editing = $derived(monitor !== null);
	const isHttp = $derived(checkType === 'http' || checkType === 'keyword');
	const isMail = $derived(checkType === 'smtp' || checkType === 'pop3' || checkType === 'imap');
	const isExpiry = $derived(checkType === 'ssl_expiry' || checkType === 'domain_expiry');
	const usesPort = $derived(['tcp', 'udp', 'ssl_expiry', 'smtp', 'pop3', 'imap'].includes(checkType));
	// Keyword checks search the body, which HEAD responses do not have.
	const methods = $derived(checkType === 'keyword' ? HTTP_METHODS.filter((m) => m !== 'HEAD') : HTTP_METHODS);
	const portPlaceholder = $derived(
		checkType === 'ssl_expiry'
			? '443'
			: checkType === 'smtp'
				? useTls ? '465' : '25'
				: checkType === 'pop3'
					? useTls ? '995' : '110'
					: checkType === 'imap'
						? useTls ? '993' : '143'
						: ''
	);
	const hostPlaceholder = $derived(
		checkType === 'dns' ? '_dmarc.example.com' : checkType === 'domain_expiry' ? 'example.com' : 'db.internal'
	);

	$effect(() => {
		if (checkType === 'keyword' && method === 'HEAD') method = 'GET';
	});

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const hint = 'mt-1 text-xs text-neutral-500';
	const radio = 'flex items-center gap-2 text-sm text-neutral-300';
	const section = 'space-y-4 rounded-lg border border-[#262626] p-4';
</script>

<!-- use:enhance keeps the typed values and the selected type on a
     validation error (the action returns fail(), so the form is not reset).
     Inputs take their initial value through defaultValue/defaultChecked:
     a plain value={...} is re-applied whenever its block re-renders (e.g.
     when the check type changes), wiping what the user typed. Selects that
     must survive such re-renders are bound to state instead. -->
<form method="POST" use:enhance class="space-y-5">
	{#if message}
		<div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">
			{message}
		</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for="name" class={label}>Name</label>
			<input id="name" name="name" type="text" required maxlength="200" defaultValue={monitor?.name ?? ''} class={input} />
		</div>
		<div>
			<label for="check_type" class={label}>Check type</label>
			<select id="check_type" name="check_type" bind:value={checkType} class={input}>
				<optgroup label="Websites and APIs">
					<option value="http">HTTP status</option>
					<option value="keyword">Keyword on page</option>
				</optgroup>
				<optgroup label="Network">
					<option value="ping">Ping (ICMP)</option>
					<option value="tcp">TCP port</option>
					<option value="udp">UDP port</option>
					<option value="dns">DNS record</option>
				</optgroup>
				<optgroup label="Email servers">
					<option value="smtp">SMTP</option>
					<option value="pop3">POP3</option>
					<option value="imap">IMAP</option>
				</optgroup>
				<optgroup label="Expiry">
					<option value="ssl_expiry">SSL certificate expiry</option>
					<option value="domain_expiry">Domain expiry</option>
				</optgroup>
			</select>
		</div>
	</div>

	{#if isHttp}
		<div class="grid gap-4 sm:grid-cols-[8rem_1fr]">
			<div>
				<label for="http_method" class={label}>Method</label>
				<select id="http_method" name="http_method" bind:value={method} class={input}>
					{#each methods as option (option)}
						<option value={option}>{option}</option>
					{/each}
				</select>
			</div>
			<div>
				<label for="url" class={label}>URL</label>
				<input id="url" name="url" type="url" required placeholder="https://example.com/health" defaultValue={monitor?.url ?? ''} class={input} />
			</div>
		</div>
		{#if checkType === 'keyword'}
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="keyword" class={label}>Keyword (case-sensitive)</label>
					<input id="keyword" name="keyword" type="text" required defaultValue={monitor?.keyword ?? ''} class={input} />
				</div>
				<div>
					<label for="keyword_mode" class={label}>Up when the page</label>
					<select id="keyword_mode" name="keyword_mode" bind:value={keywordMode} class={input}>
						<option value="contains">contains the keyword</option>
						<option value="not_contains">does not contain the keyword</option>
					</select>
				</div>
			</div>
		{/if}
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="expected_status_min" class={label}>Lowest OK status</label>
				<input id="expected_status_min" name="expected_status_min" type="number" min="100" max="599" defaultValue={monitor?.expected_status_min ?? 200} class={input} />
			</div>
			<div>
				<label for="expected_status_max" class={label}>Highest OK status</label>
				<input id="expected_status_max" name="expected_status_max" type="number" min="100" max="599" defaultValue={monitor?.expected_status_max ?? 399} class={input} />
			</div>
		</div>

		<details class={section} open={editing && (monitor?.request_headers.length || monitor?.auth_username || monitor?.request_body) ? true : undefined}>
			<summary class="cursor-pointer text-sm font-medium text-neutral-200">Request options</summary>
			{#if BODY_METHODS.includes(method)}
				<div>
					<label for="request_body" class={label}>Request body</label>
					<textarea id="request_body" name="request_body" rows="4" maxlength="65536" placeholder={'{"ping": true}'} defaultValue={monitor?.request_body ?? ''} class="{input} font-mono"></textarea>
					<p class={hint}>Sent as is, up to 64 KiB. Set a Content-Type header below if the server needs one.</p>
				</div>
			{/if}

			<fieldset class="space-y-2">
				<legend class={label}>Custom headers</legend>
				{#if editing}
					<p class="text-xs text-neutral-500">
						{#if monitor?.request_headers.length}
							Stored: {monitor.request_headers.map((h) => h.name).join(', ')} — values are never shown.
						{:else}
							No custom headers.
						{/if}
					</p>
					<div class="flex flex-wrap gap-4">
						<label class={radio}><input type="radio" name="headers_action" value="keep" bind:group={headersAction} /> Keep</label>
						<label class={radio}><input type="radio" name="headers_action" value="replace" bind:group={headersAction} /> Replace all</label>
						<label class={radio}><input type="radio" name="headers_action" value="clear" bind:group={headersAction} /> Remove all</label>
					</div>
				{/if}
				{#if !editing || headersAction === 'replace'}
					<textarea name="request_headers" rows="3" placeholder="X-Api-Key: your-key" aria-label="Custom headers, one Name: value per line" class="{input} font-mono"></textarea>
					<p class={hint}>One <code>Name: value</code> per line, up to 20. Values are encrypted and write-only.</p>
				{/if}
			</fieldset>

			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="auth_username" class={label}>Basic auth username</label>
					<input id="auth_username" name="auth_username" type="text" autocomplete="off" defaultValue={monitor?.auth_username ?? ''} class={input} />
				</div>
				<fieldset class="space-y-2">
					<legend class={label}>Basic auth password</legend>
					{#if editing}
						<div class="flex flex-wrap gap-4">
							<label class={radio}>
								<input type="radio" name="password_action" value="keep" bind:group={passwordAction} />
								Keep {monitor?.has_auth_password ? '(set)' : '(none)'}
							</label>
							<label class={radio}><input type="radio" name="password_action" value="replace" bind:group={passwordAction} /> Replace</label>
							<label class={radio}><input type="radio" name="password_action" value="clear" bind:group={passwordAction} /> Remove</label>
						</div>
					{/if}
					{#if !editing || passwordAction === 'replace'}
						<input name="auth_password" type="password" autocomplete="new-password" aria-label="Basic auth password" class={input} />
					{/if}
				</fieldset>
			</div>

			<div class="flex flex-wrap gap-6">
				<label class={radio}>
					<input type="checkbox" name="follow_redirects" defaultChecked={monitor?.follow_redirects ?? true} />
					Follow redirects (up to 10)
				</label>
				<label class={radio}>
					<input type="checkbox" name="verify_ssl" defaultChecked={monitor?.verify_ssl ?? true} />
					Verify SSL certificate
				</label>
			</div>
		</details>
	{:else}
		<div class="grid gap-4 {usesPort ? 'sm:grid-cols-[1fr_8rem]' : ''}">
			<div>
				<label for="host" class={label}>{checkType === 'domain_expiry' ? 'Domain' : 'Host'}</label>
				<input id="host" name="host" type="text" required placeholder={hostPlaceholder} defaultValue={monitor?.host ?? ''} class={input} />
			</div>
			{#if usesPort}
				<div>
					<label for="port" class={label}>Port</label>
					<input
						id="port"
						name="port"
						type="number"
						min="1"
						max="65535"
						required={checkType === 'tcp' || checkType === 'udp'}
						placeholder={portPlaceholder}
						defaultValue={monitor?.port ?? ''}
						class={input}
					/>
				</div>
			{/if}
		</div>

		{#if checkType === 'dns'}
			<div class="grid gap-4 sm:grid-cols-[8rem_1fr_1fr]">
				<div>
					<label for="dns_record_type" class={label}>Record</label>
					<select id="dns_record_type" name="dns_record_type" bind:value={recordType} class={input}>
						{#each DNS_RECORD_TYPES as type (type)}
							<option value={type}>{type}</option>
						{/each}
					</select>
				</div>
				<div>
					<label for="dns_nameserver" class={label}>Nameserver (optional)</label>
					<input id="dns_nameserver" name="dns_nameserver" type="text" placeholder="1.1.1.1 or 1.1.1.1:53" defaultValue={monitor?.dns_nameserver ?? ''} class={input} />
				</div>
				<div>
					<label for="expected_value" class={label}>An answer must contain (optional)</label>
					<input id="expected_value" name="expected_value" type="text" defaultValue={monitor?.expected_value ?? ''} class={input} />
				</div>
			</div>
		{:else if isMail}
			<label class={radio}>
				<input type="checkbox" name="use_tls" bind:checked={useTls} />
				Implicit TLS (SMTPS / POP3S / IMAPS)
			</label>
		{:else if checkType === 'udp'}
			<div>
				<label for="udp_payload" class={label}>Payload to send</label>
				<input id="udp_payload" name="udp_payload" type="text" required placeholder={'PING\\r\\n'} defaultValue={escapePayload(monitor?.udp_payload ?? '')} class="{input} font-mono" />
				<p class={hint}>Text; write \n, \r and \t for line breaks and tabs, \\ for a backslash.</p>
			</div>
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="keyword" class={label}>Reply keyword (optional)</label>
					<input id="keyword" name="keyword" type="text" defaultValue={monitor?.keyword ?? ''} class={input} />
				</div>
				<div>
					<label for="keyword_mode" class={label}>Up when the reply</label>
					<select id="keyword_mode" name="keyword_mode" bind:value={keywordMode} class={input}>
						<option value="contains">contains the keyword</option>
						<option value="not_contains">does not contain the keyword</option>
					</select>
				</div>
			</div>
		{:else if isExpiry}
			<div class="sm:w-1/2">
				<label for="expiry_threshold_days" class={label}>Alert when fewer days remain than</label>
				<input
					id="expiry_threshold_days"
					name="expiry_threshold_days"
					type="number"
					min="1"
					max="365"
					placeholder={checkType === 'ssl_expiry' ? '14' : '30'}
					defaultValue={monitor?.expiry_threshold_days ?? ''}
					class={input}
				/>
			</div>
		{/if}
	{/if}

	<div class="grid gap-4 sm:grid-cols-4">
		<div>
			<label for="interval_seconds" class={label}>Check every (seconds)</label>
			<input
				id="interval_seconds"
				name="interval_seconds"
				type="number"
				min={isExpiry ? 3600 : 30}
				placeholder={isExpiry ? '86400' : '60'}
				defaultValue={monitor?.interval_seconds ?? ''}
				class={input}
			/>
		</div>
		<div>
			<label for="timeout_seconds" class={label}>Timeout (seconds)</label>
			<input id="timeout_seconds" name="timeout_seconds" type="number" min="1" max="60" defaultValue={monitor?.timeout_seconds ?? 10} class={input} />
		</div>
		<div>
			<label for="confirmation_threshold" class={label}>Failures before alerting</label>
			<input id="confirmation_threshold" name="confirmation_threshold" type="number" min="1" max="10" defaultValue={monitor?.confirmation_threshold ?? 2} class={input} />
		</div>
		<div>
			<label for="recovery_threshold" class={label}>Successes to recover</label>
			<input id="recovery_threshold" name="recovery_threshold" type="number" min="1" max="10" defaultValue={monitor?.recovery_threshold ?? 1} class={input} />
		</div>
	</div>

	<div class="sm:w-1/2">
		<label for="group_id" class={label}>Group</label>
		<select id="group_id" name="group_id" bind:value={groupId} class={input}>
			<option value="">No group</option>
			{#each groups as group (group.id)}
				<option value={group.id}>{group.name}</option>
			{/each}
		</select>
	</div>

	<div class="flex items-center gap-3">
		<button
			type="submit"
			class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
		>
			{submitLabel}
		</button>
		<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">Cancel</a>
	</div>
</form>
```

- [ ] **Step 5: The monitor pages**

`monitors/+page.server.ts` (groups are optional: the list still renders without them):

```ts
import { error } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import type { MonitorGroup, MonitorSummary } from '#lib/uptime';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [response, groupsResponse] = await Promise.all([
		callEngine(event, `/projects/${event.params.id}/monitors`),
		callEngine(event, `/projects/${event.params.id}/monitor-groups`)
	]);
	if (!response.ok) {
		throw error(
			response.status === 403 ? 403 : 500,
			await engineErrorMessage(response, 'Could not load monitors.')
		);
	}
	const monitors: MonitorSummary[] = await response.json();
	// Groups only arrange the list: without them it still renders, flat.
	const groups: MonitorGroup[] = groupsResponse.ok ? await groupsResponse.json() : [];
	return { monitors, groups };
};
```

`monitors/+page.svelte` — client-side groups in native `<details>` sections (keyed, so a collapsed group stays collapsed across the 30 s refresh), ungrouped last, maintenance badges; unchanged look when the project has no groups:

```svelte
<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
	import StatusBadge from '#lib/components/StatusBadge.svelte';
	import { CHECK_TYPE_LABELS, formatUptime, groupMonitors, monitorTarget, timeAgo } from '#lib/uptime';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const sections = $derived(groupMonitors(data.monitors, data.groups));
	const grouped = $derived(data.groups.length > 0);

	// Re-run the load every 30 s like BetterStack's dashboard, and tick a
	// clock so "last checked" stays current in between. `refreshAll` is Kit
	// 3's replacement for the deprecated `invalidateAll`.
	let now = $state(Date.now());
	$effect(() => {
		const refresh = setInterval(() => void refreshAll(), 30_000);
		const clock = setInterval(() => (now = Date.now()), 1_000);
		return () => {
			clearInterval(refresh);
			clearInterval(clock);
		};
	});
</script>

{#snippet table(monitors: typeof data.monitors)}
	<div class="overflow-x-auto">
		<table class="w-full text-sm">
			<thead class="text-left text-xs text-neutral-500">
				<tr class="border-b border-[#262626]">
					<th class="px-6 py-2 font-medium">Status</th>
					<th class="px-6 py-2 font-medium">Name</th>
					<th class="px-6 py-2 font-medium">Type</th>
					<th class="px-6 py-2 font-medium">Target</th>
					<th class="px-6 py-2 font-medium">Uptime (24 h)</th>
					<th class="px-6 py-2 font-medium">Last checked</th>
				</tr>
			</thead>
			<tbody>
				{#each monitors as monitor (monitor.id)}
					<tr class="border-b border-[#262626] last:border-0 hover:bg-[#0a0a0a]">
						<td class="px-6 py-3">
							<div class="flex flex-wrap items-center gap-2">
								<StatusBadge status={monitor.current_status} paused={monitor.paused} />
								{#if monitor.in_maintenance}<MaintenanceBadge />{/if}
							</div>
						</td>
						<td class="px-6 py-3">
							<a
								href="/projects/{data.project.id}/monitors/{monitor.id}"
								class="font-medium text-neutral-100 hover:text-teal-300">{monitor.name}</a
							>
						</td>
						<td class="px-6 py-3 text-neutral-400">{CHECK_TYPE_LABELS[monitor.check_type]}</td>
						<td class="max-w-xs truncate px-6 py-3 font-mono text-xs text-neutral-400" title={monitorTarget(monitor)}>
							{monitorTarget(monitor)}
						</td>
						<td class="px-6 py-3 text-neutral-200 tabular-nums">{formatUptime(monitor.uptime_24h)}</td>
						<td class="px-6 py-3 text-neutral-500">{timeAgo(monitor.last_checked_at, now)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/snippet}

<div class="rounded-xl border border-[#262626] bg-[#141414]">
	<div class="flex items-center justify-between border-b border-[#262626] px-6 py-4">
		<div>
			<h2 class="text-base font-semibold text-neutral-100">Monitors</h2>
			<p class="text-xs text-neutral-500">Refreshes every 30 seconds.</p>
		</div>
		<a
			href="/projects/{data.project.id}/monitors/new"
			class="rounded-md bg-teal-500 px-3 py-1.5 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
		>
			New monitor
		</a>
	</div>

	{#if data.monitors.length === 0}
		<p class="px-6 py-10 text-center text-sm text-neutral-500">No monitors yet — create one to start checking.</p>
	{:else if !grouped}
		{@render table(data.monitors)}
	{:else}
		<!-- Native <details>: collapsing needs no script, and the keyed each
		     keeps each group's open/closed state across the 30 s refresh. -->
		{#each sections as section (section.group?.id ?? 'ungrouped')}
			{@const down = section.monitors.filter((m) => m.current_status === 'down' && !m.paused).length}
			<details open class="border-b border-[#262626] last:border-0">
				<summary class="flex cursor-pointer items-center gap-3 px-6 py-3 text-sm hover:bg-[#0a0a0a]">
					<span class="font-medium text-neutral-200">{section.group?.name ?? 'Ungrouped'}</span>
					<span class="text-xs text-neutral-500">
						{section.monitors.length} monitor{section.monitors.length === 1 ? '' : 's'}
					</span>
					{#if down > 0}<span class="text-xs text-red-400">{down} down</span>{/if}
				</summary>
				{@render table(section.monitors)}
			</details>
		{/each}
	{/if}
</div>
```

`monitors/new/+page.server.ts` and `+page.svelte`:

```ts
import { fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { MonitorGroup } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitor-groups`);
	// The group picker is optional: offer "No group" alone if this fails.
	const groups: MonitorGroup[] = response.ok ? await response.json() : [];
	return { groups };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData(), 'create');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/monitors`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not create monitor.') });
		}
		const created: { id: string } = await response.json();
		throw redirect(303, `/projects/${event.params.id}/monitors/${created.id}`);
	}
};
```

```svelte
<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">New monitor</h2>
	<div class="mt-4">
		<MonitorForm
			groups={data.groups}
			submitLabel="Create monitor"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors"
		/>
	</div>
</div>
```

`monitors/[monitorId]/edit/+page.server.ts` and `+page.svelte`:

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage } from '#lib/server/engine';
import { monitorPayloadFromForm } from '#lib/server/monitor-form';
import type { Monitor, MonitorGroup } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [response, groupsResponse] = await Promise.all([
		callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`),
		callEngine(event, `/projects/${event.params.id}/monitor-groups`)
	]);
	if (response.status === 404) throw error(404, 'Monitor not found.');
	if (!response.ok) {
		throw error(response.status === 403 ? 403 : 500, await engineErrorMessage(response, 'Could not load monitor.'));
	}
	const monitor: Monitor = await response.json();
	const groups: MonitorGroup[] = groupsResponse.ok ? await groupsResponse.json() : [];
	return { monitor, groups };
};

export const actions: Actions = {
	default: async (event) => {
		const parsed = monitorPayloadFromForm(await event.request.formData(), 'edit');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/monitors/${event.params.monitorId}`, {
			method: 'PATCH',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			return fail(response.status, { message: await engineErrorMessage(response, 'Could not save monitor.') });
		}
		throw redirect(303, `/projects/${event.params.id}/monitors/${event.params.monitorId}`);
	}
};
```

```svelte
<script lang="ts">
	import MonitorForm from '#lib/components/MonitorForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">Edit {data.monitor.name}</h2>
	<div class="mt-4">
		<MonitorForm
			monitor={data.monitor}
			groups={data.groups}
			submitLabel="Save changes"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/monitors/{data.monitor.id}"
		/>
	</div>
</div>
```

`monitors/[monitorId]/+page.svelte` (badge, type label, recovery note):

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.svelte b/nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/monitors/[monitorId]/+page.svelte
@@ -1,9 +1,11 @@
 <script lang="ts">
 	import { enhance } from '$app/forms';
 	import { refreshAll } from '$app/navigation';
+	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
 	import ResponseTimeChart from '#lib/components/ResponseTimeChart.svelte';
 	import StatusBadge from '#lib/components/StatusBadge.svelte';
 	import {
+		CHECK_TYPE_LABELS,
 		formatDuration,
 		formatMs,
 		formatUptime,
@@ -52,14 +54,21 @@
 	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
 		<div class="flex flex-wrap items-start justify-between gap-4">
 			<div>
-				<p class="text-3xl font-semibold tracking-tight {headline.class}">{headline.text}</p>
+				<div class="flex flex-wrap items-center gap-3">
+					<p class="text-3xl font-semibold tracking-tight {headline.class}">{headline.text}</p>
+					{#if data.monitor.in_maintenance}<MaintenanceBadge />{/if}
+				</div>
 				<p class="mt-1 text-sm text-neutral-500">
 					since {formatUtc(since)} ({timeAgo(since, now)})
 				</p>
 				<h2 class="mt-4 text-base font-semibold text-neutral-100">{data.monitor.name}</h2>
 				<p class="font-mono text-xs break-all text-neutral-400">{monitorTarget(data.monitor)}</p>
 				<p class="mt-1 text-xs text-neutral-500">
-					Checked every {data.monitor.interval_seconds}s · last check {timeAgo(data.monitor.last_checked_at, now)}
+					{CHECK_TYPE_LABELS[data.monitor.check_type]} · checked every {data.monitor.interval_seconds}s · last check
+					{timeAgo(data.monitor.last_checked_at, now)}
+					{#if data.monitor.recovery_threshold > 1}
+						· recovers after {data.monitor.recovery_threshold} successful checks
+					{/if}
 				</p>
 			</div>
 			<div class="flex flex-wrap gap-2">
```

Heartbeat badges — `heartbeats/+page.svelte` and `heartbeats/[heartbeatId]/+page.svelte`:

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.svelte b/nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/heartbeats/+page.svelte
@@ -2,6 +2,7 @@
 	import { enhance } from '$app/forms';
 	import { refreshAll } from '$app/navigation';
 	import CopyButton from '#lib/components/CopyButton.svelte';
+	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
 	import StatusBadge from '#lib/components/StatusBadge.svelte';
 	import { formatSeconds, pingUrl, timeAgo } from '#lib/uptime';
 	import type { PageProps } from './$types';
@@ -42,6 +43,7 @@
 						<div class="flex flex-wrap items-center justify-between gap-2">
 							<div class="flex items-center gap-4">
 								<StatusBadge status={heartbeat.current_status} paused={heartbeat.paused} />
+								{#if heartbeat.in_maintenance}<MaintenanceBadge />{/if}
 								<a
 									href="/projects/{data.project.id}/heartbeats/{heartbeat.id}"
 									class="font-medium text-neutral-100 hover:text-teal-300">{heartbeat.name}</a
```

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.svelte b/nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/heartbeats/[heartbeatId]/+page.svelte
@@ -2,6 +2,7 @@
 	import { enhance } from '$app/forms';
 	import { refreshAll } from '$app/navigation';
 	import CopyButton from '#lib/components/CopyButton.svelte';
+	import MaintenanceBadge from '#lib/components/MaintenanceBadge.svelte';
 	import StatusBadge from '#lib/components/StatusBadge.svelte';
 	import { formatSeconds, formatUtc, pingUrl, timeAgo } from '#lib/uptime';
 	import type { PageProps } from './$types';
@@ -33,7 +34,10 @@
 	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
 		<div class="flex flex-wrap items-start justify-between gap-4">
 			<div class="space-y-2">
-				<StatusBadge status={data.heartbeat.current_status} paused={data.heartbeat.paused} />
+				<div class="flex flex-wrap items-center gap-3">
+					<StatusBadge status={data.heartbeat.current_status} paused={data.heartbeat.paused} />
+					{#if data.heartbeat.in_maintenance}<MaintenanceBadge />{/if}
+				</div>
 				<h2 class="text-base font-semibold text-neutral-100">{data.heartbeat.name}</h2>
 				<p class="text-sm text-neutral-500">
 					Last ping:
```

- [ ] **Step 6: Run the checks**

Run: `cd nami-ui && npm run check && npx vitest run`
Expected: `svelte-check` 0 errors, 0 warnings; vitest 5 files, 50 tests passed.

- [ ] **Step 7: Verify live in a browser**

Start the stack (`docker compose up -d` in `nami-engine`, `cargo run` with your `.env`, `npm run dev` in `nami-ui`), log in as a project member and check, waiting for the page to hydrate before typing:
1. New monitor → type a name, pick each check type in turn: the name, timeout and group stay as typed, and each type shows its own fields.
2. Create an http POST monitor with a body, two headers (`X-Api-Key: …`) and basic auth in a group → the detail page and the page source never contain the header value or password.
3. Edit it: rename only → the engine still reports both headers and `has_auth_password: true`; "Replace" the password with an empty field → the form shows "Enter the new password, or keep the current one" and keeps your other edits; "Remove all" headers → they are gone.
4. Create dns (TXT, nameserver), ping (`127.0.0.1`, goes up), udp (payload `PING\r\n` — the edit form shows it escaped again), imap with implicit TLS (port defaults to 993) and ssl_expiry monitors.
5. The list shows the group as a collapsible section with its down count, ungrouped monitors last, and the "Maintenance" badge once a window covers a monitor (create one with the API until Task 19).

- [ ] **Step 8: Commit**

```bash
git add nami-ui/src/lib/uptime.ts nami-ui/src/lib/uptime.test.ts nami-ui/src/lib/server/monitor-form.ts \
  nami-ui/src/lib/server/monitor-form.test.ts nami-ui/src/lib/components/MonitorForm.svelte \
  nami-ui/src/lib/components/MaintenanceBadge.svelte nami-ui/src/lib/components/form-inputs.test.ts \
  'nami-ui/src/routes/(app)/projects/[id]/monitors' 'nami-ui/src/routes/(app)/projects/[id]/heartbeats'
git commit -m "feat(ui): monitor form for every check type, groups and maintenance badges (NAMI-UP-18)

Request options with write-only secrets (keep/replace/clear), recovery
threshold and group picker; the list groups monitors client-side.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 19 (NAMI-UP-19): UI — maintenance windows page and monitor-group management

Branch `feat/NAMI-UP-19-ui-maintenance-groups`, worktree `.worktrees/NAMI-UP-19/`.

**Files:**
- Create: `nami-ui/src/lib/maintenance.ts`, `nami-ui/src/lib/server/maintenance-form.ts`, `nami-ui/src/lib/components/MaintenanceWindowForm.svelte`
- Create (routes, under `nami-ui/src/routes/(app)/projects/[id]/`): `maintenance/+page.server.ts`, `maintenance/+page.svelte`, `maintenance/[windowId]/+page.server.ts`, `maintenance/[windowId]/+page.svelte`
- Modify: `nami-ui/src/lib/server/engine.ts`, `nami-ui/src/routes/(app)/projects/[id]/+layout.svelte`, `.../settings/+page.server.ts` (rewritten), `.../settings/+page.svelte`, `nami-ui/src/lib/components/form-inputs.test.ts`
- Test: `nami-ui/src/lib/maintenance.test.ts`, `nami-ui/src/lib/server/maintenance-form.test.ts`, `nami-ui/src/lib/server/engine.test.ts`, `nami-ui/src/lib/components/form-inputs.test.ts`

**Interfaces:**
- Consumes: engine routes from Tasks 16-17 (`/projects/{id}/maintenance-windows[/{window_id}]` with `active`, `/projects/{id}/monitor-groups[/{group_id}]`); `formText`, `formWholeNumber`, `FormMode`, `FormResult` (Task 18 `monitor-form.ts`); `formatUtc`, `MonitorSummary`, `MonitorGroup` (`#lib/uptime`).
- Produces:
  - `#lib/maintenance`: `WindowKind`, `MaintenanceWindow`, `WEEKDAYS`, `shortTime(time)`, `describeSchedule(window)`, `describeScope(monitorId, monitors)`, `toUtcInputValue(iso)`.
  - `#lib/server/maintenance-form`: `utcFromLocalInput(value): string | null`, `windowPayloadFromForm(form, mode?): FormResult`.
  - `#lib/server/engine`: `isUuid(value: unknown): value is string` — every id from a form field or route param is checked before it goes into an engine path.
  - `MaintenanceWindowForm.svelte` props `{ maintenanceWindow?: MaintenanceWindow | null; monitors: { id: string; name: string }[]; submitLabel: string; message?: string | null; action?: string; cancelHref?: string | null }`.
  - Pages: `/projects/[id]/maintenance` (list with "Active now", create, delete) and `/projects/[id]/maintenance/[windowId]` (edit); a "Monitor groups" card on `/projects/[id]/settings` (create, rename/reposition, delete); a "Maintenance" tab.

- [ ] **Step 1: Write the failing tests**

`nami-ui/src/lib/maintenance.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { describeSchedule, describeScope, shortTime, toUtcInputValue } from './maintenance';

const weekly = (days: number[], start: string, end: string) => ({
	kind: 'weekly' as const,
	starts_at: null,
	ends_at: null,
	days_of_week: days,
	start_time: start,
	end_time: end,
	timezone: 'Asia/Jakarta'
});

describe('describeSchedule', () => {
	it('shows a one-off window in UTC', () => {
		expect(
			describeSchedule({
				kind: 'once',
				starts_at: '2026-10-13T22:00:00Z',
				ends_at: '2026-10-14T02:00:00Z',
				days_of_week: null,
				start_time: null,
				end_time: null,
				timezone: null
			})
		).toBe('2026-10-13 22:00:00 UTC → 2026-10-14 02:00:00 UTC');
	});

	it('names the days, the times and the zone of a weekly window', () => {
		expect(describeSchedule(weekly([4, 2], '09:00:00', '17:30:00'))).toBe('Tue, Thu, 09:00–17:30 Asia/Jakarta');
		expect(describeSchedule(weekly([1, 2, 3, 4, 5], '01:00:00', '02:00:00'))).toBe('Weekdays, 01:00–02:00 Asia/Jakarta');
		expect(describeSchedule(weekly([6, 7], '01:00:00', '02:00:00'))).toBe('Weekends, 01:00–02:00 Asia/Jakarta');
		expect(describeSchedule(weekly([1, 2, 3, 4, 5, 6, 7], '00:00:00', '00:00:00'))).toBe(
			'Every day, 00:00–00:00 (next day) Asia/Jakarta'
		);
	});

	it('marks a window that runs past midnight', () => {
		expect(describeSchedule(weekly([5], '22:00:00', '02:00:00'))).toBe('Fri, 22:00–02:00 (next day) Asia/Jakarta');
	});
});

describe('describeScope', () => {
	it('names the covered monitor, or says the whole project', () => {
		const monitors = [{ id: 'm1', name: 'Checkout API' }];
		expect(describeScope(null, monitors)).toBe('All monitors and heartbeats');
		expect(describeScope('m1', monitors)).toBe('Checkout API');
		expect(describeScope('gone', monitors)).toBe('One monitor');
	});
});

describe('time helpers', () => {
	it('shortens times and renders datetime-local values in UTC', () => {
		expect(shortTime('22:00:00')).toBe('22:00');
		expect(shortTime(null)).toBe('');
		expect(toUtcInputValue('2026-10-13T22:05:30.123Z')).toBe('2026-10-13T22:05');
		expect(toUtcInputValue('2026-10-14T05:00:00+07:00')).toBe('2026-10-13T22:00');
		expect(toUtcInputValue(null)).toBe('');
	});
});
```

`nami-ui/src/lib/server/maintenance-form.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { utcFromLocalInput, windowPayloadFromForm } from './maintenance-form';

function form(fields: Record<string, string | string[]>): FormData {
	const data = new FormData();
	for (const [key, value] of Object.entries(fields)) {
		for (const item of Array.isArray(value) ? value : [value]) data.append(key, item);
	}
	return data;
}

describe('windowPayloadFromForm', () => {
	it('reads one-off times as UTC', () => {
		expect(
			windowPayloadFromForm(
				form({ title: ' DB upgrade ', kind: 'once', monitor_id: '', starts_at: '2026-10-13T22:00', ends_at: '2026-10-14T02:00' })
			)
		).toEqual({
			ok: true,
			payload: { title: 'DB upgrade', kind: 'once', starts_at: '2026-10-13T22:00:00Z', ends_at: '2026-10-14T02:00:00Z' }
		});
	});

	it('sends the weekly days, times and zone', () => {
		expect(
			windowPayloadFromForm(
				form({
					title: 'Deploys',
					kind: 'weekly',
					monitor_id: 'm-1',
					days_of_week: ['2', '4'],
					start_time: '22:00',
					end_time: '02:00',
					timezone: 'Asia/Jakarta',
					starts_at: '2026-10-13T22:00'
				})
			)
		).toEqual({
			ok: true,
			payload: {
				title: 'Deploys',
				kind: 'weekly',
				monitor_id: 'm-1',
				days_of_week: [2, 4],
				start_time: '22:00',
				end_time: '02:00',
				timezone: 'Asia/Jakarta'
			}
		});
	});

	it('on edit sends a cleared scope as null (the whole project)', () => {
		const result = windowPayloadFromForm(
			form({ title: 'x', kind: 'once', monitor_id: '', starts_at: '2026-10-13T22:00', ends_at: '2026-10-13T23:00' }),
			'edit'
		);
		expect(result.ok && result.payload.monitor_id).toBeNull();
	});

	it('asks for the missing parts', () => {
		expect(windowPayloadFromForm(form({ title: 'x', kind: 'once', starts_at: '2026-10-13T22:00', ends_at: '' }))).toEqual({
			ok: false,
			message: 'Enter a start and an end date and time'
		});
		expect(windowPayloadFromForm(form({ title: 'x', kind: 'weekly', start_time: '01:00', end_time: '02:00' }))).toEqual({
			ok: false,
			message: 'Pick at least one day'
		});
	});
});

describe('utcFromLocalInput', () => {
	it('accepts datetime-local values only', () => {
		expect(utcFromLocalInput('2026-10-13T22:00:30')).toBe('2026-10-13T22:00:30Z');
		expect(utcFromLocalInput('2026-10-13 22:00')).toBeNull();
		expect(utcFromLocalInput('2026-10-13T22:00Z')).toBeNull();
		expect(utcFromLocalInput('')).toBeNull();
	});
});
```

`src/lib/server/engine.test.ts`:

```diff
diff --git a/nami-ui/src/lib/server/engine.test.ts b/nami-ui/src/lib/server/engine.test.ts
--- a/nami-ui/src/lib/server/engine.test.ts
+++ b/nami-ui/src/lib/server/engine.test.ts
@@ -1,5 +1,5 @@
 import { afterEach, describe, it, expect, vi } from 'vitest';
-import { callEngine, engineErrorMessage, publicEngineUrl } from './engine';
+import { callEngine, engineErrorMessage, isUuid, publicEngineUrl } from './engine';
 
 describe('callEngine', () => {
 	it('forwards the cookie header and relays set-cookie back', async () => {
@@ -122,3 +122,13 @@ describe('publicEngineUrl', () => {
 		expect(publicEngineUrl()).toBe('https://status-engine.example.com');
 	});
 });
+
+describe('isUuid', () => {
+	it('accepts UUIDs only, so form values cannot steer engine paths', () => {
+		expect(isUuid('0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69')).toBe(true);
+		expect(isUuid('0B6F3F3E-6D2A-4C1E-9B7A-2F1D3C4B5A69')).toBe(true);
+		for (const value of ['', '../monitors/x', '0b6f3f3e-6d2a-4c1e-9b7a-2f1d3c4b5a69/..', 'x', null, 42]) {
+			expect(isUuid(value)).toBe(false);
+		}
+	});
+});
```

`src/lib/components/form-inputs.test.ts` — the window form follows the same convention:

```diff
diff --git a/nami-ui/src/lib/components/form-inputs.test.ts b/nami-ui/src/lib/components/form-inputs.test.ts
--- a/nami-ui/src/lib/components/form-inputs.test.ts
+++ b/nami-ui/src/lib/components/form-inputs.test.ts
@@ -1,12 +1,12 @@
 import { readFileSync } from 'node:fs';
 import { describe, expect, it } from 'vitest';
 
-// Review Focus: changing the check type re-renders parts of the monitor
-// form, and Svelte re-applies a plain value={...} or checked={...} on such
-// re-renders, wiping what the user typed — seen live: picking "SSL
-// certificate expiry" cleared the monitor name. Initial values must go
-// through defaultValue/defaultChecked, or a binding.
-describe.each(['MonitorForm.svelte'])('%s', (file) => {
+// Review Focus: changing the check type (or the window kind) re-renders
+// parts of these forms, and Svelte re-applies a plain value={...} or
+// checked={...} on such re-renders, wiping what the user typed — seen live:
+// picking "SSL certificate expiry" cleared the monitor name. Initial values
+// must go through defaultValue/defaultChecked, or a binding.
+describe.each(['MonitorForm.svelte', 'MaintenanceWindowForm.svelte'])('%s', (file) => {
 	const source = readFileSync(new URL(`./${file}`, import.meta.url), 'utf8');
 
 	it('never sets value={...} or checked={...} on an input or textarea', () => {
```

- [ ] **Step 2: Run them to see them fail**

Run: `cd nami-ui && npx vitest run`
Expected: FAIL — `Failed to resolve import "./maintenance"` / `"./maintenance-form"`, `isUuid is not a function`, and `ENOENT ... MaintenanceWindowForm.svelte`.

- [ ] **Step 3: Helpers**

`nami-ui/src/lib/maintenance.ts`:

```ts
// Shapes of nami-engine's maintenance-window JSON plus pure display
// helpers. Like the rest of the UI, one-off windows are entered and shown in
// UTC; weekly windows carry their own IANA time zone.
import { formatUtc } from './uptime';

export type WindowKind = 'once' | 'weekly';

export type MaintenanceWindow = {
	id: string;
	project_id: string;
	/** `null`: every monitor and heartbeat of the project. */
	monitor_id: string | null;
	title: string;
	kind: WindowKind;
	starts_at: string | null;
	ends_at: string | null;
	/** ISO weekdays, 1 = Monday ... 7 = Sunday. */
	days_of_week: number[] | null;
	/** "HH:MM:SS". */
	start_time: string | null;
	end_time: string | null;
	timezone: string | null;
	created_at: string;
	/** In effect right now. */
	active: boolean;
};

export const WEEKDAYS = [
	{ iso: 1, label: 'Mon' },
	{ iso: 2, label: 'Tue' },
	{ iso: 3, label: 'Wed' },
	{ iso: 4, label: 'Thu' },
	{ iso: 5, label: 'Fri' },
	{ iso: 6, label: 'Sat' },
	{ iso: 7, label: 'Sun' }
] as const;

/** "22:00:00" -> "22:00". */
export function shortTime(time: string | null): string {
	return (time ?? '').slice(0, 5);
}

function describeDays(days: number[]): string {
	const sorted = [...days].sort((a, b) => a - b);
	if (sorted.length === 7) return 'Every day';
	if (sorted.join() === '1,2,3,4,5') return 'Weekdays';
	if (sorted.join() === '6,7') return 'Weekends';
	return sorted.map((day) => WEEKDAYS[day - 1]?.label ?? `day ${day}`).join(', ');
}

/** One line saying when the window applies. */
export function describeSchedule(
	w: Pick<MaintenanceWindow, 'kind' | 'starts_at' | 'ends_at' | 'days_of_week' | 'start_time' | 'end_time' | 'timezone'>
): string {
	if (w.kind === 'once') {
		return `${w.starts_at ? formatUtc(w.starts_at) : '?'} → ${w.ends_at ? formatUtc(w.ends_at) : '?'}`;
	}
	const start = shortTime(w.start_time);
	const end = shortTime(w.end_time);
	const overnight = end <= start ? ' (next day)' : '';
	return `${describeDays(w.days_of_week ?? [])}, ${start}–${end}${overnight} ${w.timezone ?? ''}`.trim();
}

/** What the window covers, in words. */
export function describeScope(monitorId: string | null, monitors: { id: string; name: string }[]): string {
	if (!monitorId) return 'All monitors and heartbeats';
	return monitors.find((m) => m.id === monitorId)?.name ?? 'One monitor';
}

/** An engine timestamp as a `datetime-local` value in UTC ("2026-10-13T22:00"). */
export function toUtcInputValue(iso: string | null): string {
	return iso ? new Date(iso).toISOString().slice(0, 16) : '';
}
```

`nami-ui/src/lib/server/maintenance-form.ts`:

```ts
// Maintenance-window form -> nami-engine JSON body. One-off windows are
// entered as UTC `datetime-local` values (the UI shows every time in UTC);
// weekly windows send ISO weekdays, HH:MM times and an IANA zone, all
// validated again by the engine.
import { formText, type FormMode, type FormResult } from './monitor-form';

const LOCAL_DATETIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/;

/** A `datetime-local` value read as UTC: "2026-10-13T22:00" -> "2026-10-13T22:00:00Z". */
export function utcFromLocalInput(value: string): string | null {
	if (!LOCAL_DATETIME.test(value)) return null;
	return `${value.length === 16 ? `${value}:00` : value}Z`;
}

export function windowPayloadFromForm(form: FormData, mode: FormMode = 'create'): FormResult {
	const kind = formText(form, 'kind');
	const payload: Record<string, unknown> = { title: formText(form, 'title'), kind };
	const monitorId = formText(form, 'monitor_id');
	if (monitorId !== '' || mode === 'edit') payload.monitor_id = monitorId || null;

	if (kind === 'once') {
		const startsAt = utcFromLocalInput(formText(form, 'starts_at'));
		const endsAt = utcFromLocalInput(formText(form, 'ends_at'));
		if (!startsAt || !endsAt) return { ok: false, message: 'Enter a start and an end date and time' };
		payload.starts_at = startsAt;
		payload.ends_at = endsAt;
	} else if (kind === 'weekly') {
		const days = form
			.getAll('days_of_week')
			.map((day) => Number(day))
			.filter((day) => Number.isInteger(day));
		if (days.length === 0) return { ok: false, message: 'Pick at least one day' };
		payload.days_of_week = days;
		payload.start_time = formText(form, 'start_time');
		payload.end_time = formText(form, 'end_time');
		payload.timezone = formText(form, 'timezone');
	}
	return { ok: true, payload };
}
```

`src/lib/server/engine.ts`:

```diff
diff --git a/nami-ui/src/lib/server/engine.ts b/nami-ui/src/lib/server/engine.ts
--- a/nami-ui/src/lib/server/engine.ts
+++ b/nami-ui/src/lib/server/engine.ts
@@ -96,3 +96,14 @@ export async function engineErrorMessage(response: Response, fallback: string):
 export function publicEngineUrl(): string {
 	return (process.env.PUBLIC_ENGINE_URL ?? 'http://localhost:8080').replace(/\/+$/, '');
 }
+
+const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
+
+/**
+ * Ids from form fields or route params go into engine URL paths: anything
+ * but a UUID (e.g. "../monitors/…", which fetch would normalize into a
+ * different engine route) is refused before the call.
+ */
+export function isUuid(value: unknown): value is string {
+	return typeof value === 'string' && UUID.test(value);
+}
```

- [ ] **Step 4: The window form**

`nami-ui/src/lib/components/MaintenanceWindowForm.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import { onMount } from 'svelte';
	import { WEEKDAYS, shortTime, toUtcInputValue, type MaintenanceWindow, type WindowKind } from '#lib/maintenance';

	let {
		maintenanceWindow = null,
		monitors,
		submitLabel,
		message = null,
		action = undefined,
		cancelHref = null
	}: {
		maintenanceWindow?: MaintenanceWindow | null;
		monitors: { id: string; name: string }[];
		submitLabel: string;
		message?: string | null;
		action?: string;
		cancelHref?: string | null;
	} = $props();

	// Read once, in functions, on purpose: after mount the inputs own them.
	function initialKind(): WindowKind {
		return maintenanceWindow?.kind ?? 'once';
	}
	function initialTimezone(): string {
		return maintenanceWindow?.timezone ?? 'UTC';
	}
	function isNew(): boolean {
		return maintenanceWindow === null;
	}
	function initialScope(): string {
		return maintenanceWindow?.monitor_id ?? '';
	}
	let kind = $state<WindowKind>(initialKind());
	let scope = $state(initialScope());
	let timezone = $state(initialTimezone());
	const zones = ['UTC', ...Intl.supportedValuesOf('timeZone').filter((zone) => zone !== 'UTC')];

	onMount(() => {
		// A new weekly window starts out in the browser's time zone.
		if (isNew()) timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
	});

	const input =
		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
	const label = 'mb-1.5 block text-sm font-medium text-neutral-300';
	const choice = 'flex items-center gap-2 text-sm text-neutral-300';
</script>

<!-- Initial values go through defaultValue/defaultChecked (or a binding),
     never value={...}: see MonitorForm. -->
<form method="POST" {action} use:enhance class="space-y-4">
	{#if message}
		<div class="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400">{message}</div>
	{/if}

	<div class="grid gap-4 sm:grid-cols-2">
		<div>
			<label for="mw-title" class={label}>Title</label>
			<input id="mw-title" name="title" type="text" required maxlength="200" placeholder="Database upgrade" defaultValue={maintenanceWindow?.title ?? ''} class={input} />
		</div>
		<div>
			<label for="mw-scope" class={label}>Covers</label>
			<select id="mw-scope" name="monitor_id" bind:value={scope} class={input}>
				<option value="">All monitors and heartbeats</option>
				{#each monitors as monitor (monitor.id)}
					<option value={monitor.id}>{monitor.name}</option>
				{/each}
			</select>
		</div>
	</div>

	<fieldset class="flex flex-wrap gap-6">
		<legend class="sr-only">Repeats</legend>
		<label class={choice}><input type="radio" name="kind" value="once" bind:group={kind} /> One time</label>
		<label class={choice}><input type="radio" name="kind" value="weekly" bind:group={kind} /> Every week</label>
	</fieldset>

	{#if kind === 'once'}
		<div class="grid gap-4 sm:grid-cols-2">
			<div>
				<label for="mw-starts" class={label}>Starts (UTC)</label>
				<input id="mw-starts" name="starts_at" type="datetime-local" required defaultValue={toUtcInputValue(maintenanceWindow?.starts_at ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-ends" class={label}>Ends (UTC)</label>
				<input id="mw-ends" name="ends_at" type="datetime-local" required defaultValue={toUtcInputValue(maintenanceWindow?.ends_at ?? null)} class={input} />
			</div>
		</div>
	{:else}
		<fieldset>
			<legend class={label}>Days</legend>
			<div class="flex flex-wrap gap-4">
				{#each WEEKDAYS as day (day.iso)}
					<label class={choice}>
						<input type="checkbox" name="days_of_week" defaultValue={day.iso} defaultChecked={maintenanceWindow?.days_of_week?.includes(day.iso) ?? false} />
						{day.label}
					</label>
				{/each}
			</div>
		</fieldset>
		<div class="grid gap-4 sm:grid-cols-3">
			<div>
				<label for="mw-start-time" class={label}>From</label>
				<input id="mw-start-time" name="start_time" type="time" required defaultValue={shortTime(maintenanceWindow?.start_time ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-end-time" class={label}>Until</label>
				<input id="mw-end-time" name="end_time" type="time" required defaultValue={shortTime(maintenanceWindow?.end_time ?? null)} class={input} />
			</div>
			<div>
				<label for="mw-timezone" class={label}>Time zone</label>
				<input id="mw-timezone" name="timezone" type="text" required list="mw-timezones" bind:value={timezone} class={input} />
				<datalist id="mw-timezones">
					{#each zones as zone (zone)}
						<option value={zone}></option>
					{/each}
				</datalist>
			</div>
		</div>
		<p class="text-xs text-neutral-500">An end at or before the start runs past midnight into the next day.</p>
	{/if}

	<div class="flex items-center gap-3">
		<button
			type="submit"
			class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400 focus:ring-2 focus:ring-teal-500/50 focus:ring-offset-2 focus:ring-offset-[#141414] focus:outline-none"
		>
			{submitLabel}
		</button>
		{#if cancelHref}<a href={cancelHref} class="text-sm text-neutral-500 hover:text-neutral-300">Cancel</a>{/if}
	</div>
</form>
```

- [ ] **Step 5: The maintenance pages and the tab**

`maintenance/+page.server.ts`:

```ts
import { error, fail } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, isUuid } from '#lib/server/engine';
import { windowPayloadFromForm } from '#lib/server/maintenance-form';
import type { MaintenanceWindow } from '#lib/maintenance';
import type { MonitorSummary } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [windowsResponse, monitorsResponse] = await Promise.all([
		callEngine(event, `/projects/${event.params.id}/maintenance-windows`),
		callEngine(event, `/projects/${event.params.id}/monitors`)
	]);
	if (!windowsResponse.ok) {
		throw error(
			windowsResponse.status === 403 ? 403 : 500,
			await engineErrorMessage(windowsResponse, 'Could not load maintenance windows.')
		);
	}
	const windows: MaintenanceWindow[] = await windowsResponse.json();
	// Monitor names only label the scope; without them windows still list.
	const monitors = monitorsResponse.ok
		? ((await monitorsResponse.json()) as MonitorSummary[]).map(({ id, name }) => ({ id, name }))
		: [];
	return { windows, monitors };
};

export const actions: Actions = {
	create: async (event) => {
		const parsed = windowPayloadFromForm(await event.request.formData(), 'create');
		if (!parsed.ok) return fail(400, { action: 'create' as const, success: false, message: parsed.message });
		const response = await callEngine(event, `/projects/${event.params.id}/maintenance-windows`, {
			method: 'POST',
			body: JSON.stringify(parsed.payload)
		});
		if (!response.ok) {
			const message = await engineErrorMessage(response, 'Could not create the maintenance window.');
			return fail(response.status, { action: 'create' as const, success: false, message });
		}
		return { action: 'create' as const, success: true, message: 'Maintenance window created.' };
	},
	delete: async (event) => {
		const windowId = (await event.request.formData()).get('window_id');
		if (!isUuid(windowId)) {
			return fail(400, { action: 'delete' as const, success: false, message: 'Unknown maintenance window.' });
		}
		const response = await callEngine(event, `/projects/${event.params.id}/maintenance-windows/${windowId}`, {
			method: 'DELETE'
		});
		if (!response.ok) {
			const message = await engineErrorMessage(response, 'Could not delete the maintenance window.');
			return fail(response.status, { action: 'delete' as const, success: false, message });
		}
		return { action: 'delete' as const, success: true, message: 'Maintenance window deleted.' };
	}
};
```

`maintenance/+page.svelte`:

```svelte
<script lang="ts">
	import { enhance } from '$app/forms';
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import { describeSchedule, describeScope } from '#lib/maintenance';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="space-y-6">
	<div class="rounded-xl border border-[#262626] bg-[#141414]">
		<div class="border-b border-[#262626] px-6 py-4">
			<h2 class="text-base font-semibold text-neutral-100">Maintenance windows</h2>
			<p class="text-xs text-neutral-500">
				While a window is active its monitors and heartbeats are still checked, but they open no incidents and send
				no alerts.
			</p>
		</div>
		{#if form?.action === 'delete'}
			<div
				class="mx-6 mt-4 rounded-md border px-3 py-2 text-sm {form.success
					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
			>
				{form.message}
			</div>
		{/if}
		{#if data.windows.length === 0}
			<p class="px-6 py-10 text-center text-sm text-neutral-500">No maintenance windows yet — schedule one below.</p>
		{:else}
			<ul>
				{#each data.windows as window (window.id)}
					<li class="flex flex-wrap items-center justify-between gap-3 border-b border-[#262626] px-6 py-4 last:border-0">
						<div class="space-y-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="font-medium text-neutral-100">{window.title}</span>
								{#if window.active}
									<span class="rounded-full border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-xs text-sky-300">
										Active now
									</span>
								{/if}
							</div>
							<p class="text-xs text-neutral-400">{describeSchedule(window)}</p>
							<p class="text-xs text-neutral-500">{describeScope(window.monitor_id, data.monitors)}</p>
						</div>
						<div class="flex items-center gap-2">
							<a
								href="/projects/{data.project.id}/maintenance/{window.id}"
								class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100"
								>Edit</a
							>
							<form
								method="POST"
								action="?/delete"
								use:enhance={({ cancel }) => {
									if (!confirm(`Delete maintenance window "${window.title}"?`)) cancel();
								}}
							>
								<input type="hidden" name="window_id" value={window.id} />
								<button
									type="submit"
									class="rounded-md border border-[#262626] px-3 py-1.5 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
								>
									Delete
								</button>
							</form>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
		<h2 class="text-base font-semibold text-neutral-100">New maintenance window</h2>
		{#if form?.action === 'create' && form.success}
			<div class="mt-4 rounded-md border border-teal-500/30 bg-teal-500/10 px-3 py-2 text-sm text-teal-300">
				{form.message}
			</div>
		{/if}
		<div class="mt-4">
			<MaintenanceWindowForm
				action="?/create"
				monitors={data.monitors}
				submitLabel="Schedule window"
				message={form?.action === 'create' && !form.success ? form.message : null}
			/>
		</div>
	</div>
</div>
```

`maintenance/[windowId]/+page.server.ts`:

```ts
import { error, fail, redirect } from '@sveltejs/kit';
import { callEngine, engineErrorMessage, isUuid } from '#lib/server/engine';
import { windowPayloadFromForm } from '#lib/server/maintenance-form';
import type { MaintenanceWindow } from '#lib/maintenance';
import type { MonitorSummary } from '#lib/uptime';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	if (!isUuid(event.params.windowId)) throw error(404, 'Maintenance window not found.');
	const [windowResponse, monitorsResponse] = await Promise.all([
		callEngine(event, `/projects/${event.params.id}/maintenance-windows/${event.params.windowId}`),
		callEngine(event, `/projects/${event.params.id}/monitors`)
	]);
	if (windowResponse.status === 404) throw error(404, 'Maintenance window not found.');
	if (!windowResponse.ok) {
		throw error(
			windowResponse.status === 403 ? 403 : 500,
			await engineErrorMessage(windowResponse, 'Could not load the maintenance window.')
		);
	}
	const maintenanceWindow: MaintenanceWindow = await windowResponse.json();
	const monitors = monitorsResponse.ok
		? ((await monitorsResponse.json()) as MonitorSummary[]).map(({ id, name }) => ({ id, name }))
		: [];
	return { maintenanceWindow, monitors };
};

export const actions: Actions = {
	default: async (event) => {
		if (!isUuid(event.params.windowId)) return fail(404, { message: 'Maintenance window not found.' });
		const parsed = windowPayloadFromForm(await event.request.formData(), 'edit');
		if (!parsed.ok) return fail(400, { message: parsed.message });
		const response = await callEngine(
			event,
			`/projects/${event.params.id}/maintenance-windows/${event.params.windowId}`,
			{ method: 'PATCH', body: JSON.stringify(parsed.payload) }
		);
		if (!response.ok) {
			return fail(response.status, {
				message: await engineErrorMessage(response, 'Could not save the maintenance window.')
			});
		}
		throw redirect(303, `/projects/${event.params.id}/maintenance`);
	}
};
```

`maintenance/[windowId]/+page.svelte`:

```svelte
<script lang="ts">
	import MaintenanceWindowForm from '#lib/components/MaintenanceWindowForm.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
	<h2 class="text-base font-semibold text-neutral-100">Edit {data.maintenanceWindow.title}</h2>
	<div class="mt-4">
		<MaintenanceWindowForm
			maintenanceWindow={data.maintenanceWindow}
			monitors={data.monitors}
			submitLabel="Save changes"
			message={form?.message ?? null}
			cancelHref="/projects/{data.project.id}/maintenance"
		/>
	</div>
</div>
```

`+layout.svelte` (tab):

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte b/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/+layout.svelte
@@ -8,6 +8,7 @@
 		{ href: `/projects/${data.project.id}/monitors`, label: 'Monitors' },
 		{ href: `/projects/${data.project.id}/heartbeats`, label: 'Heartbeats' },
 		{ href: `/projects/${data.project.id}/incidents`, label: 'Incidents' },
+		{ href: `/projects/${data.project.id}/maintenance`, label: 'Maintenance' },
 		{ href: `/projects/${data.project.id}/settings`, label: 'Settings' }
 	]);
 </script>
```

- [ ] **Step 6: Monitor groups in project settings**

`settings/+page.server.ts` (the member actions are unchanged; groups gain a load and three actions):

```ts
import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad, RequestEvent } from './$types';
import { callEngine, engineErrorMessage, isUuid } from '#lib/server/engine';
import { formText, formWholeNumber } from '#lib/server/monitor-form';
import type { MonitorGroup } from '#lib/uptime';

export const load: PageServerLoad = async (event) => {
	const response = await callEngine(event, `/projects/${event.params.id}/monitor-groups`);
	const groups: MonitorGroup[] = response.ok ? await response.json() : [];
	return { groups };
};

/** Name and position from a group form; position defaults to 0. */
function groupBody(form: FormData): { name: string; position: number } | string {
	const position = formWholeNumber(form, 'position');
	if (position === 'invalid') return 'Position must be a whole number';
	return { name: formText(form, 'name'), position: position ?? 0 };
}

async function groupResult(response: Response, done: string, fallback: string) {
	if (response.ok) return { action: 'group' as const, success: true, message: done };
	return fail(response.status, {
		action: 'group' as const,
		success: false,
		message: await engineErrorMessage(response, fallback)
	});
}

async function groupIdFrom(event: RequestEvent): Promise<{ form: FormData; groupId: string | null }> {
	const form = await event.request.formData();
	const groupId = form.get('group_id');
	return { form, groupId: isUuid(groupId) ? groupId : null };
}

export const actions: Actions = {
	// Moved unchanged from the old /projects/[id] page.
	addMember: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, `/projects/${event.params.id}/members`, {
			method: 'POST',
			body: JSON.stringify({
				user_id: form.get('user_id'),
				project_role: form.get('project_role')
			})
		});
		const body = await response.json();
		return {
			action: 'add' as const,
			success: response.ok,
			message: response.ok ? 'Member added.' : (body.error ?? 'could not add member')
		};
	},
	removeMember: async (event) => {
		const form = await event.request.formData();
		const userId = form.get('user_id');
		const response = await callEngine(
			event,
			`/projects/${event.params.id}/members/${userId}`,
			{ method: 'DELETE' }
		);
		const body = await response.json();
		return {
			action: 'remove' as const,
			success: response.ok,
			message: response.ok ? 'Member removed.' : (body.error ?? 'could not remove member')
		};
	},
	createGroup: async (event) => {
		const body = groupBody(await event.request.formData());
		if (typeof body === 'string') return fail(400, { action: 'group' as const, success: false, message: body });
		const response = await callEngine(event, `/projects/${event.params.id}/monitor-groups`, {
			method: 'POST',
			body: JSON.stringify(body)
		});
		return groupResult(response, 'Group created.', 'Could not create the group.');
	},
	updateGroup: async (event) => {
		const { form, groupId } = await groupIdFrom(event);
		if (!groupId) return fail(400, { action: 'group' as const, success: false, message: 'Unknown group.' });
		const body = groupBody(form);
		if (typeof body === 'string') return fail(400, { action: 'group' as const, success: false, message: body });
		const response = await callEngine(event, `/projects/${event.params.id}/monitor-groups/${groupId}`, {
			method: 'PATCH',
			body: JSON.stringify(body)
		});
		return groupResult(response, 'Group saved.', 'Could not save the group.');
	},
	deleteGroup: async (event) => {
		const { groupId } = await groupIdFrom(event);
		if (!groupId) return fail(400, { action: 'group' as const, success: false, message: 'Unknown group.' });
		const response = await callEngine(event, `/projects/${event.params.id}/monitor-groups/${groupId}`, {
			method: 'DELETE'
		});
		return groupResult(response, 'Group deleted; its monitors are now ungrouped.', 'Could not delete the group.');
	}
};
```

`settings/+page.svelte`:

```diff
diff --git a/nami-ui/src/routes/(app)/projects/[id]/settings/+page.svelte b/nami-ui/src/routes/(app)/projects/[id]/settings/+page.svelte
--- a/nami-ui/src/routes/(app)/projects/[id]/settings/+page.svelte
+++ b/nami-ui/src/routes/(app)/projects/[id]/settings/+page.svelte
@@ -1,11 +1,86 @@
 <script lang="ts">
+	import { enhance } from '$app/forms';
 	import type { PageProps } from './$types';
 
-	let { form }: PageProps = $props();
+	let { data, form }: PageProps = $props();
+
+	const input =
+		'w-full rounded-md border border-[#262626] bg-[#0a0a0a] px-3 py-2 text-sm text-neutral-100 placeholder-neutral-600 focus:border-teal-500/50 focus:ring-2 focus:ring-teal-500/50 focus:outline-none';
 </script>
 
-<!-- The member forms, moved unchanged from the old project page. -->
+<!-- Monitor groups, then the member forms (moved unchanged from the old
+     project page). -->
 <div class="space-y-6">
+	<div class="rounded-xl border border-[#262626] bg-[#141414] p-6">
+		<h2 class="text-lg font-semibold text-neutral-100">Monitor groups</h2>
+		<p class="mt-1 text-sm text-neutral-500">
+			Groups arrange the monitor list, lowest position first. Deleting a group keeps its monitors, ungrouped.
+		</p>
+
+		{#if form?.action === 'group'}
+			<div
+				class="mt-4 rounded-md border px-3 py-2 text-sm {form.success
+					? 'border-teal-500/30 bg-teal-500/10 text-teal-300'
+					: 'border-red-500/30 bg-red-500/10 text-red-400'}"
+			>
+				{form.message}
+			</div>
+		{/if}
+
+		{#if data.groups.length > 0}
+			<ul class="mt-4 space-y-2">
+				{#each data.groups as group (group.id)}
+					<li>
+						<form
+							method="POST"
+							action="?/updateGroup"
+							use:enhance={({ submitter, cancel }) => {
+								const deleting = submitter?.getAttribute('formaction') === '?/deleteGroup';
+								if (deleting && !confirm(`Delete group "${group.name}"? Its monitors stay, ungrouped.`)) cancel();
+								return async ({ update }) => update({ reset: false });
+							}}
+							class="grid gap-2 sm:grid-cols-[1fr_7rem_auto_auto] sm:items-center"
+						>
+							<input type="hidden" name="group_id" value={group.id} />
+							<input name="name" type="text" required maxlength="200" value={group.name} aria-label="Group name" class={input} />
+							<input name="position" type="number" min="0" max="1000000" value={group.position} aria-label="Position" class={input} />
+							<button
+								type="submit"
+								class="rounded-md border border-[#262626] px-3 py-2 text-sm text-neutral-300 transition-colors hover:border-teal-500/30 hover:text-neutral-100"
+							>
+								Save
+							</button>
+							<button
+								type="submit"
+								formaction="?/deleteGroup"
+								class="rounded-md border border-[#262626] px-3 py-2 text-sm text-neutral-400 transition-colors hover:border-red-500/30 hover:text-red-400"
+							>
+								Delete
+							</button>
+						</form>
+					</li>
+				{/each}
+			</ul>
+		{/if}
+
+		<form method="POST" action="?/createGroup" use:enhance class="mt-4 grid gap-2 sm:grid-cols-[1fr_7rem_auto] sm:items-end">
+			<div>
+				<label for="group-name" class="mb-1.5 block text-sm font-medium text-neutral-300">New group</label>
+				<input id="group-name" name="name" type="text" required maxlength="200" placeholder="APIs" class={input} />
+			</div>
+			<div>
+				<label for="group-position" class="mb-1.5 block text-sm font-medium text-neutral-300">Position</label>
+				<input id="group-position" name="position" type="number" min="0" max="1000000" placeholder="0" class={input} />
+			</div>
+			<button
+				type="submit"
+				class="rounded-md bg-teal-500 px-4 py-2 text-sm font-medium text-neutral-950 transition-colors hover:bg-teal-400"
+			>
+				Add group
+			</button>
+		</form>
+	</div>
+
 	<p class="text-sm text-neutral-500">
 		This project has no visible member list yet — add or remove members by their user ID.
 	</p>
```

- [ ] **Step 7: Run the checks**

Run: `cd nami-ui && npm run check && npx vitest run`
Expected: `svelte-check` 0 errors, 0 warnings; vitest 7 files, 63 tests passed.

- [ ] **Step 8: Verify live in a browser**

With the stack running (Task 18, Step 7):
1. Maintenance tab → schedule a one-off window starting a few minutes ago (UTC inputs) for "All monitors and heartbeats" → it lists as "Active now"; the monitors list and the heartbeats list show "Maintenance" badges.
2. Switch to "Every week": the time zone defaults to your browser's; pick Tue + Thu 22:00–02:00 for one monitor → it lists as `Tue, Thu, 22:00–02:00 (next day) <zone>` scoped to that monitor.
3. Edit it (all fields prefilled), add Monday, make it project-wide → the list shows the change. `/projects/<id>/maintenance/not-a-uuid` is a 404.
4. Settings → add a group with a position, rename it, delete it (confirm) → "Group deleted; its monitors are now ungrouped." and the monitors list falls back to the flat table.

- [ ] **Step 9: Commit**

```bash
git add nami-ui/src/lib/maintenance.ts nami-ui/src/lib/maintenance.test.ts nami-ui/src/lib/server/maintenance-form.ts \
  nami-ui/src/lib/server/maintenance-form.test.ts nami-ui/src/lib/server/engine.ts nami-ui/src/lib/server/engine.test.ts \
  nami-ui/src/lib/components/MaintenanceWindowForm.svelte nami-ui/src/lib/components/form-inputs.test.ts \
  'nami-ui/src/routes/(app)/projects/[id]/maintenance' 'nami-ui/src/routes/(app)/projects/[id]/+layout.svelte' \
  'nami-ui/src/routes/(app)/projects/[id]/settings'
git commit -m "feat(ui): maintenance windows page and monitor groups in settings (NAMI-UP-19)

One-off (UTC) and weekly windows with list, create, edit and delete;
groups are created, renamed, reordered and deleted in project settings.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

After this task merges, tick the uptime Phase 2 items in `docs/superpowers/roadmap.md` §2 (request config, the new monitor types, recovery threshold, maintenance windows, monitor groups) and run the sub-project's security review + fix wave (CLAUDE.md).
