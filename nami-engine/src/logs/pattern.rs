//! Message patterns: the first 1,024 characters of a message with its
//! variable parts replaced, in this order:
//! - quoted strings -> `<str>`;
//! - ISO-8601 timestamps (a date and a time, `T` or a space between them,
//!   optional fraction and offset) -> `<ts>`;
//! - UUIDs -> `<uuid>`;
//! - IPv6 addresses (not identifier paths such as `a::b2`; an address may
//!   end in `::`, and a `/64` prefix length after it is a `<num>`) and IPv4
//!   addresses -> `<ip>`;
//! - `0x…` and standalone hex runs of at least 8 characters with a digit and
//!   a letter -> `<hex>`;
//! - standalone numbers -> `<num>`: integers, decimals and exponents, with
//!   an optional sign and an optional unit (a duration such as `12ms` or
//!   `1h30m`, a size such as `10KB`, a rate, or `%`). A decimal followed by
//!   letters that are not a unit keeps them (`3.5C` -> `<num>C`); an integer
//!   followed by such letters is a word and stays (`123abc`, `5xx`).
//!
//! ClickHouse derives `pattern_hash = cityHash64(pattern)` from the result,
//! so equal patterns always share a hash. Every step is linear in the length
//! of the text: the regex crate does not backtrack, and the quote scan
//! remembers a quote kind that has no closing quote left.

use std::{net::Ipv6Addr, sync::LazyLock};

use regex::{Captures, Regex};

/// Only this many leading characters of a message form its pattern.
pub const PATTERN_SOURCE_CHARS: usize = 1024;

/// A date and a time: `2026-10-13T11:59:58.250+02:00`, with a space for the
/// `T`, a comma for the fraction, no seconds, or in the basic form
/// `20261013T115958Z`. A date or a time alone is not a timestamp.
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:",
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt ][0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:[.,][0-9]+)?)?",
        r"|[0-9]{8}[Tt][0-9]{6}(?:[.,][0-9]+)?",
        r")(?:[Zz]|[+-][0-9]{2}(?::?[0-9]{2})?)?\b",
    ))
    .unwrap()
});
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap()
});
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\b")
        .unwrap()
});
/// Runs of address-like characters; the ones that are IPv6 addresses (see
/// [`looks_like_ipv6`]) are replaced.
static IPV6_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z_.:]+").unwrap());
static HEX_PREFIXED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b0[xX][0-9A-Fa-f]+\b").unwrap());
static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9A-Fa-f]{8,}\b").unwrap());

/// Digits with an optional fraction, or a fraction alone (`.5`).
const NUMBER_BODY: &str = r"(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)";
/// Units that may follow a number directly, in any letter case: time
/// (`12ms`, `3.5s`, `5m`, `2h`, `7d`), data (`10KB`, `1GiB`, `128Mi`, `1.5k`,
/// `100mbps`) and frequency (`2.4GHz`).
const UNITS: &str = "ns|us|µs|ms|s|sec|secs|min|mins|m|h|hr|hrs|d|w|wk|wks\
    |b|k|g|t|kb|mb|gb|tb|pb|ki|mi|gi|ti|pi|kib|mib|gib|tib|pib|bps|kbps|mbps|gbps\
    |hz|khz|mhz|ghz";
/// A number is standalone when no word character touches it; the captured
/// prefix (start of text or a non-word character) is kept. After the
/// number, a `%`, a unit that ends the word (compound durations such as
/// `1h30m0s` included) or just the end of the word. `12abc`, `5xx` and
/// `3d_model` are not numbers.
static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(^|\W)[-+]?{NUMBER_BODY}(?:[eE][-+]?[0-9]+)?(?:%|(?i:{UNITS})(?:{NUMBER_BODY}(?i:{UNITS}))*\b|\b)"
    ))
    .unwrap()
});
/// What is left of a decimal when letters that are not a unit cut it short
/// (`3.5C` first becomes `<num>.5C`): the fraction belongs to the number.
static SPLIT_DECIMAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<num>\.[0-9]+").unwrap());

