use std::{
    io,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    /// The private API (`PORT`, default 8080): only nami-ui may reach it. It
    /// trusts the client address nami-ui puts in `X-Forwarded-For` (the login
    /// rate limit is keyed on it), so it must never be exposed directly.
    pub port: u16,
    /// The public listener (`PUBLIC_PORT`, default 8081): only the endpoints
    /// that outside clients call without a session, such as heartbeat pings.
    /// The one port to expose to cron jobs and other machines.
    pub public_port: u16,
    /// Size of the Postgres pool (`DATABASE_MAX_CONNECTIONS`, default 20).
    pub database_max_connections: u32,
    /// How long a request or the worker waits for a free pooled connection
    /// before failing (`DATABASE_ACQUIRE_TIMEOUT_SECS`, default 5).
    pub database_acquire_timeout: Duration,
    /// The longest any one statement may run (`DATABASE_STATEMENT_TIMEOUT_SECS`,
    /// default 30), set on every pooled connection, so a hung query fails
    /// instead of silently stalling monitoring.
    pub database_statement_timeout: Duration,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// How the SMTP connection is protected (`SMTP_TLS`: `none`, the default
    /// for Mailpit in development, `starttls` or `tls`).
    pub smtp_tls: crate::email::SmtpTls,
    /// `SMTP_USERNAME` and `SMTP_PASSWORD`, set together or not at all. The
    /// password is never logged (`SmtpCredentials`' `Debug` hides it).
    pub smtp_credentials: Option<crate::email::SmtpCredentials>,
    /// The sender of every email (`SMTP_FROM`, default
    /// `Nami <noreply@nami.local>`).
    pub smtp_from: lettre::message::Mailbox,
    /// `SMTP_ALLOW_PLAINTEXT_AUTH=true` accepts sending SMTP credentials
    /// without TLS to a server that is not this machine (refused otherwise,
    /// see `email::check_plaintext_auth`). Default false.
    pub smtp_allow_plaintext_auth: bool,
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
    /// `SYSLOG_ENABLED=true` starts the RFC 5424 syslog listeners for log
    /// ingestion (off by default; same spellings as `UPTIME_WORKER_ENABLED`).
    /// They are a public ingress, like `PUBLIC_PORT`.
    pub syslog_enabled: bool,
    /// `SYSLOG_TCP_PORT` and `SYSLOG_UDP_PORT`, default 5140 each.
    pub syslog_tcp_port: u16,
    pub syslog_udp_port: u16,
    /// Where `domain_expiry` checks start their RDAP lookups
    /// (`RDAP_BASE_URL`, default `https://rdap.org`, which redirects to the
    /// registry's own RDAP server).
    pub rdap_base_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        load_dotenv_or_stop();
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("PORT must be a number"),
            public_port: std::env::var("PUBLIC_PORT")
                .unwrap_or_else(|_| "8081".to_string())
                .parse()
                .expect("PUBLIC_PORT must be a number"),
            database_max_connections: env_positive("DATABASE_MAX_CONNECTIONS", 20),
            database_acquire_timeout: Duration::from_secs(env_positive(
                "DATABASE_ACQUIRE_TIMEOUT_SECS",
                5,
            )),
            database_statement_timeout: Duration::from_secs(env_positive(
                "DATABASE_STATEMENT_TIMEOUT_SECS",
                30,
            )),
            smtp_host: std::env::var("SMTP_HOST").unwrap_or_else(|_| "localhost".to_string()),
            smtp_port: std::env::var("SMTP_PORT")
                .unwrap_or_else(|_| "1025".to_string())
                .parse()
                .expect("SMTP_PORT must be a number"),
            smtp_tls: or_stop(crate::email::parse_smtp_tls(
                std::env::var("SMTP_TLS").ok().as_deref(),
            )),
            smtp_credentials: or_stop(crate::email::smtp_credentials(
                std::env::var("SMTP_USERNAME").ok(),
                std::env::var("SMTP_PASSWORD").ok(),
            )),
            smtp_from: or_stop(crate::email::parse_from(
                std::env::var("SMTP_FROM").ok().as_deref(),
            )),
            smtp_allow_plaintext_auth: env_flag("SMTP_ALLOW_PLAINTEXT_AUTH", false),
            cookie_secure: std::env::var("COOKIE_SECURE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            encryption_key: encryption_key_from_env(),
            ..Self::from_env_defaults_only()
        }
        .with_distinct_ports()
        .with_safe_smtp_auth()
    }

    /// SMTP credentials never cross the network in the clear unless the
    /// operator accepts it (see `email::check_plaintext_auth`).
    fn with_safe_smtp_auth(self) -> Self {
        or_stop(crate::email::check_plaintext_auth(
            &self.smtp_host,
            self.smtp_tls,
            self.smtp_credentials.as_ref(),
            self.smtp_allow_plaintext_auth,
        ));
        self
    }

    /// The API and the public listener cannot share a port: the API must stay
    /// private while the public port is exposed.
    fn with_distinct_ports(self) -> Self {
        assert!(
            self.port != self.public_port,
            "PORT and PUBLIC_PORT must differ (both are {}): the API must stay private while the public port is exposed",
            self.port
        );
        self
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
            public_port: 8081,
            database_max_connections: 20,
            database_acquire_timeout: Duration::from_secs(5),
            database_statement_timeout: Duration::from_secs(30),
            smtp_host: "localhost".to_string(),
            smtp_port: 1025,
            smtp_tls: crate::email::SmtpTls::None,
            smtp_credentials: None,
            smtp_from: crate::email::DEFAULT_FROM
                .parse()
                .expect("the default sender is valid"),
            smtp_allow_plaintext_auth: false,
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
            clickhouse_user: std::env::var("CLICKHOUSE_USER")
                .unwrap_or_else(|_| "nami".to_string()),
            clickhouse_password: std::env::var("CLICKHOUSE_PASSWORD")
                .unwrap_or_else(|_| "nami".to_string()),
            clickhouse_database: std::env::var("CLICKHOUSE_DATABASE")
                .unwrap_or_else(|_| "nami".to_string()),
            ui_base_url: std::env::var("UI_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            uptime_worker_enabled: env_flag("UPTIME_WORKER_ENABLED", true),
            // Tests only: `from_env` always replaces it with the real key.
            encryption_key: crate::crypto::TEST_KEY,
            syslog_enabled: env_flag("SYSLOG_ENABLED", false),
            syslog_tcp_port: env_port("SYSLOG_TCP_PORT", crate::logs::syslog::DEFAULT_PORT),
            syslog_udp_port: env_port("SYSLOG_UDP_PORT", crate::logs::syslog::DEFAULT_PORT),
            rdap_base_url: std::env::var("RDAP_BASE_URL")
                .unwrap_or_else(|_| crate::uptime::checker::DEFAULT_RDAP_BASE_URL.to_string()),
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

/// Loads the `.env` file of the working directory (or its nearest parent), see
/// [`load_dotenv`], and stops startup if it cannot be loaded.
fn load_dotenv_or_stop() {
    let Ok(start) = std::env::current_dir() else {
        return;
    };
    if let Err(message) = load_dotenv(&start) {
        panic!("{message}");
    }
}

/// Loads the `.env` file found in `start` or its nearest parent, as
/// `dotenvy::dotenv()` would for the working directory: a variable already set
/// in the environment wins, and having no `.env` file at all is fine.
///
/// Anything else is an error, and then nothing of the file is loaded: the
/// whole file is parsed before any of it is applied. (`dotenvy` on its own
/// stops at a line it cannot parse, after loading the lines before it, so an
/// unquoted value with a space would silently leave every later key unread.)
/// The message names the file and the line, and the key when the line has
/// one, but never shows a value: the file holds secrets.
pub fn load_dotenv(start: &Path) -> Result<(), String> {
    let Some(path) = find_dotenv(start)? else {
        return Ok(());
    };
    let bytes =
        std::fs::read(&path).map_err(|e| format!("{}: cannot be read: {e}", path.display()))?;
    // `dotenvy` drops a leading byte order mark when it loads a file, but not
    // when it only parses one; drop it for both.
    let content = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    for parsed in dotenvy::from_read_iter(content) {
        if let Err(e) = parsed {
            return Err(describe_dotenv_error(&path, content, &e));
        }
    }
    dotenvy::from_read(content).map_err(|e| describe_dotenv_error(&path, content, &e))
}

/// `.env` in `start` or the nearest of its parents that has one, like
/// `dotenvy`'s own search.
fn find_dotenv(start: &Path) -> Result<Option<PathBuf>, String> {
    for dir in start.ancestors() {
        let candidate = dir.join(".env");
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(candidate)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: cannot be read: {e}", candidate.display())),
        }
    }
    Ok(None)
}

/// Why `path` could not be loaded, without a word of its values. `dotenvy`'s
/// own message quotes the offending text, which may be a secret, so the line
/// is located and named instead.
fn describe_dotenv_error(path: &Path, content: &[u8], error: &dotenvy::Error) -> String {
    let file = path.display();
    match error {
        dotenvy::Error::LineParse(text, _) => {
            let content = String::from_utf8_lossy(content);
            let advice = "quote values that contain spaces or special characters, e.g. KEY=\"a value\"; nothing from the file was loaded";
            match locate_unparsable_line(&content, text) {
                Some((number, Some(key))) => {
                    format!("{file}: line {number} ({key}) cannot be parsed: {advice}")
                }
                Some((number, None)) => format!("{file}: line {number} cannot be parsed: {advice}"),
                None => format!("{file}: a line cannot be parsed: {advice}"),
            }
        }
        dotenvy::Error::Io(e) => format!("{file}: cannot be read: {e}"),
        _ => format!("{file}: cannot be loaded (a value is not valid unicode)"),
    }
}

/// The 1-based number of the first line of `content` that `dotenvy` cannot
/// parse on its own and that holds `text` (the part of the line `dotenvy`
/// reported), with the line's key when it has a well-formed one.
fn locate_unparsable_line(content: &str, text: &str) -> Option<(usize, Option<String>)> {
    let needle = text.lines().next().unwrap_or_default().trim();
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .filter(|(_, line)| line.contains(needle))
        .find(|(_, line)| dotenvy::from_read_iter(line.as_bytes()).any(|parsed| parsed.is_err()))
        .map(|(index, line)| (index + 1, dotenv_key(line)))
}

/// The key of a `KEY=value` line (`export KEY=value` too), if it is a plain
/// name: anything else might be part of a value, so it is not shown.
fn dotenv_key(line: &str) -> Option<String> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    let mut chars = key.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    plain.then(|| key.to_string())
}

