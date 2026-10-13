//! Thin client for ClickHouse's HTTP interface, built on the existing
//! `reqwest` dependency (no driver crate). Shared by the uptime sub-project
//! now and the logs/metrics sub-projects later.
//!
//! Every value reaches ClickHouse as a query parameter — a `{name:Type}`
//! placeholder in the SQL plus a `param_name=value` URL parameter — never by
//! string interpolation, so no SQL text in this module is built with
//! `format!`. The two kinds of parameter are handled differently on purpose:
//!
//! - Typed values (`String`, `UUID`, `Int64`, ...) are parsed by ClickHouse in
//!   its TSV "escaped" text format, so they are TSV-escaped first (see
//!   `escape_param_value`) and then arrive byte-for-byte.
//! - Database and table names travel as `Identifier` parameters. ClickHouse
//!   uses an `Identifier` value verbatim — it does not decode TSV escapes for
//!   it, so escaping would corrupt the name — which leaves validation as the
//!   defence: a name must match `[A-Za-z_][A-Za-z0-9_]*` (see
//!   `validate_identifier`) and is then sent raw. Anything else is rejected
//!   with [`ClickHouseError::InvalidIdentifier`] before any request is made.

use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

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

/// The longest a `SELECT` may run on the server (`max_execution_time`): a
/// little under the client's own 30 s timeout, so a slow query ends with
/// ClickHouse's own `TIMEOUT_EXCEEDED` error, which says what happened, rather
/// than with the client giving up first.
pub const QUERY_MAX_EXECUTION_SECS: u64 = 25;

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
    /// The request did not complete. The request URL is stripped from the
    /// wrapped error (see the `From` impl below).
    #[error("clickhouse request failed: {0}")]
    Transport(#[source] reqwest::Error),
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
    /// A database or table name that is not a plain `[A-Za-z_][A-Za-z0-9_]*`
    /// identifier. Nothing was sent to ClickHouse.
    #[error("invalid clickhouse identifier {0:?}: expected [A-Za-z_][A-Za-z0-9_]*")]
    InvalidIdentifier(String),
}

