//! Shared infrastructure with no domain logic: configuration, the Postgres
//! pool and migrations, the ClickHouse client, email, encryption, errors,
//! application state, the HTTP router (which mounts every domain's routes),
//! the listeners (`serve`), middleware and the health endpoint.
pub mod clickhouse;
pub mod config;
pub mod crypto;
pub mod db;
pub mod download;
pub mod email;
pub mod error;
pub mod health;
pub mod middleware;
pub mod router;
pub mod serve;
pub mod state;
