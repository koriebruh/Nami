create extension if not exists pgcrypto;

create table users (
    id uuid primary key default gen_random_uuid(),
    email text not null unique,
    password_hash text,
    email_verified_at timestamptz,
    global_role text not null default 'member'
        check (global_role in ('admin', 'member', 'viewer')),
    totp_secret text,
    totp_enabled boolean not null default false,
    created_at timestamptz not null default now()
);
