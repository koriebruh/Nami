//! A cap on what decoding one request body may allocate.
//!
//! A decoded body takes far more memory than its bytes. A MessagePack `0` is
//! one byte and becomes a 32-byte `serde_json::Value`; `{"a":1}` is four bytes
//! and becomes a B-tree node of over 600. The limits on the wire (10 MiB) and
//! after gzip (50 MiB) bound the *input*, not what it decodes to: 50 MiB of
//! MessagePack zeros inside one array took 1.8 GB. Both decoders
//! ([`super::msgpack`] and the JSON one in [`super::ingest`]) therefore charge
//! a [`Budget`] for everything they are about to build, before they build it,
//! and stop with [`TooLarge`] once it is spent: the request is a 413, and the
//! work and the memory it cost are bounded by [`MAX_DECODED_BYTES`] however
//! small the body is.
//!
//! The costs are estimates of the heap a value takes in `serde_json`, rounded
//! up where a container grows by doubling (a `Vec` is up to twice its length
//! in capacity), so a budget is a bound on memory and not an exact count. The
//! dearest bytes are those of tiny containers: `{"b":1},` is 8 bytes of JSON
//! and [`MAP_BYTES`] (over 600) of memory, `[0],` is 4 bytes and over 200.
//! Measured against a counting allocator (`tests/logs_decode_memory.rs`), the
//! shapes that cost the most per byte stop with 150 to 190 MiB live at once.

/// What decoding one request body may cost, in the estimated bytes of
/// [`Budget`]. A legitimate batch is well below it: 10,000 events of 40 fields
/// (19 MiB of JSON) are charged 88 MiB and hold 60 MiB. The most that is held
/// at once while decoding is about this much (plus the body itself, up to
/// 50 MiB inflated, and the rows built from it), and
/// `routes::logs_ingest::MAX_DECODES_IN_FLIGHT` bounds how many decodes run at
/// the same time.
pub const MAX_DECODED_BYTES: usize = 192 * 1024 * 1024;

/// A `serde_json::Value` (32 bytes), doubled: it sits in an array or an object
/// that may have room for as many again, and may be copied while it grows.
pub const VALUE_BYTES: usize = 64;
/// What a string takes beyond its characters: the allocation's own overhead
/// and its rounding up to the allocator's granularity.
pub const STRING_BYTES: usize = 32;
/// What an array takes beyond its items: its first allocation, room for four
/// values (4 x 32 bytes and the allocation's overhead).
pub const ARRAY_BYTES: usize = 144;
/// What an object takes whatever it holds: a B-tree node with room for 11
/// entries (11 keys of 24 bytes and 11 values of 32 bytes, rounded up).
pub const MAP_BYTES: usize = 640;
/// What an object's entry takes beyond its value and the characters of its
/// key: the key's slot (24 bytes) and its share of the node's slack.
pub const KEY_BYTES: usize = 40;

/// The body decodes to more than its [`Budget`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("decoded size over the limit")]
pub struct TooLarge;

/// What is left of the estimated bytes a body may decode to.
#[derive(Debug, Clone)]
pub struct Budget {
    limit: usize,
    spent: usize,
}

impl Budget {
    pub fn new(limit: usize) -> Budget {
        Budget { limit, spent: 0 }
    }

    /// The budget of one request body, [`MAX_DECODED_BYTES`].
    pub fn for_request() -> Budget {
        Budget::new(MAX_DECODED_BYTES)
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// How much has been charged so far.
    pub fn spent(&self) -> usize {
        self.spent
    }

    /// Charges `bytes`, or fails (charging nothing) when that would pass the
    /// limit. Call it *before* allocating what it pays for.
    pub fn charge(&mut self, bytes: usize) -> Result<(), TooLarge> {
        match self.spent.checked_add(bytes) {
            Some(total) if total <= self.limit => {
                self.spent = total;
                Ok(())
            }
            _ => Err(TooLarge),
        }
    }

    /// A value that holds a string of `len` bytes.
    pub fn string(&mut self, len: usize) -> Result<(), TooLarge> {
        self.charge(VALUE_BYTES.saturating_add(STRING_BYTES).saturating_add(len))
    }

    /// An array (itself a value) before any of its items.
    pub fn array(&mut self) -> Result<(), TooLarge> {
        self.charge(VALUE_BYTES + ARRAY_BYTES)
    }

    /// An object (itself a value) before any of its entries.
    pub fn object(&mut self) -> Result<(), TooLarge> {
        self.charge(VALUE_BYTES + MAP_BYTES)
    }

    /// The key of an object's entry, of `len` bytes.
    pub fn key(&mut self, len: usize) -> Result<(), TooLarge> {
        self.charge(KEY_BYTES.saturating_add(STRING_BYTES).saturating_add(len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_add_up_and_a_charge_that_does_not_fit_charges_nothing() {
        let mut budget = Budget::new(100);
        assert_eq!(budget.charge(60), Ok(()));
        assert_eq!(budget.charge(41), Err(TooLarge));
        assert_eq!(budget.spent(), 60, "a refused charge costs nothing");
        assert_eq!(budget.charge(40), Ok(()));
        assert_eq!(budget.charge(1), Err(TooLarge));
        assert_eq!((budget.spent(), budget.limit()), (100, 100));
    }

    #[test]
    fn a_charge_too_large_to_add_is_refused_not_wrapped() {
        let mut budget = Budget::new(usize::MAX);
        assert_eq!(budget.charge(usize::MAX - 1), Ok(()));
        assert_eq!(budget.charge(usize::MAX - 1), Err(TooLarge));
        assert_eq!(budget.string(usize::MAX), Err(TooLarge));
    }

    #[test]
    fn what_a_value_costs_follows_what_it_holds() {
        let mut budget = Budget::new(usize::MAX);
        budget.string(10).unwrap();
        assert_eq!(budget.spent(), VALUE_BYTES + STRING_BYTES + 10);
        budget.object().unwrap();
        budget.key(3).unwrap();
        budget.array().unwrap();
        assert_eq!(
            budget.spent(),
            VALUE_BYTES
                + STRING_BYTES
                + 10
                + VALUE_BYTES
                + MAP_BYTES
                + KEY_BYTES
                + STRING_BYTES
                + 3
                + VALUE_BYTES
                + ARRAY_BYTES
        );
    }
}
