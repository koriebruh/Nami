create table monitors (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    check_type text not null check (check_type in ('http', 'keyword', 'tcp')),
    url text,                 -- http, keyword
    host text,                -- tcp
    port int,                 -- tcp
    http_method text not null default 'GET' check (http_method in ('GET', 'HEAD')),
    keyword text,             -- keyword
    keyword_mode text not null default 'contains'
        check (keyword_mode in ('contains', 'not_contains')),
    expected_status_min int not null default 200,
    expected_status_max int not null default 399,
    interval_seconds int not null default 60 check (interval_seconds >= 30),
    timeout_seconds int not null default 10 check (timeout_seconds between 1 and 60),
    confirmation_threshold int not null default 2 check (confirmation_threshold between 1 and 10),
    paused boolean not null default false,
    current_status text not null default 'pending'
        check (current_status in ('pending', 'up', 'down')),
    consecutive_failures int not null default 0,
    last_checked_at timestamptz,
    next_check_at timestamptz not null default now(),
    created_at timestamptz not null default now()
);

-- The worker's claim query scans unpaused monitors in next_check_at order.
create index monitors_due_idx on monitors (next_check_at) where not paused;
create index monitors_project_idx on monitors (project_id);
