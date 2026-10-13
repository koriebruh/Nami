//! Uptime monitoring: performing checks (`checker`), storing results in
//! ClickHouse (`store`), the background worker and its state machine
//! (`worker`), heartbeat pings and the overdue sweep (`heartbeat`), and
//! alert delivery (`alerts`).
pub mod alerts;
pub mod checker;
pub mod heartbeat;
pub mod store;
pub mod worker;
