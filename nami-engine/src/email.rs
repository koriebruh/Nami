use std::{fmt, net::IpAddr, time::Duration};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, SinglePart},
    transport::smtp::{self, authentication::Credentials},
};

use crate::error::AppError;

/// The longest one email may take, from connecting to the last SMTP command.
///
/// lettre's async transport applies its own `timeout` setting to the TCP
/// connect only: the greeting and every command after it are read with no
/// deadline, so a server that accepts the connection and then says nothing
/// would hold a send forever (checked against lettre 0.11.23 with a listener
/// that never replies). `Mailer::send` therefore bounds the whole exchange
/// itself.
pub const SMTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The sender of every email unless `SMTP_FROM` names another.
pub const DEFAULT_FROM: &str = "Nami <noreply@nami.local>";

/// How the connection to the SMTP server is protected (`SMTP_TLS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Plain SMTP, never encrypted: for a local catcher such as Mailpit. The
    /// default.
    None,
    /// A plain connection upgraded with STARTTLS (usually port 587). The
    /// upgrade is required: a server that does not offer it gets neither the
    /// credentials nor the email.
    StartTls,
    /// TLS from the first byte, "SMTPS" (usually port 465).
    Tls,
}

/// `SMTP_TLS`: `none`, `starttls` or `tls`, case-insensitively; unset or empty
/// means `none`. Anything else is an error naming the variable.
pub fn parse_smtp_tls(value: Option<&str>) -> Result<SmtpTls, String> {
    match value.map(str::trim).unwrap_or_default() {
        "" => Ok(SmtpTls::None),
        v if v.eq_ignore_ascii_case("none") => Ok(SmtpTls::None),
        v if v.eq_ignore_ascii_case("starttls") => Ok(SmtpTls::StartTls),
        v if v.eq_ignore_ascii_case("tls") => Ok(SmtpTls::Tls),
        other => Err(format!(
            "SMTP_TLS must be one of none/starttls/tls, got {other:?}"
        )),
    }
}

/// A username and password for the SMTP server (`SMTP_USERNAME`,
/// `SMTP_PASSWORD`). `Debug` never shows the password.
#[derive(Clone, PartialEq, Eq)]
pub struct SmtpCredentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for SmtpCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// `SMTP_USERNAME` and `SMTP_PASSWORD`: both or neither (an empty value counts
/// as unset). The error names the variables, never their values.
pub fn smtp_credentials(
    username: Option<String>,
    password: Option<String>,
) -> Result<Option<SmtpCredentials>, String> {
    let set = |value: Option<String>| value.filter(|v| !v.is_empty());
    match (set(username), set(password)) {
        (Some(username), Some(password)) => Ok(Some(SmtpCredentials { username, password })),
        (None, None) => Ok(None),
        _ => Err("SMTP_USERNAME and SMTP_PASSWORD must be set together, or neither".to_string()),
    }
}

/// `SMTP_FROM`: the sender, such as `Nami <noreply@example.com>`; unset or
/// empty means [`DEFAULT_FROM`].
pub fn parse_from(value: Option<&str>) -> Result<Mailbox, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty());
    value.unwrap_or(DEFAULT_FROM).parse().map_err(|_| {
        format!(
            "SMTP_FROM must be a sender such as `Nami <noreply@example.com>`, got {:?}",
            value.unwrap_or_default()
        )
    })
}

/// Whether `host` is this machine: `localhost` or a loopback address
/// (127.0.0.0/8, `::1`, also IPv4-mapped), bracketed or not. Any other name,
/// a container's service name included, is another machine.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.trim_end_matches('.').eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

/// Refuses SMTP credentials that would cross the network in the clear: with
/// `SMTP_TLS=none` the password reaches the server as plain text, which is only
/// acceptable when the server is this machine (see [`is_loopback_host`]) or
/// when the operator says so with `SMTP_ALLOW_PLAINTEXT_AUTH=true` (a relay on
/// a network they trust). The message names the variables, never their values.
pub fn check_plaintext_auth(
    host: &str,
    tls: SmtpTls,
    credentials: Option<&SmtpCredentials>,
    allow_plaintext_auth: bool,
) -> Result<(), String> {
    if credentials.is_none()
        || tls != SmtpTls::None
        || allow_plaintext_auth
        || is_loopback_host(host)
    {
        return Ok(());
    }
    Err("SMTP_USERNAME and SMTP_PASSWORD are set with SMTP_TLS=none and an SMTP_HOST that is not this machine: the password would cross the network in the clear. Set SMTP_TLS=starttls or SMTP_TLS=tls, or SMTP_ALLOW_PLAINTEXT_AUTH=true to accept that".to_string())
}

/// Everything the mailer needs to reach the SMTP server and sign its emails.
/// `Debug` never shows the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub credentials: Option<SmtpCredentials>,
    pub from: Mailbox,
}

