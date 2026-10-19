//! Syslog ingestion: RFC 5424 over TCP (`framing`) and UDP (one message per
//! datagram), enabled with `SYSLOG_ENABLED` (off by default). A frame
//! authenticates with `[nami token="<source token>"]`; frames that are
//! malformed, carry no or an unknown token, belong to a source over its daily
//! quota or its rate limit, or find the engine overloaded are dropped and
//! counted ([`SyslogStats`], plus a warning at most every 10 s), never
//! answered. The token is never logged: warnings name a peer address or a
//! source id, and the message types print no token (`parse`).
//!
//! These listeners are a public ingress, like `PUBLIC_PORT`: expose the ports
//! only to the machines that ship logs (a firewall or a private network).
//! There is no TLS and no per-IP limit; the token is the credential, and
//! what a sender can cost is bounded below.
//!
//! The pipeline is the one of HTTP ingest (`routes::logs_ingest`), batched:
//! 1. at intake, on the connection's task: size cap, parse, token shape
//!    check ([`crate::models::source::is_well_formed_token`], so garbage costs
//!    no query), then the frame is queued;
//! 2. the batcher takes up to `batch_max_rows` frames (or waits at most
//!    `batch_max_wait`) and looks all their tokens up with ONE Postgres query
//!    (`find_by_token_hashes`), so a flood never becomes a query per frame
//!    and a rotated or deleted token stops working from the next batch;
//! 3. per source, in order: rate limit (a bucket of its own, since a frame is
//!    one event where an HTTP request is a whole batch), then the daily
//!    quota (read once per source and batch, like HTTP's check before the
//!    body), both counted in the usage tables like HTTP ingest;
//! 4. normalization and row building on a blocking thread (`spawn_blocking`),
//!    one ClickHouse insert, then usage is recorded.
//!
//! Resource bounds (nothing grows with what a peer sends):
//! - at most [`MAX_TCP_CONNECTIONS`] TCP connections; further ones are closed
//!   at once and counted as `refused_connections`;
//! - a frame is at most 64 KiB ([`MAX_FRAME_BYTES`]); an octet count above
//!   that is refused before anything is allocated, a UDP datagram larger than
//!   that is dropped; a TCP connection buffers at most one frame;
//! - a TCP connection that completes no frame in `idle_timeout` (5 minutes)
//!   is closed, also in the middle of a frame;
//! - frames wait in a queue of at most [`QUEUE_CAPACITY`] frames and
//!   [`QUEUE_MAX_BYTES`] of frame bytes (a frame holds its bytes until it is
//!   stored or dropped); beyond that frames are dropped (`overloaded`);
//! - one batch is at most [`BATCH_MAX_ROWS`] rows, and a source's usage and
//!   quota are read at most once per batch.

pub mod framing;
pub mod parse;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use tokio::{
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};
use uuid::Uuid;

use self::{
    framing::{FrameError, FrameReader, MAX_FRAME_BYTES},
    parse::SyslogMessage,
};
use super::{rate_limit::TokenBuckets, store};
use crate::{
    auth::token::hash_token,
    config::Config,
    models::source::{self, IngestSource},
    state::AppState,
};

pub const DEFAULT_PORT: u16 = 5140;
/// A TCP connection that completes no frame for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Open TCP connections; extra ones are closed at once.
pub const MAX_TCP_CONNECTIONS: usize = 256;
/// Frames waiting for the batcher; beyond this they are dropped (counted as
/// `overloaded`) instead of growing memory.
pub const QUEUE_CAPACITY: usize = 10_000;
/// Bytes of frames waiting for the batcher or being stored (the raw frame
/// sizes; what a parsed frame holds is of the same order).
pub const QUEUE_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const BATCH_MAX_ROWS: usize = 1000;
pub const BATCH_MAX_WAIT: Duration = Duration::from_secs(1);
/// Events per second one source may send over syslog, and the burst it may
/// start with. Counted per frame, so well above HTTP's 50 requests a second
/// (a request carries up to 10,000 events).
pub const EVENTS_PER_SECOND: f64 = 2_000.0;
pub const EVENTS_BURST: f64 = 4_000.0;
const WARN_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct SyslogConfig {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    pub idle_timeout: Duration,
    pub batch_max_rows: usize,
    pub batch_max_wait: Duration,
    pub max_tcp_connections: usize,
    pub queue_max_bytes: usize,
    pub events_per_second: f64,
    pub events_burst: f64,
}

