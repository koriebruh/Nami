//! `ping` checks: one ICMP echo request over an unprivileged datagram
//! socket (`SOCK_DGRAM` + `IPPROTO_ICMP`, no root or `CAP_NET_RAW`). On
//! Linux, and so in Docker, the engine's group must be inside
//! `net.ipv4.ping_group_range`.

use std::{
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence, SurgeError};

use super::checker::{Observation, allowed_addresses};
use crate::models::monitor::Monitor;

/// The classic 56 data bytes of `ping`.
const PAYLOAD: [u8; 56] = [0; 56];

pub(crate) async fn probe(monitor: &Monitor, timeout: Duration) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let ip = match resolve(host).await {
        Ok(ip) => ip,
        Err(message) => return Observation::down(None, None, message),
    };
    let kind = if ip.is_ipv4() { ICMP::V4 } else { ICMP::V6 };
    let client = match Client::new(&Config::builder().kind(kind).build()) {
        Ok(client) => client,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Observation::down(
                None,
                None,
                "ICMP sockets are not permitted for the engine (on Linux, add its group to net.ipv4.ping_group_range)"
                    .to_string(),
            );
        }
        Err(e) => {
            return Observation::down(None, None, format!("cannot open an ICMP socket: {e}"));
        }
    };
    let mut pinger = client.pinger(ip, PingIdentifier(rand::random())).await;
    // `perform_check`'s deadline is the one that counts: this only has to be
    // later than it.
    pinger.timeout(timeout + Duration::from_secs(1));
    match pinger.ping(PingSequence(0), &PAYLOAD).await {
        Ok((_reply, round_trip)) => Observation::up_after(round_trip),
        Err(SurgeError::Timeout { .. }) => {
            Observation::down(None, None, format!("timeout after {}s", timeout.as_secs()))
        }
        Err(e) => Observation::down(None, None, format!("ping failed: {e}")),
    }
}

/// The address to ping for `host`: a literal as is, a name through the system
/// resolver. Addresses no check may contact (see
/// `checker::is_forbidden_destination`) are dropped from what a name resolves
/// to, however the name is spelled (`2852039166` resolves to the metadata
/// address); when nothing else is left the result is the generic refusal. The
/// address that is judged is the one that is pinged: the name is resolved once.
/// IPv4 is preferred.
async fn resolve(host: &str) -> Result<IpAddr, String> {
    let resolved: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, 0)],
        Err(_) => tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| format!("could not resolve {host}: {e}"))?
            .collect(),
    };
    let allowed = allowed_addresses(resolved).map_err(|refused| refused.to_string())?;
    allowed
        .iter()
        .map(SocketAddr::ip)
        .find(IpAddr::is_ipv4)
        .or_else(|| allowed.first().map(SocketAddr::ip))
        .ok_or_else(|| format!("could not resolve {host}: no addresses"))
}
