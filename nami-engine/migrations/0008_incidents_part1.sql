-- Incident management, part 1 (NAMI-INC-1): severities, the incident
-- lifecycle (open -> acknowledged -> resolved, reopen), the timeline.

create table incident_severities (
    id uuid default gen_random_uuid() constraint incident_severities_pkey primary key,
    project_id uuid not null
        constraint incident_severities_project_id_fkey references projects(id) on delete cascade,
    name text not null
        constraint incident_severities_name_format check (name ~ '^[a-z0-9_-]{1,32}$'),
    -- 1 = most severe.
    rank int not null
        constraint incident_severities_rank_range check (rank between 1 and 1000),
    is_default boolean not null default false,
    constraint incident_severities_name_unique unique (project_id, name),
    constraint incident_severities_rank_unique unique (project_id, rank)
);
create unique index incident_severities_one_default on incident_severities (project_id) where is_default;

-- Every project, however it is created, starts with the spec's three.
create function seed_incident_severities() returns trigger language plpgsql as $$
begin
    insert into incident_severities (project_id, name, rank, is_default)
    values (new.id, 'critical', 1, false), (new.id, 'high', 2, true), (new.id, 'low', 3, false);
    return new;
end
$$;
create trigger projects_seed_incident_severities after insert on projects
    for each row execute function seed_incident_severities();

insert into incident_severities (project_id, name, rank, is_default)
select p.id, s.name, s.rank, s.is_default
from projects p
cross join (values ('critical', 1, false), ('high', 2, true), ('low', 3, false)) as s(name, rank, is_default);

alter table incidents drop constraint incidents_status_check;
alter table incidents add constraint incidents_status_check
    check (status in ('open', 'acknowledged', 'resolved'));

-- A resolved row without a time cannot come from the worker (it always sets
-- resolved_at), but a hand-written row could: give it one before
-- incidents_resolved_has_time below checks every existing row.
update incidents set resolved_at = started_at where status = 'resolved' and resolved_at is null;

alter table incidents
    add column source text not null default 'manual'
        constraint incidents_source_known check (source in
            ('monitor', 'heartbeat', 'log_alert', 'metric_alert', 'manual', 'integration', 'report_form')),
    -- No action on delete: deleting a severity an incident uses is refused
    -- (checked at the end of the statement, so a project's cascade still
    -- removes both).
    add column severity_id uuid
        constraint incidents_severity_id_fkey references incident_severities(id) on delete no action,
    add column metadata jsonb not null default '{}'::jsonb
        constraint incidents_metadata_is_object check (jsonb_typeof(metadata) = 'object'),
    add column postmortem text
        constraint incidents_postmortem_size check (octet_length(postmortem) <= 102400),
    add column acknowledged_at timestamptz,
    add column acknowledged_by uuid
        constraint incidents_acknowledged_by_fkey references users(id) on delete set null,
    add column resolved_by uuid
        constraint incidents_resolved_by_fkey references users(id) on delete set null,
    add column created_by uuid
        constraint incidents_created_by_fkey references users(id) on delete set null,
    add constraint incidents_acknowledged_has_time check (status <> 'acknowledged' or acknowledged_at is not null),
    add constraint incidents_resolved_has_time check (status <> 'resolved' or resolved_at is not null);

update incidents set source = case
    when monitor_id is not null then 'monitor'
    when heartbeat_id is not null then 'heartbeat'
    when log_alert_id is not null then 'log_alert'
    else 'manual' end;

-- A row inserted without `source` (existing tests seed incidents in plain SQL
-- with only monitor_id, for example) still gets the source its foreign key
-- says, never the 'manual' default.
create function incidents_derive_source() returns trigger language plpgsql as $$
begin
    new.source := case
        when new.monitor_id is not null then 'monitor'
        when new.heartbeat_id is not null then 'heartbeat'
        when new.log_alert_id is not null then 'log_alert'
        else new.source end;
    return new;
end
$$;
create trigger incidents_derive_source before insert on incidents
    for each row execute function incidents_derive_source();

