-- Output of the heartbeat ping that opened the incident (sanitized, at most
-- 10 KiB plus a truncation marker; see uptime::heartbeat::sanitize_output).
-- Numbers 0020-0021 are left to the Phase 2 final fix wave.
alter table incidents add column details text
    check (details is null or octet_length(details) <= 10240 + 16);
