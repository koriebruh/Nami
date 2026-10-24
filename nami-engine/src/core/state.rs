use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    core::clickhouse::ClickHouse, core::config::Config, core::crypto::Cipher, core::email,
    logs::LogsState,
};

/// A ClickHouse database name that is never created. `AppState::for_test`
/// binds its client to it, so a test that touches ClickHouse without opting
/// into `ClickHouse::create_test_database()` fails loudly (UNKNOWN_DATABASE)
/// instead of reading or writing the developer's real `nami` database.
const UNCONFIGURED_TEST_CLICKHOUSE_DATABASE: &str = "nami_unconfigured_in_tests";

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: email::Mailer,
    pub clickhouse: ClickHouse,
    /// Seals and opens monitor secrets; built from `config.encryption_key`.
    pub cipher: Cipher,
    /// Log ingest rate limits and usage counters (in memory, per instance).
    pub logs: Arc<LogsState>,
    pub config: Config,
}

impl AppState {
    /// Test-only constructor (same precedent as
    /// `core::router::with_fallback_connect_info`): every field gets the
    /// defaults tests expect — `Config::from_env_for_test()`, the Mailpit
    /// mailer, a ClickHouse client bound to a database that does not
    /// exist, a cipher over the fixed `core::crypto::TEST_KEY`, and fresh log
    /// ingest state (default rate limits, no usage). Tests override
    /// single fields with struct-update syntax, e.g.
    /// `AppState { mailer: Mailer::new("localhost", 1), ..AppState::for_test(pool) }`,
    /// so adding a field to `AppState` never touches test files again.
    pub fn for_test(pool: PgPool) -> AppState {
        let config = Config::from_env_for_test();
        AppState {
            db: pool,
            mailer: email::Mailer::new(&config.smtp_host, config.smtp_port),
            clickhouse: ClickHouse::new(
                &config.clickhouse_url,
                &config.clickhouse_user,
                &config.clickhouse_password,
                UNCONFIGURED_TEST_CLICKHOUSE_DATABASE,
            ),
            cipher: Cipher::new(&config.encryption_key),
            logs: Arc::new(LogsState::default()),
            config,
        }
    }
}
