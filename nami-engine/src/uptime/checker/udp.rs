//! `udp` checks: send `udp_payload` and wait for one datagram back. Up when
//! a reply arrives and, if `keyword` is set, the reply contains it (or, with
//! `keyword_mode = not_contains`, does not).

use std::{io::ErrorKind, net::SocketAddr, time::Instant};

use tokio::net::UdpSocket;

use super::{
    DESTINATION_NOT_ALLOWED, Observation, allowed_addresses, describe_io_error, elapsed_ms,
};
use crate::uptime::monitor::model::Monitor;

/// The most of a reply that is kept (and searched for the keyword); a longer
/// datagram is truncated by the OS.
const MAX_REPLY_BYTES: usize = 16 * 1024;

pub(crate) async fn probe(monitor: &Monitor, started: Instant) -> Observation {
    let (Some(host), Some(port), Some(payload)) = (
        monitor.host.as_deref(),
        monitor.port,
        monitor.udp_payload.as_deref(),
    ) else {
        return Observation::down(
            None,
            None,
            "monitor has no host, port or payload".to_string(),
        );
    };
    let Ok(port) = u16::try_from(port) else {
        return Observation::down(None, None, format!("invalid port {port}"));
    };
    let target = match resolve(host, port).await {
        Ok(target) => target,
        Err(message) => return Observation::down(None, None, message),
    };
    let local: SocketAddr = if target.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let reply = async {
        let socket = UdpSocket::bind(local).await?;
        // A connected socket only accepts datagrams from the target, and an
        // ICMP "port unreachable" surfaces as ECONNREFUSED.
        socket.connect(target).await?;
        socket.send(payload.as_bytes()).await?;
        let mut buffer = vec![0u8; MAX_REPLY_BYTES];
        let len = socket.recv(&mut buffer).await?;
        buffer.truncate(len);
        Ok::<_, std::io::Error>(buffer)
    };
    let reply = match reply.await {
        Ok(reply) => reply,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            return Observation::down(
                None,
                None,
                "connection refused (port unreachable)".to_string(),
            );
        }
        Err(e) => return Observation::down(None, None, describe_io_error(&e)),
    };
    let Some(keyword) = monitor.keyword.as_deref() else {
        return Observation::up(None, started);
    };
    let found = String::from_utf8_lossy(&reply).contains(keyword);
    match (monitor.keyword_mode.as_str(), found) {
        ("not_contains", true) => Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" found in the reply"),
        ),
        ("not_contains", false) | (_, true) => Observation::up(None, started),
        (_, false) => Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" not found in the reply"),
        ),
    }
}

/// The address to send to: `host` resolved once, the addresses no check may
/// contact dropped, IPv4 preferred. The datagram goes to the address that was
/// judged.
async fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?;
    let allowed = allowed_addresses(resolved).map_err(|_| DESTINATION_NOT_ALLOWED.to_string())?;
    allowed
        .iter()
        .find(|addr| addr.is_ipv4())
        .or(allowed.first())
        .copied()
        .ok_or_else(|| format!("could not resolve {host}: no addresses"))
}
