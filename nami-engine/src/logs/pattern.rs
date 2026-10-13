//! Message patterns: the first 1,024 characters of a message with its
//! variable parts replaced, in this order: quoted strings -> `<str>`, UUIDs
//! -> `<uuid>`, IPv4/IPv6 addresses -> `<ip>`, `0x…` and standalone hex runs
//! of at least 8 characters with a digit and a letter -> `<hex>`, standalone
//! integers/decimals (optional sign) -> `<num>`. ClickHouse derives
//! `pattern_hash = cityHash64(pattern)` from the result, so equal patterns
//! always share a hash.

use std::{net::Ipv6Addr, sync::LazyLock};

use regex::{Captures, Regex};

/// Only this many leading characters of a message form its pattern.
pub const PATTERN_SOURCE_CHARS: usize = 1024;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap()
});
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\b")
        .unwrap()
});
/// Runs of address-like characters; the ones that parse as IPv6 are replaced.
static IPV6_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z_.:]+").unwrap());
static HEX_PREFIXED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b0[xX][0-9A-Fa-f]+\b").unwrap());
static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9A-Fa-f]{8,}\b").unwrap());
/// A number is standalone when no word character touches it; the captured
/// prefix (start of text or a non-word character) is kept.
static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|\W)[-+]?[0-9]+(?:\.[0-9]+)?\b").unwrap());

pub fn normalize_message(message: &str) -> String {
    let head = match message.char_indices().nth(PATTERN_SOURCE_CHARS) {
        Some((cut, _)) => &message[..cut],
        None => message,
    };
    let text = replace_quoted(head);
    let text = UUID.replace_all(&text, "<uuid>");
    let text = IPV4.replace_all(&text, "<ip>");
    let text = IPV6_CANDIDATE.replace_all(&text, |caps: &Captures| replace_ipv6(&caps[0]));
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
    text.into_owned()
}

/// `"…"` always, and `'…'` when the opening quote does not follow a letter
/// or digit and the closing one is not followed by one, so apostrophes in
/// words ("can't", "user's") stay as they are. Backslash escapes are
/// skipped; an unterminated quote is left alone.
fn replace_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let opens = c == '"' || (c == '\'' && (i == 0 || !chars[i - 1].is_alphanumeric()));
        if opens && let Some(end) = closing_quote(&chars, i) {
            out.push_str("<str>");
            i = end + 1;
            continue;
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

/// `<ip>` when the run (minus trailing `.`/`:` punctuation) is an IPv6
/// address with at least one digit — so `std::io` or `a::b` stay.
fn replace_ipv6(run: &str) -> String {
    let core = run.trim_end_matches(['.', ':']);
    let is_address = core.contains(':')
        && core.bytes().any(|b| b.is_ascii_digit())
        && core.parse::<Ipv6Addr>().is_ok();
    if is_address {
        format!("<ip>{}", &run[core.len()..])
    } else {
        run.to_string()
    }
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
            "GET <str> took 12ms"
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
            "v2 abc123 123abc snake_42 took 150ms"
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
