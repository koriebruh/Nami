-- ssl_expiry and domain_expiry monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in ('http', 'keyword', 'tcp', 'ping', 'dns', 'ssl_expiry', 'domain_expiry'));

alter table monitors
    add column expiry_threshold_days int
        check (expiry_threshold_days between 1 and 365),
    -- Expiry dates move slowly: these types are checked at most hourly.
    add constraint monitors_expiry_interval_check
        check (check_type not in ('ssl_expiry', 'domain_expiry') or interval_seconds >= 3600);
