//! Request configuration of http/keyword monitors: method, body, custom
//! headers and basic auth. Header values and the basic-auth password are
//! write-only secrets: they are sealed with `crypto::Cipher` before they
//! reach the database and no API response ever contains them.
//!
//! Every sealed value is bound to its column and to its monitor (see
//! [`headers_context`] and [`password_context`]), so a blob copied onto
//! another row, or opened as the other secret, does not open.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::is_invisible_or_bidi_char;
use crate::crypto::{Cipher, DecryptError, SecretString};

pub const HTTP_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
/// The only methods that may carry `request_body`.
pub const BODY_METHODS: [&str; 3] = ["POST", "PUT", "PATCH"];
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS: usize = 20;
pub const MAX_HEADER_NAME_BYTES: usize = 256;
pub const MAX_HEADER_VALUE_BYTES: usize = 1024;
pub const MAX_USERNAME_CHARS: usize = 256;
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Framing and hop-by-hop headers the HTTP client owns; letting users set
/// them would enable request smuggling or break the request (hyper and h2
/// mishandle `TE`, `Upgrade` and the like: an HTTP/2 target fails with an
/// opaque error).
pub const RESERVED_HEADERS: [&str; 10] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "te",
    "upgrade",
    "keep-alive",
    "proxy-connection",
    "trailer",
    "expect",
];

/// The column part of the AES-GCM associated data (see `crypto`) of
/// `monitors.request_headers_enc` and `monitors.auth_password_enc`. What is
/// actually used is [`headers_context`] / [`password_context`], which add the
/// monitor id.
pub const HEADERS_CONTEXT: &str = "monitors.request_headers";
pub const PASSWORD_CONTEXT: &str = "monitors.auth_password";

/// The AES-GCM associated data of one monitor's sealed headers:
/// `monitors.request_headers:{id}`.
pub fn headers_context(monitor_id: Uuid) -> String {
    format!("{HEADERS_CONTEXT}:{monitor_id}")
}

/// The AES-GCM associated data of one monitor's sealed basic-auth password:
/// `monitors.auth_password:{id}`.
pub fn password_context(monitor_id: Uuid) -> String {
    format!("{PASSWORD_CONTEXT}:{monitor_id}")
}

/// One custom request header. `Debug` shows the name only.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestHeader {
    pub name: String,
    pub value: SecretString,
}

/// The plaintext inside `request_headers_enc`, as it is written: a JSON list
/// of these, borrowed so that sealing makes no further copy of a value.
#[derive(Serialize)]
struct SealedHeader<'a> {
    name: &'a str,
    value: &'a str,
}

/// The same, as it is read back.
#[derive(Deserialize)]
struct OpenedHeader {
    name: String,
    value: String,
}

/// What a create/PATCH does to the stored headers.
#[derive(Debug, Clone, PartialEq)]
pub enum HeadersChange {
    /// Leave them as they are; `names` are the stored names, for validation.
    Keep {
        names: Vec<String>,
    },
    Clear,
    Replace(Vec<RequestHeader>),
}

impl HeadersChange {
    /// The header names the monitor has once this change is applied.
    pub fn names(&self) -> Vec<&str> {
        match self {
            HeadersChange::Keep { names } => names.iter().map(String::as_str).collect(),
            HeadersChange::Clear => Vec::new(),
            HeadersChange::Replace(headers) => headers.iter().map(|h| h.name.as_str()).collect(),
        }
    }
}

/// What a create/PATCH does to the stored basic-auth password.
#[derive(Debug, Clone, PartialEq)]
pub enum PasswordChange {
    /// Leave it as it is; `present` says whether one is stored.
    Keep {
        present: bool,
    },
    Clear,
    Replace(SecretString),
}

impl PasswordChange {
    pub fn is_set(&self) -> bool {
        match self {
            PasswordChange::Keep { present } => *present,
            PasswordChange::Clear => false,
            PasswordChange::Replace(_) => true,
        }
    }
}