impl From<reqwest::Error> for ClickHouseError {
    /// `reqwest::Error`'s `Display` and `Debug` include the request URL, and
    /// here the URL carries every `param_*` value (and would carry
    /// credentials if the configured URL had any). Strip it so none of that
    /// can reach a log line.
    fn from(error: reqwest::Error) -> Self {
        ClickHouseError::Transport(error.without_url())
    }
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
        // Never through a proxy from the environment (`HTTP_PROXY`, ...): every
        // request carries the credentials and the query parameters.
        let http = reqwest::Client::builder()
            .no_proxy()
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
        let pairs = self.settings_with_database()?;
        self.post(pairs, sql.to_string()).await.map(|_| ())
    }

    /// `INSERT INTO <database>.<table> FORMAT JSONEachRow`, one serde-encoded
    /// JSON object per line. An empty slice is a no-op, but `table` is still
    /// validated first, so a bad name fails the same way whatever the data.
    ///
    /// Sent with `input_format_skip_unknown_fields=0`: ClickHouse defaults to
    /// silently dropping keys that match no column, so a misspelled struct
    /// field would be stored as the column's default instead of failing.
    pub async fn insert_rows<T: Serialize>(
        &self,
        table: &str,
        rows: &[T],
    ) -> Result<(), ClickHouseError> {
        let mut pairs = self.settings_with_database()?;
        pairs.push((
            "input_format_skip_unknown_fields".to_string(),
            "0".to_string(),
        ));
        pairs.push((
            "query".to_string(),
            "INSERT INTO {db:Identifier}.{table:Identifier} FORMAT JSONEachRow".to_string(),
        ));
        pairs.push(identifier_param("db", &self.database)?);
        pairs.push(identifier_param("table", table)?);
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for row in rows {
            body.push_str(&serde_json::to_string(row).map_err(ClickHouseError::Encode)?);
            body.push('\n');
        }
        self.post(pairs, body).await.map(|_| ())
    }

    /// Runs a `SELECT` and deserializes each `JSONEachRow` line into `T`.
    /// Write placeholders as `{name:Type}` in `sql` and pass the values in
    /// `params` as `("name", value)` — never interpolate values into `sql`.
    ///
    /// Sent with `readonly=2`, so ClickHouse refuses DDL, inserts and
    /// mutations on this path (error `READONLY`) and `SETTINGS readonly = 0`
    /// cannot lift that: a statement smuggled into `sql` cannot change data.
    /// (`readonly=2`, unlike `1`, still accepts the per-request settings sent
    /// here.) Use [`Self::execute`] or [`Self::insert_rows`] to write.
    ///
    /// Also sent with `max_execution_time` ([`QUERY_MAX_EXECUTION_SECS`]) and
    /// `cancel_http_readonly_queries_on_client_close=1`: a SELECT stops on the
    /// server once it runs too long, or once its caller has gone (a request
    /// whose own deadline passed drops the connection), instead of running on
    /// for nobody.
    pub async fn query_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<T>, ClickHouseError> {
        let mut pairs = self.settings_with_database()?;
        pairs.push(("default_format".to_string(), "JSONEachRow".to_string()));
        // Buffer the whole result server-side, so an error raised mid-query
        // becomes a non-2xx status instead of trailing text in a 200 body.
        pairs.push(("wait_end_of_query".to_string(), "1".to_string()));
        pairs.push(("readonly".to_string(), "2".to_string()));
        pairs.push((
            "max_execution_time".to_string(),
            QUERY_MAX_EXECUTION_SECS.to_string(),
        ));
        pairs.push((
            "cancel_http_readonly_queries_on_client_close".to_string(),
            "1".to_string(),
        ));
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
        // Not `settings_with_database()`: the `database` setting would make
        // ClickHouse reject the request while the database does not exist yet.
        let mut pairs = Self::base_settings();
        pairs.push(identifier_param("db", &self.database)?);
        self.post(
            pairs,
            "CREATE DATABASE IF NOT EXISTS {db:Identifier}".to_string(),
        )
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
        let mut pairs = Self::base_settings();
        pairs.push(identifier_param("db", &self.database)?);
        self.post(
            pairs,
            "DROP DATABASE IF EXISTS {db:Identifier} SYNC".to_string(),
        )
        .await
        .map(|_| ())
    }

    /// The settings every request carries (see `BASE_SETTINGS`).
    fn base_settings() -> Vec<(String, String)> {
        BASE_SETTINGS
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    /// `base_settings()` plus the `database` setting, which scopes unqualified
    /// table names to this client's database. ClickHouse does not parse that
    /// value as SQL, but it is validated like every other identifier so no
    /// request ever carries an unchecked database name.
    fn settings_with_database(&self) -> Result<Vec<(String, String)>, ClickHouseError> {
        let mut pairs = Self::base_settings();
        pairs.push((
            "database".to_string(),
            validate_identifier(&self.database)?.to_string(),
        ));
        Ok(pairs)
    }

    /// POSTs `body` with `pairs` as URL parameters. Always POST: ClickHouse
    /// treats GET as read-only. Credentials go in an `Authorization: Basic`
    /// header, never the URL.
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
            // `basic_auth` marks the header sensitive: it is redacted from
            // `Debug` output and never put in HTTP/2's header compression table.
            .basic_auth(&self.user, Some(&self.password))
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

/// Makes a typed query-parameter value (`String`, `UUID`, `Int64`, ...) arrive
/// byte-for-byte. ClickHouse parses those values in its "escaped" (TSV) text
/// format: a raw tab or newline is a parse error, and backslash sequences are
/// decoded (`C:\new` would arrive as `C:` + newline + `ew`). Escaping these
/// four characters makes every value arrive intact (verified against
/// ClickHouse 26.9). Quotes need no escaping — the value is never parsed as
/// SQL.
///
/// Not for `Identifier` parameters: ClickHouse takes those verbatim, without
/// decoding TSV escapes (verified against 26.9: `a\b` names an identifier with
/// one backslash), so escaping would change the name. They go through
/// [`identifier_param`] instead.
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

/// The `param_<name>=<value>` pair for a `{name:Identifier}` placeholder.
/// Unlike typed values it is not TSV-escaped (see [`escape_param_value`]):
/// the value is validated and sent raw.
fn identifier_param(name: &str, value: &str) -> Result<(String, String), ClickHouseError> {
    Ok((
        format!("param_{name}"),
        validate_identifier(value)?.to_string(),
    ))
}

/// Accepts exactly the names matching `^[A-Za-z_][A-Za-z0-9_]*$` and rejects
/// everything else (an empty string, a leading digit, `.`, `` ` ``, quotes,
/// whitespace, `;`, `\`, control characters, non-ASCII). Hand-rolled rather
/// than a regex: it needs no dependency, and a trailing newline cannot slip
/// past a `$` anchor.
fn validate_identifier(name: &str) -> Result<&str, ClickHouseError> {
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(name)
    } else {
        Err(ClickHouseError::InvalidIdentifier(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{ClickHouse, ClickHouseError, escape_param_value, validate_identifier};

    /// Every SELECT is bounded on the server and stops there when the client
    /// gives up (say, a request whose own deadline passed): a slow query left
    /// behind must not keep burning ClickHouse's CPU.
    #[tokio::test]
    async fn selects_are_bounded_and_cancelled_when_the_client_goes_away() {
        use std::collections::HashMap;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"n\":1}\n"))
            .mount(&server)
            .await;
        let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_guard_probe");

        let rows: Vec<serde_json::Value> = ch.query_rows("SELECT 1 AS n", &[]).await.unwrap();
        assert_eq!(rows, vec![serde_json::json!({ "n": 1 })]);
        ch.execute("SELECT 1").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let settings = |index: usize| -> HashMap<String, String> {
            requests[index].url.query_pairs().into_owned().collect()
        };
        let select = settings(0);
        assert_eq!(
            select.get("max_execution_time").map(String::as_str),
            Some("25")
        );
        assert_eq!(
            select
                .get("cancel_http_readonly_queries_on_client_close")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(select.get("readonly").map(String::as_str), Some("2"));
        // DDL and inserts are not cut short: only reads are.
        let statement = settings(1);
        assert!(
            !statement.contains_key("max_execution_time"),
            "{statement:?}"
        );
    }

    #[test]
    fn escapes_exactly_the_tsv_special_characters() {
        assert_eq!(escape_param_value("it's \"fine\""), "it's \"fine\"");
        assert_eq!(escape_param_value("C:\\new"), "C:\\\\new");
        assert_eq!(escape_param_value("a\tb\nc\rd"), "a\\tb\\nc\\rd");
    }

    #[test]
    fn identifiers_are_limited_to_plain_ascii_names() {
        for ok in [
            "uptime_checks",
            "_x",
            "A",
            "a1",
            "Table_Name_2",
            "nami_test_0123456789abcdef0123456789abcdef",
        ] {
            assert!(validate_identifier(ok).is_ok(), "{ok:?} should be accepted");
        }
        for bad in [
            "", "1a", "a b", "a.b", "a;b", "a\\b", "a-b", "a`b", "a'b", "a\"b", "a\nb", "a\n",
            "a\tb", "a\0b", "a/b", "a$b", "a%b", "\u{e9}", "a\u{e9}",
        ] {
            assert!(
                matches!(validate_identifier(bad), Err(ClickHouseError::InvalidIdentifier(ref name)) if name == bad),
                "{bad:?} should be rejected"
            );
        }
    }
}
