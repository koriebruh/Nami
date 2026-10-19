-- Saved explorer views (Logs part 2). `query` is a logs::saved_query::ViewQuery
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
