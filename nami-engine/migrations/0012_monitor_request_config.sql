-- Request configuration for http/keyword monitors (Uptime Phase 2).
alter table monitors drop constraint monitors_http_method_check;
alter table monitors add constraint monitors_http_method_check
    check (http_method in ('GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'));

alter table monitors
    add column request_body text,
    -- AES-256-GCM (src/crypto.rs, associated data
    -- "monitors.request_headers:<this row's id>") of the JSON list
    -- [{"name","value"}]. Only the names are kept in the clear, so reads never
    -- need the key.
    add column request_headers_enc bytea,
    add column request_header_names text[] not null default '{}',
    add column auth_username text,
    -- AES-256-GCM of the basic-auth password (associated data
    -- "monitors.auth_password:<this row's id>"), so a sealed value copied onto
    -- another row does not open there.
    add column auth_password_enc bytea,
    add column follow_redirects boolean not null default true,
    add column verify_ssl boolean not null default true;
