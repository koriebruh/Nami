//! Thin client for ClickHouse's HTTP interface, built on the existing
//! `reqwest` dependency (no driver crate). Shared by the uptime sub-project
//! now and the logs/metrics sub-projects later.
//!
//! Every value reaches ClickHouse as a query parameter — a `{name:Type}`
//! placeholder in the SQL plus a `param_name=value` URL parameter — never by
//! string interpolation. Database and table names travel the same way, as
//! `Identifier` parameters, so no SQL text in this module is built with
//! `format!`.

use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

/// Settings sent with every request. ClickHouse 26.x already defaults to
/// these, but sending them pins the wire format this client relies on:
/// - `date_time_input_format=best_effort`: accept chrono's RFC 3339 output
///   (`2026-10-13T10:00:00.123456789Z`) in inserted rows.
/// - `date_time_output_format=iso`: emit `2026-10-13T10:00:00.123Z`, which
///   chrono's `DateTime<Utc>` deserializer accepts.
/// - `output_format_json_quote_64bit_integers=0`: emit `count()` (UInt64)
///   as a JSON number, not a string (older servers quote by default).
const BASE_SETTINGS: [(&str, &str); 3] = [
    ("date_time_input_format", "best_effort"),
    ("date_time_output_format", "iso"),
    ("output_format_json_quote_64bit_integers", "0"),
];

/// Idempotent DDL applied by [`ClickHouse::migrate`], in order. Table names
/// are unqualified because every request carries the `database` setting.
const MIGRATIONS: [&str; 1] = ["CREATE TABLE IF NOT EXISTS uptime_checks (
    monitor_id UUID,
    project_id UUID,
    checked_at DateTime64(3, 'UTC'),
    status Enum8('up' = 1, 'down' = 2),
    status_code Nullable(UInt16),
    response_time_ms Nullable(UInt32),
    error Nullable(String)
) ENGINE = MergeTree
ORDER BY (monitor_id, checked_at)
TTL toDateTime(checked_at) + INTERVAL 180 DAY"];

/// Prefix of the throwaway databases created by
/// [`ClickHouse::create_test_database`].
pub const TEST_DATABASE_PREFIX: &str = "nami_test_";

#[derive(Debug, thiserror::Error)]
pub enum ClickHouseError {
    #[error("clickhouse request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// Non-2xx response; `body` is ClickHouse's own error text.
    #[error("clickhouse returned HTTP {status}: {body}")]
    Server { status: u16, body: String },
    #[error("could not encode a row for clickhouse: {0}")]
    Encode(serde_json::Error),
    #[error("could not decode clickhouse row {line:?}: {source}")]
    Decode {
        line: String,
        source: serde_json::Error,
    },
}

#[derive(Clone)]
pub struct ClickHouse {
    http: reqwest::Client,
    url: reqwest::Url,
    user: String,
    password: String,
    database: String,
}

impl ClickHouse {
    /// Panics on an unparsable `url`: like a non-numeric `PORT` in
    /// `Config::from_env`, a broken `CLICKHOUSE_URL` is a startup error.
    pub fn new(url: &str, user: &str, password: &str, database: &str) -> Self {
        let url = reqwest::Url::parse(url).expect("CLICKHOUSE_URL must be a valid URL");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build the ClickHouse HTTP client");
        Self {
            http,
            url,
            user: user.to_string(),
            password: password.to_string(),
            database: database.to_string(),
        }
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// Runs one statement (DDL, `OPTIMIZE`, ...) against this client's database.
    pub async fn execute(&self, sql: &str) -> Result<(), ClickHouseError> {
        self.post(self.settings(true), sql.to_string()).await.map(|_| ())
    }

    /// `INSERT INTO <database>.<table> FORMAT JSONEachRow`, one serde-encoded
    /// JSON object per line. An empty slice is a no-op.
    pub async fn insert_rows<T: Serialize>(
        &self,
        table: &str,
        rows: &[T],
    ) -> Result<(), ClickHouseError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for row in rows {
            body.push_str(&serde_json::to_string(row).map_err(ClickHouseError::Encode)?);
            body.push('\n');
        }
        let mut pairs = self.settings(true);
        pairs.push((
            "query".to_string(),
            "INSERT INTO {db:Identifier}.{table:Identifier} FORMAT JSONEachRow".to_string(),
        ));
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        pairs.push(("param_table".to_string(), escape_param_value(table)));
        self.post(pairs, body).await.map(|_| ())
    }