impl SyslogConfig {
    /// All interfaces on `SYSLOG_TCP_PORT` / `SYSLOG_UDP_PORT`.
    pub fn from_config(config: &Config) -> SyslogConfig {
        SyslogConfig {
            tcp_addr: SocketAddr::from(([0, 0, 0, 0], config.syslog_tcp_port)),
            udp_addr: SocketAddr::from(([0, 0, 0, 0], config.syslog_udp_port)),
            idle_timeout: IDLE_TIMEOUT,
            batch_max_rows: BATCH_MAX_ROWS,
            batch_max_wait: BATCH_MAX_WAIT,
            max_tcp_connections: MAX_TCP_CONNECTIONS,
            queue_max_bytes: QUEUE_MAX_BYTES,
            events_per_second: EVENTS_PER_SECOND,
            events_burst: EVENTS_BURST,
        }
    }
}

/// Counters since startup (per engine instance).
#[derive(Debug, Default)]
pub struct SyslogStats {
    /// Frames read off the wire.
    pub received: AtomicU64,
    /// Frames stored as rows.
    pub accepted: AtomicU64,
    /// Not RFC 5424, or broken framing (the connection is closed).
    pub malformed: AtomicU64,
    /// No `[nami token=...]`, or a token of no source.
    pub unauthenticated: AtomicU64,
    /// The source is over its daily byte quota.
    pub over_quota: AtomicU64,
    /// The source is over its rate limit.
    pub rate_limited: AtomicU64,
    /// The queue to the batcher was full.
    pub overloaded: AtomicU64,
    /// Lost to a database or ClickHouse error.
    pub store_failed: AtomicU64,
    /// TCP connections closed at once because the limit was reached.
    pub refused_connections: AtomicU64,
}

/// A plain copy of [`SyslogStats`], for comparisons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyslogStatsSnapshot {
    pub received: u64,
    pub accepted: u64,
    pub malformed: u64,
    pub unauthenticated: u64,
    pub over_quota: u64,
    pub rate_limited: u64,
    pub overloaded: u64,
    pub store_failed: u64,
    pub refused_connections: u64,
}

impl SyslogStats {
    pub fn snapshot(&self) -> SyslogStatsSnapshot {
        let get = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SyslogStatsSnapshot {
            received: get(&self.received),
            accepted: get(&self.accepted),
            malformed: get(&self.malformed),
            unauthenticated: get(&self.unauthenticated),
            over_quota: get(&self.over_quota),
            rate_limited: get(&self.rate_limited),
            overloaded: get(&self.overloaded),
            store_failed: get(&self.store_failed),
            refused_connections: get(&self.refused_connections),
        }
    }
}

fn bump(counter: &AtomicU64, by: u64) {
    counter.fetch_add(by, Ordering::Relaxed);
}

/// The bound addresses (useful when binding port 0) and the counters.
#[derive(Debug, Clone)]
pub struct SyslogHandle {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    pub stats: Arc<SyslogStats>,
}

/// Binds both listeners and starts them and the batcher in the background.
pub async fn spawn(state: AppState, config: SyslogConfig) -> std::io::Result<SyslogHandle> {
    let tcp = TcpListener::bind(config.tcp_addr).await?;
    let udp = UdpSocket::bind(config.udp_addr).await?;
    let stats = Arc::new(SyslogStats::default());
    let warnings = Arc::new(DropWarnings::default());
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let handle = SyslogHandle {
        tcp_addr: tcp.local_addr()?,
        udp_addr: udp.local_addr()?,
        stats: Arc::clone(&stats),
    };
    let intake = Arc::new(Intake {
        tx,
        queue_bytes: Arc::new(Semaphore::new(config.queue_max_bytes)),
        stats: Arc::clone(&stats),
        warnings: Arc::clone(&warnings),
    });
    let batcher = Batcher {
        state,
        stats,
        warnings,
        limiter: TokenBuckets::new(config.events_per_second, config.events_burst),
        max_rows: config.batch_max_rows,
        max_wait: config.batch_max_wait,
    };
    tokio::spawn(batcher.run(rx));
    tokio::spawn(run_tcp(
        tcp,
        Arc::clone(&intake),
        config.idle_timeout,
        config.max_tcp_connections,
    ));
    tokio::spawn(run_udp(udp, intake));
    Ok(handle)
}

/// One warning per [`WARN_EVERY`] at most, with how many drops it covers.
/// Never includes frame content: frames carry tokens.
#[derive(Debug, Default)]
struct DropWarnings {
    last: Mutex<Option<Instant>>,
    suppressed: AtomicU64,
}

