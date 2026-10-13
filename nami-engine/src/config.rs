#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub cookie_secure: bool,
    pub oauth_redirect_base_url: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_auth_url: String,
    pub google_token_url: String,
    pub google_userinfo_url: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub github_auth_url: String,
    pub github_token_url: String,
    pub github_userinfo_url: String,
    pub clickhouse_url: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,
    pub clickhouse_database: String,
    /// Base URL of nami-ui, used to build links in alert emails.
    pub ui_base_url: String,
    /// `UPTIME_WORKER_ENABLED=false` serves the API without the background
    /// uptime checker (e.g. a second, API-only replica). Accepts
    /// `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`, case-insensitively; any
    /// other value stops startup (see `env_flag`).
    pub uptime_worker_enabled: bool,
    /// AES-256 key for secrets at rest (`NAMI_ENCRYPTION_KEY`, base64). Never
    /// logged: `Config` deliberately does not implement `Debug`.
    pub encryption_key: [u8; crate::crypto::KEY_LEN],
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();
        Self {
            database_url: std::env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("PORT must be a number"),
            smtp_host: std::env::var("SMTP_HOST").unwrap_or_else(|_| "localhost".to_string()),
            smtp_port: std::env::var("SMTP_PORT")
                .unwrap_or_else(|_| "1025".to_string())
                .parse()
                .expect("SMTP_PORT must be a number"),
            cookie_secure: std::env::var("COOKIE_SECURE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            encryption_key: encryption_key_from_env(),
            ..Self::from_env_defaults_only()
        }
    }

    /// Test-only constructor: doesn't require `DATABASE_URL`/`GOOGLE_CLIENT_ID`/etc. to be
    /// set, so tests don't need a `.env` file. `sqlx::test` supplies its own pool directly,
    /// so `database_url` is unused in tests. Identical to `from_env_defaults_only()` (which
    /// already defaults `database_url`/`port`/`smtp_host`/`smtp_port`), kept as its own named
    /// constructor for call-site clarity at test sites.
    pub fn from_env_for_test() -> Self {
        Self::from_env_defaults_only()
    }

    /// Shared OAuth-defaults block for `from_env()` and `from_env_for_test()`: each
    /// `*_url` field is overridable via env (so tests can point them at a `wiremock`
    /// server) while defaulting to the real Google/GitHub endpoints.
    fn from_env_defaults_only() -> Self {
        Self {
            database_url: String::new(),
            port: 8080,
            smtp_host: "localhost".to_string(),
            smtp_port: 1025,
            cookie_secure: false,
            oauth_redirect_base_url: std::env::var("OAUTH_REDIRECT_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            google_client_id: std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
            google_auth_url: std::env::var("GOOGLE_AUTH_URL")
                .unwrap_or_else(|_| "https://accounts.google.com/o/oauth2/v2/auth".to_string()),
            google_token_url: std::env::var("GOOGLE_TOKEN_URL")
                .unwrap_or_else(|_| "https://oauth2.googleapis.com/token".to_string()),
            google_userinfo_url: std::env::var("GOOGLE_USERINFO_URL")
                .unwrap_or_else(|_| "https://www.googleapis.com/oauth2/v3/userinfo".to_string()),
            github_client_id: std::env::var("GITHUB_CLIENT_ID").unwrap_or_default(),
            github_client_secret: std::env::var("GITHUB_CLIENT_SECRET").unwrap_or_default(),
            github_auth_url: std::env::var("GITHUB_AUTH_URL")
                .unwrap_or_else(|_| "https://github.com/login/oauth/authorize".to_string()),
            github_token_url: std::env::var("GITHUB_TOKEN_URL")
                .unwrap_or_else(|_| "https://github.com/login/oauth/access_token".to_string()),
            github_userinfo_url: std::env::var("GITHUB_USERINFO_URL")
                .unwrap_or_else(|_| "https://api.github.com/user".to_string()),
            clickhouse_url: std::env::var("CLICKHOUSE_URL")
                .unwrap_or_else(|_| "http://localhost:8123".to_string()),
            clickhouse_user: std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "nami".to_string()),
            clickhouse_password: std::env::var("CLICKHOUSE_PASSWORD")
                .unwrap_or_else(|_| "nami".to_string()),
            clickhouse_database: std::env::var("CLICKHOUSE_DATABASE")
                .unwrap_or_else(|_| "nami".to_string()),
            ui_base_url: std::env::var("UI_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            uptime_worker_enabled: env_flag("UPTIME_WORKER_ENABLED", true),
            // Tests only: `from_env` always replaces it with the real key.
            encryption_key: crate::crypto::TEST_KEY,
        }
    }
}

