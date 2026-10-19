//! Uptime monitoring: performing checks (`checker`, with one module per
//! non-http probe), storing results in ClickHouse (`store`), the background
//! worker and its state machine (`worker`), heartbeat pings and the overdue
//! sweep (`heartbeat`), TLS for the non-http checks (`tls`), and alert delivery
//! (`alerts`).
pub mod alerts;
pub mod checker;
pub mod dns;
pub mod expiry;
pub mod heartbeat;
pub mod ping;
pub mod store;
pub mod tls;
pub mod worker;
