//! Log management: turning ingested events into rows (`normalize`,
//! `pattern`) and storing them in ClickHouse (`store`).
pub mod normalize;
pub mod pattern;
pub mod store;
