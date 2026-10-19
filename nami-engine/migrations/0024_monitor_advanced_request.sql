-- Advanced request options (Phase 3 spec §5). proxy_password_enc is AES-GCM
-- sealed with AAD 'monitors.proxy_password:<id>' (see models/monitor_request.rs).
alter table monitors
    add column ip_version text not null default 'any'
        check (ip_version in ('any', 'ipv4', 'ipv6')),
    add column remember_cookies boolean not null default false,
    add column proxy_url text,
    add column proxy_username text,
    add column proxy_password_enc bytea,
    add constraint monitors_proxy_credentials_need_url
        check (proxy_url is not null or (proxy_username is null and proxy_password_enc is null));