update incidents i set severity_id = s.id
from incident_severities s where s.project_id = i.project_id and s.is_default;

-- One unresolved incident per source: an acknowledged incident blocks a new
-- one as much as an open one does (the old indexes only covered 'open').
drop index incidents_one_open_per_monitor;
drop index incidents_one_open_per_heartbeat;
drop index incidents_one_open_per_log_alert;
create unique index incidents_one_unresolved_per_monitor on incidents (monitor_id) where status <> 'resolved';
create unique index incidents_one_unresolved_per_heartbeat on incidents (heartbeat_id) where status <> 'resolved';
create unique index incidents_one_unresolved_per_log_alert on incidents (log_alert_id) where status <> 'resolved';

create table incident_events (
    id bigint generated always as identity constraint incident_events_pkey primary key,
    incident_id uuid not null
        constraint incident_events_incident_id_fkey references incidents(id) on delete cascade,
    kind text not null
        constraint incident_events_kind_known check (kind in (
            'opened', 'acknowledged', 'resolved', 'reopened', 'escalated', 'notification_sent',
            'notification_failed', 'comment', 'postmortem_updated', 'integration_event', 'step_skipped',
            'escalation_fallback', 'escalation_exhausted', 'severity_changed', 'metadata_updated',
            'ticket_created')),
    actor_id uuid
        constraint incident_events_actor_id_fkey references users(id) on delete set null,
    message text
        constraint incident_events_message_size check (octet_length(message) <= 10240),
    metadata jsonb not null default '{}'::jsonb
        constraint incident_events_metadata_is_object check (jsonb_typeof(metadata) = 'object'),
    created_at timestamptz not null default now()
);
-- The timeline of one incident, in order.
create index incident_events_incident_idx on incident_events (incident_id, id);

-- History for incidents that predate the timeline.
insert into incident_events (incident_id, kind, created_at)
select id, 'opened', started_at from incidents;
insert into incident_events (incident_id, kind, created_at)
select id, 'resolved', resolved_at from incidents where status = 'resolved';