pub fn normalize_message(message: &str) -> String {
    let head = match message.char_indices().nth(PATTERN_SOURCE_CHARS) {
        Some((cut, _)) => &message[..cut],
        None => message,
    };
    let text = replace_quoted(head);
    let text = TIMESTAMP.replace_all(&text, "<ts>");
    let text = UUID.replace_all(&text, "<uuid>");
    // Before IPv4, so an IPv4-mapped address (`::ffff:192.0.2.1`) is one `<ip>`.
    let text = IPV6_CANDIDATE.replace_all(&text, |caps: &Captures| replace_ipv6(&caps[0]));
    let text = IPV4.replace_all(&text, "<ip>");
    let text = HEX_PREFIXED.replace_all(&text, "<hex>");
    let text = HEX_RUN.replace_all(&text, |caps: &Captures| {
        let run = &caps[0];
        let has_digit = run.bytes().any(|b| b.is_ascii_digit());
        let has_letter = run.bytes().any(|b| b.is_ascii_alphabetic());
        if has_digit && has_letter {
            "<hex>".to_string()
        } else {
            run.to_string()
        }
    });
    let text = NUMBER.replace_all(&text, "${1}<num>");
    let text = SPLIT_DECIMAL.replace_all(&text, "<num>");
    text.into_owned()
}

