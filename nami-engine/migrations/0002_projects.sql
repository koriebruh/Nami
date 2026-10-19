-- Projects domain baseline: projects and their members. Squashed on
-- 2026-10-19 from 0003, 0004 and 0007.
create table projects (
    id uuid primary key default gen_random_uuid(),
    name text not null,
    slug text not null unique,
    created_at timestamptz not null default now(),
    created_by uuid references users(id)
);

create table project_members (
    project_id uuid not null references projects(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    project_role text not null check (project_role in ('admin', 'member', 'viewer')),
    primary key (project_id, user_id)
);
