-- smtp, pop3, imap and udp monitor types (Phase 2).
alter table monitors drop constraint monitors_check_type_check;
alter table monitors add constraint monitors_check_type_check
    check (check_type in (
        'http', 'keyword', 'tcp', 'ping', 'dns', 'ssl_expiry', 'domain_expiry',
        'smtp', 'pop3', 'imap', 'udp'
    ));

alter table monitors
    add column use_tls boolean not null default false,  -- smtp/pop3/imap: implicit TLS
    add column udp_payload text;                         -- udp: the datagram to send
