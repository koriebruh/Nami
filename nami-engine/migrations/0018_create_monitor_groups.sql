create table monitor_groups (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    name text not null,
    position int not null default 0,
    created_at timestamptz not null default now(),
    -- Target of monitors' (group_id, project_id) foreign key.
    unique (id, project_id)
);

create index monitor_groups_project_idx on monitor_groups (project_id);

alter table monitors
    add column group_id uuid,
    -- The pair makes a group of another project impossible to reference;
    -- deleting a group ungroups its monitors (only group_id is cleared).
    -- MATCH SIMPLE: a null group_id is not checked.
    add constraint monitors_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id);
