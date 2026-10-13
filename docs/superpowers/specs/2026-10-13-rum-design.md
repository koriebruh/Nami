# Real User Monitoring

## Context

Ninth sub-project (roadmap §9), mirroring Better Stack RUM: a small JS SDK
served by Nami records page views, web vitals, errors, custom events and
sampled session replays from real browsers, plus cookieless website
analytics and funnels. Builds on Error tracking (JS errors), Metrics (the
shared alert evaluator), Incident Management and the ClickHouse client.
Starts after Error tracking merges.

## Goals

### Applications

`rum_apps` per project: `name`, `client_key` (`nami_rum_` + 24 random
bytes base64url; public, not a secret), `allowed_origins` (required, 1-20
exact origins), optional `error_app_id` (the Error tracking app that
receives JS errors), `session_sample_rate` (0-1, default 1),
`replay_sample_rate` (0-1, default 0 = off), `replay_mask_all_text`
(default false), `respect_dnt` (default true), `retention_days` (default
90), `replay_retention_days` (default 30), `daily_event_cap`,
`daily_replay_bytes_cap`. Members manage; admins delete.

### JS SDK

- Built from `nami-rum/` (TypeScript) and served by nami-ui at versioned,
  immutable URLs: `/rum/v1/nami-rum.js` (core, budget 12 KiB gzip,
  bundles `web-vitals`) and `/rum/v1/nami-rum-replay.js` (rrweb record,
  loaded only for sampled sessions), both with
  `Access-Control-Allow-Origin: *`. The setup page shows the snippet with
  its SRI hash.
- `NamiRUM.init({clientKey, endpoint, release?, environment?, storage?})`
  captures:
  - page views (initial load and SPA navigations through the History API
    and `popstate`) and `page_leave` with engaged time (on
    `visibilitychange` to hidden);
  - web vitals LCP, CLS, INP, TTFB and FCP via `web-vitals` (value,
    rating, navigation type);
  - JS errors (`error`, `unhandledrejection`) as Sentry-shaped events;
  - custom events `NamiRUM.track(name, props)` (name <= 64 chars, <= 20
    props, values <= 256 chars) and `NamiRUM.identify(userId, traits)`
    (<= 10 traits).
- Events queue in memory and flush every 5 s, at 50 events, and on page
  hide via `navigator.sendBeacon` (`fetch` with `keepalive` as fallback).
- Session id: random 128-bit, kept in `sessionStorage` (per tab; a new
  one after 30 min idle). `storage: false` keeps it in memory only (each
  page load is then a session). Analytics never depend on it.
- The SDK never sets cookies. With `respect_dnt` it does nothing when
  `navigator.doNotTrack === "1"` or `navigator.globalPrivacyControl` is
  true.

### Beacon ingest

- `POST /api/v1/rum/ingest?k=<client key>` (engine): body `{session_id,
  page_view_id, events: [...]}` as `text/plain` or `application/json`
  (sendBeacon cannot set headers), <= 64 KiB, <= 100 events. Unknown
  event types and fields are dropped.
- CORS: `Access-Control-Allow-Origin` echoes the request origin only when
  it is in `allowed_origins`; any other origin → 403. Origin checks limit
  casual abuse; they are not authentication.
- Limits: per app 100 requests/s (burst 200), per client IP 20/s, and
  `daily_event_cap` → 429. With `respect_dnt`, a `DNT: 1` request header
  drops the batch.
- Server-side enrichment, after which the raw IP is discarded:
  `visitor_hash`, country (optional GeoIP), browser / OS / device type
  from the User-Agent. Known bot user agents are dropped.
- JS errors go to the linked Error tracking app through its normal
  pipeline (normalization, scrubbing, grouping, symbolication, spike
  protection), tagged with `rum_session_id`. Without a linked app they
  are counted and dropped.

### Session replay

- Sampled at session start with `replay_sample_rate` (client side); the
  server rejects replay uploads for apps with replay off (403).
