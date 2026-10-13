create table oauth_identities (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    provider text not null check (provider in ('google', 'github')),
    provider_user_id text not null,
    created_at timestamptz not null default now(),
    unique (provider, provider_user_id)
);