impl DropWarnings {
    fn warn(&self, peer: Option<SocketAddr>, reason: &str) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| at.elapsed() < WARN_EVERY) {
            bump(&self.suppressed, 1);
            return;
        }
        *last = Some(Instant::now());
        let suppressed = self.suppressed.swap(0, Ordering::Relaxed);
        match peer {
            Some(peer) => tracing::warn!(%peer, reason, suppressed, "syslog frame dropped"),
            None => tracing::warn!(reason, suppressed, "syslog frame dropped"),
        }
    }
}

/// A parsed, token-shaped frame waiting for the batcher. `Debug` is not
/// derived: the message holds the token (and its own `Debug` hides it).
struct Pending {
    token_hash: String,
    message: SyslogMessage,
    received_at: DateTime<Utc>,
    /// The frame's share of [`QUEUE_MAX_BYTES`], given back once it is
    /// stored or dropped.
    _bytes: OwnedSemaphorePermit,
}

/// Parses frames from both listeners and queues them for the batcher.
struct Intake {
    tx: mpsc::Sender<Pending>,
    queue_bytes: Arc<Semaphore>,
    stats: Arc<SyslogStats>,
    warnings: Arc<DropWarnings>,
}

impl Intake {
    fn handle_frame(&self, frame: &[u8], peer: SocketAddr) {
        bump(&self.stats.received, 1);
        // Room first: under a flood, a frame that cannot be queued is not
        // even parsed.
        let permits = u32::try_from(frame.len().max(1)).unwrap_or(u32::MAX);
        let Ok(bytes) = Arc::clone(&self.queue_bytes).try_acquire_many_owned(permits) else {
            self.overloaded(peer);
            return;
        };
        let message = match parse::parse(frame) {
            Ok(message) => message,
            Err(error) => {
                bump(&self.stats.malformed, 1);
                self.warnings.warn(Some(peer), &error.to_string());
                return;
            }
        };
        // A token of the wrong shape is no source's token: no query for it.
        let Some(token) = message.token().filter(|t| source::is_well_formed_token(t)) else {
            bump(&self.stats.unauthenticated, 1);
            self.warnings
                .warn(Some(peer), "no valid [nami token=\"...\"] structured data");
            return;
        };
        let token_hash = hash_token(token);
        let pending = Pending {
            token_hash,
            message,
            received_at: Utc::now(),
            _bytes: bytes,
        };
        if self.tx.try_send(pending).is_err() {
            self.overloaded(peer);
        }
    }

    fn overloaded(&self, peer: SocketAddr) {
        bump(&self.stats.overloaded, 1);
        self.warnings.warn(Some(peer), "ingest queue full");
    }
}

