// `email::Mailer`: what a message looks like on the wire, and how long a send
// may take. Requires `docker compose up -d mailpit`.
//
// Kept apart from `tests/email.rs`, whose test clears the whole Mailpit inbox:
// these tests share the inbox with the rest of the suite and only ever look
// at messages addressed to a recipient of their own.
mod common;

use std::time::{Duration, Instant};

use base64::Engine;
use common::{LogCapture, settled_mail, stored_mail_for, unique_email};
use nami_engine::{
    email::{Mailer, SMTP_TIMEOUT, SmtpCredentials, SmtpSettings, SmtpTls},
    error::AppError,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
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

// ---------------------------------------------------------------------------
// Final review: SMTP for production (TLS modes, credentials, sender) and a log
// that never quotes the server.
// ---------------------------------------------------------------------------

/// What a fake SMTP server saw: the client's command lines, in order, and
/// whether the client started a TLS handshake (a TLS record begins 0x16 0x03).
#[derive(Debug, Default)]
struct Session {
    commands: Vec<String>,
    tls_hello: bool,
}

/// Starts a one-connection SMTP server on a free port. It greets, answers EHLO
/// with `ehlo` (a complete multi-line reply), answers STARTTLS with 220 and
/// then only looks for the client's TLS handshake, answers RCPT TO with
/// `rcpt` and everything else the way a server that accepts the message does.
async fn fake_smtp(ehlo: &'static str, rcpt: &'static str) -> (u16, JoinHandle<Session>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let mut session = Session::default();
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        write.write_all(b"220 fake.test ESMTP\r\n").await.unwrap();
        let mut in_data = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let line = line.trim_end().to_string();
            if in_data {
                if line == "." {
                    in_data = false;
                    write.write_all(b"250 2.0.0 queued\r\n").await.unwrap();
                }
                continue;
            }
            session.commands.push(line.clone());
            let command = line.to_ascii_uppercase();
            let reply: &str = if command.starts_with("EHLO") {
                ehlo
            } else if command.starts_with("STARTTLS") {
                write.write_all(b"220 2.0.0 go ahead\r\n").await.unwrap();
                let mut hello = [0u8; 2];
                session.tls_hello =
                    reader.read_exact(&mut hello).await.is_ok() && hello == [0x16, 0x03];
                break;
            } else if command.starts_with("AUTH") {
                "235 2.7.0 accepted\r\n"
            } else if command.starts_with("RCPT") {
                rcpt
            } else if command.starts_with("DATA") {
                in_data = true;
                "354 go on\r\n"
            } else if command.starts_with("QUIT") {
                write.write_all(b"221 bye\r\n").await.unwrap();
                break;
            } else {
                "250 ok\r\n"
            };
            write.write_all(reply.as_bytes()).await.unwrap();
        }
        session
    });
    (port, server)
}

const EHLO_PLAIN: &str = "250-fake.test\r\n250 8BITMIME\r\n";
const EHLO_AUTH: &str = "250-fake.test\r\n250 AUTH PLAIN LOGIN\r\n";
const EHLO_STARTTLS: &str = "250-fake.test\r\n250 STARTTLS\r\n";
const RCPT_OK: &str = "250 ok\r\n";

fn settings(port: u16, tls: SmtpTls) -> SmtpSettings {
    SmtpSettings {
        tls,
        ..SmtpSettings::plain("127.0.0.1", port)
    }
}

async fn send_with(settings: &SmtpSettings) -> Result<(), AppError> {
    Mailer::from_settings_with_timeout(settings, Duration::from_secs(3))
        .unwrap()
        .send("someone@example.com", "hello", "hello".to_string())
        .await
}

/// `none` speaks plain SMTP all the way, signs with the configured sender and
/// authenticates when there are credentials.
#[tokio::test]
async fn plain_smtp_sends_with_the_configured_sender_and_credentials() {
    let (port, server) = fake_smtp(EHLO_AUTH, RCPT_OK).await;
    let settings = SmtpSettings {
        credentials: Some(SmtpCredentials {
            username: "alerts".to_string(),
            password: "hunter2".to_string(),
        }),
        from: "Nami Alerts <alerts@nami.test>".parse().unwrap(),
        ..settings(port, SmtpTls::None)
    };

    send_with(&settings).await.unwrap();

    let session = server.await.unwrap();
    let auth = session
        .commands
        .iter()
        .find(|c| c.starts_with("AUTH PLAIN "))
        .expect("the client authenticates");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth.trim_start_matches("AUTH PLAIN "))
        .unwrap();
    assert_eq!(decoded, b"\0alerts\0hunter2");
    assert!(
        session
            .commands
            .iter()
            .any(|c| c == "MAIL FROM:<alerts@nami.test>"),
        "{:?}",
        session.commands
    );
    assert!(!session.commands.iter().any(|c| c.starts_with("STARTTLS")));
}

