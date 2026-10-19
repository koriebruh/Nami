//! Validation of the targets of non-http monitors: the host every host-based
//! type names ([`validate_host`]) and the nameserver of a dns monitor
//! ([`parse_nameserver`]).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use crate::core::validate::has_invisible_or_bidi;

/// The longest `host` a monitor may name, in characters.
pub(crate) const MAX_HOST_CHARS: usize = 253;

/// The port a nameserver is asked on when none is given.
const DEFAULT_NAMESERVER_PORT: u16 = 53;

const NOT_A_HOST: &str = "host must be a host name or an IP address (no scheme, path or port)";

/// The text rules every `host` field obeys, whatever the check type: `raw`
/// trimmed, or the reason it is refused. A host is shown to every viewer, put
/// in alert emails and handed to resolvers and sockets, so it may hold no
/// invisible or text-direction characters (checked on the raw text: U+2028 and
/// U+2029 are whitespace, which `trim` would drop at the edges instead of
/// rejecting them), at most [`MAX_HOST_CHARS`] characters, and none of the
/// whitespace, control characters or `/ ? # @` that make up a scheme, path,
/// query, fragment or credentials.
///
/// An empty host passes: wording "host is required" is up to the caller.
pub(crate) fn check_host_text(raw: &str) -> Result<&str, String> {
    if has_invisible_or_bidi(raw) {
        return Err("host must not contain invisible or text-direction characters".into());
    }
    let host = raw.trim();
    if host.chars().count() > MAX_HOST_CHARS {
        return Err(format!("host must be at most {MAX_HOST_CHARS} characters"));
    }
    if host
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '@'))
    {
        return Err(
            "host must not contain whitespace, control characters or any of / ? # @".into(),
        );
    }
    Ok(host)
}

/// A host name or IP literal, normalized: trimmed, and lower-cased (a name) or
/// in its canonical spelling (an address). It passes [`check_host_text`], and
/// is then either an IP address (IPv6 with or without brackets, IPv4 without)
/// or a name of dot-separated labels of 1-63 letters, digits, `-` or `_` (`_`
/// for service names such as `_dmarc.example.com`) that do not start or end
/// with `-`, with an optional trailing dot. Non-ASCII (international) names
/// must be written in punycode.
///
/// A name whose last label is a number (`2852039166`, `0xa9fea9fe`, `127.1`,
/// `010.0.0.1`) is refused: no such name exists (top-level domains are never
/// all-numeric), but `getaddrinfo` reads it as an address in decimal, octal or
/// hex, so it would reach the metadata address (`2852039166`) past any check
/// of the spelling. Checks judge the addresses a host resolves to (see
/// `checker::allowed_addresses`); this keeps such names from being stored.
pub fn validate_host(raw: &str) -> Result<String, String> {
    let host = check_host_text(raw)?;
    if host.is_empty() {
        return Err("host is required".into());
    }
    if let Some(ip) = ip_literal(host) {
        return Ok(ip.to_string());
    }
    let host = host.to_ascii_lowercase();
    let name = host.strip_suffix('.').unwrap_or(&host);
    let last_label = name.rsplit('.').next().unwrap_or_default();
    if name.split('.').all(is_label) && !looks_numeric(last_label) {
        Ok(host)
    } else {
        Err(NOT_A_HOST.into())
    }
}

/// An IP address written as itself: `[v6]` (brackets only around IPv6), `v6`
/// or `v4`.
fn ip_literal(host: &str) -> Option<IpAddr> {
    match host.strip_prefix('[') {
        Some(bracketed) => bracketed
            .strip_suffix(']')?
            .parse::<Ipv6Addr>()
            .ok()
            .map(IpAddr::V6),
        None => host.parse().ok(),
    }
}

