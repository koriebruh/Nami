-- Auth domain baseline: users, OAuth identities, email verification and
-- password reset tokens. Squashed on 2026-10-19 from 0001, 0002, 0005, 0006
-- and 0008 (see docs/superpowers/decisions/2026-10-19-domain-restructure.md).
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
    created_at timestamptz not null default now(),
    password_changed_at timestamptz not null default now()
);

create table oauth_identities (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    provider text not null check (provider in ('google', 'github')),
    provider_user_id text not null,
    created_at timestamptz not null default now(),
    unique (provider, provider_user_id)
);

create table email_verification_tokens (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null,
    expires_at timestamptz not null,
    consumed_at timestamptz
);

create table password_reset_tokens (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null,
    expires_at timestamptz not null,
    consumed_at timestamptz
);

create index idx_email_verification_tokens_token_hash on email_verification_tokens(token_hash);
create index idx_password_reset_tokens_token_hash on password_reset_tokens(token_hash);