async fn run_tcp(
    listener: TcpListener,
    intake: Arc<Intake>,
    idle_timeout: Duration,
    max_connections: usize,
) {
    let permits = Arc::new(Semaphore::new(max_connections));
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    // Over the limit: close at once, nothing is read.
                    bump(&intake.stats.refused_connections, 1);
                    intake.warnings.warn(Some(peer), "too many connections");
                    drop(stream);
                    continue;
                };
                let intake = Arc::clone(&intake);
                tokio::spawn(async move {
                    read_connection(stream, peer, &intake, idle_timeout).await;
                    drop(permit);
                });
            }
            Err(error) => {
                // e.g. out of file descriptors: back off instead of spinning.
                tracing::warn!(error = %error, "syslog TCP accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn read_connection(
    stream: TcpStream,
    peer: SocketAddr,
    intake: &Intake,
    idle_timeout: Duration,
) {
    let mut frames = FrameReader::new(stream);
    loop {
        match tokio::time::timeout(idle_timeout, frames.next_frame()).await {
            // Idle for too long, closed by the peer, or a socket error.
            Err(_) | Ok(Ok(None)) | Ok(Err(FrameError::Io(_))) => return,
            Ok(Ok(Some(frame))) => intake.handle_frame(&frame, peer),
            Ok(Err(error)) => {
                bump(&intake.stats.malformed, 1);
                intake.warnings.warn(Some(peer), &error.to_string());
                return;
            }
        }
    }
}

async fn run_udp(socket: UdpSocket, intake: Arc<Intake>) {
    // One byte more than a frame may hold: a datagram that fills the buffer
    // is too large (the rest was cut off by the kernel).
    let mut buf = vec![0u8; MAX_FRAME_BYTES + 1];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, peer)) if len > MAX_FRAME_BYTES => {
                bump(&intake.stats.received, 1);
                bump(&intake.stats.malformed, 1);
                intake
                    .warnings
                    .warn(Some(peer), "datagram longer than 64 KiB");
            }
            Ok((len, peer)) => {
                let frame = buf[..len].trim_ascii();
                if !frame.is_empty() {
                    intake.handle_frame(frame, peer);
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "syslog UDP receive failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

struct Batcher {
    state: AppState,
    stats: Arc<SyslogStats>,
    warnings: Arc<DropWarnings>,
    limiter: TokenBuckets,
    max_rows: usize,
    max_wait: Duration,
}

impl Batcher {
    async fn run(self, mut rx: mpsc::Receiver<Pending>) {
        let mut batch = Vec::with_capacity(self.max_rows);
        loop {
            if rx.recv_many(&mut batch, self.max_rows).await == 0 {
                return;
            }
            let deadline = tokio::time::Instant::now() + self.max_wait;
            while batch.len() < self.max_rows {
                let room = self.max_rows - batch.len();
                match tokio::time::timeout_at(deadline, rx.recv_many(&mut batch, room)).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            self.store(std::mem::take(&mut batch)).await;
            batch = Vec::with_capacity(self.max_rows);
        }
    }

    /// Decides for each frame of the batch whether its source may have it
    /// stored (token, rate limit, quota), stores those that may and counts
    /// the rest.
    async fn store(&self, batch: Vec<Pending>) {
        let state = &self.state;
        let mut hashes: Vec<String> = batch.iter().map(|p| p.token_hash.clone()).collect();
        hashes.sort_unstable();
        hashes.dedup();
        let sources: HashMap<String, IngestSource> = match source::find_by_token_hashes(
            &state.db, &hashes,
        )
        .await
        {
            Ok(found) => found.into_iter().collect(),
            Err(error) => {
                bump(&self.stats.store_failed, batch.len() as u64);
                tracing::error!(%error, frames = batch.len(), "syslog: could not look up source tokens");
                return;
            }
        };
        let today = Utc::now().date_naive();
        let mut over_quota: HashMap<Uuid, bool> = HashMap::new();
        let mut unauthenticated = 0u64;
        let mut allowed: Vec<(Pending, IngestSource)> = Vec::with_capacity(batch.len());
        for pending in batch {
            let Some(source) = sources.get(&pending.token_hash) else {
                unauthenticated += 1;
                continue;
            };
            if !self.limiter.try_acquire(source.id) {
                bump(&self.stats.rate_limited, 1);
                state.logs.usage.record_rate_limited(source.id, today);
                self.warnings.warn(None, "a source is over its rate limit");
                continue;
            }
            let blocked = match over_quota.get(&source.id) {
                Some(blocked) => *blocked,
                None => {
                    let blocked = match source.daily_byte_quota {
                        None => false,
                        // A failed usage read lets the frame through: the
                        // quota bounds volume, it is not an access control.
                        Some(quota) => state
                            .logs
                            .usage
                            .bytes_today(&state.db, source.id, today)
                            .await
                            .is_ok_and(|used| used >= quota),
                    };
                    over_quota.insert(source.id, blocked);
                    blocked
                }
            };
            if blocked {
                bump(&self.stats.over_quota, 1);
                state.logs.usage.record_over_quota(source.id, today, 1);
                continue;
            }
            allowed.push((pending, source.clone()));
        }
        if unauthenticated > 0 {
            bump(&self.stats.unauthenticated, unauthenticated);
            self.warnings.warn(None, "unknown source token");
        }
        if allowed.is_empty() {
            return;
        }
        let count = allowed.len() as u64;
        // Normalization and row building are CPU work: off the async runtime.
        let built = tokio::task::spawn_blocking(move || {
            let mut per_source: HashMap<Uuid, (u64, u64)> = HashMap::new();
            let rows: Vec<store::LogRow> = allowed
                .into_iter()
                .map(|(pending, source)| {
                    let event = pending.message.into_event(pending.received_at);
                    let row = store::build_row(event, &source);
                    let counted = per_source.entry(source.id).or_default();
                    counted.0 += 1;
                    counted.1 += row.stored_bytes();
                    row
                })
                .collect();
            (rows, per_source)
        })
        .await;
        let (rows, per_source) = match built {
            Ok(built) => built,
            Err(error) => {
                bump(&self.stats.store_failed, count);
                tracing::error!(%error, "syslog: building rows failed");
                return;
            }
        };
        match store::insert_logs(&state.clickhouse, &rows).await {
            Ok(()) => {
                bump(&self.stats.accepted, rows.len() as u64);
                for (source_id, (events, bytes)) in per_source {
                    state
                        .logs
                        .usage
                        .record_accepted(source_id, today, events, bytes);
                }
            }
            Err(error) => {
                bump(&self.stats.store_failed, rows.len() as u64);
                tracing::error!(%error, rows = rows.len(), "syslog: failed to store log rows");
            }
        }
    }
}
