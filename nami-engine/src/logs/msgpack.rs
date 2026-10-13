//! A small MessagePack decoder for ingest bodies (`application/msgpack`):
//! one map, or an array of maps, into `serde_json` values. Written for
//! untrusted input:
//! - declared lengths are checked against the bytes actually left and
//!   never used to preallocate, so "2^32 elements" in a 6-byte body fails
//!   at once instead of allocating;
//! - at most [`MAX_NESTING`] arrays/maps nest (the same count serde_json
//!   applies to JSON bodies);
//! - map keys must be strings; strings must be UTF-8;
//! - `bin` values become base64 strings, timestamp extensions (type -1)
//!   RFC 3339 strings, other extensions `null`; NaN/infinite floats `null`.

use base64::Engine;
use chrono::{DateTime, SecondsFormat};
use serde_json::{Map, Number, Value};

/// Arrays/maps may nest this deep, counting the outermost one (serde_json's
/// recursion limit for JSON bodies is the same).
pub const MAX_NESTING: usize = 127;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MsgpackError {
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("reserved marker byte 0xc1")]
    ReservedMarker,
    #[error("map keys must be strings")]
    NonStringKey,
    #[error("nesting deeper than {MAX_NESTING} arrays/maps")]
    TooDeep,
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("trailing bytes after the value")]
    TrailingBytes,
    #[error("expected a map or an array of maps")]
    NotAnObject,
    #[error("too many events")]
    TooManyEvents,
}

type Result<T> = std::result::Result<T, MsgpackError>;

/// Decodes exactly one value (no trailing bytes).
pub fn decode_value(input: &[u8]) -> Result<Value> {
    let mut decoder = Decoder { input, pos: 0 };
    let value = decoder.value(0)?;
    decoder.finish()?;
    Ok(value)
}

/// The events of an ingest body: one map, or an array of at most
/// `max_events` maps.
pub fn decode_events(input: &[u8], max_events: usize) -> Result<Vec<Map<String, Value>>> {
    let mut decoder = Decoder { input, pos: 0 };
    let events = match decoder.peek()? {
        marker if is_map(marker) => vec![decoder.object(0)?],
        marker if is_array(marker) => {
            decoder.pos += 1;
            let len = decoder.array_len(marker)?;
            if len > max_events {
                return Err(MsgpackError::TooManyEvents);
            }
            decoder.check_items(len, 1)?;
            let mut events = Vec::new();
            for _ in 0..len {
                events.push(decoder.object(1)?);
            }
            events
        }
        _ => return Err(MsgpackError::NotAnObject),
    };
    decoder.finish()?;
    Ok(events)
}

fn is_map(marker: u8) -> bool {
    matches!(marker, 0x80..=0x8f | 0xde | 0xdf)
}

fn is_array(marker: u8) -> bool {
    matches!(marker, 0x90..=0x9f | 0xdc | 0xdd)
}

fn is_str(marker: u8) -> bool {
    matches!(marker, 0xa0..=0xbf | 0xd9..=0xdb)
}

struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn remaining(&self) -> usize {
        self.input.len() - self.pos
    }

    fn finish(&self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(MsgpackError::TrailingBytes)
        }
    }

    fn peek(&self) -> Result<u8> {
        self.input
            .get(self.pos)
            .copied()
            .ok_or(MsgpackError::UnexpectedEnd)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.remaining() {
            return Err(MsgpackError::UnexpectedEnd);
        }
        let bytes = &self.input[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn be<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self
            .take(N)?
            .try_into()
            .expect("take returns exactly N bytes"))
    }

    fn u16(&mut self) -> Result<usize> {
        Ok(u16::from_be_bytes(self.be()?) as usize)
    }

    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_be_bytes(self.be()?) as usize)
    }

    /// Each item needs at least `min_bytes`: a declared count the input
    /// cannot possibly hold fails before any work.
    fn check_items(&self, count: usize, min_bytes: usize) -> Result<()> {
        if count.saturating_mul(min_bytes) > self.remaining() {
            Err(MsgpackError::UnexpectedEnd)
        } else {
            Ok(())
        }
    }

    fn array_len(&mut self, marker: u8) -> Result<usize> {
        match marker {
            0x90..=0x9f => Ok((marker & 0x0f) as usize),
            0xdc => self.u16(),
            _ => self.u32(),
        }
    }

    fn map_len(&mut self, marker: u8) -> Result<usize> {
        match marker {
            0x80..=0x8f => Ok((marker & 0x0f) as usize),
            0xde => self.u16(),
            _ => self.u32(),
        }
    }

    /// A map at nesting `depth` (the number of containers around it).
    fn object(&mut self, depth: usize) -> Result<Map<String, Value>> {
        let marker = self.byte()?;
        if !is_map(marker) {
            return Err(MsgpackError::NotAnObject);
        }
        let len = self.map_len(marker)?;
        self.map_entries(len, depth)
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        let marker = self.byte()?;
        Ok(match marker {
            0x00..=0x7f => Value::from(marker),
            0x80..=0x8f | 0xde | 0xdf => {
                let len = self.map_len(marker)?;
                Value::Object(self.map_entries(len, depth)?)
            }
            0x90..=0x9f | 0xdc | 0xdd => {
                let len = self.array_len(marker)?;
                self.array_items(len, depth)?
            }
            0xa0..=0xbf => self.str((marker & 0x1f) as usize)?,
            0xc0 => Value::Null,
            0xc1 => return Err(MsgpackError::ReservedMarker),
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xc4 => {
                let len = self.byte()? as usize;
                self.bin(len)?
            }
            0xc5 => {
                let len = self.u16()?;
                self.bin(len)?
            }
            0xc6 => {
                let len = self.u32()?;
                self.bin(len)?
            }
            0xc7 => {
                let len = self.byte()? as usize;
                self.ext(len)?
            }
            0xc8 => {
                let len = self.u16()?;
                self.ext(len)?
            }
            0xc9 => {
                let len = self.u32()?;
                self.ext(len)?
            }
            0xca => float(f32::from_be_bytes(self.be()?) as f64),
            0xcb => float(f64::from_be_bytes(self.be()?)),
            0xcc => Value::from(self.byte()?),
            0xcd => Value::from(u16::from_be_bytes(self.be()?)),
            0xce => Value::from(u32::from_be_bytes(self.be()?)),
            0xcf => Value::from(u64::from_be_bytes(self.be()?)),
            0xd0 => Value::from(i8::from_be_bytes(self.be()?)),
            0xd1 => Value::from(i16::from_be_bytes(self.be()?)),
            0xd2 => Value::from(i32::from_be_bytes(self.be()?)),
            0xd3 => Value::from(i64::from_be_bytes(self.be()?)),
            0xd4 => self.ext(1)?,
            0xd5 => self.ext(2)?,
            0xd6 => self.ext(4)?,
            0xd7 => self.ext(8)?,
            0xd8 => self.ext(16)?,
            0xd9 => {
                let len = self.byte()? as usize;
                self.str(len)?
            }
            0xda => {
                let len = self.u16()?;
                self.str(len)?
            }
            0xdb => {
                let len = self.u32()?;
                self.str(len)?
            }
            0xe0..=0xff => Value::from(marker as i8),
        })
    }

    fn array_items(&mut self, len: usize, depth: usize) -> Result<Value> {
        if depth + 1 > MAX_NESTING {
            return Err(MsgpackError::TooDeep);
        }
        self.check_items(len, 1)?;
        let mut items = Vec::new();
        for _ in 0..len {
            items.push(self.value(depth + 1)?);
        }
        Ok(Value::Array(items))
    }

    fn map_entries(&mut self, len: usize, depth: usize) -> Result<Map<String, Value>> {
        if depth + 1 > MAX_NESTING {
            return Err(MsgpackError::TooDeep);
        }
        self.check_items(len, 2)?;
        let mut map = Map::new();
        for _ in 0..len {
            // Reject a non-string key before decoding it (it may be a deep map).
            let marker = self.peek()?;
            if !is_str(marker) {
                return Err(MsgpackError::NonStringKey);
            }
            let Value::String(key) = self.value(depth + 1)? else {
                return Err(MsgpackError::NonStringKey);
            };
            let value = self.value(depth + 1)?;
            map.insert(key, value);
        }
        Ok(map)
    }

    fn str(&mut self, len: usize) -> Result<Value> {
        let bytes = self.take(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| MsgpackError::InvalidUtf8)?;
        Ok(Value::String(text.to_string()))
    }

    fn bin(&mut self, len: usize) -> Result<Value> {
        let bytes = self.take(len)?;
        Ok(Value::String(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))
    }

    fn ext(&mut self, len: usize) -> Result<Value> {
        let ext_type = self.byte()? as i8;
        let data = self.take(len)?;
        if ext_type != -1 {
            return Ok(Value::Null);
        }
        Ok(timestamp(data).map(Value::String).unwrap_or(Value::Null))
    }
}

