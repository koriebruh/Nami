//! Uptime monitoring: performing checks (`checker`), storing results in
//! ClickHouse (`store`), the background worker and its state machine
//! (`worker`), and alert delivery (`alerts`).
pub mod alerts;
pub mod checker;
pub mod store;
pub mod worker;
