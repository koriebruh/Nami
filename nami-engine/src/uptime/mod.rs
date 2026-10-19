//! Uptime monitoring: performing checks (`checker`, with one module per
//! non-http probe), storing results in ClickHouse (`store`), the background
//! worker and its state machine (`worker`), heartbeat pings and the overdue
//! sweep (`heartbeat`), the heartbeat ping log in ClickHouse (`pings`), TLS for
//! the non-http checks (`tls`), and alert delivery
//! (`alerts`).
pub mod alerts;
pub mod availability;
pub mod checker;
pub mod dns;
pub mod expiry;
pub mod heartbeat;
pub mod import;
pub mod mail;
pub mod ping;
pub mod pings;
pub mod store;
pub mod tls;
pub mod udp;
pub mod worker;
