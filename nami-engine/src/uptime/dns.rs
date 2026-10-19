//! `dns` checks: resolve `host` for one record type, against
//! `dns_nameserver` or the system resolver, and optionally require an answer
//! that contains `expected_value` (case-insensitive).

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use hickory_resolver::{
    TokioResolver,
    config::{NameServerConfig, ResolveHosts, ResolverConfig},
    net::{DnsError, NetError, runtime::TokioRuntimeProvider},
    proto::{
        op::ResponseCode,
        rr::{RData, RecordType},
    },
};

use super::checker::{
    DESTINATION_NOT_ALLOWED, Observation, elapsed_ms, is_forbidden_destination, printable,
};
use crate::models::monitor::{DNS_RECORD_TYPES, Monitor};

pub(crate) async fn probe(monitor: &Monitor, timeout: Duration, started: Instant) -> Observation {
    let (Some(host), Some(record_type)) =
        (monitor.host.as_deref(), monitor.dns_record_type.as_deref())
    else {
        return Observation::down(None, None, "monitor has no host or record type".to_string());
    };
    let parsed = DNS_RECORD_TYPES
        .contains(&record_type)
        .then(|| record_type.parse::<RecordType>().ok())
        .flatten();
    let Some(record_type) = parsed else {
        return Observation::down(
            None,
            None,
            format!("unsupported record type {record_type:?}"),
        );
    };
    let resolver = match build_resolver(monitor.dns_nameserver.as_deref(), timeout) {
        Ok(resolver) => resolver,
        Err(message) => return Observation::down(None, None, message),
    };
    // The trailing dot makes the name fully qualified: no search domains.
    let fqdn = if host.ends_with('.') {
        host.to_string()
    } else {
        format!("{host}.")
    };
    let host = host.trim_end_matches('.');
    let answers: Vec<String> = match resolver.lookup(fqdn.as_str(), record_type).await {
        Ok(lookup) => lookup
            .answers()
            .iter()
            .filter(|record| record.record_type() == record_type)
            .filter_map(|record| readable(&record.data))
            .collect(),
        Err(e) => {
            // A response time only when a nameserver answered.
            let answered = matches!(e, NetError::Dns(_)).then(|| elapsed_ms(started));
            return Observation::down(None, answered, describe_error(&e, host, record_type));
        }
    };
    if answers.is_empty() {
        return Observation::down(
            None,
            Some(elapsed_ms(started)),
            format!("no {record_type} records found for {host}"),
        );
    }
    if let Some(expected) = monitor.expected_value.as_deref() {
        let needle = expected.to_lowercase();
        if !answers
            .iter()
            .any(|answer| answer.to_lowercase().contains(&needle))
        {
            return Observation::down(
                None,
                Some(elapsed_ms(started)),
                format!(
                    "no {record_type} answer contains \"{expected}\" (got: {})",
                    printable(&answers.join(", "), 200)
                ),
            );
        }
    }
    Observation::up(None, started)
}

/// A fresh resolver per check (no cache, no hosts file): every check asks the
/// nameserver again. `perform_check`'s deadline is the one that counts, so the
/// resolver's own timeout only has to be later than it.
///
/// A nameserver the monitor names is refused when no check may contact it
/// (see `checker::is_forbidden_destination`). The system resolver's own
/// servers are not judged: the host's configuration picks them, and the
/// monitor only chooses the name to look up.
fn build_resolver(nameserver: Option<&str>, timeout: Duration) -> Result<TokioResolver, String> {
    let mut builder = match nameserver {
        Some(raw) => {
            let addr: SocketAddr = raw
                .parse()
                .map_err(|_| format!("invalid nameserver {raw:?}"))?;
            if is_forbidden_destination(addr.ip()) {
                return Err(DESTINATION_NOT_ALLOWED.to_string());
            }
            let mut server = NameServerConfig::udp_and_tcp(addr.ip());
            for connection in &mut server.connections {
                connection.port = addr.port();
            }
            TokioResolver::builder_with_config(
                ResolverConfig::from_name_servers(vec![server]),
                TokioRuntimeProvider::default(),
            )
        }
        None => TokioResolver::builder_tokio()
            .map_err(|e| format!("cannot read the system DNS configuration: {e}"))?,
    };
    let options = builder.options_mut();
    options.timeout = timeout + Duration::from_secs(1);
    options.attempts = 1;
    options.cache_size = 0;
    options.use_hosts_file = ResolveHosts::Never;
    builder
        .build()
        .map_err(|e| format!("cannot build a DNS resolver: {e}"))
}

/// One answer as the text `expected_value` is matched against: addresses as
/// written, names without the trailing dot, MX as "preference exchange", TXT
/// strings concatenated.
fn readable(data: &RData) -> Option<String> {
    let name = |name: &dyn std::fmt::Display| name.to_string().trim_end_matches('.').to_string();
    Some(match data {
        RData::A(a) => a.0.to_string(),
        RData::AAAA(aaaa) => aaaa.0.to_string(),
        RData::CNAME(cname) => name(&cname.0),
        RData::NS(ns) => name(&ns.0),
        RData::MX(mx) => format!("{} {}", mx.preference, name(&mx.exchange)),
        RData::TXT(txt) => txt
            .txt_data
            .iter()
            .map(|part| String::from_utf8_lossy(part))
            .collect(),
        _ => return None,
    })
}

fn describe_error(error: &NetError, host: &str, record_type: RecordType) -> String {
    if error.is_nx_domain() {
        return format!("NXDOMAIN: {host} does not exist");
    }
    if error.is_no_records_found() {
        return format!("no {record_type} records found for {host}");
    }
    match error {
        NetError::Dns(DnsError::ResponseCode(code)) => {
            format!("DNS server answered {}", response_code_name(*code))
        }
        NetError::Timeout => "DNS query timed out".to_string(),
        other => format!("DNS lookup failed: {}", printable(&other.to_string(), 200)),
    }
}

fn response_code_name(code: ResponseCode) -> String {
    match code {
        ResponseCode::ServFail => "SERVFAIL".to_string(),
        ResponseCode::Refused => "REFUSED".to_string(),
        ResponseCode::FormErr => "FORMERR".to_string(),
        ResponseCode::NotImp => "NOTIMP".to_string(),
        other => other.to_str().to_string(),
    }
}
