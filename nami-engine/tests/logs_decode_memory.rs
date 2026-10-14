// What decoding a hostile body takes in memory. The request limits bound the
// bytes of a body, not what it decodes to: 50 MiB of MessagePack zeros inside
// one array decoded to 1.8 GB. A counting allocator measures the heap that is
// live at once (the figure an unbounded decode would blow up, and one that
// does not depend on the machine the way RSS does). It measures the decoders
// (`what_a_hostile_body_decodes_to_is_bounded...`) and then the whole of what
// a body costs on its way to ClickHouse: decoded, normalized into rows, and
// encoded as the insert body (`what_a_body_costs_once_it_is_stored...`),
// because a body that decodes to little can store as a great deal (a control
// character is one byte in a message and six in the JSON that is sent). This
// binary runs one test, which calls them in turn, so nothing else allocates
// while it measures.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
    time::{Duration, Instant},
};

use axum::body::Bytes;
use nami_engine::{
    logs::{
        budget::MAX_DECODED_BYTES,
        ingest::{
            self, BodyEncoding, BodyFormat, IngestError, LARGE_BODY_BYTES, MAX_DECOMPRESSED_BYTES,
            MAX_ENCODED_BYTES, MAX_EVENTS, gunzip,
        },
    },
    models::source::IngestSource,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(live, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            grow(layout.size());
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            grow(layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        unsafe { System.dealloc(block, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // While a block moves, the old one and the new one can both be live.
        grow(new_size);
        let moved = unsafe { System.realloc(block, layout, new_size) };
        if moved.is_null() {
            LIVE.fetch_sub(new_size, Relaxed);
        } else {
            LIVE.fetch_sub(layout.size(), Relaxed);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What `work` returns, and the most heap that was live at once while it ran,
/// beyond what was live before.
fn peak_extra<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let result = work();
    (result, PEAK.load(Relaxed) - before)
}

const MIB: usize = 1024 * 1024;
const JSON: BodyEncoding = BodyEncoding {
    format: BodyFormat::Json,
    gzip: false,
};
const MSGPACK: BodyEncoding = BodyEncoding {
    format: BodyFormat::MessagePack,
    gzip: false,
};

fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// The most a decode may hold at once: the budget, which is an estimate that
/// is not below what is built, plus room for a container that is growing.
const DECODE_CEILING: usize = MAX_DECODED_BYTES + 32 * MIB;

fn assert_refused_within_the_ceiling(
    what: &str,
    body: Vec<u8>,
    encoding: BodyEncoding,
) -> (usize, Duration) {
    let body = Bytes::from(body);
    let started = Instant::now();
    let (outcome, peak) = peak_extra(|| ingest::decode_events(body, encoding));
    let took = started.elapsed();
    match outcome {
        Err(IngestError::PayloadTooLarge(message)) => {
            assert!(
                message.contains("expands to more than"),
                "{what}: {message}"
            )
        }
        other => panic!("{what}: {:?}", other.map(|events| events.len())),
    }
    eprintln!(
        "{what}: refused after {took:?}, peak {} MiB (ceiling {} MiB)",
        peak / MIB,
        DECODE_CEILING / MIB
    );
    assert!(
        peak <= DECODE_CEILING,
        "{what}: {} MiB live at once, over the {} MiB ceiling",
        peak / MIB,
        DECODE_CEILING / MIB
    );
    (peak, took)
}

#[test]
fn what_a_body_costs_in_memory_is_bounded_from_decoding_to_the_insert_body() {
    what_a_hostile_body_decodes_to_is_bounded_whatever_its_size();
    what_a_body_costs_once_it_is_stored_is_bounded_too();
}

fn what_a_hostile_body_decodes_to_is_bounded_whatever_its_size() {
    // 50 MiB of MessagePack zeros in one array: 51 KB gzipped, 1.8 GB decoded
    // without a budget.
    let count = 50 * MIB;
    let mut msgpack_zeros = vec![0x81, 0xa1, b'a', 0xdd];
    msgpack_zeros.extend((count as u32).to_be_bytes());
    msgpack_zeros.resize(msgpack_zeros.len() + count, 0);
    assert_refused_within_the_ceiling("50 MiB of MessagePack zeros", msgpack_zeros, MSGPACK);

    // 50 MB of JSON `0,`: 0.9 GB without a budget.
    let mut json_zeros = b"{\"a\":[".to_vec();
    json_zeros.extend("0,".repeat(25_000_000).bytes());
    json_zeros.extend(b"0]}");
    assert_refused_within_the_ceiling("25 million JSON zeros", json_zeros, JSON);

    // The dearest bytes of all: `{"b":1},` is 8 bytes and a B-tree node of
    // over 600. The budget is what keeps this one, 8 MB here, from taking
    // 700 MB.
    let mut tiny_objects = b"{\"a\":[".to_vec();
    tiny_objects.extend("{\"b\":1},".repeat(1_000_000).bytes());
    tiny_objects.extend(b"{\"b\":1}]}");
    assert_refused_within_the_ceiling("a million JSON objects", tiny_objects.clone(), JSON);

    // The same shape as MessagePack: [{"b":1} x 1,000,000] inside one event.
    let mut packed_objects = vec![0x81, 0xa1, b'a', 0xdd];
    packed_objects.extend(1_000_000u32.to_be_bytes());
    for _ in 0..1_000_000 {
        packed_objects.extend([0x81, 0xa1, b'b', 0x01]);
    }
    assert_refused_within_the_ceiling("a million MessagePack objects", packed_objects, MSGPACK);

    // Arrays of one: `[0],` is 4 bytes and, in memory, a slot, the array's
    // first allocation (room for four values) and the item.
    let mut singletons = b"{\"a\":[".to_vec();
    singletons.extend("[0],".repeat(2_000_000).bytes());
    singletons.extend(b"[0]]}");
    assert_refused_within_the_ceiling("two million JSON arrays of one", singletons, JSON);
    let mut packed_singletons = vec![0x81, 0xa1, b'a', 0xdd];
    packed_singletons.extend(2_000_000u32.to_be_bytes());
    for _ in 0..2_000_000 {
        packed_singletons.extend([0x91, 0x00]);
    }
    assert_refused_within_the_ceiling(
        "two million MessagePack arrays of one",
        packed_singletons,
        MSGPACK,
    );

    // Short strings: each a slot, a small allocation and room to grow.
    let mut strings = b"{\"a\":[".to_vec();
    strings.extend("\"abcdefgh\",".repeat(4_000_000).bytes());
    strings.extend(b"\"abcdefgh\"]}");
    assert_refused_within_the_ceiling("four million short JSON strings", strings, JSON);

    // One object with a very large number of keys.
    let mut keys = b"{".to_vec();
    for n in 0..2_500_000u32 {
        keys.extend(format!("\"k{n}\":0,").bytes());
    }
    keys.extend(b"\"last\":0}");
    assert_refused_within_the_ceiling("an object of 2.5 million keys", keys, JSON);
    let mut packed_keys = vec![0x81, 0xa1, b'a', 0xdf];
    packed_keys.extend(2_500_000u32.to_be_bytes());
    for n in 0..2_500_000u32 {
        let key = format!("k{n}");
        packed_keys.push(0xa0 | key.len() as u8);
        packed_keys.extend(key.bytes());
        packed_keys.push(0x00);
    }
    assert_refused_within_the_ceiling(
        "a MessagePack object of 2.5 million keys",
        packed_keys,
        MSGPACK,
    );

    // Inflating stops at the cap: 3 MB on the wire that would inflate to 3 GiB
    // is refused with no more than the cap (and a growing buffer) in memory.
    let bomb = gzip(&vec![0u8; MIB]).repeat(3_000);
    let (refused, peak) = peak_extra(|| gunzip(&bomb));
    assert!(matches!(refused, Err(IngestError::PayloadTooLarge(_))));
    eprintln!("a 3 GiB gzip bomb: peak {} MiB", peak / MIB);
    assert!(
        peak <= 2 * MAX_DECOMPRESSED_BYTES + 32 * MIB,
        "{} MiB live at once while inflating",
        peak / MIB
    );

    // A big batch that is entirely legitimate decodes, within the same
    // ceiling, so the ceiling is not simply the price of refusing everything.
    let events: Vec<Value> = (0..MAX_EVENTS)
        .map(|n| {
            let mut fields = Map::new();
            for i in 0..40 {
                fields.insert(
                    format!("field_{i:02}"),
                    json!(format!("value {n} {i} {}", "x".repeat(24))),
                );
            }
            Value::Object(fields)
        })
        .collect();
    let batch = Bytes::from(serde_json::to_vec(&events).unwrap());
    drop(events);
    let body_len = batch.len();
    let (decoded, peak) = peak_extra(|| ingest::decode_events(batch, JSON));
    assert_eq!(decoded.unwrap().len(), MAX_EVENTS);
    eprintln!(
        "10,000 events of 40 fields ({} MiB of JSON): peak {} MiB",
        body_len / MIB,
        peak / MIB
    );
    assert!(peak <= DECODE_CEILING);
}

fn source() -> IngestSource {
    IngestSource {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        retention_days: 30,
        daily_byte_quota: None,
    }
}

/// The most one body may hold, beyond itself, from its values to its insert
/// body ([`ingest::build_batch`]): the values at their most (the decode
/// ceiling) and the rows made from them (capped at [`MAX_ENCODED_BYTES`]) are
/// alive together; the insert body is built only after the values are gone.
const BATCH_CEILING: usize = DECODE_CEILING + MAX_ENCODED_BYTES;

/// A MessagePack string.
fn msgpack_str(out: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        n if n < 32 => out.push(0xa0 | n as u8),
        n if n < 256 => out.extend([0xd9, n as u8]),
        n if n < 65_536 => {
            out.push(0xda);
            out.extend((n as u16).to_be_bytes());
        }
        n => {
            out.push(0xdb);
            out.extend((n as u32).to_be_bytes());
        }
    }
    out.extend(bytes);
}

/// `events` maps of two strings of control characters, as MessagePack: the
/// message is 64 KiB and the other field 43,689 bytes, which as JSON text is
/// just under the 256 KiB cap of `fields`.
fn control_character_events(events: usize, message_len: usize, field_len: usize) -> Vec<u8> {
    let (message, field) = (vec![1u8; message_len], vec![1u8; field_len]);
    let mut out = vec![0xdc];
    out.extend((events as u16).to_be_bytes());
    for _ in 0..events {
        out.push(if field_len > 0 { 0x82 } else { 0x81 });
        msgpack_str(&mut out, b"message");
        msgpack_str(&mut out, &message);
        if field_len > 0 {
            msgpack_str(&mut out, b"a");
            msgpack_str(&mut out, &field);
        }
    }
    out
}

/// Runs the work of `prepare` on `body` and says how much memory it took at
/// most, beyond the body itself.
fn batch(
    what: &str,
    body: Vec<u8>,
    encoding: BodyEncoding,
) -> (Result<ingest::Batch, IngestError>, usize) {
    let body = Bytes::from(body);
    let started = Instant::now();
    let (outcome, peak) =
        peak_extra(|| ingest::build_batch(body, encoding, &source(), chrono::Utc::now()));
    eprintln!(
        "{what}: {} after {:?}, peak {} MiB (ceiling {} MiB)",
        match &outcome {
            Ok(batch) => format!(
                "{} events, {} MiB encoded",
                batch.events,
                batch.body.len() / MIB
            ),
            Err(IngestError::PayloadTooLarge(message)) => format!("refused ({message})"),
            Err(other) => format!("{other:?}"),
        },
        started.elapsed(),
        peak / MIB,
        BATCH_CEILING / MIB
    );
    assert!(
        peak <= BATCH_CEILING,
        "{what}: {} MiB live at once, over the {} MiB ceiling",
        peak / MIB,
        BATCH_CEILING / MIB
    );
    (outcome, peak)
}

fn assert_refused_as_too_large_once_stored(
    what: &str,
    outcome: Result<ingest::Batch, IngestError>,
) {
    match outcome {
        Err(IngestError::PayloadTooLarge(message)) => {
            assert!(message.contains("once stored"), "{what}: {message}")
        }
        other => panic!("{what}: {:?}", other.map(|batch| batch.events)),
    }
}

/// A body that decodes to little can store as a great deal. Each 0x01 in a
/// string is one byte in MessagePack and one in the row, but six in the JSON
/// of the insert (`\u0001`), and the `fields` text, which is JSON already, has
/// its backslashes escaped a second time inside its row. A 60 KB gzipped
/// MessagePack body of 479 such events (52 MB inflated, 50 MiB charged to the
/// decode budget) made rows of 150 MiB and an insert body of 322 MiB, which
/// `String`'s doubling grew to a buffer of 512 MiB and kept while ClickHouse
/// was waited for. Now it is refused as the rows pass their cap, and what
/// slips under it is encoded to the byte, or refused before anything is built.
fn what_a_body_costs_once_it_is_stored_is_bounded_too() {
    // The case: message and field both full of control characters.
    let (outcome, _) = batch(
        "479 events of control characters",
        control_character_events(479, 65_536, 43_689),
        MSGPACK,
    );
    assert_refused_as_too_large_once_stored("rows over the cap", outcome);

    // Rows well under the cap (51 MB stored) that encode to 311 MB: refused by
    // the measuring pass of the encoder, which builds nothing.
    let (outcome, peak) = batch(
        "790 events of a 64 KiB message of control characters",
        control_character_events(790, 65_536, 0),
        MSGPACK,
    );
    assert_refused_as_too_large_once_stored("an insert body over the cap", outcome);
    assert!(
        peak < DECODE_CEILING,
        "nothing like the {} MiB body was built: {} MiB",
        311,
        peak / MIB
    );

    // A message that is not a string is its JSON text, which used to be made
    // whole: here 100,000 strings of 500 control characters, 300 MB of text
    // (more with the doubling of the buffer) for a 64 KiB message.
    let mut spread = vec![0x81];
    msgpack_str(&mut spread, b"message");
    spread.extend([0xdd]);
    spread.extend(100_000u32.to_be_bytes());
    let chunk = vec![1u8; 500];
    for _ in 0..100_000 {
        msgpack_str(&mut spread, &chunk);
    }
    let (outcome, _) = batch(
        "a message that is an array of 100,000 strings",
        spread,
        MSGPACK,
    );
    let one = outcome.expect("one event, a message of 64 KiB");
    assert_eq!(one.events, 1);
    assert!(one.bytes < 70 * 1024, "{}", one.bytes);

    // A large batch that is entirely legitimate goes through, to the byte:
    // 150 events of a 64 KiB message is 9.9 MB, over the size at which an
    // insert body keeps its decode place.
    let events: Vec<Value> = (0..150)
        .map(|n| json!({ "message": format!("{n} {}", "x".repeat(65_000)), "n": n }))
        .collect();
    let (outcome, _) = batch(
        "150 events of 64 KiB of text",
        serde_json::to_vec(&events).unwrap(),
        JSON,
    );
    let large = outcome.unwrap();
    assert_eq!(large.events, 150);
    assert!(large.body.len() > LARGE_BODY_BYTES, "{}", large.body.len());
    assert_eq!(
        large.body.capacity(),
        large.body.len(),
        "the insert body is built to its size, with no doubling left over"
    );
}