-- Notification channels (NAMI-INC-3). A channel belongs to a project (a team
-- room, admin-managed) or to one user (personal). Secrets are sealed with
-- core::crypto::Cipher under per-row contexts; `summary` is what the UI may show.
create table notification_channels (
    id uuid constraint notification_channels_pkey primary key,
    project_id uuid
        constraint notification_channels_project_id_fkey references projects(id) on delete cascade,
    user_id uuid
        constraint notification_channels_user_id_fkey references users(id) on delete cascade,
    name text not null
        constraint notification_channels_name_length check (char_length(name) between 1 and 100),
    kind text not null
        constraint notification_channels_kind_known
        check (kind in ('email', 'webhook', 'slack', 'teams', 'telegram', 'discord')),
    config_enc bytea not null,
    signing_secret_enc bytea,
    summary text not null,
    enabled boolean not null default true,
    last_tested_at timestamptz,
    created_by uuid
        constraint notification_channels_created_by_fkey references users(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint notification_channels_one_owner check ((project_id is null) <> (user_id is null)),
    constraint notification_channels_webhook_has_secret
        check ((kind = 'webhook') = (signing_secret_enc is not null)),
    -- A user's own verified address covers personal email (R8).
    constraint notification_channels_no_personal_email check (user_id is null or kind <> 'email')
);
create index notification_channels_project_idx on notification_channels (project_id) where project_id is not null;
create index notification_channels_user_idx on notification_channels (user_id) where user_id is not null;

-- On-call schedules (NAMI-INC-7). The rotation is evaluated by
-- oncall::rotation (pure); restrictions are TimeWindow objects in the
-- schedule's zone.
create table on_call_schedules (
    id uuid default gen_random_uuid() constraint on_call_schedules_pkey primary key,
    project_id uuid not null
        constraint on_call_schedules_project_id_fkey references projects(id) on delete cascade,
    name text not null,
    timezone text not null,
    rotation text not null
        constraint on_call_schedules_rotation_known check (rotation in ('daily', 'weekly', 'custom')),
    rotation_hours int
        constraint on_call_schedules_rotation_hours_range check (rotation_hours between 1 and 8760),
    handoff_time time not null,
    start_date date not null,
    restrictions jsonb not null default '[]'::jsonb
        constraint on_call_schedules_restrictions_is_array check (jsonb_typeof(restrictions) = 'array'),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint on_call_schedules_name_unique unique (project_id, name),
    constraint on_call_schedules_custom_has_hours check ((rotation = 'custom') = (rotation_hours is not null))
);

-- A deleted user's row goes by cascade and leaves a gap in the positions:
-- the cycle skips that slot until the next save rewrites them as 0..n-1.
create table on_call_participants (
    schedule_id uuid not null
        constraint on_call_participants_schedule_id_fkey references on_call_schedules(id) on delete cascade,
    position int not null
        constraint on_call_participants_position_range check (position >= 0),
    user_id uuid not null
        constraint on_call_participants_user_id_fkey references users(id) on delete cascade,
    constraint on_call_participants_pkey primary key (schedule_id, position),
    constraint on_call_participants_user_unique unique (schedule_id, user_id)
);

create table on_call_overrides (
    id uuid default gen_random_uuid() constraint on_call_overrides_pkey primary key,
    schedule_id uuid not null
        constraint on_call_overrides_schedule_id_fkey references on_call_schedules(id) on delete cascade,
    user_id uuid not null
        constraint on_call_overrides_user_id_fkey references users(id) on delete cascade,
    starts_at timestamptz not null,
    ends_at timestamptz not null,
    created_by uuid
        constraint on_call_overrides_created_by_fkey references users(id) on delete set null,
    created_at timestamptz not null default now(),
    constraint on_call_overrides_ends_after_start check (ends_at > starts_at)
);
-- The current and upcoming overrides of one schedule.
create index on_call_overrides_schedule_idx on on_call_overrides (schedule_id, ends_at);

-- The durable notification queue (NAMI-INC-5). One row per (incident,
-- round, target): `round` names the escalation pass and step that queued it
-- (e.g. 'p0r0s1', see incidents::escalation) or 'resolved', so re-running a
-- step never notifies the same target twice and a sent row is never resent.
create table notification_deliveries (
    id uuid default gen_random_uuid() constraint notification_deliveries_pkey primary key,
    incident_id uuid not null
        constraint notification_deliveries_incident_id_fkey references incidents(id) on delete cascade,
    event text not null
        constraint notification_deliveries_event_known check (event in ('triggered', 'resolved')),
    round text not null
        constraint notification_deliveries_round_length check (char_length(round) between 1 and 32),
    target_key text not null,
    user_id uuid
        constraint notification_deliveries_user_id_fkey references users(id) on delete cascade,
    channel_id uuid
        constraint notification_deliveries_channel_id_fkey references notification_channels(id) on delete cascade,
    status text not null default 'pending'
        constraint notification_deliveries_status_known
        check (status in ('pending', 'sending', 'sent', 'failed', 'cancelled')),
    attempts int not null default 0
        constraint notification_deliveries_attempts_range check (attempts between 0 and 5),
    next_attempt_at timestamptz not null default now(),
    locked_until timestamptz,
    last_error text
        constraint notification_deliveries_last_error_length check (char_length(last_error) <= 200),
    created_at timestamptz not null default now(),
    sent_at timestamptz,
    constraint notification_deliveries_target_unique unique (incident_id, round, target_key),
    constraint notification_deliveries_has_target check (user_id is not null or channel_id is not null),
    constraint notification_deliveries_sent_has_time check (status <> 'sent' or sent_at is not null)
);
create index notification_deliveries_due_idx on notification_deliveries (next_attempt_at)
    where status in ('pending', 'sending');
-- Rate caps look at recent sends per channel and per user.
create index notification_deliveries_channel_sent_idx on notification_deliveries (channel_id, sent_at)
    where channel_id is not null and status = 'sent';
create index notification_deliveries_email_sent_idx on notification_deliveries (user_id, sent_at)
    where channel_id is null and status = 'sent';