/// `NAMI_ENCRYPTION_KEY` is required outside tests: a missing or malformed
/// key stops startup instead of running with secrets nobody can decrypt
/// later (or encrypted under a key nobody kept).
fn encryption_key_from_env() -> [u8; crate::crypto::KEY_LEN] {
    let raw = std::env::var("NAMI_ENCRYPTION_KEY").unwrap_or_default();
    crate::crypto::parse_key(&raw).unwrap_or_else(|message| panic!("{message}"))
}

/// Reads a boolean env flag, `default` when it is unset. A value that is not
/// a recognised spelling (see `parse_bool_flag`) panics at startup instead of
/// falling back to a guess: for `UPTIME_WORKER_ENABLED`, an "API-only" replica
/// that silently kept its worker running would double-run every check.
fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => flag_from_value(name, &value),
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(value)) => {
            panic!("{name} must be valid unicode, got {value:?}")
        }
    }
}

/// `parse_bool_flag`, panicking with a message that names the variable and
/// the offending value when it is not recognised.
fn flag_from_value(name: &str, value: &str) -> bool {
    parse_bool_flag(value).unwrap_or_else(|| {
        panic!(
            "{name} must be one of true/false/1/0/yes/no/on/off (case-insensitive), got {value:?}"
        )
    })
}

/// `true`/`1`/`yes`/`on` and `false`/`0`/`no`/`off`, case-insensitively.
/// Anything else (including an empty string or surrounding whitespace) is
/// `None`.
fn parse_bool_flag(value: &str) -> Option<bool> {
    const TRUE: [&str; 4] = ["true", "1", "yes", "on"];
    const FALSE: [&str; 4] = ["false", "0", "no", "off"];
    if TRUE.iter().any(|spelling| value.eq_ignore_ascii_case(spelling)) {
        Some(true)
    } else if FALSE.iter().any(|spelling| value.eq_ignore_ascii_case(spelling)) {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{flag_from_value, parse_bool_flag};

    #[test]
    fn parse_bool_flag_accepts_the_documented_spellings_case_insensitively() {
        for value in ["true", "TRUE", "True", "1", "yes", "YES", "Yes", "on", "ON", "On"] {
            assert_eq!(parse_bool_flag(value), Some(true), "{value:?}");
        }
        for value in ["false", "FALSE", "False", "0", "no", "NO", "No", "off", "OFF", "Off"] {
            assert_eq!(parse_bool_flag(value), Some(false), "{value:?}");
        }
    }

    #[test]
    fn parse_bool_flag_rejects_everything_else() {
        for value in [
            "", " ", "true ", " false", "treu", "fasle", "2", "-1", "01", "00", "y", "n", "t", "f",
            "enabled", "disabled", "truee", "nope", "null", "none", "\u{ff54}rue",
        ] {
            assert_eq!(parse_bool_flag(value), None, "{value:?}");
        }
    }

    #[test]
    fn flag_from_value_returns_the_parsed_value() {
        assert!(flag_from_value("UPTIME_WORKER_ENABLED", "On"));
        assert!(!flag_from_value("UPTIME_WORKER_ENABLED", "off"));
    }

    #[test]
    #[should_panic(
        expected = "UPTIME_WORKER_ENABLED must be one of true/false/1/0/yes/no/on/off (case-insensitive), got \"fasle\""
    )]
    fn flag_from_value_panics_with_a_clear_message_on_anything_else() {
        flag_from_value("UPTIME_WORKER_ENABLED", "fasle");
    }
}
