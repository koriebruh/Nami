use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;

/// How the Postgres pool is sized and bounded (see the `database_*` fields of
/// [`Config`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    pub statement_timeout: Duration,
}

impl PoolSettings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_connections: config.database_max_connections,
            acquire_timeout: config.database_acquire_timeout,
            statement_timeout: config.database_statement_timeout,
        }
    }
}

/// The pool's options: its size, how long a caller waits for a connection, and
/// a `statement_timeout` set on every connection as it is opened, so that no
/// statement (a hung query, a lock wait) can hold a connection, and whatever
/// waits on it, for longer than that. The value travels as a bound parameter
/// of `set_config`, never as SQL text.
pub fn pool_options(settings: &PoolSettings) -> PgPoolOptions {
    let statement_timeout_ms = settings.statement_timeout.as_millis().to_string();
    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .after_connect(move |conn, _meta| {
            let statement_timeout_ms = statement_timeout_ms.clone();
            Box::pin(async move {
                sqlx::query("select set_config('statement_timeout', $1, false)")
                    .bind(statement_timeout_ms)
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
}

pub async fn connect_pool(
    database_url: &str,
    settings: &PoolSettings,
) -> Result<PgPool, sqlx::Error> {
    pool_options(settings).connect(database_url).await
}
