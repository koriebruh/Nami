//! `ssl_expiry` (the leaf certificate of a TLS handshake) and
//! `domain_expiry` (the RDAP `expiration` event) checks. Both go down when
//! the expiry is less than `expiry_threshold_days` away, and say how long is
//! left.

use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{
    checker::{
        CheckContext, DESTINATION_NOT_ALLOWED, MAX_BODY_BYTES, Observation, describe_request_error,
        elapsed_ms, names_forbidden_address, read_capped,
    },
    tls::{self, date_of, days},
};
use crate::models::monitor::Monitor;

pub const DEFAULT_SSL_THRESHOLD_DAYS: i32 = 14;
pub const DEFAULT_DOMAIN_THRESHOLD_DAYS: i32 = 30;

pub(crate) async fn probe_ssl(
    ctx: &CheckContext,
    monitor: &Monitor,
    started: Instant,
) -> Observation {
    let Some(host) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no host".to_string());
    };
    let Some(port) = monitor.port.and_then(|port| u16::try_from(port).ok()) else {
        return Observation::down(None, None, "monitor has no valid port".to_string());
    };
    let stream = match tls::connect(ctx.tls.clone(), host, port).await {
        Ok(stream) => stream,
        Err(message) => return Observation::down(None, None, message),
    };
    let response_time = elapsed_ms(started);
    let (_, connection) = stream.get_ref();
    let Some(leaf) = connection
        .peer_certificates()
        .and_then(|chain| chain.first())
    else {
        return Observation::down(
            None,
            Some(response_time),
            "the server sent no certificate".to_string(),
        );
    };
    let not_after = match x509_parser::parse_x509_certificate(leaf) {
        Ok((_, certificate)) => certificate.validity().not_after.timestamp(),
        Err(_) => {
            return Observation::down(
                None,
                Some(response_time),
                "could not parse the server certificate".to_string(),
            );
        }
    };
    let threshold = monitor
        .expiry_threshold_days
        .unwrap_or(DEFAULT_SSL_THRESHOLD_DAYS);
    judge("certificate", not_after, threshold, response_time)
}

pub(crate) async fn probe_domain(
    ctx: &CheckContext,
    monitor: &Monitor,
    started: Instant,
) -> Observation {
    let Some(domain) = monitor.host.as_deref() else {
        return Observation::down(None, None, "monitor has no domain".to_string());
    };
    let url = match rdap_url(&ctx.rdap_base_url, domain) {
        Ok(url) => url,
        Err(message) => return Observation::down(None, None, message),
    };
    // The base URL is operator config, but an IP literal never reaches the
    // client's guarded resolver: refuse a forbidden one here (names, and every
    // redirect hop, are judged by the client itself).
    if names_forbidden_address(&url) {
        return Observation::down(None, None, DESTINATION_NOT_ALLOWED.to_string());
    }
    // rdap.org answers with a redirect to the registry's RDAP server: follow
    // it (no custom headers, certificates verified). Same client as every
    // check: no proxy, guarded resolver, no Referer, and no timeout of its
    // own: the deadline in `perform_check` covers the whole lookup.
    let client = ctx.http.select(true, true, false);
    let request = client.get(url).header(
        reqwest::header::ACCEPT,
        "application/rdap+json, application/json",
    );
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            return Observation::down(
                None,
                None,
                format!("RDAP lookup failed: {}", describe_request_error(&e)),
            );
        }
    };
    let code = response.status().as_u16();
    if code != 200 {
        return Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!("RDAP lookup failed: HTTP {code}"),
        );
    }
    let body = match read_capped(&mut response, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(e) => {
            return Observation::down(
                Some(code),
                None,
                format!("RDAP lookup failed: {}", describe_request_error(&e)),
            );
        }
    };
    let response_time = elapsed_ms(started);
    let Ok(rdap) = serde_json::from_slice::<RdapDomain>(&body) else {
        return Observation::down(
            Some(code),
            Some(response_time),
            "RDAP response is not valid JSON".to_string(),
        );
    };
    let expiration = rdap
        .events
        .iter()
        .filter(|event| event.action.eq_ignore_ascii_case("expiration"))
        .find_map(|event| event.date.as_deref())
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok());
    let Some(expiration) = expiration else {
        return Observation::down(
            Some(code),
            Some(response_time),
            "RDAP response has no expiration date".to_string(),
        );
    };
    let threshold = monitor
        .expiry_threshold_days
        .unwrap_or(DEFAULT_DOMAIN_THRESHOLD_DAYS);
    let mut observation = judge("domain", expiration.timestamp(), threshold, response_time);
    observation.status_code = Some(code);
    observation
}

/// The RDAP domain object: only its events matter here.
#[derive(Deserialize)]
struct RdapDomain {
    #[serde(default)]
    events: Vec<RdapEvent>,
}

#[derive(Deserialize)]
struct RdapEvent {
    #[serde(rename = "eventAction")]
    action: String,
    #[serde(rename = "eventDate")]
    date: Option<String>,
}

/// `{base}/domain/{domain}`, the domain pushed as one percent-encoded path
/// segment.
fn rdap_url(base: &str, domain: &str) -> Result<reqwest::Url, String> {
    let invalid = || "RDAP_BASE_URL is not a valid http(s) URL".to_string();
    let mut url = reqwest::Url::parse(base).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    url.path_segments_mut()
        .map_err(|_| invalid())?
        .pop_if_empty()
        .push("domain")
        .push(domain);
    Ok(url)
}

/// Up unless `expires_at` (Unix seconds) is less than `threshold_days`
/// away; the error names the subject, the days left (or since) and the date.
fn judge(subject: &str, expires_at: i64, threshold_days: i32, response_time: u32) -> Observation {
    let seconds_left = expires_at - Utc::now().timestamp();
    if seconds_left >= i64::from(threshold_days) * 86_400 {
        return Observation {
            status_code: None,
            response_time_ms: Some(response_time),
            error: None,
        };
    }
    let date = date_of(expires_at);
    let error = if seconds_left < 0 {
        format!(
            "{subject} expired {} ago ({date})",
            days((-seconds_left) / 86_400)
        )
    } else {
        format!(
            "{subject} expires in {} ({date})",
            days(seconds_left / 86_400)
        )
    };
    Observation::down(None, Some(response_time), error)
}