impl SmtpSettings {
    /// Plain SMTP to `host:port`, without credentials, from [`DEFAULT_FROM`]:
    /// a local catcher such as Mailpit.
    pub fn plain(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            tls: SmtpTls::None,
            credentials: None,
            from: DEFAULT_FROM.parse().expect("the default sender is valid"),
        }
    }

    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            host: config.smtp_host.clone(),
            port: config.smtp_port,
            tls: config.smtp_tls,
            credentials: config.smtp_credentials.clone(),
            from: config.smtp_from.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    timeout: Duration,
}

impl Mailer {
    /// Plain SMTP to `smtp_host:smtp_port` (see [`SmtpSettings::plain`]).
    pub fn new(smtp_host: &str, smtp_port: u16) -> Self {
        Self::with_timeout(smtp_host, smtp_port, SMTP_TIMEOUT)
    }

    /// Like [`Mailer::new`] with another limit for one email (see
    /// [`SMTP_TIMEOUT`]).
    pub fn with_timeout(smtp_host: &str, smtp_port: u16, timeout: Duration) -> Self {
        Self::from_settings_with_timeout(&SmtpSettings::plain(smtp_host, smtp_port), timeout)
            .expect("plain SMTP needs no TLS setup")
    }

    /// The mailer `settings` describe: plain SMTP, STARTTLS (required, so a
    /// server that does not offer it gets nothing) or implicit TLS, verified
    /// against the system's trusted certificates, with the credentials if
    /// there are any. An error (a host name TLS cannot verify) stops startup.
    pub fn from_settings(settings: &SmtpSettings) -> Result<Self, String> {
        Self::from_settings_with_timeout(settings, SMTP_TIMEOUT)
    }

    pub fn from_settings_with_timeout(
        settings: &SmtpSettings,
        timeout: Duration,
    ) -> Result<Self, String> {
        let tls_setup = |e: smtp::Error| {
            format!(
                "SMTP_TLS: cannot set up TLS for SMTP_HOST {:?}: {e}",
                settings.host
            )
        };
        let builder = match settings.tls {
            SmtpTls::None => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&settings.host)
            }
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
                    .map_err(tls_setup)?
            }
            SmtpTls::Tls => {
                AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host).map_err(tls_setup)?
            }
        };
        let builder = builder.port(settings.port).timeout(Some(timeout));
        let builder = match &settings.credentials {
            Some(credentials) => builder.credentials(Credentials::new(
                credentials.username.clone(),
                credentials.password.clone(),
            )),
            None => builder,
        };
        Ok(Self {
            transport: builder.build(),
            from: settings.from.clone(),
            timeout,
        })
    }

    /// The limit for one email.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Sends a plain-text message (`text/plain; charset=utf-8`, so non-ASCII
    /// names and text render correctly). Gives up after [`Self::timeout`].
    ///
    /// A failure is logged as a kind (see [`smtp_error_kind`]) and the SMTP
    /// reply code, never as lettre's error text: that quotes the server's
    /// reply, and a reply to a recipient often names the recipient.
    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError> {
        let email = Message::builder()
            .from(self.from.clone())
            .to(to
                .parse()
                .map_err(|_| AppError::Validation("invalid recipient".into()))?)
            .subject(subject)
            .singlepart(SinglePart::plain(body))
            .map_err(|_| AppError::Validation("could not build email".into()))?;

        match tokio::time::timeout(self.timeout, self.transport.send(email)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                let code = e.status().map(|code| code.to_string());
                tracing::error!(
                    kind = %smtp_error_kind(&e),
                    code = %code.as_deref().unwrap_or("none"),
                    "failed to send email"
                );
                Err(AppError::Validation("could not send email".into()))
            }
            Err(_elapsed) => {
                tracing::error!(timeout = ?self.timeout, "timed out sending email");
                Err(AppError::Validation("could not send email".into()))
            }
        }
    }
}

/// What went wrong sending an email, in one word: `timeout`, `rejected` (the
/// server answered with an error code), `tls`, `connection` (connecting,
/// reading or writing failed) or `other`.
pub fn smtp_error_kind(error: &smtp::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_transient() || error.is_permanent() {
        "rejected"
    } else if error.is_tls() {
        "tls"
    } else if caused_by_io(error) {
        "connection"
    } else {
        "other"
    }
}

