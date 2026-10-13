-- Worker liveness. The uptime worker records here, on every heartbeat sweep,
-- that it is alive. When it starts again (or comes back after the database
-- was unreachable), the gap since the last record is an outage in which no
-- ping could be received: only the heartbeats whose deadline fell inside that
-- gap get a fresh period from its end (see nami-engine/src/uptime/heartbeat.rs,
-- `note_worker_alive`). One row at most.
create table worker_state (
    id int primary key default 1 check (id = 1),
    last_alive_at timestamptz not null
);

-- The end of the last outage that a heartbeat's deadline fell into (null:
-- none). The sweep counts a heartbeat's period from the latest of this, its
-- last ping, its last resume and its creation. Stored per heartbeat so that
-- the fresh period survives later restarts.
alter table heartbeats add column outage_anchor_at timestamptz;

-- Every incident of one monitor or heartbeat: the per-source incident lists,
-- and the cascades that delete a monitor's or a heartbeat's incidents, look
-- them up by these columns (the unique indexes from 0010/0011 cover only the
-- open ones).
create index incidents_monitor_idx on incidents (monitor_id) where monitor_id is not null;
create index incidents_heartbeat_idx on incidents (heartbeat_id) where heartbeat_id is not null;
