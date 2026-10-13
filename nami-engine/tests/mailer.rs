// `email::Mailer`: what a message looks like on the wire, and how long a send
// may take. Requires `docker compose up -d mailpit`.
//
// Kept apart from `tests/email.rs`, whose test clears the whole Mailpit inbox:
// these tests share the inbox with the rest of the suite and only ever look
// at messages addressed to a recipient of their own.
mod common;

use std::time::{Duration, Instant};

use common::{settled_mail, stored_mail_for, unique_email};
use nami_engine::{
    email::{Mailer, SMTP_TIMEOUT},
    error::AppError,
};

#[tokio::test]
async fn one_email_may_take_at_most_ten_seconds_unless_told_otherwise() {
    assert_eq!(SMTP_TIMEOUT, Duration::from_secs(10));
    assert_eq!(Mailer::new("localhost", 1025).timeout(), SMTP_TIMEOUT);
    assert_eq!(
        Mailer::with_timeout("localhost", 1025, Duration::from_secs(3)).timeout(),
        Duration::from_secs(3)
    );
}

/// A message says what it is: `text/plain; charset=utf-8`, with the MIME
/// version that makes a client honour that (and the transfer encoding), so
/// non-ASCII names and text come out right.
#[tokio::test]
async fn messages_are_utf8_plain_text_and_non_ascii_survives() {
    let recipient = unique_email("mailer");
    let subject = "[Nami] Café API ☕ is DOWN";
    let body = "Cause: gagal menyambung … koneksi ditolak 日本語 🦀\n\n\
                Details: http://localhost:5173/projects/00000000-0000-0000-0000-000000000000/monitors/00000000-0000-0000-0000-000000000000\n";

    Mailer::new("localhost", 1025)
        .send(&recipient, subject, body.to_string())
        .await
        .unwrap();

    settled_mail(&recipient, 1).await;
    let stored = stored_mail_for(&recipient).await;
    assert_eq!(stored.len(), 1);
    let message = &stored[0];
    assert_eq!(message.subject, subject);
    assert_eq!(
        message.text.trim_end(),
        body.trim_end(),
        "{:?}",
        message.text
    );
    let (headers, _) = message.raw.split_once("\n\n").expect("a header block");
    let headers = headers.to_ascii_lowercase();
    assert!(headers.contains("\nmime-version: 1.0\n"), "{headers}");
    assert!(
        headers.contains("\ncontent-type: text/plain; charset=utf-8\n"),
        "{headers}"
    );
    assert!(!headers.contains("text/html"), "{headers}");
}

/// A server that accepts the connection and never says a word (an SMTP server
/// that is up but hung). lettre's own `timeout` setting does not cover this:
/// its async transport applies it to the TCP connect only, so without the
/// deadline `Mailer::send` puts around the whole exchange this would wait
/// forever. The outer timeout is only there to fail the test, not to hang it.
#[tokio::test]
async fn a_server_that_accepts_and_never_answers_cannot_hold_a_send_forever() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let mailer = Mailer::with_timeout("127.0.0.1", port, Duration::from_millis(600));

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        mailer.send("someone@example.com", "hello", "hello".to_string()),
    )
    .await
    .expect("send must give up by itself instead of waiting for the silent server");
    let took = started.elapsed();

    assert!(
        matches!(&result, Err(AppError::Validation(message)) if message == "could not send email"),
        "{result:?}"
    );
    assert!(
        took >= Duration::from_millis(500),
        "gave up after {took:?}, before the limit"
    );
    assert!(took < Duration::from_secs(5), "gave up after {took:?}");
}