- rrweb recording with `maskAllInputs: true` (password fields always
  masked, not configurable), `maskAllText` per `replay_mask_all_text`,
  class `nami-mask` masked and class `nami-block` replaced by a
  placeholder; no canvas, cross-origin iframes, console or network
  bodies; stylesheets inlined at record time.
- Chunked upload `POST /api/v1/rum/replay?k=…`: header `{session_id, seq,
  ts_start, ts_end}` + gzip'd rrweb events (`CompressionStream`), one
  chunk every 10 s or 100 KiB; <= 1 MiB compressed and <= 10 MiB
  decompressed (checked by streaming decompression → 413); a session
  caps at 4 h / 2,000 chunks; `daily_replay_bytes_cap` → 429.
- Stored in ClickHouse `rum_replay_chunks` (compressed bytes as sent,
  TTL `replay_retention_days`).
- Player: `rrweb-player` in nami-ui loads chunks through the BFF. The
  replay iframe is sandboxed without `allow-scripts`, so recorded scripts
  and event-handler attributes never run. The player route sends its own
  CSP: `script-src` nonce-only, `style-src 'unsafe-inline'` (recorded
  styles), `img-src` / `font-src https: data:` — recorded pages load
  images from their original URLs, which the UI notes.
- Sessions list (filters: has replay, has error, URL visited, user id,
  country, device, date) with a timeline of page views, errors (linked to
  their issue) and custom events beside the player; error events link
  back to the replay.

### Website analytics

- `visitor_hash` = SHA-256(daily salt ‖ app id ‖ IP ‖ User-Agent),
  truncated to 64 bits. Salts are random per UTC day, kept in Postgres
  `rum_daily_salts` and deleted after 48 h, so hashes cannot be linked
  across days or reversed. Unique visitors are unique per day; longer
  ranges sum daily uniques (documented).
- Dimensions: path (query string and fragment removed, UTM kept apart),
  host, referrer host and path (own origins excluded), `utm_source`,
  `utm_medium`, `utm_campaign`, `utm_term`, `utm_content`, country
  (`GEOIP_DB_PATH` to a MaxMind-format `.mmdb` such as GeoLite2 or DB-IP
  Lite; without it `unknown`), browser, OS, device type.
- Metrics over a range, with any dimension filters: visitors, visits
  (page views of one visitor hash with gaps < 30 min), page views, bounce
  rate, visit duration (from engaged time), time series, and top pages,
  entry and exit pages, referrers, UTM values, countries, browsers, OS
  and devices. `GET /projects/{id}/rum/apps/{app_id}/analytics` (viewer).

### Web vitals

- Per URL group (path with numeric, UUID and hex segments replaced by
  `:id`) and device type: p75 of each vital over the range, sample count,
  and the good / needs-improvement / poor split by web.dev thresholds
  (LCP 2.5 s / 4 s, INP 200 / 500 ms, CLS 0.1 / 0.25, FCP 1.8 / 3 s,
  TTFB 0.8 / 1.8 s).
- `rum_vital_alerts`: vital, optional URL group pattern, device type, p75
  threshold, window, min samples (default 20; fewer → no data, state
  kept), escalation policy, severity. Evaluated by the shared alert
  evaluator from Metrics (confirmation/recovery periods); incidents get
  `source = 'rum_alert'` and `rum_alert_id`.

### Funnels

`rum_funnels` per app: 2-8 ordered steps, each a page (exact path or glob
with `*`) or a custom event name. A query over a range returns per step
the sessions that reached it in order within one session (ClickHouse
`windowFunnel`, window <= 4 h), conversion from the previous and from the
first step, and drop-off. Funnels need session ids (`storage: false`
limits them to single-page steps; documented).

### Storage (ClickHouse)

