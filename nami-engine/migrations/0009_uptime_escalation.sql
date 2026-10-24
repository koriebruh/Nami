-- Monitors and heartbeats pick an escalation policy and a severity for the
-- incidents they open (NAMI-INC-10). Null = the project default; deleting the
-- policy or severity falls back to the default. Runs after
-- 0008_incidents_part1.sql, which creates both referenced tables.
alter table monitors
    add column escalation_policy_id uuid
        constraint monitors_escalation_policy_id_fkey references escalation_policies(id) on delete set null,
    add column severity_id uuid
        constraint monitors_severity_id_fkey references incident_severities(id) on delete set null;
alter table heartbeats
    add column escalation_policy_id uuid
        constraint heartbeats_escalation_policy_id_fkey references escalation_policies(id) on delete set null,
    add column severity_id uuid
        constraint heartbeats_severity_id_fkey references incident_severities(id) on delete set null;
-- Deleting a policy or a severity clears it on its sources (set null) through these.
create index monitors_escalation_policy_idx on monitors (escalation_policy_id)
    where escalation_policy_id is not null;
create index monitors_severity_idx on monitors (severity_id) where severity_id is not null;
create index heartbeats_escalation_policy_idx on heartbeats (escalation_policy_id)
    where escalation_policy_id is not null;
create index heartbeats_severity_idx on heartbeats (severity_id) where severity_id is not null;
-- R31: when one of the source's step notifications was last sent
-- (incidents::deliveries). An automatic open or reopen of its incident runs
-- its first step no sooner than SOURCE_PAGE_GAP_SECS after it. Read by
-- primary key, so the hold costs the same however many incidents the source
-- has had.
alter table monitors add column last_paged_at timestamptz;
alter table heartbeats add column last_paged_at timestamptz;
