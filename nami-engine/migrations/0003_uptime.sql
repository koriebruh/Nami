-- Uptime domain baseline: monitor groups, monitors, maintenance windows,
-- heartbeats and the worker's liveness record. Squashed on 2026-10-19 from
-- 0009, 0011-0019, 0023 and 0024. Column order is the order the original
-- migrations created and added the columns in.

create table monitor_groups (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    position int not null default 0,
    created_at timestamptz not null default now(),
    -- Target of the (group_id, project_id) foreign keys of monitors and
    -- heartbeats.
    unique (id, project_id)
);

create index monitor_groups_project_idx on monitor_groups (project_id);

create table monitors (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    check_type text not null check (check_type in (
        'http', 'keyword', 'tcp', 'ping', 'dns', 'ssl_expiry', 'domain_expiry',
        'smtp', 'pop3', 'imap', 'udp'
    )),
    url text,                 -- http, keyword
    host text,                -- tcp
    port int,                 -- tcp
    http_method text not null default 'GET'
        check (http_method in ('GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS')),
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
    created_at timestamptz not null default now(),
    -- Request configuration for http/keyword monitors.
    request_body text,
    -- AES-256-GCM (src/core/crypto.rs, associated data
    -- "monitors.request_headers:<this row's id>") of the JSON list
    -- [{"name","value"}]. Only the names are kept in the clear, so reads never
    -- need the key.
    request_headers_enc bytea,
    request_header_names text[] not null default '{}',
    auth_username text,
    -- AES-256-GCM of the basic-auth password (associated data
    -- "monitors.auth_password:<this row's id>"), so a sealed value copied onto
    -- another row does not open there.
    auth_password_enc bytea,
    follow_redirects boolean not null default true,
    verify_ssl boolean not null default true,
    -- Consecutive successes a down monitor needs before it recovers
    -- (BetterStack's "recovery period").
    recovery_threshold int not null default 1
        check (recovery_threshold between 1 and 10),
    consecutive_successes int not null default 0,
    -- dns monitors.
    dns_record_type text
        check (dns_record_type in ('A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS')),
    dns_nameserver text,      -- "ip:port"; null = the system resolver
    expected_value text,      -- an answer must contain it (optional)
    -- ssl_expiry and domain_expiry monitors.
    expiry_threshold_days int
        check (expiry_threshold_days between 1 and 365),
    use_tls boolean not null default false,  -- smtp/pop3/imap: implicit TLS
    udp_payload text,                         -- udp: the datagram to send
    group_id uuid,
    -- Advanced request options. proxy_password_enc is AES-GCM sealed with AAD
    -- 'monitors.proxy_password:<id>' (see uptime/monitor/request.rs).
    ip_version text not null default 'any'
        check (ip_version in ('any', 'ipv4', 'ipv6')),
    remember_cookies boolean not null default false,
    proxy_url text,
    proxy_username text,
    proxy_password_enc bytea,
    -- Expiry dates move slowly: these types are checked at most hourly.
    constraint monitors_expiry_interval_check
        check (check_type not in ('ssl_expiry', 'domain_expiry') or interval_seconds >= 3600),
    -- Lets other tables reference a monitor together with its project, so the
    -- database itself refuses a reference to another project's monitor.
    constraint monitors_id_project_key unique (id, project_id),
    -- The pair makes a group of another project impossible to reference;
    -- deleting a group ungroups its monitors (only group_id is cleared).
    -- MATCH SIMPLE: a null group_id is not checked.
    constraint monitors_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id),
    constraint monitors_proxy_credentials_need_url
        check (proxy_url is not null or (proxy_username is null and proxy_password_enc is null))
);

-- The worker's claim query scans unpaused monitors in next_check_at order.
create index monitors_due_idx on monitors (next_check_at) where not paused;
create index monitors_project_idx on monitors (project_id);

-- Maintenance windows: while one covers a monitor or heartbeat, its checks and
-- pings are still recorded, but its state machine stands still (no incident
-- opens or resolves, no alert, no counter moves).
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
    -- When the heartbeat was last resumed from a pause (null: never). The
    -- sweep counts a heartbeat's period from the latest of this, the last
    -- ping and the creation, so a resume gives the job a fresh period.
    resumed_at timestamptz,
    created_at timestamptz not null default now(),
    -- The end of the last outage that a heartbeat's deadline fell into (null:
    -- none). The sweep counts a heartbeat's period from the latest of this, its
    -- last ping, its last resume and its creation. Stored per heartbeat so that
    -- the fresh period survives later restarts.
    outage_anchor_at timestamptz,
    -- One group list per project for monitors and heartbeats. Same composite
    -- key as monitors: a group of another project cannot be referenced, and
    -- deleting a group ungroups its heartbeats.
    group_id uuid,
    constraint heartbeats_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id)
);

create index heartbeats_project_idx on heartbeats (project_id);
create index heartbeats_group_idx on heartbeats (group_id) where group_id is not null;

-- Worker liveness. The uptime worker records here, on every heartbeat sweep,
-- that it is alive. When it starts again (or comes back after the database
-- was unreachable), the gap since the last record is an outage in which no
-- ping could be received: only the heartbeats whose deadline fell inside that
-- gap get a fresh period from its end (see nami-engine/src/uptime/heartbeat/mod.rs,
-- `note_worker_alive`). One row at most.
create table worker_state (
    id int primary key default 1 check (id = 1),
    last_alive_at timestamptz not null
);
