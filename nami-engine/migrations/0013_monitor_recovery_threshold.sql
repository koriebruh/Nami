-- Consecutive successes a down monitor needs before it recovers
-- (BetterStack's "recovery period").
alter table monitors
    add column recovery_threshold int not null default 1
        check (recovery_threshold between 1 and 10),
    add column consecutive_successes int not null default 0;
