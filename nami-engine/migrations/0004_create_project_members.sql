create table project_members (
    project_id uuid not null references projects(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    project_role text not null check (project_role in ('admin', 'member', 'viewer')),
    primary key (project_id, user_id)
);