/// A header as API responses show it: the value is never returned.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeaderView {
    pub name: String,
    pub value_set: bool,
}

/// RFC 7230 `tchar`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Checks one header and returns it with its name trimmed. Error messages
/// name the header, and only when its name is a valid token; they never echo
/// a value, nor an invalid name (a `Name: value` pasted into the name field
/// would show its value).
pub fn validate_header(header: RequestHeader) -> Result<RequestHeader, String> {
    let name = header.name.trim().to_string();
    if name.len() > MAX_HEADER_NAME_BYTES {
        return Err(format!(
            "header names must be at most {MAX_HEADER_NAME_BYTES} bytes"
        ));
    }
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        return Err("a header name is not a valid HTTP header name: use letters, digits and ! # $ % & ' * + - . ^ _ ` | ~ only".into());
    }
    if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "header {name} cannot be set: the checker manages Host, Content-Length, Transfer-Encoding, Connection and the other hop-by-hop headers"
        ));
    }
    let value = header.value.expose();
    // An empty value would replace a stored secret with nothing ("Replace
    // all" with a line left unfilled); HTTP trims the blanks around a value.
    if value.trim_matches([' ', '\t']).is_empty() {
        return Err(format!("header {name}: enter a value"));
    }
    if value.contains(['\r', '\n']) {
        return Err(format!("header {name}: values must not contain CR or LF"));
    }
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Err(format!(
            "header {name}: values must not contain control characters"
        ));
    }
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(format!(
            "header {name}: values must be at most {MAX_HEADER_VALUE_BYTES} bytes"
        ));
    }
    Ok(RequestHeader {
        name,
        value: header.value,
    })
}

pub fn validate_headers(headers: Vec<RequestHeader>) -> Result<Vec<RequestHeader>, String> {
    if headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} request headers are allowed"));
    }
    let headers: Vec<RequestHeader> = headers
        .into_iter()
        .map(validate_header)
        .collect::<Result<_, _>>()?;
    // Header names are case-insensitive: `X-A` and `x-a` would both be sent.
    for (i, header) in headers.iter().enumerate() {
        if headers[..i]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(&header.name))
        {
            return Err(format!("header {} is listed twice", header.name));
        }
    }
    Ok(headers)
}

/// Basic-auth user id (RFC 7617): no `:` and no control characters. It is
/// shown to members, so it also follows the rule of names, urls and hosts:
/// no invisible or text-direction characters.
pub fn validate_username(raw: &str) -> Result<String, String> {
    if raw.contains(':') || raw.chars().any(char::is_control) {
        return Err("auth_username must not contain ':' or control characters".into());
    }
    if raw.chars().any(is_invisible_or_bidi_char) {
        return Err("auth_username must not contain invisible or text-direction characters".into());
    }
    if raw.chars().count() > MAX_USERNAME_CHARS {
        return Err(format!(
            "auth_username must be at most {MAX_USERNAME_CHARS} characters"
        ));
    }
    Ok(raw.to_string())
}

pub fn validate_password(password: &SecretString) -> Result<(), String> {
    let value = password.expose();
    if value.chars().any(char::is_control) {
        return Err("auth_password must not contain control characters".into());
    }
    if value.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "auth_password must be at most {MAX_PASSWORD_BYTES} bytes"
        ));
    }
    Ok(())
}

/// Seals the header list for monitor `monitor_id`. The JSON it seals holds
/// every value in the clear, so that buffer is wiped once the value is sealed
/// (best effort: a `SecretString` is a plain `String`, wiped by nobody).
pub fn seal_headers(cipher: &Cipher, monitor_id: Uuid, headers: &[RequestHeader]) -> Vec<u8> {
    let stored: Vec<SealedHeader<'_>> = headers
        .iter()
        .map(|h| SealedHeader {
            name: &h.name,
            value: h.value.expose(),
        })
        .collect();
    let json =
        Zeroizing::new(serde_json::to_vec(&stored).expect("a list of strings always serializes"));
    cipher.encrypt(&headers_context(monitor_id), &json)
}

