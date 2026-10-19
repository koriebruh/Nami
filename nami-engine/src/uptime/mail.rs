//! `smtp`, `pop3` and `imap` checks: connect (over implicit TLS when
//! `use_tls`), read the server greeting, and require the protocol's ready
//! reply: SMTP `220`, POP3 `+OK`, IMAP `* OK` or `* PREAUTH`.
//!
//! The greeting is attacker-controlled text: it is read with a hard cap, and
//! only ever quoted through [`printable`] (and, like every error, cleaned by
//! `perform_check`).

use std::time::Instant;

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    net::TcpStream,
};

use super::{
    checker::{
        CheckContext, DESTINATION_NOT_ALLOWED, Observation, allowed_addresses, describe_io_error,
        elapsed_ms, printable,
    },
    tls,
};
use crate::models::monitor::{IpVersion, Monitor};

/// A greeting is one line; never keep more than this of it.
const MAX_GREETING_BYTES: usize = 512;

/// Chars of a wrong greeting quoted in the error.
const QUOTED_GREETING_CHARS: usize = 100;

pub(crate) async fn probe(ctx: &CheckContext, monitor: &Monitor, started: Instant) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let Some(port) = monitor.port.and_then(|port| u16::try_from(port).ok()) else {
        return Observation::down(None, None, "monitor has no valid port".to_string());
    };
    let greeting = if monitor.use_tls {
        match tls::connect(ctx.tls.clone(), host, port, IpVersion::Any).await {
            Ok(mut stream) => read_greeting(&mut stream).await,
            Err(message) => return Observation::down(None, None, message),
        }
    } else {
        match connect(host, port).await {
            Ok(mut stream) => read_greeting(&mut stream).await,
            Err(message) => return Observation::down(None, None, message),
        }
    };
    let greeting = match greeting {
        Ok(greeting) => greeting,
        Err(message) => return Observation::down(None, None, message),
    };
    if is_ready(&monitor.check_type, &greeting) {
        Observation::up(None, started)
    } else {
        Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!(
                "unexpected greeting: {}",
                printable(&greeting, QUOTED_GREETING_CHARS)
            ),
        )
    }
}

/// Resolves `host` once, drops the addresses no check may contact and
/// connects to what is left.
async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    // A (host, port) tuple, not a "host:port" string, so IPv6 literals work.
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| describe_io_error(&e))?;
    let allowed = allowed_addresses(resolved).map_err(|_| DESTINATION_NOT_ALLOWED.to_string())?;
    TcpStream::connect(&allowed[..])
        .await
        .map_err(|e| describe_io_error(&e))
}

/// The first line the server sends, without its line break: read until a
/// newline arrives, the server closes, or [`MAX_GREETING_BYTES`] are in. Never
/// holds more than that.
async fn read_greeting<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, String> {
    let mut greeting = Vec::with_capacity(128);
    let mut chunk = [0u8; 256];
    while !greeting.contains(&b'\n') && greeting.len() < MAX_GREETING_BYTES {
        let room = (MAX_GREETING_BYTES - greeting.len()).min(chunk.len());
        let read = stream
            .read(&mut chunk[..room])
            .await
            .map_err(|e| describe_io_error(&e))?;
        if read == 0 {
            break;
        }
        greeting.extend_from_slice(&chunk[..read]);
    }
    if greeting.is_empty() {
        return Err("connection closed before the server greeting".to_string());
    }
    let end = greeting
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(greeting.len());
    Ok(String::from_utf8_lossy(&greeting[..end])
        .trim_end_matches('\r')
        .to_string())
}

fn is_ready(check_type: &str, greeting: &str) -> bool {
    match check_type {
        "smtp" => starts_with_reply(greeting, "220"),
        "pop3" => starts_with_reply(greeting, "+OK"),
        "imap" => starts_with_reply(greeting, "* OK") || starts_with_reply(greeting, "* PREAUTH"),
        _ => false,
    }
}

/// Whether `greeting` begins with the status word `reply` (case-insensitive)
/// as a whole word: followed by a space, a hyphen (SMTP's multi-line form) or
/// nothing, so `2200` and `+OKAY` do not count.
fn starts_with_reply(greeting: &str, reply: &str) -> bool {
    let Some(head) = greeting.get(..reply.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(reply)
        && matches!(
            greeting.as_bytes().get(reply.len()),
            None | Some(b' ' | b'-')
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_word_must_stand_alone() {
        assert!(is_ready("smtp", "220"));
        assert!(is_ready("smtp", "220-multi"));
        assert!(!is_ready("smtp", "2200 x"));
        assert!(!is_ready("smtp", "5220 x"));
        assert!(is_ready("pop3", "+ok ready"));
        assert!(!is_ready("pop3", "+OKAY"));
        assert!(is_ready("imap", "* OK ready"));
        assert!(is_ready("imap", "* PREAUTH x"));
        assert!(!is_ready("imap", "* OKAY"));
        assert!(!is_ready("imap", "*OK"));
        assert!(!is_ready("smtp", "é"));
    }
}
