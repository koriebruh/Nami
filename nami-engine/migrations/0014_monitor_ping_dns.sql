-- ping and dns monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in ('http', 'keyword', 'tcp', 'ping', 'dns'));

alter table monitors
    add column dns_record_type text
        check (dns_record_type in ('A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS')),
    add column dns_nameserver text,   -- "ip:port"; null = the system resolver
    add column expected_value text;   -- an answer must contain it (optional)
