-- Log sources (Logs sub-project; migrations 0040-0059 are reserved for it).
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
    -- The same token sealed with AES-256-GCM (crypto::Cipher, associated
    -- data "sources.token:<this row's id>", so a sealed value copied onto
    -- another row does not open there) so members can see it again.
    token_enc bytea not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index sources_project_idx on sources (project_id);
