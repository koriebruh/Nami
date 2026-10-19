-- Logs domain baseline: sources (with their ingest pipeline), daily usage
-- counters and saved views. Squashed on 2026-10-19 from 0040-0043.
create table sources (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    platform text not null default 'http'
        check (platform in ('http', 'syslog', 'vector', 'docker', 'other')),
    -- Copied onto every ingested row (ClickHouse TTL): a change affects new
    -- rows only.
    retention_days int not null default 30 check (retention_days between 1 and 365),
    -- null = unlimited.
    daily_byte_quota bigint check (daily_byte_quota >= 1048576),
    -- SHA-256 (hex) of the ingest token: the ingest paths look sources up by
    -- it, so a rotated token stops working the moment this row changes.
    token_hash text not null unique,
    -- The same token sealed with AES-256-GCM (core::crypto::Cipher, associated
    -- data "sources.token:<this row's id>", so a sealed value copied onto
    -- another row does not open there) so members can see it again.
    token_enc bytea not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    -- Per-source ingest pipeline: an ordered list of at most 50 rules,
    -- replaced as a whole by PUT .../pipeline and validated by the engine
    -- (logs::ingest::pipeline::compile) before it is stored. The engine caches
    -- compiled rules per source keyed on sources.updated_at, so every write
    -- here must bump updated_at (logs::source::model::set_pipeline does).
    pipeline jsonb not null default '[]'::jsonb
        check (jsonb_typeof(pipeline) = 'array' and jsonb_array_length(pipeline) <= 50)
);

create index sources_project_idx on sources (project_id);

-- Per source and UTC day ingestion counters. Engine instances count in
-- memory and add their deltas here every 10 s (additive upsert); rows are
-- kept 400 days.
create table source_usage_daily (
    source_id uuid not null references sources(id) on delete cascade,
    day date not null,
    -- Accepted events and their bytes (UTF-8 length of message + fields).
    events bigint not null default 0,
    bytes bigint not null default 0,
    -- Ingest pipeline outcomes.
    events_dropped bigint not null default 0,
    events_sampled_out bigint not null default 0,
    -- Rejected requests (HTTP) and frames (syslog).
    requests_rate_limited bigint not null default 0,
    requests_over_quota bigint not null default 0,
    primary key (source_id, day)
);

create index source_usage_daily_day_idx on source_usage_daily (day);

-- Saved explorer views. `query` is a logs::view::saved_query::ViewQuery
-- validated by the engine; members create, edit and delete, viewers read.
create table log_views (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    query jsonb not null default '{}'::jsonb check (jsonb_typeof(query) = 'object'),
    created_by uuid references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, name)
);
