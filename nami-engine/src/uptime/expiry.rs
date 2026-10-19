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
use crate::models::monitor::{IpVersion, Monitor};

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
    let stream = match tls::connect(ctx.tls.clone(), host, port, monitor.ip_version()).await {
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
    let client = ctx.http.select(true, true, false, IpVersion::Any);
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
    let expiration = match expiration_of(&rdap) {
        Ok(expiration) => expiration,
        Err(message) => {
            return Observation::down(Some(code), Some(response_time), message.to_string());
        }
    };
    let threshold = monitor
        .expiry_threshold_days
        .unwrap_or(DEFAULT_DOMAIN_THRESHOLD_DAYS);
    let mut observation = judge("domain", expiration, threshold, response_time);
    observation.status_code = Some(code);
    observation
}

/// The RDAP domain object: only its events matter here. They are read
/// leniently: registries differ, and an event that lacks a field (or is not
/// an object at all) must not hide the one that matters.
#[derive(Deserialize)]
struct RdapDomain {
    #[serde(default)]
    events: serde_json::Value,
}

const NO_EXPIRATION_EVENT: &str = "RDAP response has no expiration event";
const BAD_EXPIRATION_DATE: &str = "RDAP expiration date is not valid";

/// The Unix time of the first `expiration` event with a date that parses
/// (RFC 3339, or a bare `YYYY-MM-DD` taken as midnight UTC).
fn expiration_of(rdap: &RdapDomain) -> Result<i64, &'static str> {
    let events = rdap
        .events
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut found_event = false;
    for event in events {
        let is_expiration = event
            .get("eventAction")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|action| action.eq_ignore_ascii_case("expiration"));
        if !is_expiration {
            continue;
        }
        found_event = true;
        let date = event.get("eventDate").and_then(serde_json::Value::as_str);
        if let Some(at) = date.and_then(parse_rdap_date) {
            return Ok(at);
        }
    }
    Err(if found_event {
        BAD_EXPIRATION_DATE
    } else {
        NO_EXPIRATION_EVENT
    })
}

fn parse_rdap_date(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Some(at.timestamp());
    }
    let day = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
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

/// Up unless `expires_at` (Unix seconds) is within `threshold_days` of now,
/// the boundary included: exactly that many days left is down. The error names
/// the subject, the days left (or since) and the date.
fn judge(subject: &str, expires_at: i64, threshold_days: i32, response_time: u32) -> Observation {
    judge_at(
        Utc::now().timestamp(),
        subject,
        expires_at,
        threshold_days,
        response_time,
    )
}

fn judge_at(
    now: i64,
    subject: &str,
    expires_at: i64,
    threshold_days: i32,
    response_time: u32,
) -> Observation {
    let seconds_left = expires_at - now;
    if seconds_left > i64::from(threshold_days) * 86_400 {
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

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn is_down(subject: &str, left_seconds: i64, threshold: i32) -> bool {
        judge_at(NOW, subject, NOW + left_seconds, threshold, 5)
            .error
            .is_some()
    }

    #[test]
    fn exactly_the_threshold_is_down_and_one_second_more_is_up_for_both_types() {
        for subject in ["certificate", "domain"] {
            for threshold in [1, 14, 30, 365] {
                let n = i64::from(threshold);
                assert!(is_down(subject, (n - 1) * DAY, threshold), "{subject} N-1");
                assert!(is_down(subject, n * DAY, threshold), "{subject} N");
                assert!(!is_down(subject, n * DAY + 1, threshold), "{subject} N+1s");
                assert!(!is_down(subject, (n + 1) * DAY, threshold), "{subject} N+1");
            }
        }
    }

    #[test]
    fn the_message_says_how_long_is_left() {
        let at = judge_at(NOW, "domain", NOW + 30 * DAY, 30, 5);
        assert_eq!(
            at.error.as_deref(),
            Some("domain expires in 30 days (2027-02-14)")
        );
    }

    #[test]
    fn rdap_dates_accept_rfc3339_and_bare_days() {
        assert_eq!(parse_rdap_date("2030-01-01T00:00:00Z"), Some(1_893_456_000));
        assert_eq!(
            parse_rdap_date("2030-01-01T02:00:00+02:00"),
            Some(1_893_456_000)
        );
        assert_eq!(parse_rdap_date("2030-01-01"), Some(1_893_456_000));
        assert_eq!(parse_rdap_date(" 2030-01-01 "), Some(1_893_456_000));
        assert_eq!(parse_rdap_date("soon"), None);
        assert_eq!(parse_rdap_date("2030-13-45"), None);
    }

    fn domain(events: serde_json::Value) -> RdapDomain {
        RdapDomain { events }
    }

    #[test]
    fn the_first_expiration_event_with_a_usable_date_wins() {
        use serde_json::json;
        let events = json!([
            "junk",
            { "eventDate": "2030-01-01" },
            { "eventAction": "expiration" },
            { "eventAction": "expiration", "eventDate": "garbage" },
            { "eventAction": "Expiration", "eventDate": "2030-01-01" },
            { "eventAction": "expiration", "eventDate": "2031-01-01" },
        ]);
        assert_eq!(expiration_of(&domain(events)), Ok(1_893_456_000));
        let none = json!([{ "eventAction": "registration", "eventDate": "2000-01-01" }]);
        assert_eq!(expiration_of(&domain(none)), Err(NO_EXPIRATION_EVENT));
        assert_eq!(
            expiration_of(&domain(json!(null))),
            Err(NO_EXPIRATION_EVENT)
        );
        assert_eq!(
            expiration_of(&domain(json!("events"))),
            Err(NO_EXPIRATION_EVENT)
        );
        let bad = json!([{ "eventAction": "expiration", "eventDate": "garbage" }]);
        assert_eq!(expiration_of(&domain(bad)), Err(BAD_EXPIRATION_DATE));
        let undated = json!([{ "eventAction": "expiration" }]);
        assert_eq!(expiration_of(&domain(undated)), Err(BAD_EXPIRATION_DATE));
    }
}
