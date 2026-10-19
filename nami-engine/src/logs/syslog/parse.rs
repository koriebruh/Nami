//! RFC 5424 syslog messages:
//! `<PRI>1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA [MSG]`.
//! The header is checked strictly (field lengths, printable ASCII); the
//! message may be any bytes (a leading UTF-8 BOM is dropped, invalid UTF-8
//! becomes U+FFFD).

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::logs::normalize::{LogEvent, in_accepted_range, level_from_number};

/// The structured-data element that carries the source token:
/// `[nami token="<source token>"]`. It is never stored.
pub const NAMI_SD_ID: &str = "nami";
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// `Debug` hides the structured-data values (the source token is one) and
/// the message text.
#[derive(Clone, PartialEq)]
pub struct SyslogMessage {
    pub facility: u8,
    pub severity: u8,
    pub timestamp: Option<DateTime<Utc>>,
    pub hostname: Option<String>,
    pub app_name: Option<String>,
    pub procid: Option<String>,
    pub msgid: Option<String>,
    pub structured_data: Vec<SdElement>,
    pub message: String,
}

/// `Debug` prints the parameter names only: the values can be the token.
#[derive(Clone, PartialEq)]
pub struct SdElement {
    pub id: String,
    pub params: Vec<(String, String)>,
}

impl fmt::Debug for SdElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let SdElement { id, params } = self;
        let names: Vec<&str> = params.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("SdElement")
            .field("id", id)
            .field("params", &names)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for SyslogMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a new field cannot be left out by accident.
        let SyslogMessage {
            facility,
            severity,
            timestamp,
            hostname,
            app_name,
            procid,
            msgid,
            structured_data,
            message,
        } = self;
        f.debug_struct("SyslogMessage")
            .field("facility", facility)
            .field("severity", severity)
            .field("timestamp", timestamp)
            .field("hostname", hostname)
            .field("app_name", app_name)
            .field("procid", procid)
            .field("msgid", msgid)
            .field("structured_data", structured_data)
            .field("message_len", &message.len())
            .finish()
    }
}

/// Static messages only: an error never echoes frame content (which may
/// hold a token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("missing or invalid PRI")]
    Pri,
    #[error("not an RFC 5424 version 1 message")]
    Version,
    #[error("invalid TIMESTAMP")]
    Timestamp,
    #[error("invalid HOSTNAME, APP-NAME, PROCID or MSGID")]
    Header,
    #[error("invalid STRUCTURED-DATA")]
    StructuredData,
}