    /// Runs a `SELECT` and deserializes each `JSONEachRow` line into `T`.
    /// Write placeholders as `{name:Type}` in `sql` and pass the values in
    /// `params` as `("name", value)` — never interpolate values into `sql`.
    pub async fn query_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<T>, ClickHouseError> {
        let mut pairs = self.settings(true);
        pairs.push(("default_format".to_string(), "JSONEachRow".to_string()));
        // Buffer the whole result server-side, so an error raised mid-query
        // becomes a non-2xx status instead of trailing text in a 200 body.
        pairs.push(("wait_end_of_query".to_string(), "1".to_string()));
        for (name, value) in params {
            pairs.push((format!("param_{name}"), escape_param_value(value)));
        }
        let text = self.post(pairs, sql.to_string()).await?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|source| ClickHouseError::Decode {
                    line: line.to_string(),
                    source,
                })
            })
            .collect()
    }

    /// Creates the database if needed, then applies every migration.
    /// Idempotent: safe to run on every startup.
    pub async fn migrate(&self) -> Result<(), ClickHouseError> {
        // Not `settings(true)`: the `database` setting would make ClickHouse
        // reject the request while the database does not exist yet.
        let mut pairs = self.settings(false);
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        self.post(pairs, "CREATE DATABASE IF NOT EXISTS {db:Identifier}".to_string())
            .await?;
        for ddl in MIGRATIONS {
            self.execute(ddl).await?;
        }
        Ok(())
    }

    /// Test-only (same precedent as `router::with_fallback_connect_info` and
    /// `AppState::for_test`): creates a uniquely named `nami_test_<uuid>`
    /// database on the server named by `CLICKHOUSE_URL`/`CLICKHOUSE_USER`/
    /// `CLICKHOUSE_PASSWORD` (defaults match docker-compose), applies the
    /// migrations, and returns a client bound to it. Tests run in parallel,
    /// so each one gets its own database. Call [`Self::drop_database`] at the
    /// end of the test; a test that panics first leaves its database behind,
    /// which is harmless.
    pub async fn create_test_database() -> Result<ClickHouse, ClickHouseError> {
        let config = crate::config::Config::from_env_for_test();
        let name = format!("{TEST_DATABASE_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let client = ClickHouse::new(
            &config.clickhouse_url,
            &config.clickhouse_user,
            &config.clickhouse_password,
            &name,
        );
        client.migrate().await?;
        Ok(client)
    }

    /// Test-only counterpart to [`Self::create_test_database`]. Panics instead
    /// of dropping anything not named `nami_test_*`, so a test can never drop
    /// the real `nami` database.
    pub async fn drop_database(&self) -> Result<(), ClickHouseError> {
        assert!(
            self.database.starts_with(TEST_DATABASE_PREFIX),
            "drop_database only drops {TEST_DATABASE_PREFIX}* databases, not {:?}",
            self.database
        );
        let mut pairs = self.settings(false);
        pairs.push(("param_db".to_string(), escape_param_value(&self.database)));
        self.post(pairs, "DROP DATABASE IF EXISTS {db:Identifier} SYNC".to_string())
            .await
            .map(|_| ())
    }

    fn settings(&self, with_database: bool) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = BASE_SETTINGS
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        if with_database {
            pairs.push(("database".to_string(), self.database.clone()));
        }
        pairs
    }

    /// POSTs `body` with `pairs` as URL parameters. Always POST: ClickHouse
    /// treats GET as read-only. Credentials go in headers, never the URL.
    async fn post(
        &self,
        pairs: Vec<(String, String)>,
        body: String,
    ) -> Result<String, ClickHouseError> {
        let mut url = self.url.clone();
        {
            let mut query = url.query_pairs_mut();
            for (name, value) in &pairs {
                query.append_pair(name, value);
            }
        }
        let response = self
            .http
            .post(url)
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ClickHouseError::Server {
                status: status.as_u16(),
                body: text.trim().to_string(),
            });
        }
        Ok(text)
    }
}

/// ClickHouse parses query-parameter values in its "escaped" (TSV) text
/// format: a raw tab or newline is a parse error, and backslash sequences are
/// decoded (`C:\new` would arrive as `C:` + newline + `ew`). Escaping these
/// four characters makes every value arrive byte-for-byte (verified against
/// ClickHouse 26.9). Quotes need no escaping — the value is never parsed as
/// SQL.
fn escape_param_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_param_value;

    #[test]
    fn escapes_exactly_the_tsv_special_characters() {
        assert_eq!(escape_param_value("it's \"fine\""), "it's \"fine\"");
        assert_eq!(escape_param_value("C:\\new"), "C:\\\\new");
        assert_eq!(escape_param_value("a\tb\nc\rd"), "a\\tb\\nc\\rd");
    }
}