/// The setting, or a stop at startup with `parse`'s message (which names the
/// variable and never holds a secret).
fn or_stop<T>(parsed: Result<T, String>) -> T {
    parsed.unwrap_or_else(|message| panic!("{message}"))
}

/// Reads a positive whole number from the environment, `default` when it is
/// unset. Anything else (zero, a negative or fractional number, text) stops
/// startup with a message naming the variable, instead of guessing.
fn env_positive<T>(name: &str, default: T) -> T
where
    T: FromStr + PartialOrd + Default,
{
    match std::env::var(name) {
        Ok(value) => positive_from_value(name, &value),
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(value)) => {
            panic!("{name} must be valid unicode, got {value:?}")
        }
    }
}

/// `value` as a positive whole number, or a panic that names `name`.
fn positive_from_value<T>(name: &str, value: &str) -> T
where
    T: FromStr + PartialOrd + Default,
{
    match value.parse::<T>() {
        Ok(number) if number > T::default() => number,
        _ => panic!("{name} must be a positive whole number, got {value:?}"),
    }
}

/// A port number from the environment, `default` when unset; anything else
/// stops startup.
fn env_port(name: &str, default: u16) -> u16 {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a port number (0-65535), got {value:?}")),
        Err(_) => default,
    }
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
    if TRUE
        .iter()
        .any(|spelling| value.eq_ignore_ascii_case(spelling))
    {
        Some(true)
    } else if FALSE
        .iter()
        .any(|spelling| value.eq_ignore_ascii_case(spelling))
    {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Config, flag_from_value, load_dotenv, locate_unparsable_line, parse_bool_flag,
        positive_from_value,
    };

    /// A fresh, empty directory of this test's own under the system temp dir.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nami-config-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A line that does not parse is an error naming the line and its key,
    /// never a value, and nothing of the file is loaded, not even the lines
    /// before it.
    #[test]
    fn a_dotenv_line_that_does_not_parse_is_an_error_and_loads_nothing() {
        let dir = scratch_dir("bad");
        let before = format!("NAMI_CONFIG_TEST_{}", std::process::id());
        std::fs::write(
            dir.join(".env"),
            format!(
                "# settings\n{before}=first-value\nexport SMTP_FROM=Nami <noreply@nami.local>\nSMTP_PASSWORD=hunter2\n"
            ),
        )
        .unwrap();

        let message = load_dotenv(&dir).unwrap_err();

        assert!(
            message.contains(".env: line 3 (SMTP_FROM) cannot be parsed"),
            "{message}"
        );
        for value in ["noreply", "Nami <", "hunter2", "first-value"] {
            assert!(!message.contains(value), "{value:?} leaked: {message}");
        }
        assert!(std::env::var(&before).is_err(), "nothing was loaded");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A line whose start is not a plain key is located by number only: its
    /// text may be a pasted secret.
    #[test]
    fn a_line_without_a_plain_key_is_named_by_its_number_only() {
        let dir = scratch_dir("nokey");
        std::fs::write(dir.join(".env"), "A=1\ns3cr3t pasted here\n").unwrap();

        let message = load_dotenv(&dir).unwrap_err();

        assert!(
            message.contains(".env: line 2 cannot be parsed"),
            "{message}"
        );
        assert!(!message.contains("s3cr3t"), "{message}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The same text earlier in a line that parses does not mislead the
    /// search for the bad line.
    #[test]
    fn the_bad_line_is_the_one_that_does_not_parse_on_its_own() {
        assert_eq!(
            locate_unparsable_line("A=\"x y\"\n# B=x y\nB=x y\n", "x y"),
            Some((3, Some("B".to_string())))
        );
    }

    /// No `.env` anywhere up the tree is fine: the environment is all there is.
    #[test]
    fn a_missing_dotenv_file_is_fine() {
        let dir = scratch_dir("none");
        assert_eq!(load_dotenv(&dir), Ok(()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The example file is what operators copy: every line of it must parse,
    /// or dotenvy stops at the bad one and every later key goes unread.
    #[test]
    fn the_shipped_env_example_parses_to_its_last_line() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.env.example");
        let content = std::fs::read(path).unwrap();
        let pairs: Vec<(String, String)> = dotenvy::from_read_iter(&content[..])
            .collect::<Result<_, _>>()
            .expect("every line of .env.example parses");
        let value = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(value("SMTP_FROM"), Some("Nami <noreply@nami.local>"));
        for key in [
            "COOKIE_SECURE",
            "CLICKHOUSE_URL",
            "UI_BASE_URL",
            "UPTIME_WORKER_ENABLED",
            "NAMI_ENCRYPTION_KEY",
        ] {
            assert!(value(key).is_some(), "{key} is read");
        }
    }

    #[test]
    fn positive_numbers_are_read_as_given() {
        assert_eq!(
            positive_from_value::<u32>("DATABASE_MAX_CONNECTIONS", "1"),
            1
        );
        assert_eq!(
            positive_from_value::<u32>("DATABASE_MAX_CONNECTIONS", "50"),
            50
        );
        assert_eq!(
            positive_from_value::<u64>("DATABASE_ACQUIRE_TIMEOUT_SECS", "5"),
            5
        );
    }

    #[test]
    fn zero_negative_fractional_and_text_values_stop_startup() {
        for value in ["0", "-1", "1.5", "", " 5", "five", "5s"] {
            let result = std::panic::catch_unwind(|| {
                positive_from_value::<u64>("DATABASE_STATEMENT_TIMEOUT_SECS", value)
            });
            let message = *result
                .expect_err(value)
                .downcast::<String>()
                .expect("a formatted message");
            assert_eq!(
                message,
                format!(
                    "DATABASE_STATEMENT_TIMEOUT_SECS must be a positive whole number, got {value:?}"
                )
            );
        }
    }

    #[test]
    fn the_defaults_keep_the_api_and_the_public_listener_apart() {
        let config = Config::from_env_for_test();
        assert_eq!((config.port, config.public_port), (8080, 8081));
        assert_eq!(config.database_max_connections, 20);
        assert_eq!(config.database_acquire_timeout.as_secs(), 5);
        assert_eq!(config.database_statement_timeout.as_secs(), 30);
    }

    #[test]
    #[should_panic(expected = "PORT and PUBLIC_PORT must differ (both are 9000)")]
    fn the_api_and_the_public_listener_cannot_share_a_port() {
        Config {
            port: 9000,
            public_port: 9000,
            ..Config::from_env_for_test()
        }
        .with_distinct_ports();
    }

    #[test]
    fn parse_bool_flag_accepts_the_documented_spellings_case_insensitively() {
        for value in [
            "true", "TRUE", "True", "1", "yes", "YES", "Yes", "on", "ON", "On",
        ] {
            assert_eq!(parse_bool_flag(value), Some(true), "{value:?}");
        }
        for value in [
            "false", "FALSE", "False", "0", "no", "NO", "No", "off", "OFF", "Off",
        ] {
            assert_eq!(parse_bool_flag(value), Some(false), "{value:?}");
        }
    }

    #[test]
    fn parse_bool_flag_rejects_everything_else() {
        for value in [
            "",
            " ",
            "true ",
            " false",
            "treu",
            "fasle",
            "2",
            "-1",
            "01",
            "00",
            "y",
            "n",
            "t",
            "f",
            "enabled",
            "disabled",
            "truee",
            "nope",
            "null",
            "none",
            "\u{ff54}rue",
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
