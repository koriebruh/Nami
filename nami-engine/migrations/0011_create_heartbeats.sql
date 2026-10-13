create table heartbeats (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    token text not null unique,
    period_seconds int not null check (period_seconds >= 30),
    grace_seconds int not null default 60 check (grace_seconds >= 0),
    paused boolean not null default false,
    current_status text not null default 'pending'
        check (current_status in ('pending', 'up', 'down')),
    last_ping_at timestamptz,
    created_at timestamptz not null default now()
);

create index heartbeats_project_idx on heartbeats (project_id);

-- Completes the spec's incidents table: 0010 could not reference
-- heartbeats before this table existed.
alter table incidents add column heartbeat_id uuid references heartbeats(id) on delete cascade;

-- At most one open incident per heartbeat (mirrors the monitor index).
create unique index incidents_one_open_per_heartbeat on incidents (heartbeat_id) where status = 'open';