/// `starttls` upgrades the connection before anything else: after EHLO it asks
/// for STARTTLS and starts a TLS handshake, never sending the message (or
/// credentials) in the clear.
#[tokio::test]
async fn starttls_upgrades_before_sending_anything() {
    let (port, server) = fake_smtp(EHLO_STARTTLS, RCPT_OK).await;
    let settings = SmtpSettings {
        credentials: Some(SmtpCredentials {
            username: "alerts".to_string(),
            password: "hunter2".to_string(),
        }),
        ..settings(port, SmtpTls::StartTls)
    };

    assert!(
        send_with(&settings).await.is_err(),
        "the fake server cannot finish TLS"
    );

    let session = server.await.unwrap();
    assert!(
        session.commands.iter().any(|c| c == "STARTTLS"),
        "{:?}",
        session.commands
    );
    assert!(session.tls_hello, "a TLS handshake follows the STARTTLS");
    assert!(
        !session
            .commands
            .iter()
            .any(|c| c.starts_with("MAIL") || c.starts_with("AUTH")),
        "{:?}",
        session.commands
    );
}

/// `starttls` cannot be downgraded: a server that does not offer STARTTLS gets
/// neither the credentials nor the message.
#[tokio::test]
async fn starttls_refuses_a_server_that_does_not_offer_it() {
    let (port, server) = fake_smtp(EHLO_AUTH, RCPT_OK).await;
    let settings = SmtpSettings {
        credentials: Some(SmtpCredentials {
            username: "alerts".to_string(),
            password: "hunter2".to_string(),
        }),
        ..settings(port, SmtpTls::StartTls)
    };

    assert!(send_with(&settings).await.is_err());

    let session = server.await.unwrap();
    assert!(
        !session
            .commands
            .iter()
            .any(|c| c.starts_with("MAIL") || c.starts_with("AUTH")),
        "{:?}",
        session.commands
    );
}

/// `tls` is TLS from the first byte: the client opens with a TLS handshake
/// instead of waiting for an SMTP greeting.
#[tokio::test]
async fn implicit_tls_opens_with_a_tls_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut first = [0u8; 2];
        stream.read_exact(&mut first).await.unwrap();
        first
    });

    assert!(send_with(&settings(port, SmtpTls::Tls)).await.is_err());

    assert_eq!(
        server.await.unwrap(),
        [0x16, 0x03],
        "a TLS record comes first"
    );
}

/// The configured sender reaches a real server (Mailpit) on the plain path.
#[tokio::test]
async fn mailpit_still_receives_plain_smtp_with_the_configured_sender() {
    let recipient = unique_email("sender");
    let settings = SmtpSettings {
        from: "Nami Alerts <alerts@nami.test>".parse().unwrap(),
        ..SmtpSettings::plain("localhost", 1025)
    };
    Mailer::from_settings(&settings)
        .unwrap()
        .send(&recipient, "[Nami] sender check", "hello".to_string())
        .await
        .unwrap();

    settled_mail(&recipient, 1).await;
    let stored = stored_mail_for(&recipient).await;
    assert_eq!(stored.len(), 1);
    let (headers, _) = stored[0].raw.split_once("\n\n").expect("a header block");
    assert!(
        headers.contains("\nFrom: \"Nami Alerts\" <alerts@nami.test>\n"),
        "{headers}"
    );
    assert!(
        headers.contains("\nReturn-Path: <alerts@nami.test>\n"),
        "{headers}"
    );
}

/// A refusal is logged as its kind and code only: the server's reply, which
/// here names the recipient, never reaches the log.
#[tokio::test]
async fn a_failed_send_logs_its_kind_and_code_never_the_servers_text() {
    let log = LogCapture::default();
    let _logging = log.install();
    let (port, server) = fake_smtp(
        EHLO_PLAIN,
        "550 5.1.1 <victim@example.com>: Recipient address rejected: User unknown\r\n",
    )
    .await;
    let mailer =
        Mailer::from_settings_with_timeout(&settings(port, SmtpTls::None), Duration::from_secs(3))
            .unwrap();

    let result = mailer
        .send("victim@example.com", "hello", "hello".to_string())
        .await;
    server.abort();

    assert!(result.is_err());
    let lines = log.lines();
    let failures: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("failed to send email"))
        .collect();
    assert_eq!(failures.len(), 1, "{lines:?}");
    assert!(failures[0].contains("kind=rejected"), "{}", failures[0]);
    assert!(failures[0].contains("code=550"), "{}", failures[0]);
    let all = lines.join("\n");
    assert!(!all.contains("victim@example.com"), "{all}");
    assert!(!all.contains("Recipient address rejected"), "{all}");
}

/// A server that cannot be reached at all is a `connection` failure.
#[tokio::test]
async fn an_unreachable_server_is_logged_as_a_connection_failure() {
    let log = LogCapture::default();
    let _logging = log.install();
    // Nothing listens on port 1.
    let result = Mailer::new("127.0.0.1", 1)
        .send("someone@example.com", "hello", "hello".to_string())
        .await;

    assert!(result.is_err());
    let lines = log.lines();
    let failure = lines
        .iter()
        .find(|line| line.contains("failed to send email"))
        .expect("the failure is logged");
    assert!(failure.contains("kind=connection"), "{failure}");
    assert!(failure.contains("code=none"), "{failure}");
}
