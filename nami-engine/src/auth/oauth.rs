use crate::core::config::Config;
use oauth2::{
    AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RedirectUrl, TokenUrl,
    basic::BasicClient,
};

#[derive(Clone, Copy, Debug)]
pub enum Provider {
    Google,
    Github,
}

impl Provider {
    // Named `parse` rather than `from_str` to avoid the clippy
    // `should_implement_trait` lint (`from_str` is the recognized
    // `std::str::FromStr` trait method name); this type intentionally
    // doesn't implement that trait (it returns `Option`, not `Result`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google" => Some(Provider::Google),
            "github" => Some(Provider::Github),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Github => "github",
        }
    }

    pub fn userinfo_url(&self, config: &Config) -> String {
        match self {
            Provider::Google => config.google_userinfo_url.clone(),
            Provider::Github => config.github_userinfo_url.clone(),
        }
    }

    // oauth2 5.x encodes which endpoints have been set in the client's type
    // (`EndpointSet`/`EndpointNotSet`), so the return type must spell out the
    // generic params in the same order the `Client` type declares them: auth,
    // device-auth, introspection, revocation, token. We set auth + token.
    pub fn client(
        &self,
        config: &Config,
    ) -> BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet> {
        let (client_id, client_secret, auth_url, token_url) = match self {
            Provider::Google => (
                &config.google_client_id,
                &config.google_client_secret,
                &config.google_auth_url,
                &config.google_token_url,
            ),
            Provider::Github => (
                &config.github_client_id,
                &config.github_client_secret,
                &config.github_auth_url,
                &config.github_token_url,
            ),
        };

        BasicClient::new(ClientId::new(client_id.clone()))
            .set_client_secret(ClientSecret::new(client_secret.clone()))
            .set_auth_uri(AuthUrl::new(auth_url.clone()).unwrap())
            .set_token_uri(TokenUrl::new(token_url.clone()).unwrap())
            .set_redirect_uri(
                RedirectUrl::new(format!(
                    "{}/auth/oauth/{}/callback",
                    config.oauth_redirect_base_url,
                    self.as_str()
                ))
                .unwrap(),
            )
    }
}