```sql
CREATE TABLE IF NOT EXISTS rum_events (
    project_id UUID,
    app_id UUID,
    dt DateTime64(3, 'UTC'),
    event_type LowCardinality(String),  -- page_view|page_leave|web_vital|custom|identify
    session_id String,
    page_view_id String,
    visitor_hash UInt64,
    host LowCardinality(String),
    path String,
    referrer_host LowCardinality(String),
    referrer_path String,
    utm_source LowCardinality(String),
    utm_medium LowCardinality(String),
    utm_campaign String,
    utm_term String,
    utm_content String,
    country LowCardinality(String),
    browser LowCardinality(String),
    os LowCardinality(String),
    device LowCardinality(String),
    vital_name LowCardinality(String),
    vital_value Float64,
    vital_rating LowCardinality(String),
    event_name LowCardinality(String),
    props Map(String, String),
    engaged_ms UInt32,
    user_id String,
    release LowCardinality(String),
    environment LowCardinality(String),
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(dt)
ORDER BY (project_id, app_id, event_type, dt)
TTL toDateTime(dt) + toIntervalDay(retention_days);

CREATE TABLE IF NOT EXISTS rum_replay_chunks (
    project_id UUID,
    app_id UUID,
    session_id String,
    seq UInt32,
    ts_start DateTime64(3, 'UTC'),
    ts_end DateTime64(3, 'UTC'),
    data String,                        -- gzip'd rrweb events, as uploaded
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(ts_start)
ORDER BY (project_id, app_id, session_id, seq)
TTL toDateTime(ts_start) + toIntervalDay(retention_days);
```

Postgres: `rum_apps`, `rum_daily_salts`, `rum_vital_alerts`,
`rum_funnels`, `rum_app_usage_daily`; `incidents` gains `rum_alert_id`
and the `rum_alert` source.

### UI

RUM section in the project nav: Applications (create, allowed origins,
snippet with SRI, settings), Analytics (range, filters, time series,
breakdown tables), Web vitals (per-URL-group table with p75 and rating
bars, trend chart, alerts), Sessions with the replay player, Funnels
(builder and result chart).

### RBAC

Viewers read analytics, vitals, sessions and replays; members manage
apps, alerts and funnels; admins delete apps and run user erasure.

## Privacy

- No cookies, ever. Analytics need no client storage; the session id
  lives in `sessionStorage` and can be turned off (`storage: false`).
- IP addresses are used in memory for the visitor hash and GeoIP lookup
  and are never stored or logged (RUM routes are excluded from
  per-request IP logging).
- DNT and Global Privacy Control are honored when `respect_dnt` is on,
  in the SDK and on the server.
- Replay masking defaults: every input masked, passwords always; text
  masking and block classes available.
- `identify` stores the user id as given (the customer chooses an opaque
  id). `DELETE /projects/{id}/rum/apps/{app_id}/users/{user_id}` (admin)
  erases that user's events and replays.
- Retention per app; the setup page lists what is collected.

## Security requirements (tested)

- Client key is public: unknown key → 401; an origin outside
  `allowed_origins` → 403; per-app, per-IP and daily caps → 429.
- Payload limits: beacon > 64 KiB or > 100 events → 413; replay chunk
  > 1 MiB or > 10 MiB decompressed → 413; malformed JSON → 400 without
  panics.
- Replay XSS: a crafted chunk with `<script>`, `onerror=` attributes,
  `javascript:` URLs and `srcdoc` iframes executes nothing in the player
  (browser test); the iframe sandbox never includes `allow-scripts`.
- Analytics values (paths, referrers, UTM values, props, user ids) render
  escaped everywhere.
- No raw IPs in ClickHouse, Postgres or logs (the test inspects stored
  rows); salts older than 48 h are gone.
- Forwarded JS errors respect Error tracking's limits and scrubbing.
- Cross-project isolation on every query and replay fetch → 404.
- The RUM pipeline makes no outbound requests (GeoIP is a local file).

## Non-goals

Mobile SDKs, heatmaps and rage clicks, network or console capture in
replays, canvas and cross-origin iframe recording, frontend-to-backend
trace propagation, account/group analytics, cookie-based cross-day
visitor identity (by design), A/B testing.