/// Any failure (wrong key, another monitor's blob, tampering, malformed
/// plaintext) is a [`DecryptError`].
pub fn open_headers(
    cipher: &Cipher,
    monitor_id: Uuid,
    sealed: &[u8],
) -> Result<Vec<RequestHeader>, DecryptError> {
    let json = Zeroizing::new(cipher.decrypt(&headers_context(monitor_id), sealed)?);
    let stored: Vec<OpenedHeader> = serde_json::from_slice(&json).map_err(|_| DecryptError)?;
    Ok(stored
        .into_iter()
        .map(|h| RequestHeader {
            name: h.name,
            value: SecretString::new(h.value),
        })
        .collect())
}

pub fn seal_password(cipher: &Cipher, monitor_id: Uuid, password: &SecretString) -> Vec<u8> {
    cipher.encrypt(&password_context(monitor_id), password.expose().as_bytes())
}

pub fn open_password(
    cipher: &Cipher,
    monitor_id: Uuid,
    sealed: &[u8],
) -> Result<SecretString, DecryptError> {
    let bytes = cipher.decrypt(&password_context(monitor_id), sealed)?;
    String::from_utf8(bytes)
        .map(SecretString::new)
        .map_err(|_| DecryptError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::TEST_KEY;

    fn header(name: &str, value: &str) -> RequestHeader {
        RequestHeader {
            name: name.to_string(),
            value: SecretString::new(value),
        }
    }

    #[test]
    fn header_names_are_http_tokens() {
        for name in ["X-Api-Key", "x_y", "A1", "a.b", "Accept", "!#$%&'*+-.^_`|~"] {
            assert_eq!(validate_header(header(name, "v")).unwrap().name, name);
        }
        for name in [
            "", " ", "X Bad", "X-Bad:", "(x)", "x\"y", "x@y", "x,y", "x;y", "x<y", "x=y", "x>y",
            "x?y", "x[y", "x\\y", "x]y", "x{y", "x}y", "x/y", "X-\u{fc}", "x\u{7f}", "x\ty",
            "x\r\ny",
        ] {
            let error = validate_header(header(name, "v")).unwrap_err();
            assert!(
                error.contains("not a valid HTTP header name"),
                "{name:?}: {error}"
            );
        }
    }

    #[test]
    fn a_name_is_trimmed_and_the_length_cap_counts_bytes() {
        assert_eq!(
            validate_header(header("  X-Api-Key \t", "v")).unwrap().name,
            "X-Api-Key"
        );
        assert!(validate_header(header(&"x".repeat(256), "v")).is_ok());
        let error = validate_header(header(&"x".repeat(257), "v")).unwrap_err();
        assert_eq!(error, "header names must be at most 256 bytes");
        // Too long is reported before anything about the characters, and an
        // invalid name is never echoed back.
        let error = validate_header(header(&format!("{} ", "x:".repeat(200)), "v")).unwrap_err();
        assert!(!error.contains("x:x:"), "{error}");
    }

    #[test]
    fn reserved_names_are_refused_in_any_case_and_their_neighbours_are_not() {
        for name in [
            "Host",
            "host",
            "HOST",
            "Content-Length",
            "content-LENGTH",
            "Transfer-Encoding",
            "Connection",
            "CONNECTION",
            " Host ",
            // Hop-by-hop headers that hyper or h2 mishandle (an `Upgrade` or
            // `TE` breaks an HTTP/2 target with an opaque error).
            "TE",
            "te",
            "Upgrade",
            "Keep-Alive",
            "keep-alive",
            "Proxy-Connection",
            "Trailer",
            "Expect",
        ] {
            let error = validate_header(header(name, "v")).unwrap_err();
            assert!(error.contains("cannot be set"), "{name:?}: {error}");
        }
        for name in [
            "Hosts",
            "X-Host",
            "Content-Type",
            "Content-Lengths",
            "Connection-Id",
            "Keep-Alive-Timeout",
            "X-Upgrade",
            "Tes",
            "Expects",
        ] {
            assert!(validate_header(header(name, "v")).is_ok(), "{name}");
        }
    }

    #[test]
    fn values_hold_text_and_tabs_but_no_line_breaks_or_control_characters() {
        for value in [
            "Bearer abc.def",
            "a\tb",
            "caf\u{e9} \u{2615}",
            "quote\" and \\ back",
            &"v".repeat(1024),
            &"\u{e9}".repeat(512),
        ] {
            assert!(validate_header(header("X-A", value)).is_ok(), "{value:?}");
        }
        for value in ["a\rb", "a\nb", "a\r\nX-Injected: 1", "\r", "\n"] {
            assert_eq!(
                validate_header(header("X-A", value)).unwrap_err(),
                "header X-A: values must not contain CR or LF",
                "{value:?}"
            );
        }
        for value in ["a\0b", "a\u{1}b", "a\u{1b}b", "a\u{7f}b", "a\u{85}b"] {
            assert_eq!(
                validate_header(header("X-A", value)).unwrap_err(),
                "header X-A: values must not contain control characters",
                "{value:?}"
            );
        }
        for value in ["v".repeat(1025), "\u{e9}".repeat(513)] {
            assert_eq!(
                validate_header(header("X-A", &value)).unwrap_err(),
                "header X-A: values must be at most 1024 bytes"
            );
        }
    }

    /// "Replace all" with a value line left empty would store an empty
    /// secret in place of the one the monitor had.
    #[test]
    fn a_header_needs_a_value() {
        for value in ["", " ", "\t", "  \t "] {
            assert_eq!(
                validate_header(header("X-Api-Key", value)).unwrap_err(),
                "header X-Api-Key: enter a value",
                "{value:?}"
            );
        }
        assert!(validate_header(header("X-A", " v ")).is_ok());
    }

    #[test]
    fn a_name_may_appear_only_once_whatever_its_case() {
        for names in [["X-A", "x-a"], ["Accept", "ACCEPT"], ["X-A", " X-A "]] {
            let list = names.iter().map(|name| header(name, "v")).collect();
            let error = validate_headers(list).unwrap_err();
            assert_eq!(
                error,
                format!("header {} is listed twice", names[1].trim()),
                "{names:?}"
            );
        }
        let distinct = vec![header("X-A", "v"), header("X-B", "v"), header("X-AA", "v")];
        assert_eq!(validate_headers(distinct).unwrap().len(), 3);
    }

    #[test]
    fn errors_name_the_header_never_its_value() {
        for (name, value) in [
            ("X-Api-Key", "secret-1\r\nX: y"),
            ("X-Api-Key", "secret-2\0"),
            ("X-Api-Key", &format!("secret-3{}", "a".repeat(1100))),
            ("Host", "secret-4"),
            ("Bad Name", "secret-5"),
        ] {
            let error = validate_header(header(name, value)).unwrap_err();
            assert!(!error.contains("secret-"), "{error}");
        }
    }

    #[test]
    fn at_most_twenty_headers() {
        let list = |count: usize| -> Vec<RequestHeader> {
            (0..count)
                .map(|i| header(&format!("X-H{i}"), "v"))
                .collect()
        };
        assert_eq!(validate_headers(list(20)).unwrap().len(), 20);
        assert_eq!(validate_headers(Vec::new()).unwrap(), Vec::new());
        assert_eq!(
            validate_headers(list(21)).unwrap_err(),
            "at most 20 request headers are allowed"
        );
    }

    #[test]
    fn usernames_follow_rfc_7617_and_the_rule_of_names() {
        for raw in [
            "monitor",
            "jos\u{e9}",
            "a b",
            "a\u{200c}b",
            &"u".repeat(256),
        ] {
            assert_eq!(validate_username(raw).unwrap(), raw);
        }
        for raw in ["a:b", ":", "a\nb", "a\0b", "a\u{85}b", "a\u{7f}b"] {
            assert_eq!(
                validate_username(raw).unwrap_err(),
                "auth_username must not contain ':' or control characters",
                "{raw:?}"
            );
        }
        for raw in ["a\u{200b}b", "a\u{202e}b", "\u{feff}a", "a\u{2028}"] {
            assert_eq!(
                validate_username(raw).unwrap_err(),
                "auth_username must not contain invisible or text-direction characters",
                "{raw:?}"
            );
        }
        assert_eq!(
            validate_username(&"u".repeat(257)).unwrap_err(),
            "auth_username must be at most 256 characters"
        );
        assert!(
            validate_username(&"\u{e9}".repeat(256)).is_ok(),
            "characters, not bytes"
        );
    }

    #[test]
    fn passwords_are_opaque_text_without_control_characters() {
        for raw in [
            "",
            "pass word",
            "p\u{e4}ss:w\u{f6}rd",
            "\u{200b}",
            &"p".repeat(1024),
        ] {
            assert!(
                validate_password(&SecretString::new(raw)).is_ok(),
                "{raw:?}"
            );
        }
        for raw in ["a\nb", "a\rb", "a\0b", "a\tb", "a\u{7f}b", "a\u{85}b"] {
            assert_eq!(
                validate_password(&SecretString::new(raw)).unwrap_err(),
                "auth_password must not contain control characters",
                "{raw:?}"
            );
        }
        assert_eq!(
            validate_password(&SecretString::new("p".repeat(1025))).unwrap_err(),
            "auth_password must be at most 1024 bytes"
        );
    }

    #[test]
    fn changes_know_the_names_and_whether_a_password_is_set() {
        let keep = HeadersChange::Keep {
            names: vec!["X-A".to_string(), "X-B".to_string()],
        };
        assert_eq!(keep.names(), ["X-A", "X-B"]);
        assert!(HeadersChange::Clear.names().is_empty());
        assert_eq!(
            HeadersChange::Replace(vec![header("X-C", "v")]).names(),
            ["X-C"]
        );
        assert!(PasswordChange::Keep { present: true }.is_set());
        assert!(!PasswordChange::Keep { present: false }.is_set());
        assert!(!PasswordChange::Clear.is_set());
        assert!(PasswordChange::Replace(SecretString::new("p")).is_set());
    }

    #[test]
    fn contexts_name_the_column_and_the_monitor() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(headers_context(a), format!("monitors.request_headers:{a}"));
        assert_eq!(password_context(a), format!("monitors.auth_password:{a}"));
        assert_ne!(headers_context(a), headers_context(b));
        assert_ne!(headers_context(a), password_context(a));
    }

    #[test]
    fn headers_survive_sealing_whatever_they_hold() {
        let cipher = Cipher::new(&TEST_KEY);
        let id = Uuid::new_v4();
        let headers = vec![
            header("X-Api-Key", "abc\"def\\ghi"),
            header("X-Empty", ""),
            header("X-Unicode", "caf\u{e9} \u{1f680}\ttab"),
            header("X-Json", "{\"a\":[1,2]}"),
        ];
        let sealed = seal_headers(&cipher, id, &headers);
        assert_eq!(open_headers(&cipher, id, &sealed).unwrap(), headers);
        let none = seal_headers(&cipher, id, &[]);
        assert_eq!(open_headers(&cipher, id, &none).unwrap(), Vec::new());
    }

    #[test]
    fn a_sealed_value_hides_its_plaintext_and_is_never_the_same_twice() {
        let cipher = Cipher::new(&TEST_KEY);
        let id = Uuid::new_v4();
        let headers = [header("X-Api-Key", "needle-secret-123")];
        let (one, two) = (
            seal_headers(&cipher, id, &headers),
            seal_headers(&cipher, id, &headers),
        );
        assert_ne!(one, two, "a fresh nonce for every seal");
        for sealed in [&one, &two] {
            for plain in ["needle-secret-123", "X-Api-Key"] {
                assert!(!sealed.windows(plain.len()).any(|w| w == plain.as_bytes()));
            }
        }
        let password = SecretString::new("needle-password-456");
        let sealed = seal_password(&cipher, id, &password);
        assert!(
            !sealed
                .windows(password.expose().len())
                .any(|w| w == password.expose().as_bytes())
        );
        assert_eq!(open_password(&cipher, id, &sealed).unwrap(), password);
    }

    #[test]
    fn a_sealed_value_opens_only_for_its_monitor_its_column_and_its_key() {
        let cipher = Cipher::new(&TEST_KEY);
        let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
        let headers = seal_headers(&cipher, id, &[header("X-A", "v")]);
        let password = seal_password(&cipher, id, &SecretString::new("p"));

        assert_eq!(open_headers(&cipher, other, &headers), Err(DecryptError));
        assert_eq!(open_password(&cipher, other, &password), Err(DecryptError));
        assert_eq!(open_password(&cipher, id, &headers), Err(DecryptError));
        assert_eq!(open_headers(&cipher, id, &password), Err(DecryptError));
        let wrong_key = Cipher::new(&[5u8; 32]);
        assert_eq!(open_headers(&wrong_key, id, &headers), Err(DecryptError));
        assert_eq!(open_password(&wrong_key, id, &password), Err(DecryptError));
    }

    #[test]
    fn tampered_truncated_and_empty_blobs_fail_cleanly() {
        let cipher = Cipher::new(&TEST_KEY);
        let id = Uuid::new_v4();
        let headers = seal_headers(&cipher, id, &[header("X-A", "v")]);
        let password = seal_password(&cipher, id, &SecretString::new("p"));
        for position in [0, 1, 12, headers.len() - 1] {
            let mut tampered = headers.clone();
            tampered[position] ^= 0x01;
            assert_eq!(open_headers(&cipher, id, &tampered), Err(DecryptError));
        }
        let mut tampered = password.clone();
        *tampered.last_mut().unwrap() ^= 0x80;
        assert_eq!(open_password(&cipher, id, &tampered), Err(DecryptError));
        for sealed in [&headers[..headers.len() - 1], &headers[..5], &[][..]] {
            assert_eq!(open_headers(&cipher, id, sealed), Err(DecryptError));
            assert_eq!(open_password(&cipher, id, sealed), Err(DecryptError));
        }
    }

    /// Sealed, but not what a header list is: that is a failure to open, not
    /// a panic and not an empty list.
    #[test]
    fn a_blob_that_opens_to_something_else_is_a_decrypt_error() {
        let cipher = Cipher::new(&TEST_KEY);
        let id = Uuid::new_v4();
        for plaintext in [
            &b"not json"[..],
            b"{\"name\":\"x\"}",
            b"[{\"name\":1}]",
            b"",
        ] {
            let sealed = cipher.encrypt(&headers_context(id), plaintext);
            assert_eq!(open_headers(&cipher, id, &sealed), Err(DecryptError));
        }
        let sealed = cipher.encrypt(&password_context(id), &[0xff, 0xfe]);
        assert_eq!(open_password(&cipher, id, &sealed), Err(DecryptError));
    }

    #[test]
    fn headers_print_their_names_and_never_their_values() {
        let printed = format!("{:?}", header("X-Api-Key", "needle-secret-123"));
        assert!(printed.contains("X-Api-Key"), "{printed}");
        assert!(!printed.contains("needle-secret-123"), "{printed}");
        let change = HeadersChange::Replace(vec![header("X-Api-Key", "needle-secret-123")]);
        let password = PasswordChange::Replace(SecretString::new("needle-password-456"));
        for printed in [format!("{change:?}"), format!("{password:?}")] {
            assert!(!printed.contains("needle-"), "{printed}");
        }
    }
}