fn is_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `label` is a number as `inet_aton` reads one: decimal or octal
/// digits, or `0x` and hex digits. `label` is lower-case.
fn looks_numeric(label: &str) -> bool {
    match label.strip_prefix("0x") {
        Some(hex) => hex.bytes().all(|b| b.is_ascii_hexdigit()),
        None => !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// `IP`, `IP:port` or `[IPv6]:port` (port 53 when omitted), normalized to
/// the `SocketAddr` form the resolver needs. Only an address: a name would
/// have to be resolved by the system, and the destination is judged at check
/// time (see `uptime::checker::dns`).
pub fn parse_nameserver(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let addr = raw
        .parse::<SocketAddr>()
        .ok()
        .or_else(|| {
            raw.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, DEFAULT_NAMESERVER_PORT))
        })
        .filter(|addr| addr.port() != 0)
        .ok_or_else(|| {
            "dns_nameserver must be an IP address, optionally with a port (1.1.1.1 or 1.1.1.1:53)"
                .to_string()
        })?;
    Ok(addr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_normalized() {
        assert_eq!(validate_host(" Example.COM ").unwrap(), "example.com");
        assert_eq!(validate_host("example.com.").unwrap(), "example.com.");
        assert_eq!(
            validate_host("_dmarc.example.com").unwrap(),
            "_dmarc.example.com"
        );
        assert_eq!(validate_host("localhost").unwrap(), "localhost");
        assert_eq!(validate_host("10.0.0.1").unwrap(), "10.0.0.1");
        assert_eq!(validate_host("[2001:DB8::1]").unwrap(), "2001:db8::1");
        assert_eq!(validate_host("2001:db8::1").unwrap(), "2001:db8::1");
    }

    #[test]
    fn anything_that_is_not_a_bare_host_is_rejected() {
        for raw in [
            "",
            " ",
            ".",
            "a..b",
            "-",
            "-a.test",
            "a-.test",
            "https://x.test",
            "x.test/path",
            "x.test:80",
            "a b",
            "x\n.test",
            "ä.test",
        ] {
            assert!(validate_host(raw).is_err(), "{raw:?} must be rejected");
        }
        assert!(
            validate_host(&format!("{}.test", "a".repeat(64))).is_err(),
            "labels are at most 63 bytes"
        );
        assert!(
            validate_host(&"a.".repeat(127)).is_err(),
            "names are at most 253 bytes"
        );
    }

    #[test]
    fn the_text_rules_of_a_tcp_host_apply_with_the_same_words() {
        let invisible = "host must not contain invisible or text-direction characters";
        for raw in [
            "db\u{200B}.internal",
            "db\u{202E}.internal",
            "\u{2029}db",
            "db\u{2028}",
            "\u{FEFF}db",
            "db\u{00AD}",
        ] {
            assert_eq!(validate_host(raw).unwrap_err(), invisible, "{raw:?}");
            assert_eq!(check_host_text(raw).unwrap_err(), invisible, "{raw:?}");
        }
        let bare = "host must not contain whitespace, control characters or any of / ? # @";
        for raw in [
            "db internal",
            "db\tinternal",
            "db\u{0}",
            "db\u{a0}internal",
            "tcp://db",
            "db/path",
            "db?x=1",
            "db#x",
            "user@db",
        ] {
            assert_eq!(validate_host(raw).unwrap_err(), bare, "{raw:?}");
        }
        let too_long = "host must be at most 253 characters";
        assert_eq!(validate_host(&"a".repeat(254)).unwrap_err(), too_long);
        assert_eq!(
            validate_host(&"\u{e9}".repeat(254)).unwrap_err(),
            too_long,
            "characters, not bytes"
        );
        assert_eq!(check_host_text(&"\u{e9}".repeat(253)).unwrap().len(), 506);
        let name_of_253 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        assert_eq!(validate_host(&name_of_253).unwrap(), name_of_253);
    }

    #[test]
    fn the_shared_text_rules_trim_and_leave_emptiness_to_the_caller() {
        assert_eq!(
            check_host_text("  db.example.com\t").unwrap(),
            "db.example.com"
        );
        assert_eq!(check_host_text("").unwrap(), "");
        assert_eq!(check_host_text(" \t\n").unwrap(), "");
        assert_eq!(validate_host("  ").unwrap_err(), "host is required");
    }

    #[test]
    fn brackets_belong_around_an_ipv6_address_only() {
        assert_eq!(validate_host("[::1]").unwrap(), "::1");
        for raw in ["[10.0.0.1]", "[db]", "[::1", "::1]", "[[::1]]", "[]"] {
            assert_eq!(validate_host(raw).unwrap_err(), NOT_A_HOST, "{raw:?}");
        }
        // Scope ids are not part of an address here.
        assert!(validate_host("fe80::1%eth0").is_err());
        assert!(validate_host("[fe80::1%25eth0]").is_err());
    }

    /// `getaddrinfo` reads these as IPv4 addresses (`2852039166` is
    /// 169.254.169.254), so they are never stored as names.
    #[test]
    fn numbers_the_resolver_reads_as_addresses_are_not_names() {
        for raw in [
            "2852039166",
            "0xa9fea9fe",
            "0XA9FEA9FE",
            "0251.0376.0251.0376",
            "169.254.43518",
            "127.1",
            "010.0.0.1",
            "0",
            "0x",
            "1.2.3",
            "1.2.3.4.5",
            "db.123",
            "db.0x1f",
            "127.1.",
            "10.0.0.1.",
        ] {
            assert_eq!(validate_host(raw).unwrap_err(), NOT_A_HOST, "{raw:?}");
        }
        // Real addresses are addresses, and names that merely contain digits
        // are names.
        for (raw, stored) in [
            ("169.254.169.254", "169.254.169.254"),
            ("127.0.0.1", "127.0.0.1"),
            ("1.example.com", "1.example.com"),
            ("123.example", "123.example"),
            ("db1", "db1"),
            ("0x1f.example.com", "0x1f.example.com"),
            ("a1.1a", "a1.1a"),
            ("4.3.2.1.in-addr.arpa", "4.3.2.1.in-addr.arpa"),
        ] {
            assert_eq!(validate_host(raw).unwrap(), stored, "{raw:?}");
        }
    }

    #[test]
    fn nameservers_get_an_explicit_port() {
        assert_eq!(parse_nameserver("1.1.1.1").unwrap(), "1.1.1.1:53");
        assert_eq!(
            parse_nameserver("127.0.0.1:5353").unwrap(),
            "127.0.0.1:5353"
        );
        assert_eq!(parse_nameserver("::1").unwrap(), "[::1]:53");
        assert_eq!(parse_nameserver("[::1]:5353").unwrap(), "[::1]:5353");
        assert_eq!(parse_nameserver("  9.9.9.9 ").unwrap(), "9.9.9.9:53");
        for raw in ["", "dns.google", "1.1.1.1:0", "1.1.1.1:99999", "1.1.1"] {
            assert!(parse_nameserver(raw).is_err(), "{raw:?} must be rejected");
        }
    }

    #[test]
    fn a_nameserver_is_an_address_and_nothing_else() {
        for raw in [
            "2852039166",
            "0xa9fea9fe",
            "127.1",
            "https://1.1.1.1",
            "1.1.1.1/dns-query",
            "1.1.1.1 8.8.8.8",
            "1.1.1.1\n8.8.8.8",
            "1.1.1.1,8.8.8.8",
            "user@1.1.1.1",
            "[1.1.1.1]:53",
            "[::1]",
            "::1:53:53x",
            "1.1.1.1:",
            "1.1.1.1:-1",
            "1.1.1.1:53:53",
            "localhost",
            "\u{200B}1.1.1.1",
        ] {
            // One fixed message: nothing of the input is echoed back.
            assert_eq!(
                parse_nameserver(raw).unwrap_err(),
                "dns_nameserver must be an IP address, optionally with a port (1.1.1.1 or 1.1.1.1:53)",
                "{raw:?}"
            );
        }
    }
}