pub fn parse(frame: &[u8]) -> Result<SyslogMessage, ParseError> {
    let mut p = Parser {
        input: frame,
        pos: 0,
    };
    let pri = p.pri()?;
    p.version()?;
    let timestamp = p.timestamp()?;
    let hostname = p.header_field(255)?;
    let app_name = p.header_field(48)?;
    let procid = p.header_field(128)?;
    let msgid = p.header_field(32)?;
    let structured_data = p.structured_data()?;
    let message = p.message()?;
    Ok(SyslogMessage {
        facility: pri / 8,
        severity: pri % 8,
        timestamp,
        hostname,
        app_name,
        procid,
        msgid,
        structured_data,
        message,
    })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// The bytes up to (not including) the next space or the end.
    fn token(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b' ') {
            self.pos += 1;
        }
        &self.input[start..self.pos]
    }

    fn pri(&mut self) -> Result<u8, ParseError> {
        if !self.eat(b'<') {
            return Err(ParseError::Pri);
        }
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) && self.pos - start < 3 {
            self.pos += 1;
        }
        let digits = &self.input[start..self.pos];
        if digits.is_empty() || !self.eat(b'>') {
            return Err(ParseError::Pri);
        }
        let value: u16 = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or(ParseError::Pri)?;
        if value > 191 {
            return Err(ParseError::Pri);
        }
        Ok(value as u8)
    }

    fn version(&mut self) -> Result<(), ParseError> {
        if self.token() == b"1" && self.eat(b' ') {
            Ok(())
        } else {
            Err(ParseError::Version)
        }
    }

    fn timestamp(&mut self) -> Result<Option<DateTime<Utc>>, ParseError> {
        let raw = self.token();
        if !self.eat(b' ') {
            return Err(ParseError::Timestamp);
        }
        if raw == b"-" {
            return Ok(None);
        }
        let text = std::str::from_utf8(raw).map_err(|_| ParseError::Timestamp)?;
        DateTime::parse_from_rfc3339(text)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| ParseError::Timestamp)
    }

    /// NILVALUE or 1..=`max` printable ASCII characters, then a space.
    fn header_field(&mut self, max: usize) -> Result<Option<String>, ParseError> {
        let raw = self.token();
        if !self.eat(b' ')
            || raw.is_empty()
            || raw.len() > max
            || !raw.iter().all(|b| (33..=126).contains(b))
        {
            return Err(ParseError::Header);
        }
        if raw == b"-" {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(raw).into_owned()))
    }

    fn structured_data(&mut self) -> Result<Vec<SdElement>, ParseError> {
        if self.eat(b'-') {
            return Ok(Vec::new());
        }
        let mut elements = Vec::new();
        while self.peek() == Some(b'[') {
            elements.push(self.sd_element()?);
        }
        if elements.is_empty() {
            Err(ParseError::StructuredData)
        } else {
            Ok(elements)
        }
    }

    fn sd_element(&mut self) -> Result<SdElement, ParseError> {
        self.eat(b'[');
        let id = self.sd_name()?;
        let mut params = Vec::new();
        loop {
            if self.eat(b']') {
                return Ok(SdElement { id, params });
            }
            if !self.eat(b' ') {
                return Err(ParseError::StructuredData);
            }
            let name = self.sd_name()?;
            if !self.eat(b'=') || !self.eat(b'"') {
                return Err(ParseError::StructuredData);
            }
            params.push((name, self.param_value()?));
        }
    }

    /// SD-NAME: 1..=32 printable ASCII characters except `=`, space, `]`, `"`.
    fn sd_name(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| (33..=126).contains(&b) && !matches!(b, b'=' | b']' | b'"'))
        {
            self.pos += 1;
        }
        let name = &self.input[start..self.pos];
        if name.is_empty() || name.len() > 32 {
            return Err(ParseError::StructuredData);
        }
        Ok(String::from_utf8_lossy(name).into_owned())
    }

    /// PARAM-VALUE after its opening quote, through the closing one. `\"`,
    /// `\\` and `\]` are unescaped; any other backslash is kept as is.
    fn param_value(&mut self) -> Result<String, ParseError> {
        let mut value = Vec::new();
        loop {
            match self.peek() {
                None => return Err(ParseError::StructuredData),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(String::from_utf8_lossy(&value).into_owned());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(escaped @ (b'"' | b'\\' | b']')) => {
                            value.push(escaped);
                            self.pos += 1;
                        }
                        _ => value.push(b'\\'),
                    }
                }
                Some(byte) => {
                    value.push(byte);
                    self.pos += 1;
                }
            }
        }
    }

    /// Nothing, or a space and the message (BOM dropped, trailing CR/LF
    /// trimmed, invalid UTF-8 replaced).
    fn message(&mut self) -> Result<String, ParseError> {
        if self.peek().is_none() {
            return Ok(String::new());
        }
        if !self.eat(b' ') {
            return Err(ParseError::StructuredData);
        }
        let mut rest = &self.input[self.pos..];
        rest = rest.strip_prefix(BOM).unwrap_or(rest);
        while let Some((last, init)) = rest.split_last() {
            if matches!(last, b'\n' | b'\r') {
                rest = init;
            } else {
                break;
            }
        }
        Ok(String::from_utf8_lossy(rest).into_owned())
    }
}

impl SyslogMessage {
    /// The source token from `[nami token="..."]`, if present.
    pub fn token(&self) -> Option<&str> {
        self.structured_data
            .iter()
            .filter(|element| element.id == NAMI_SD_ID)
            .flat_map(|element| element.params.iter())
            .find(|(name, _)| name == "token")
            .map(|(_, value)| value.as_str())
    }

