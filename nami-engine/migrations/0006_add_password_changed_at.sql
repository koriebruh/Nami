alter table users add column password_changed_at timestamptz not null default now();
