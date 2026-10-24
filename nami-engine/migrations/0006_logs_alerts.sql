-- Log alerts (Logs part 2): a count of matching log rows over a window,
-- compared with a threshold. The worker claims due alerts like monitors
-- (for update skip locked) and opens/resolves incidents through
-- incidents.log_alert_id.
create table log_alerts (
    id uuid default gen_random_uuid() constraint log_alerts_pkey primary key,
    project_id uuid not null
        constraint log_alerts_project_id_fkey references projects(id) on delete cascade,
    name text not null,
    -- logs::view::saved_query::LogQuery, validated by the engine.
    query jsonb not null default '{}'::jsonb
        constraint log_alerts_query_is_object check (jsonb_typeof(query) = 'object'),
    threshold bigint not null
        constraint log_alerts_threshold_range check (threshold between 0 and 1000000000000),
    comparison text not null
        constraint log_alerts_comparison_known check (comparison in ('gt', 'lt')),
    window_seconds int not null
        constraint log_alerts_window_range check (window_seconds between 60 and 86400),
    interval_seconds int not null
        constraint log_alerts_interval_range check (interval_seconds between 60 and 86400),
    enabled boolean not null default true,
    state text not null default 'ok'
        constraint log_alerts_state_known check (state in ('ok', 'alerting')),
    last_evaluated_at timestamptz,
    last_count bigint,
    -- Why the last evaluation failed (fixed, user-safe text); null after a success.
    last_error text,
    last_error_at timestamptz,
    next_evaluation_at timestamptz not null default now(),
    created_by uuid
        constraint log_alerts_created_by_fkey references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    -- `lt 0` can never fire.
    constraint log_alerts_lt_needs_a_threshold check (comparison = 'gt' or threshold >= 1)
);

create index log_alerts_project_idx on log_alerts (project_id);
-- The worker's claim: due, enabled alerts, most overdue first.
create index log_alerts_due_idx on log_alerts (next_evaluation_at) where enabled;
