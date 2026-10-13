#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
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
        }
    }
}