/// `"…"` always, and `'…'` when the opening quote does not follow a letter
/// or digit and the closing one is not followed by one, so apostrophes in
/// words ("can't", "user's") stay as they are. Backslash escapes are
/// skipped; an unterminated quote is left alone.
fn replace_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    // Whether the search for a closing `"` / `'` has already come up empty.
    // Which quotes can close does not depend on where the search starts, so
    // after one failure no later quote of that kind can close either, and
    // skipping them keeps the whole scan linear (each search that succeeds
    // consumes the text it covered, each kind can fail only once).
    let mut exhausted = [false; 2];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let kind = match c {
            '"' => Some(0),
            '\'' if i == 0 || !chars[i - 1].is_alphanumeric() => Some(1),
            _ => None,
        };
        if let Some(kind) = kind
            && !exhausted[kind]
        {
            match closing_quote(&chars, i) {
                Some(end) => {
                    out.push_str("<str>");
                    i = end + 1;
                    continue;
                }
                None => exhausted[kind] = true,
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn closing_quote(chars: &[char], open: usize) -> Option<usize> {
    let quote = chars[open];
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => {
                let ends =
                    quote == '"' || chars.get(i + 1).is_none_or(|next| !next.is_alphanumeric());
                if ends {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// `<ip>` when the run is an IPv6 address (see [`looks_like_ipv6`]) once
/// trailing punctuation is taken off. A trailing `::` belongs to the address
/// (`2001:db8::`, the `fe80::` of `fe80::/10`) but the `:` of
/// `2001:db8::1: refused` and the `.` of `::1.` do not, so the run is tried
/// without its trailing dots first (an address never ends in a dot, but may
/// end in `::`), then without any trailing dots and colons. Two tries at
/// most, so the scan stays linear.
fn replace_ipv6(run: &str) -> String {
    if !run.contains(':') {
        return run.to_string();
    }
    let candidates = [run.trim_end_matches('.'), run.trim_end_matches(['.', ':'])];
    for core in candidates {
        if looks_like_ipv6(core) {
            return format!("<ip>{}", &run[core.len()..]);
        }
    }
    run.to_string()
}

/// An IPv6 address, as opposed to an identifier path that merely parses as
/// one: `a::b2` or `be::f2` are made of hex digits and `::` too, but no
/// address is written like that. Besides parsing, a token needs a group that
/// marks it as an address: one starting with a digit (an identifier cannot)
/// or one of four characters containing a digit (`fe80`, `ff02`, `c0a8`).
fn looks_like_ipv6(text: &str) -> bool {
    text.contains(':') && text.split(':').any(is_address_group) && text.parse::<Ipv6Addr>().is_ok()
}

fn is_address_group(group: &str) -> bool {
    group.starts_with(|c: char| c.is_ascii_digit())
        || (group.len() == 4 && group.bytes().any(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::normalize_message;

    #[test]
    fn the_spec_example() {
        assert_eq!(
            normalize_message("user 42 logged in from 10.0.0.7"),
            "user <num> logged in from <ip>"
        );
    }

    #[test]
    fn quoted_strings_go_first_so_their_contents_never_leak() {
        assert_eq!(
            normalize_message(r#"GET "/api/users/42" took 12ms"#),
            "GET <str> took <num>"
        );
        assert_eq!(
            normalize_message(r#"say "a \"quoted\" 7" now"#),
            "say <str> now"
        );
        assert_eq!(
            normalize_message("can't reach 'db-1' (user's pool)"),
            "can't reach <str> (user's pool)"
        );
        assert_eq!(normalize_message("'it's fine' said 2"), "<str> said <num>");
        assert_eq!(
            normalize_message(r#"unterminated "quote 42"#),
            r#"unterminated "quote <num>"#
        );
    }

    #[test]
    fn uuids_ips_hex_and_numbers() {
        assert_eq!(
            normalize_message("order 3F2A9C1E-0B6F-4C1E-9B7A-2F1D3C4B5A69 paid"),
            "order <uuid> paid"
        );
        assert_eq!(
            normalize_message("peer 192.168.1.255:8443 closed"),
            "peer <ip>:<num> closed"
        );
        assert_eq!(
            normalize_message("from fe80::1%eth0 and [2001:db8::8a2e:370:7334]:80"),
            "from <ip>%eth0 and [<ip>]:<num>"
        );
        assert_eq!(normalize_message("ping ::1."), "ping <ip>.");
        assert_eq!(
            normalize_message("ptr 0x7ffd5e8c and 0XAB"),
            "ptr <hex> and <hex>"
        );
        assert_eq!(
            normalize_message("commit a1b2c3d4e5 on deadbeef"),
            "commit <hex> on deadbeef"
        );
        assert_eq!(
            normalize_message("ids 12345678 -5 +3.25 x=-7 1,2"),
            "ids <num> <num> <num> x=<num> <num>,<num>"
        );
    }

    #[test]
    fn numbers_inside_words_and_code_paths_are_left_alone() {
        assert_eq!(
            normalize_message("v2 abc123 123abc snake_42 took 150ms"),
            "v2 abc123 123abc snake_42 took <num>"
        );
        assert_eq!(
            normalize_message("std::io::Error at a::b"),
            "std::io::Error at a::b"
        );
        assert_eq!(
            normalize_message("at 10:00:00 retry 3/5"),
            "at <num>:<num>:<num> retry <num>/<num>"
        );
        assert_eq!(
            normalize_message("256.1.1.1 is not an address"),
            "<num>.<num> is not an address"
        );
    }

    #[test]
    fn durations_sizes_percentages_and_decimals_are_numbers() {
        for (message, expected) in [
            ("took 12ms", "took <num>"),
            (
                "took 3.5s then 250us, 12µs and 7ns",
                "took <num> then <num>, <num> and <num>",
            ),
            ("waited 5m, 2h and 7d", "waited <num>, <num> and <num>"),
            ("ran 1h30m0s and 2m30.5s", "ran <num> and <num>"),
            ("sent 10KB, 3.2MB and 1GiB", "sent <num>, <num> and <num>"),
            ("cpu 45% mem 99.5%", "cpu <num> mem <num>"),
            ("rate 1e-3 or 2.5E+6", "rate <num> or <num>"),
            ("offset .5 and -.25", "offset <num> and <num>"),
            ("p99 latency 0.25ms", "p99 latency <num>"),
            (
                "mem 128Mi of 1Gi, 12.5k req/s",
                "mem <num> of <num>, <num> req/s",
            ),
            (
                "temperature -3.5C and 1.5px",
                "temperature <num>C and <num>px",
            ),
            ("limit 100MB/s", "limit <num>/s"),
            ("5 ms apart", "<num> ms apart"),
        ] {
            assert_eq!(normalize_message(message), expected, "{message}");
        }
    }

    #[test]
    fn a_number_followed_by_other_letters_is_part_of_a_word() {
        for word in [
            "123abc", "5xx", "1st", "12msx", "10kbs", "sha256", "x86_64", "utf8", "http2", "2fa",
            "3d_model",
        ] {
            let message = format!("see {word} here");
            assert_eq!(normalize_message(&message), message, "{word}");
        }
    }

    #[test]
    fn iso_8601_timestamps_are_one_placeholder() {
        for (message, expected) in [
            ("at 2026-10-13T11:59:58Z done", "at <ts> done"),
            ("at 2026-10-13T11:59:58.250+02:00 done", "at <ts> done"),
            ("2026-10-13 11:59:58,123 INFO started", "<ts> INFO started"),
            ("2026-10-13T11:59:58-0500 x", "<ts> x"),
            ("2026-10-13T11:59Z", "<ts>"),
            ("stamp 20261013T115958Z", "stamp <ts>"),
            ("lowercase 2026-10-13t11:59:58z", "lowercase <ts>"),
        ] {
            assert_eq!(normalize_message(message), expected, "{message}");
        }
        assert_eq!(
            normalize_message("a 2026-10-13T11:59:58Z b"),
            normalize_message("a 2025-01-02T03:04:05.678+09:00 b"),
            "different timestamps, same pattern"
        );
        // Not timestamps: a date alone, a time alone, and quoted text.
        assert_eq!(normalize_message("on 2026-10-13"), "on <num>-<num>-<num>");
        assert_eq!(normalize_message("at 11:59:58"), "at <num>:<num>:<num>");
        assert_eq!(
            normalize_message(r#"at "2026-10-13T11:59:58Z""#),
            "at <str>"
        );
    }

    #[test]
    fn ipv6_addresses_are_ips_but_scoped_identifiers_are_not() {
        for address in [
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::8a2e:370:7334",
            "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
            "::ffff:c0a8:1",
        ] {
            assert_eq!(
                normalize_message(&format!("peer {address} up")),
                "peer <ip> up",
                "{address}"
            );
        }
        assert_eq!(
            normalize_message("listen [::1]:8080"),
            "listen [<ip>]:<num>"
        );
        for token in [
            "a::b",
            "a::b2",
            "be::f2",
            "d1::b2",
            "dead::beef1",
            "std::io::Error",
            "ns::fn1",
            "Foo::bar2",
        ] {
            let message = format!("call {token} failed");
            assert_eq!(
                normalize_message(&message),
                message,
                "{token} is a code path, not an address"
            );
        }
    }

    #[test]
    fn ipv6_addresses_ending_in_a_double_colon_and_prefixes_are_whole_ips() {
        for (message, expected) in [
            ("net 2001:db8::", "net <ip>"),
            ("net 2001:db8:: is down", "net <ip> is down"),
            ("route fe80::/10", "route <ip>/<num>"),
            (
                "route 2001:db8::/32 via fe80::1",
                "route <ip>/<num> via <ip>",
            ),
            ("see 2001:db8::.", "see <ip>."),
            ("peer 2001:db8::1: refused", "peer <ip>: refused"),
            ("ping ::1.", "ping <ip>."),
        ] {
            assert_eq!(normalize_message(message), expected, "{message}");
        }
        // A trailing `::` on a scoped identifier is still not an address.
        for token in ["a::", "dead::", "std::", "Foo::", "cafe::"] {
            let message = format!("call {token} failed");
            assert_eq!(normalize_message(&message), message, "{token}");
        }
    }

    #[test]
    fn the_quote_scan_is_linear_on_quotes_that_never_close() {
        // `normalize_message` only ever passes 1,024 characters on, so this
        // reaches the scan itself. Every ` 'a` below opens a quote that
        // nothing closes, and so does every `"` of the second text (each one
        // after the first is escaped by the backslash before it). A scan
        // that restarted at each of them would need seconds for these; this
        // one needs milliseconds.
        let started = std::time::Instant::now();
        for text in [" 'a".repeat(60_000), format!("\"{}", "\\\"".repeat(60_000))] {
            assert_eq!(super::replace_quoted(&text), text);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn adversarial_heads_are_normalized_in_one_pass() {
        // Every pattern here is linear by construction; this guards against
        // swapping in a backtracking construct or a quadratic scan. The
        // bound is generous: a 1,024-character head takes microseconds.
        let started = std::time::Instant::now();
        for message in [
            "1".repeat(2000) + "x",
            "a:".repeat(2000),
            "0x".repeat(2000),
            "1.".repeat(2000),
            "\"".repeat(2000),
            " 'a".repeat(700),
            "2026-10-13T".repeat(200),
            "::1:".repeat(500),
            "9ms".repeat(700),
            "\\\"'".repeat(500),
        ] {
            let pattern = normalize_message(&message);
            assert!(pattern.len() < 8 * 1024, "{} bytes", pattern.len());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn only_the_first_1024_characters_count() {
        let message = format!("{}{}", "é".repeat(1024), " 42 tail");
        assert_eq!(normalize_message(&message), "é".repeat(1024));
        let same_head = format!("{} different tail", "x".repeat(1024));
        assert_eq!(
            normalize_message(&same_head),
            normalize_message(&"x".repeat(1024))
        );
    }

    #[test]
    fn equal_shapes_give_equal_patterns() {
        let a = normalize_message("user 42 logged in from 10.0.0.7 as \"alice\"");
        let b = normalize_message("user 7 logged in from 172.16.0.1 as \"bob\"");
        assert_eq!(a, b);
        assert_ne!(a, normalize_message("user 7 logged out"));
    }

    #[test]
    fn empty_and_placeholder_like_text_is_stable() {
        assert_eq!(normalize_message(""), "");
        assert_eq!(normalize_message("<num> <ip>"), "<num> <ip>");
    }
}
