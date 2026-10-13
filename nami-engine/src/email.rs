use std::time::Duration;

use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::SinglePart};

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

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    timeout: Duration,
}

impl Mailer {
    pub fn new(smtp_host: &str, smtp_port: u16) -> Self {
        Self::with_timeout(smtp_host, smtp_port, SMTP_TIMEOUT)
    }

    /// Like [`Mailer::new`] with another limit for one email (see
    /// [`SMTP_TIMEOUT`]).
    pub fn with_timeout(smtp_host: &str, smtp_port: u16, timeout: Duration) -> Self {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
            .port(smtp_port)
            .timeout(Some(timeout))
            .build();
        Self { transport, timeout }
    }

    /// The limit for one email.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Sends a plain-text message (`text/plain; charset=utf-8`, so non-ASCII
    /// names and text render correctly). Gives up after [`Self::timeout`].
    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError> {
        let email = Message::builder()
            .from("Nami <noreply@nami.local>".parse().unwrap())
            .to(to
                .parse()
                .map_err(|_| AppError::Validation("invalid recipient".into()))?)
            .subject(subject)
            .singlepart(SinglePart::plain(body))
            .map_err(|_| AppError::Validation("could not build email".into()))?;

        match tokio::time::timeout(self.timeout, self.transport.send(email)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "failed to send email");
                Err(AppError::Validation("could not send email".into()))
            }
            Err(_elapsed) => {
                tracing::error!(timeout = ?self.timeout, "timed out sending email");
                Err(AppError::Validation("could not send email".into()))
            }
        }
    }
}
