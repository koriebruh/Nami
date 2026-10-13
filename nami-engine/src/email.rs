use lettre::{AsyncTransport, AsyncSmtpTransport, Tokio1Executor, Message};
use crate::error::AppError;

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl Mailer {
    pub fn new(smtp_host: &str, smtp_port: u16) -> Self {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
            .port(smtp_port)
            .build();
        Self { transport }
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError> {
        let email = Message::builder()
            .from("Nami <noreply@nami.local>".parse().unwrap())
            .to(to.parse().map_err(|_| AppError::Validation("invalid recipient".into()))?)
            .subject(subject)
            .body(body)
            .map_err(|_| AppError::Validation("could not build email".into()))?;

        self.transport
            .send(email)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to send email");
                AppError::Validation("could not send email".into())
            })?;
        Ok(())
    }
}