fn caused_by_io(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(err) = current {
        if err.is::<std::io::Error>() {
            return true;
        }
        current = err.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_tls_accepts_its_three_modes_and_defaults_to_none() {
        for (value, expected) in [
            (None, SmtpTls::None),
            (Some(""), SmtpTls::None),
            (Some("none"), SmtpTls::None),
            (Some("NONE"), SmtpTls::None),
            (Some("starttls"), SmtpTls::StartTls),
            (Some("STARTTLS"), SmtpTls::StartTls),
            (Some("tls"), SmtpTls::Tls),
            (Some("Tls"), SmtpTls::Tls),
        ] {
            assert_eq!(parse_smtp_tls(value), Ok(expected), "{value:?}");
        }
    }

    #[test]
    fn smtp_tls_refuses_anything_else() {
        for value in ["ssl", "true", "yes", "start-tls", "tls1.2", "smtps"] {
            assert_eq!(
                parse_smtp_tls(Some(value)),
                Err(format!(
                    "SMTP_TLS must be one of none/starttls/tls, got {value:?}"
                ))
            );
        }
    }

    #[test]
    fn credentials_come_in_pairs() {
        let some = |v: &str| Some(v.to_string());
        assert_eq!(smtp_credentials(None, None), Ok(None));
        assert_eq!(smtp_credentials(some(""), some("")), Ok(None));
        assert_eq!(
            smtp_credentials(some("alerts"), some("hunter2")),
            Ok(Some(SmtpCredentials {
                username: "alerts".to_string(),
                password: "hunter2".to_string()
            }))
        );
        for (username, password) in [
            (some("alerts"), None),
            (some("alerts"), some("")),
            (None, some("hunter2")),
        ] {
            let error = smtp_credentials(username, password).unwrap_err();
            assert_eq!(
                error,
                "SMTP_USERNAME and SMTP_PASSWORD must be set together, or neither"
            );
            assert!(!error.contains("hunter2"));
        }
    }

    #[test]
    fn the_sender_defaults_to_nami_and_must_be_a_mailbox() {
        assert_eq!(parse_from(None).unwrap().to_string(), DEFAULT_FROM);
        assert_eq!(parse_from(Some("  ")).unwrap().to_string(), DEFAULT_FROM);
        assert_eq!(
            parse_from(Some("Nami Alerts <alerts@example.com>"))
                .unwrap()
                .to_string(),
            "Nami Alerts <alerts@example.com>"
        );
        assert_eq!(
            parse_from(Some("alerts@example.com"))
                .unwrap()
                .email
                .to_string(),
            "alerts@example.com"
        );
        for bad in ["not an address", "Nami <>", "a@b@c"] {
            assert!(
                parse_from(Some(bad))
                    .unwrap_err()
                    .starts_with("SMTP_FROM must be"),
                "{bad}"
            );
        }
    }

    fn alerts_login() -> SmtpCredentials {
        SmtpCredentials {
            username: "alerts".to_string(),
            password: "hunter2-secret".to_string(),
        }
    }

    #[test]
    fn plaintext_credentials_to_another_machine_are_refused() {
        let credentials = alerts_login();
        for host in [
            "smtp.example.com",
            "mailpit",
            "localhost.example.com",
            "127.0.0.1.nip.io",
            "10.0.0.5",
            "192.168.1.10",
            "::2",
            "[2001:db8::1]",
        ] {
            let message =
                check_plaintext_auth(host, SmtpTls::None, Some(&credentials), false).unwrap_err();
            for name in [
                "SMTP_USERNAME",
                "SMTP_PASSWORD",
                "SMTP_TLS",
                "SMTP_HOST",
                "SMTP_ALLOW_PLAINTEXT_AUTH",
            ] {
                assert!(message.contains(name), "{host}: {message}");
            }
            assert!(!message.contains("hunter2"), "{message}");
        }
    }

    #[test]
    fn credentials_are_fine_over_tls_to_this_machine_or_when_allowed() {
        let credentials = alerts_login();
        for tls in [SmtpTls::StartTls, SmtpTls::Tls] {
            assert_eq!(
                check_plaintext_auth("smtp.example.com", tls, Some(&credentials), false),
                Ok(())
            );
        }
        for host in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
        ] {
            assert_eq!(
                check_plaintext_auth(host, SmtpTls::None, Some(&credentials), false),
                Ok(()),
                "{host}"
            );
        }
        assert_eq!(
            check_plaintext_auth("smtp.example.com", SmtpTls::None, Some(&credentials), true),
            Ok(()),
            "allowed explicitly"
        );
        assert_eq!(
            check_plaintext_auth("smtp.example.com", SmtpTls::None, None, false),
            Ok(()),
            "no credentials, nothing to protect"
        );
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let settings = SmtpSettings {
            credentials: Some(SmtpCredentials {
                username: "alerts".to_string(),
                password: "hunter2-very-secret".to_string(),
            }),
            ..SmtpSettings::plain("smtp.example.com", 587)
        };
        let printed = format!("{settings:?}");
        assert!(printed.contains("alerts"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[test]
    fn every_mode_builds_a_transport() {
        for tls in [SmtpTls::None, SmtpTls::StartTls, SmtpTls::Tls] {
            let settings = SmtpSettings {
                tls,
                ..SmtpSettings::plain("smtp.example.com", 587)
            };
            assert!(Mailer::from_settings(&settings).is_ok(), "{tls:?}");
        }
    }
}
