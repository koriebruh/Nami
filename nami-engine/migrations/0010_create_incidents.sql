-- The spec's incidents table minus heartbeat_id, which
-- 0011_create_heartbeats.sql adds once the heartbeats table exists.
create table incidents (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    monitor_id uuid references monitors(id) on delete cascade,
    title text not null,
    cause text,
    status text not null default 'open' check (status in ('open', 'resolved')),
    started_at timestamptz not null default now(),
    resolved_at timestamptz
);

-- Incident lists filter by project and status, newest first.
create index incidents_project_status_idx on incidents (project_id, status, started_at desc);

-- At most one open incident per monitor. The worker relies on this to stay
-- idempotent even if two engine instances race on the same monitor.
create unique index incidents_one_open_per_monitor on incidents (monitor_id) where status = 'open';