fn float(value: f64) -> Value {
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// The MessagePack timestamp extension (32-, 64- and 96-bit forms) as an
/// RFC 3339 string; `None` for an invalid length or value.
fn timestamp(data: &[u8]) -> Option<String> {
    let (seconds, nanos) = match data.len() {
        4 => (u32::from_be_bytes(data.try_into().ok()?) as i64, 0u32),
        8 => {
            let raw = u64::from_be_bytes(data.try_into().ok()?);
            ((raw & 0x3_ffff_ffff) as i64, (raw >> 34) as u32)
        }
        12 => (
            i64::from_be_bytes(data[4..].try_into().ok()?),
            u32::from_be_bytes(data[..4].try_into().ok()?),
        ),
        _ => return None,
    };
    if nanos > 999_999_999 {
        return None;
    }
    let dt = DateTime::from_timestamp(seconds, nanos)?;
    Some(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn encode(value: &Value) -> Vec<u8> {
        rmp_serde::to_vec(value).unwrap()
    }

    #[test]
    fn decodes_what_a_standard_encoder_produces() {
        let value = json!({
            "message": "hello",
            "n": [0, 1, -1, 127, 128, -33, 65535, 4_294_967_296_u64, -9_000_000_000_i64, 1.5],
            "nested": { "ok": true, "none": null, "empty": {} },
            "long": "x".repeat(70_000)
        });
        assert_eq!(decode_value(&encode(&value)).unwrap(), value);
    }

    #[test]
    fn bin_becomes_base64_and_extensions_become_timestamps_or_null() {
        // { "b": bin8 [1,2,3], "t32": ts32(1), "t64": ts64(1 s + 500 ms), "t96": ts96(-1 s), "x": ext(5) }
        let mut input = vec![0x85];
        input.extend([0xa1, b'b', 0xc4, 3, 1, 2, 3]);
        input.extend([0xa3, b't', b'3', b'2', 0xd6, 0xff, 0, 0, 0, 1]);
        let ts64: u64 = (500_000_000u64 << 34) | 1;
        input.extend([0xa3, b't', b'6', b'4', 0xd7, 0xff]);
        input.extend(ts64.to_be_bytes());
        input.extend([0xa3, b't', b'9', b'6', 0xc7, 12, 0xff, 0, 0, 0, 0]);
        input.extend((-1i64).to_be_bytes());
        input.extend([0xa1, b'x', 0xd4, 5, 0]);
        assert_eq!(
            decode_value(&input).unwrap(),
            json!({
                "b": "AQID",
                "t32": "1970-01-01T00:00:01Z",
                "t64": "1970-01-01T00:00:01.500Z",
                "t96": "1969-12-31T23:59:59Z",
                "x": null
            })
        );
    }

    #[test]
    fn invalid_timestamps_and_non_finite_floats_become_null() {
        // ts64 with nanoseconds > 999,999,999, then a 3-byte ts (no such form).
        let bad_nanos: u64 = (1_000_000_000u64 << 34) | 1;
        let mut input = vec![0x93, 0xd7, 0xff];
        input.extend(bad_nanos.to_be_bytes());
        input.extend([0xc7, 3, 0xff, 1, 2, 3]);
        input.push(0xcb);
        input.extend(f64::NAN.to_be_bytes());
        assert_eq!(decode_value(&input).unwrap(), json!([null, null, null]));
    }

    #[test]
    fn events_are_one_map_or_an_array_of_maps() {
        let one = encode(&json!({ "message": "a" }));
        assert_eq!(decode_events(&one, 10).unwrap().len(), 1);
        let many = encode(&json!([{ "message": "a" }, { "message": "b" }]));
        assert_eq!(decode_events(&many, 10).unwrap().len(), 2);
        assert_eq!(decode_events(&many, 1), Err(MsgpackError::TooManyEvents));
        assert_eq!(
            decode_events(&encode(&json!([1])), 10),
            Err(MsgpackError::NotAnObject)
        );
        assert_eq!(
            decode_events(&encode(&json!("text")), 10),
            Err(MsgpackError::NotAnObject)
        );
    }

    #[test]
    fn non_string_keys_are_rejected() {
        // { 1: "a" } and { {}: 1 }
        assert_eq!(
            decode_value(&[0x81, 0x01, 0xa1, b'a']),
            Err(MsgpackError::NonStringKey)
        );
        assert_eq!(
            decode_value(&[0x81, 0x80, 0x01]),
            Err(MsgpackError::NonStringKey)
        );
    }

    #[test]
    fn huge_declared_lengths_fail_without_allocating() {
        for input in [
            vec![0xdd, 0xff, 0xff, 0xff, 0xff],       // array32 of 2^32-1
            vec![0xdf, 0xff, 0xff, 0xff, 0xff, 0xa0], // map32
            vec![0xdb, 0xff, 0xff, 0xff, 0xff, b'a'], // str32
            vec![0xc6, 0xff, 0xff, 0xff, 0xff, 0],    // bin32
            vec![0xc9, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00], // ext32
        ] {
            assert_eq!(
                decode_value(&input),
                Err(MsgpackError::UnexpectedEnd),
                "{input:?}"
            );
        }
        assert_eq!(
            decode_events(&[0xdd, 0x00, 0x00, 0x27, 0x11], 10_000),
            Err(MsgpackError::TooManyEvents)
        );
        assert_eq!(
            decode_events(&[0xdd, 0x00, 0x00, 0x27, 0x10], 10_000),
            Err(MsgpackError::UnexpectedEnd)
        );
    }

    #[test]
    fn nesting_is_limited_like_json() {
        let nested = |depth: usize| {
            let mut input = vec![0x91; depth - 1];
            input.push(0x90);
            input
        };
        assert!(decode_value(&nested(MAX_NESTING)).is_ok());
        assert_eq!(
            decode_value(&nested(MAX_NESTING + 1)),
            Err(MsgpackError::TooDeep)
        );
        let as_json = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(serde_json::from_str::<Value>(&as_json(MAX_NESTING)).is_ok());
        assert!(
            serde_json::from_str::<Value>(&as_json(MAX_NESTING + 1)).is_err(),
            "serde_json agrees"
        );
    }

    #[test]
    fn invalid_utf8_reserved_markers_and_trailing_bytes_are_errors() {
        assert_eq!(
            decode_value(&[0xa2, 0xff, 0xfe]),
            Err(MsgpackError::InvalidUtf8)
        );
        assert_eq!(decode_value(&[0xc1]), Err(MsgpackError::ReservedMarker));
        assert_eq!(
            decode_value(&[0xc0, 0xc0]),
            Err(MsgpackError::TrailingBytes)
        );
        assert_eq!(decode_value(&[]), Err(MsgpackError::UnexpectedEnd));
    }

    #[test]
    fn every_truncation_of_a_valid_body_is_an_error_not_a_panic() {
        let input =
            encode(&json!([{ "message": "hello", "n": [1, 2.5, -70000], "m": { "k": "v" } }]));
        for len in 0..input.len() {
            assert!(
                decode_events(&input[..len], 100).is_err(),
                "prefix of {len} bytes"
            );
        }
        assert!(decode_events(&input, 100).is_ok());
    }

    #[test]
    fn random_bytes_never_panic() {
        // xorshift: deterministic, no extra dependency.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = decode_value(&bytes);
            let _ = decode_events(&bytes, 10_000);
        }
    }
}
