use nami_engine::core::email::Mailer;

use crate::common;

// Requires `docker compose up -d mailpit`. A fresh address per run, so tests
// that send mail at the same time never see or clear each other's messages.
#[tokio::test]
async fn sent_email_is_captured_by_mailpit() {
    let recipient = format!("someone-{}@example.com", uuid::Uuid::new_v4());

    Mailer::new("localhost", 1025)
        .send(&recipient, "Verify your email", "click here".to_string())
        .await
        .unwrap();

    let messages = common::settled_mail(&recipient, 1).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, "Verify your email");
}
