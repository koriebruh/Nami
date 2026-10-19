-- One group list per project for monitors and heartbeats (spec §3). Same
-- composite key as monitors (0018): a group of another project cannot be
-- referenced, and deleting a group ungroups its heartbeats.
alter table heartbeats
    add column group_id uuid,
    add constraint heartbeats_group_fkey foreign key (group_id, project_id)
        references monitor_groups (id, project_id) on delete set null (group_id);

create index heartbeats_group_idx on heartbeats (group_id) where group_id is not null;
