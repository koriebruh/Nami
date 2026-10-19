use nami_engine::email::Mailer;
use serde_json::Value;

#[tokio::test]
async fn sent_email_is_captured_by_mailpit() {
    // Requires `docker compose up -d mailpit` to be running first.
    reqwest::Client::new()
        .delete("http://localhost:8025/api/v1/messages")
        .send()
        .await
        .unwrap();

    let mailer = Mailer::new("localhost", 1025);
    mailer
        .send(
            "someone@example.com",
            "Verify your email",
            "click here".to_string(),
        )
        .await
        .unwrap();

    let body: Value = reqwest::get("http://localhost:8025/api/v1/messages")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["Subject"], "Verify your email");
}
