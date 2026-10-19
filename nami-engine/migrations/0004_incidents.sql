-- Incidents domain baseline. Squashed on 2026-10-19 from 0010, the incident
-- parts of 0011 and 0019, and 0022.
create table incidents (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    monitor_id uuid references monitors(id) on delete cascade,
    title text not null,
    cause text,
    status text not null default 'open' check (status in ('open', 'resolved')),
    started_at timestamptz not null default now(),
    resolved_at timestamptz,
    heartbeat_id uuid references heartbeats(id) on delete cascade,
    -- Output of the heartbeat ping that opened the incident (sanitized, at most
    -- 10 KiB plus a truncation marker; see uptime::heartbeat::sanitize_output).
    details text
        check (details is null or octet_length(details) <= 10240 + 16)
);

-- Incident lists filter by project and status, newest first.
create index incidents_project_status_idx on incidents (project_id, status, started_at desc);

-- At most one open incident per monitor and per heartbeat. The worker relies
-- on this to stay idempotent even if two engine instances race on the same
-- monitor.
create unique index incidents_one_open_per_monitor on incidents (monitor_id) where status = 'open';
create unique index incidents_one_open_per_heartbeat on incidents (heartbeat_id) where status = 'open';

-- Every incident of one monitor or heartbeat: the per-source incident lists,
-- and the cascades that delete a monitor's or a heartbeat's incidents, look
-- them up by these columns (the unique indexes above cover only the open
-- ones).
create index incidents_monitor_idx on incidents (monitor_id) where monitor_id is not null;
create index incidents_heartbeat_idx on incidents (heartbeat_id) where heartbeat_id is not null;