    /// The event this message becomes: severity -> level; every
    /// structured-data element except `nami` -> `fields.<SD-ID>` (a param
    /// repeated within an element becomes an array); `facility`,
    /// `hostname`, `app_name`, `procid` and `msgid` -> fields of those
    /// names. A timestamp outside the accepted range is replaced by the
    /// receive time and kept as `fields.timestamp`.
    pub fn into_event(self, received_at: DateTime<Utc>) -> LogEvent {
        let mut fields = Map::new();
        for element in self.structured_data {
            if element.id == NAMI_SD_ID {
                continue;
            }
            let mut params = Map::new();
            for (name, value) in element.params {
                match params.get_mut(&name) {
                    Some(Value::Array(values)) => values.push(Value::String(value)),
                    Some(existing) => {
                        *existing = Value::Array(vec![existing.take(), Value::String(value)])
                    }
                    None => {
                        params.insert(name, Value::String(value));
                    }
                }
            }
            fields.insert(element.id, Value::Object(params));
        }
        fields.insert("facility".to_string(), Value::from(self.facility));
        for (key, value) in [
            ("hostname", self.hostname),
            ("app_name", self.app_name),
            ("procid", self.procid),
            ("msgid", self.msgid),
        ] {
            if let Some(value) = value {
                fields.insert(key.to_string(), Value::String(value));
            }
        }
        let dt = match self.timestamp {
            Some(dt) if in_accepted_range(dt, received_at) => dt,
            Some(dt) => {
                fields.insert(
                    "timestamp".to_string(),
                    Value::String(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
                );
                received_at
            }
            None => received_at,
        };
        LogEvent::new(
            dt,
            level_from_number(self.severity.into()),
            self.message,
            fields,
            String::new(),
            String::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn debug_output_never_holds_the_token_or_the_message() {
        let m = parse(b"<14>1 - h a - - [nami token=\"s3cr3t-token\"][o k=\"v4lue\"] private text")
            .unwrap();
        for text in [format!("{m:?}"), format!("{:#?}", m.structured_data)] {
            for secret in ["s3cr3t-token", "v4lue", "private text"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        assert!(format!("{m:?}").contains("message_len"));
    }

    #[test]
    fn parses_the_rfc_5424_examples() {
        let m = parse(b"<34>1 2003-10-11T22:14:15.003Z mymachine.example.com su - ID47 - \xEF\xBB\xBF'su root' failed for lonvick on /dev/pts/8").unwrap();
        assert_eq!((m.facility, m.severity), (4, 2));
        assert_eq!(m.timestamp, Some(at("2003-10-11T22:14:15.003Z")));
        assert_eq!(m.hostname.as_deref(), Some("mymachine.example.com"));
        assert_eq!(
            (
                m.app_name.as_deref(),
                m.procid.as_deref(),
                m.msgid.as_deref()
            ),
            (Some("su"), None, Some("ID47"))
        );
        assert!(m.structured_data.is_empty());
        assert_eq!(
            m.message, "'su root' failed for lonvick on /dev/pts/8",
            "the BOM is dropped"
        );

        let m = parse(b"<165>1 2003-08-24T05:14:15.000003-07:00 192.0.2.1 myproc 8710 - - %% It's time to make the do-nuts.").unwrap();
        assert_eq!((m.facility, m.severity), (20, 5));
        assert_eq!(m.timestamp, Some(at("2003-08-24T12:14:15.000003Z")));
        assert_eq!(m.procid.as_deref(), Some("8710"));
        assert_eq!(m.message, "%% It's time to make the do-nuts.");

        let m = parse(b"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"][examplePriority@32473 class=\"high\"]").unwrap();
        assert_eq!(m.structured_data.len(), 2);
        assert_eq!(m.structured_data[0].id, "exampleSDID@32473");
        assert_eq!(
            m.structured_data[0].params[1],
            ("eventSource".to_string(), "Application".to_string())
        );
        assert_eq!(
            m.structured_data[1].params,
            vec![("class".to_string(), "high".to_string())]
        );
        assert_eq!(m.message, "", "no MSG part");
    }

    #[test]
    fn param_values_unescape_only_quote_backslash_and_bracket() {
        let m = parse(br#"<14>1 - - - - - [x a="q\"uote \\back \]br \n"] m"#).unwrap();
        assert_eq!(m.structured_data[0].params[0].1, r#"q"uote \back ]br \n"#);
        assert_eq!(m.timestamp, None);
        assert_eq!(m.message, "m");
    }

    #[test]
    fn the_token_comes_from_the_nami_element_only() {
        let m = parse(b"<14>1 - h a - - [origin ip=\"1.2.3.4\"][nami token=\"abc\"] hi").unwrap();
        assert_eq!(m.token(), Some("abc"));
        assert_eq!(
            parse(b"<14>1 - h a - - [nami tokenx=\"abc\"] hi")
                .unwrap()
                .token(),
            None
        );
        assert_eq!(
            parse(b"<14>1 - h a - - [other token=\"abc\"] hi")
                .unwrap()
                .token(),
            None
        );
        assert_eq!(parse(b"<14>1 - h a - - - hi").unwrap().token(), None);
    }

    #[test]
    fn malformed_frames_are_errors() {
        for (frame, expected) in [
            (&b""[..], ParseError::Pri),
            (b"no pri", ParseError::Pri),
            (b"<>1 - - - - - -", ParseError::Pri),
            (b"<192>1 - - - - - -", ParseError::Pri),
            (b"<1234>1 - - - - - -", ParseError::Pri),
            (
                b"<34>Oct 11 22:14:15 mymachine su: 'su root' failed",
                ParseError::Version,
            ),
            (b"<34>2 - - - - - -", ParseError::Version),
            (b"<34>1 yesterday - - - - -", ParseError::Timestamp),
            (b"<34>1 -", ParseError::Timestamp),
            (b"<34>1 - - - - -", ParseError::Header),
            (b"<34>1 - h\xC3\xA9 - - - -", ParseError::Header),
            (
                b"<34>1 - - - - ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -",
                ParseError::Header,
            ),
            (
                b"<34>1 - - - - - [x a=\"unterminated]",
                ParseError::StructuredData,
            ),
            (b"<34>1 - - - - - [x a=b]", ParseError::StructuredData),
            (b"<34>1 - - - - - [x]extra", ParseError::StructuredData),
            (b"<34>1 - - - - - x", ParseError::StructuredData),
            (b"<34>1 - - - - - -msg", ParseError::StructuredData),
            (b"<34>1 - - - - - [=x]", ParseError::StructuredData),
        ] {
            assert_eq!(
                parse(frame),
                Err(expected),
                "{:?}",
                String::from_utf8_lossy(frame)
            );
        }
        let long_host = format!("<34>1 - {} - - - -", "h".repeat(256));
        assert_eq!(parse(long_host.as_bytes()), Err(ParseError::Header));
    }

    #[test]
    fn events_map_severity_header_fields_and_structured_data() {
        let received = at("2026-10-13T12:00:00Z");
        let frame = b"<165>1 2026-10-13T11:59:59.5Z web-1 nginx 4123 ID47 [nami token=\"secret\"][origin ip=\"10.0.0.1\" ip=\"10.0.0.2\" software=\"x\"] GET / 200";
        let event = parse(frame).unwrap().into_event(received);
        assert_eq!(event.level, "notice");
        assert_eq!(event.dt, at("2026-10-13T11:59:59.500Z"));
        assert_eq!(event.message, "GET / 200");
        assert_eq!(
            Value::Object(event.fields),
            json!({
                "origin": { "ip": ["10.0.0.1", "10.0.0.2"], "software": "x" },
                "facility": 20,
                "hostname": "web-1",
                "app_name": "nginx",
                "procid": "4123",
                "msgid": "ID47"
            })
        );
        for (severity, level) in [
            (0, "emergency"),
            (1, "alert"),
            (2, "critical"),
            (3, "error"),
            (4, "warn"),
            (6, "info"),
            (7, "debug"),
        ] {
            let frame = format!("<{severity}>1 - - - - - -");
            assert_eq!(
                parse(frame.as_bytes()).unwrap().into_event(received).level,
                level
            );
        }
    }

    #[test]
    fn missing_or_out_of_range_timestamps_use_the_receive_time() {
        let received = at("2026-10-13T12:00:00Z");
        assert_eq!(
            parse(b"<14>1 - - - - - - m")
                .unwrap()
                .into_event(received)
                .dt,
            received
        );
        let old = parse(b"<14>1 2003-10-11T22:14:15Z - - - - - m")
            .unwrap()
            .into_event(received);
        assert_eq!(old.dt, received);
        assert_eq!(old.fields["timestamp"], "2003-10-11T22:14:15Z");
    }

    #[test]
    fn invalid_utf8_in_the_message_is_replaced_and_trailing_newlines_trimmed() {
        let m = parse(b"<14>1 - - - - - - caf\xC3\xA9 \xFF\xFE end\r\n").unwrap();
        assert_eq!(m.message, "caf\u{e9} \u{fffd}\u{fffd} end");
    }

    #[test]
    fn arbitrary_bytes_and_every_truncation_never_panic() {
        let valid = b"<165>1 2026-10-13T11:59:59.5Z web-1 nginx 4123 ID47 [nami token=\"abc\"][o a=\"1\\\"\"] msg";
        for len in 0..=valid.len() {
            let _ = parse(&valid[..len]);
        }
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const ALPHABET: &[u8] = b"<>1 -[]=\"\\0123456789abcT:Z.\xff\n";
        for _ in 0..20_000 {
            let len = (next() % 80) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            if let Ok(message) = parse(&bytes) {
                let _ = message.into_event(Utc::now());
            }
        }
    }
}
