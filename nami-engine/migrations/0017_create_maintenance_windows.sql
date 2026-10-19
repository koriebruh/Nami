-- Maintenance windows (NAMI-UP-16): while one covers a monitor or heartbeat,
-- its checks and pings are still recorded, but its state machine stands still
-- (no incident opens or resolves, no alert, no counter moves).

-- Lets other tables reference a monitor together with its project, so the
-- database itself refuses a reference to another project's monitor.
alter table monitors add constraint monitors_id_project_key unique (id, project_id);

create table maintenance_windows (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    -- null = every monitor and every heartbeat of the project; otherwise the
    -- one monitor it covers (heartbeats are only covered by project-wide
    -- windows).
    monitor_id uuid,
    title text not null,
    kind text not null check (kind in ('once', 'weekly')),
    -- kind = 'once': from starts_at (inclusive) to ends_at (exclusive).
    starts_at timestamptz,
    ends_at timestamptz,
    -- kind = 'weekly': on each ISO weekday (1 = Monday ... 7 = Sunday) the
    -- window runs from start_time to end_time, wall-clock time in timezone (an
    -- IANA name); an end_time at or before start_time ends on the following
    -- day (equal times: a full day).
    days_of_week smallint[],
    start_time time,
    end_time time,
    timezone text,
    created_at timestamptz not null default now(),
    -- MATCH SIMPLE: a null monitor_id (project-wide) is not checked.
    foreign key (monitor_id, project_id) references monitors (id, project_id) on delete cascade,
    check (
        (kind = 'once'
            and starts_at is not null and ends_at is not null and ends_at > starts_at
            and days_of_week is null and start_time is null and end_time is null and timezone is null)
        or (kind = 'weekly'
            and starts_at is null and ends_at is null
            and cardinality(days_of_week) between 1 and 7
            and days_of_week <@ array[1, 2, 3, 4, 5, 6, 7]::smallint[]
            and start_time is not null and end_time is not null and timezone is not null)
    )
);

create index maintenance_windows_project_idx on maintenance_windows (project_id);
-- The heartbeat sweep reads every project-wide window.
create index maintenance_windows_project_wide_idx on maintenance_windows (project_id)
    where monitor_id is null;
-- Deleting a monitor cascades to its windows through the composite key.
create index maintenance_windows_monitor_idx on maintenance_windows (monitor_id, project_id)
    where monitor_id is not null;
