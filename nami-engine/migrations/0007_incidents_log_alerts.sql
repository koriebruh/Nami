-- Incidents opened by log alerts (Logs part 2): logs::alert opens and
-- resolves them through incidents::model::{open,resolve}_for_log_alert.
alter table incidents add column log_alert_id uuid
    constraint incidents_log_alert_id_fkey references log_alerts(id) on delete cascade;
-- At most one open incident per log alert (mirrors monitors and heartbeats).
create unique index incidents_one_open_per_log_alert on incidents (log_alert_id) where status = 'open';
-- Every incident of one log alert (the per-alert list and the delete cascade),
-- like incidents_monitor_idx and incidents_heartbeat_idx in 0004_incidents.sql.
create index incidents_log_alert_idx on incidents (log_alert_id) where log_alert_id is not null;
