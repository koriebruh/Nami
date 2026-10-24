-- Log alerts pick an escalation policy and a severity for the incidents they
-- open (NAMI-INC-10), as monitors and heartbeats do (0009_uptime_escalation.sql).
alter table log_alerts
    add column escalation_policy_id uuid
        constraint log_alerts_escalation_policy_id_fkey references escalation_policies(id) on delete set null,
    add column severity_id uuid
        constraint log_alerts_severity_id_fkey references incident_severities(id) on delete set null;
create index log_alerts_escalation_policy_idx on log_alerts (escalation_policy_id)
    where escalation_policy_id is not null;
create index log_alerts_severity_idx on log_alerts (severity_id) where severity_id is not null;
-- R31: when one of the alert's step notifications was last sent, as on
-- monitors and heartbeats (0009_uptime_escalation.sql).
alter table log_alerts add column last_paged_at timestamptz;
