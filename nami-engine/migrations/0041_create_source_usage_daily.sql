-- Per source and UTC day ingestion counters. Engine instances count in
-- memory and add their deltas here every 10 s (additive upsert); rows are
-- kept 400 days.
create table source_usage_daily (
    source_id uuid not null references sources(id) on delete cascade,
    day date not null,
    -- Accepted events and their bytes (UTF-8 length of message + fields).
    events bigint not null default 0,
    bytes bigint not null default 0,
    -- Ingest pipeline outcomes (the pipeline arrives with Logs part 2).
    events_dropped bigint not null default 0,
    events_sampled_out bigint not null default 0,
    -- Rejected requests (HTTP) and frames (syslog).
    requests_rate_limited bigint not null default 0,
    requests_over_quota bigint not null default 0,
    primary key (source_id, day)
);

create index source_usage_daily_day_idx on source_usage_daily (day);
