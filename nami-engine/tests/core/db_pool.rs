// Requires `docker compose up -d postgres`.
//
// The Postgres pool as `main.rs` builds it (`core::db::pool_options`): bounded in
// size, in how long a caller waits for a connection, and in how long any one
// statement may run.
use std::time::{Duration, Instant};

use nami_engine::{
    core::config::Config,
    core::db::{PoolSettings, pool_options, run_migrations},
};

/// A pool with `settings` over this test's database.
async fn pool_with(pool: &sqlx::PgPool, settings: PoolSettings) -> sqlx::PgPool {
    pool_options(&settings)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

#[test]
fn the_settings_come_from_the_config() {
    assert_eq!(
        PoolSettings::from_config(&Config::from_env_for_test()),
        PoolSettings {
            max_connections: 20,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(30),
        }
    );
}

/// Every connection the pool opens carries the statement timeout, so a hung
/// query fails instead of holding its connection (and whatever waits on it).
#[sqlx::test]
async fn a_statement_running_past_the_timeout_is_cancelled(pool: sqlx::PgPool) {
    let bounded = pool_with(
        &pool,
        PoolSettings {
            max_connections: 2,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_millis(1500),
        },
    )
    .await;

    // Both connections at once, so the pool has to open each of them.
    let mut connections = [
        bounded.acquire().await.unwrap(),
        bounded.acquire().await.unwrap(),
    ];
    for conn in &mut connections {
        let setting: String = sqlx::query_scalar("show statement_timeout")
            .fetch_one(&mut **conn)
            .await
            .unwrap();
        assert_eq!(setting, "1500ms");
    }
    drop(connections);

    let started = Instant::now();
    let error = sqlx::query("select pg_sleep(5)")
        .execute(
            &pool_with(
                &pool,
                PoolSettings {
                    max_connections: 1,
                    acquire_timeout: Duration::from_secs(5),
                    statement_timeout: Duration::from_millis(1500),
                },
            )
            .await,
        )
        .await
        .expect_err("the statement outlives the timeout");
    let elapsed = started.elapsed();

    let code = error
        .as_database_error()
        .and_then(|e| e.code().map(|code| code.to_string()));
    assert_eq!(code.as_deref(), Some("57014"), "query_canceled: {error}");
    assert!(
        elapsed < Duration::from_secs(4),
        "cancelled after {elapsed:?}"
    );
    // A statement within the limit is unaffected.
    sqlx::query("select pg_sleep(0.1)")
        .execute(&bounded)
        .await
        .unwrap();
}

/// With every connection taken, a caller gets an error after the acquire
/// timeout instead of waiting for as long as the holder likes.
#[sqlx::test]
async fn waiting_for_a_connection_gives_up_after_the_acquire_timeout(pool: sqlx::PgPool) {
    let bounded = pool_with(
        &pool,
        PoolSettings {
            max_connections: 1,
            acquire_timeout: Duration::from_millis(500),
            statement_timeout: Duration::from_secs(30),
        },
    )
    .await;
    let _held = bounded.acquire().await.unwrap();

    let started = Instant::now();
    let error = bounded.acquire().await.expect_err("no connection is free");
    let elapsed = started.elapsed();

    assert!(matches!(error, sqlx::Error::PoolTimedOut), "{error}");
    assert!(
        (Duration::from_millis(400)..Duration::from_secs(3)).contains(&elapsed),
        "gave up after {elapsed:?}"
    );
}

/// Migrations run on a connection of their own with no statement timeout: a
/// wait for the migration table (another instance migrating) or a long
/// migration is never cancelled half-way through startup, even when the
/// database itself sets a timeout for every new connection. The same run on a
/// pool with the request timeout is cancelled, which is why it is not done
/// there.
#[sqlx::test]
async fn migrations_wait_out_a_lock_that_would_cancel_them_on_the_pool(pool: sqlx::PgPool) {
    // Every new connection to this database gets a 1 s statement timeout.
    sqlx::query(
        "do $$ begin
             execute format('alter database %I set statement_timeout = %L', current_database(), '1s');
         end $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Another instance busy with the migration table, for 2.5 s.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("lock table _sqlx_migrations in access exclusive mode")
        .execute(&mut *holder)
        .await
        .unwrap();

    let bounded = pool_with(
        &pool,
        PoolSettings {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(1),
        },
    )
    .await;
    let cancelled = sqlx::migrate!("./migrations")
        .run(&bounded)
        .await
        .expect_err("on the pool the wait is cancelled");
    assert!(
        cancelled.to_string().contains("statement timeout"),
        "{cancelled}"
    );
    // The cancelled run keeps holding the migrator's advisory lock on its
    // connection: closing the pool lets it go.
    bounded.close().await;

    let options = (*pool.connect_options()).clone();
    let started = Instant::now();
    let (migrated, ()) = tokio::join!(run_migrations(&options), async {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        holder.rollback().await.unwrap();
    });
    let waited = started.elapsed();

    migrated.expect("the migration run waited the lock out");
    assert!(
        waited >= Duration::from_millis(2400),
        "it waited for the lock, past the database's 1 s timeout: {waited:?}"
    );
}
