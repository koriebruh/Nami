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
//! 4. per source of the batch, its compiled pipeline
//!    (`logs::pipeline_cache`, keyed on the `updated_at` the token lookup
//!    returned, so an edit applies from the next batch); the frames of a
//!    source whose stored pipeline no longer compiles are dropped and
//!    counted as `store_failed` (fail closed: a redact rule must never stop
//!    applying silently);
//! 5. normalization, the pipeline, row building and splitting the rows into
//!    inserts of at most `max_insert_bytes` of encoded body each, all on one
//!    blocking thread (`spawn_blocking`, [`build_and_chunk`]), with the step
//!    HTTP ingest uses (`store::RowBatch`, the same pipeline and rows under
//!    the same caps; a frame whose row alone is larger is dropped and counted
//!    as `too_large`); then each insert, encoded off the runtime too, followed
//!    by recording its usage. What the pipeline dropped or sampled out is
//!    counted in the usage tables, never as `accepted`.
//!
//! Resource bounds (nothing grows with what a peer sends):
//! - at most [`MAX_TCP_CONNECTIONS`] TCP connections, and at most
//!   [`MAX_TCP_CONNECTIONS_PER_IP`] of one peer address (an IPv6 peer counts
//!   per /64, the block one host usually gets); further ones are closed at
//!   once and counted as `refused_connections`. A connection also ends at its
//!   first frame that does not parse, has no well-formed token (an honest
//!   shipper never sends one) or carries a token the batcher recently found
//!   no source for ([`UnknownTokens`]), and only frames that pass intake
//!   restart its idle timer, so a peer cannot keep a slot with garbage or
//!   with made-up tokens;
//! - a frame is at most 64 KiB ([`MAX_FRAME_BYTES`]); an octet count above
//!   that is refused before anything is allocated, a UDP datagram larger than
//!   that is dropped; a TCP connection buffers at most one frame;
//! - a TCP connection that completes no frame in `idle_timeout` (5 minutes)
//!   is closed, also in the middle of a frame;
//! - frames wait in a queue of at most [`QUEUE_CAPACITY`] frames and
//!   [`QUEUE_MAX_BYTES`] of frame bytes (a frame holds its bytes until its
//!   batch has been inserted or the frame is dropped, so the budget bounds
//!   frames in flight; the rows built from them add about as much again
//!   while the insert runs); beyond that frames are dropped (`overloaded`);
//! - one batch is at most [`BATCH_MAX_ROWS`] rows, and a source's usage and
//!   quota are read at most once per batch.

pub mod framing;
pub mod parse;

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
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
use super::{pipeline::CompiledPipeline, rate_limit::TokenBuckets, store};
use crate::{
    auth::token::hash_token,
    core::clickhouse::ClickHouseError,
    core::config::Config,
    core::state::AppState,
    models::source::{self, IngestSource},
};

pub const DEFAULT_PORT: u16 = 5140;
/// A TCP connection that completes no frame for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Open TCP connections; extra ones are closed at once.
pub const MAX_TCP_CONNECTIONS: usize = 256;
/// Open TCP connections of one peer address; extra ones are closed at once.
pub const MAX_TCP_CONNECTIONS_PER_IP: usize = 16;
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
/// How long a token the batcher found no source for is refused at intake.
pub const UNKNOWN_TOKEN_TTL: Duration = Duration::from_secs(300);
/// Most unknown tokens remembered (their hashes); the oldest go first.
pub const MAX_UNKNOWN_TOKENS: usize = 10_000;

#[derive(Debug, Clone)]
pub struct SyslogConfig {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    pub idle_timeout: Duration,
    pub batch_max_rows: usize,
    pub batch_max_wait: Duration,
    pub max_tcp_connections: usize,
    pub max_tcp_connections_per_ip: usize,
    pub queue_max_bytes: usize,
    pub events_per_second: f64,
    pub events_burst: f64,
    /// The largest insert body the batcher sends (the encoded cap of HTTP
    /// ingest, `ingest::MAX_ENCODED_BYTES`); a batch over it is split.
    pub max_insert_bytes: usize,
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
            max_tcp_connections_per_ip: MAX_TCP_CONNECTIONS_PER_IP,
            queue_max_bytes: QUEUE_MAX_BYTES,
            events_per_second: EVENTS_PER_SECOND,
            events_burst: EVENTS_BURST,
            max_insert_bytes: super::ingest::MAX_ENCODED_BYTES,
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
    /// Dropped because the row alone encodes to more than
    /// `max_insert_bytes`.
    pub too_large: AtomicU64,
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
    pub too_large: u64,
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
            too_large: get(&self.too_large),
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
    let unknown = Arc::new(UnknownTokens::new(UNKNOWN_TOKEN_TTL, MAX_UNKNOWN_TOKENS));
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
        unknown: Arc::clone(&unknown),
    });
    let batcher = Batcher {
        state,
        stats,
        warnings,
        unknown,
        limiter: TokenBuckets::new(config.events_per_second, config.events_burst),
        max_rows: config.batch_max_rows,
        max_wait: config.batch_max_wait,
        max_insert_bytes: config.max_insert_bytes,
    };
    tokio::spawn(batcher.run(rx));
    tokio::spawn(run_tcp(
        tcp,
        Arc::clone(&intake),
        config.idle_timeout,
        config.max_tcp_connections,
        config.max_tcp_connections_per_ip,
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

/// Hashes of well-formed tokens the batcher looked up and found no source
/// for, each refused at intake for `ttl` (a rotated or deleted token, or one
/// made up). Without it such frames pass intake: they would restart a
/// connection's idle timer and keep its slot, and cost a lookup each batch.
/// Bounded: at most `max` hashes, the oldest forgotten first.
#[derive(Debug)]
struct UnknownTokens {
    ttl: Duration,
    max: usize,
    entries: Mutex<UnknownEntries>,
}

#[derive(Debug, Default)]
struct UnknownEntries {
    added: HashMap<String, Instant>,
    /// The same hashes in the order they were added (each once).
    order: std::collections::VecDeque<String>,
}

impl UnknownTokens {
    fn new(ttl: Duration, max: usize) -> UnknownTokens {
        UnknownTokens {
            ttl,
            max,
            entries: Mutex::new(UnknownEntries::default()),
        }
    }

    fn contains(&self, hash: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .added
            .get(hash)
            .is_some_and(|added| added.elapsed() < self.ttl)
    }

    fn insert(&self, hash: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if self.max == 0
            || entries
                .added
                .get(hash)
                .is_some_and(|added| now.duration_since(*added) < self.ttl)
        {
            return;
        }
        // Forget the expired ones (this one included), then the oldest while
        // full.
        while let Some(oldest) = entries.order.front() {
            let expired = entries
                .added
                .get(oldest)
                .is_none_or(|added| now.duration_since(*added) >= self.ttl);
            if !expired && entries.order.len() < self.max {
                break;
            }
            let oldest = entries.order.pop_front().expect("front exists");
            entries.added.remove(&oldest);
        }
        // Insertion times only grow, so the expired hashes are a prefix of
        // `order`: this one, if it was there, is gone now.
        entries.added.insert(hash.to_string(), now);
        entries.order.push_back(hash.to_string());
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .added
            .len()
    }
}

/// The key a peer's connections are counted under: its IPv4 address (also
/// when it arrives IPv4-mapped), or the /64 of its IPv6 address, the block
/// one host or site usually gets, so a peer cannot pass the per-address cap
/// by moving through the addresses of its own block.
fn peer_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(std::net::Ipv6Addr::from(
                u128::from(v6) & !((1u128 << 64) - 1),
            )),
        },
    }
}

/// A parsed, token-shaped frame waiting for the batcher. `Debug` is not
/// derived: the message holds the token (and its own `Debug` hides it).
struct Pending {
    token_hash: String,
    message: SyslogMessage,
    received_at: DateTime<Utc>,
    /// The frame's share of [`QUEUE_MAX_BYTES`], given back once its batch
    /// has been inserted or the frame is dropped.
    _bytes: OwnedSemaphorePermit,
}

/// What became of a frame at intake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intaken {
    /// Queued for the batcher.
    Queued,
    /// Not parsed or queued: no room. Not the sender's fault.
    Overloaded,
    /// Not RFC 5424, no well-formed token, or a token recently found to
    /// belong to no source: a TCP connection ends here.
    Rejected,
}

/// Parses frames from both listeners and queues them for the batcher.
struct Intake {
    tx: mpsc::Sender<Pending>,
    queue_bytes: Arc<Semaphore>,
    stats: Arc<SyslogStats>,
    warnings: Arc<DropWarnings>,
    unknown: Arc<UnknownTokens>,
}

impl Intake {
    fn handle_frame(&self, frame: &[u8], peer: SocketAddr) -> Intaken {
        bump(&self.stats.received, 1);
        // Room first: under a flood, a frame that cannot be queued is not
        // even parsed.
        let permits = u32::try_from(frame.len().max(1)).unwrap_or(u32::MAX);
        let Ok(bytes) = Arc::clone(&self.queue_bytes).try_acquire_many_owned(permits) else {
            self.overloaded(peer);
            return Intaken::Overloaded;
        };
        let message = match parse::parse(frame) {
            Ok(message) => message,
            Err(error) => {
                bump(&self.stats.malformed, 1);
                self.warnings.warn(Some(peer), &error.to_string());
                return Intaken::Rejected;
            }
        };
        // A token of the wrong shape is no source's token: no query for it.
        let Some(token) = message.token().filter(|t| source::is_well_formed_token(t)) else {
            bump(&self.stats.unauthenticated, 1);
            self.warnings
                .warn(Some(peer), "no valid [nami token=\"...\"] structured data");
            return Intaken::Rejected;
        };
        let token_hash = hash_token(token);
        if self.unknown.contains(&token_hash) {
            bump(&self.stats.unauthenticated, 1);
            self.warnings.warn(Some(peer), "unknown source token");
            return Intaken::Rejected;
        }
        let pending = Pending {
            token_hash,
            message,
            received_at: Utc::now(),
            _bytes: bytes,
        };
        if self.tx.try_send(pending).is_err() {
            self.overloaded(peer);
            return Intaken::Overloaded;
        }
        Intaken::Queued
    }

    fn overloaded(&self, peer: SocketAddr) {
        bump(&self.stats.overloaded, 1);
        self.warnings.warn(Some(peer), "ingest queue full");
    }
}

/// How many connections each peer address has open, capped. An entry exists
/// only while its count is above zero.
#[derive(Debug)]
struct PeerConnections {
    max: usize,
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// One open connection of a peer; its count goes down when it is dropped.
struct PeerConnection {
    peers: Arc<PeerConnections>,
    ip: IpAddr,
}

impl PeerConnections {
    fn new(max: usize) -> Arc<PeerConnections> {
        Arc::new(PeerConnections {
            max,
            open: Mutex::new(HashMap::new()),
        })
    }

    fn try_enter(self: &Arc<Self>, ip: IpAddr) -> Option<PeerConnection> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let count = open.entry(ip).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(PeerConnection {
            peers: Arc::clone(self),
            ip,
        })
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        let mut open = self.peers.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = open.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.ip);
            }
        }
    }
}

async fn run_tcp(
    listener: TcpListener,
    intake: Arc<Intake>,
    idle_timeout: Duration,
    max_connections: usize,
    max_per_ip: usize,
) {
    let permits = Arc::new(Semaphore::new(max_connections));
    let peers = PeerConnections::new(max_per_ip);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let place = Arc::clone(&permits)
                    .try_acquire_owned()
                    .ok()
                    .and_then(|permit| Some((permit, peers.try_enter(peer_key(peer.ip()))?)));
                let Some((permit, peer_slot)) = place else {
                    // Over a limit: close at once, nothing is read.
                    bump(&intake.stats.refused_connections, 1);
                    intake.warnings.warn(Some(peer), "too many connections");
                    drop(stream);
                    continue;
                };
                let intake = Arc::clone(&intake);
                tokio::spawn(async move {
                    read_connection(stream, peer, &intake, idle_timeout).await;
                    drop((permit, peer_slot));
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
    // Restarted only by a frame that passes intake: a peer sending garbage or
    // frames nobody can use does not keep its connection alive.
    let mut deadline = tokio::time::Instant::now() + idle_timeout;
    loop {
        match tokio::time::timeout_at(deadline, frames.next_frame()).await {
            // Idle for too long, closed by the peer, or a socket error.
            Err(_) | Ok(Ok(None)) | Ok(Err(FrameError::Io(_))) => return,
            Ok(Ok(Some(frame))) => match intake.handle_frame(&frame, peer) {
                Intaken::Queued => deadline = tokio::time::Instant::now() + idle_timeout,
                Intaken::Overloaded => {}
                // Already counted; nothing legitimate sends these.
                Intaken::Rejected => return,
            },
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
    unknown: Arc<UnknownTokens>,
    limiter: TokenBuckets,
    max_rows: usize,
    max_wait: Duration,
    max_insert_bytes: usize,
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
                // The lookup worked and found nothing: refused at intake
                // from now on, for a while.
                self.unknown.insert(&pending.token_hash);
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
        // Each source's compiled pipeline (one query per cache miss). A
        // source whose stored rules no longer compile has its frames dropped:
        // stored without them, a redacted value could leak.
        let mut pipelines: HashMap<Uuid, Arc<CompiledPipeline>> = HashMap::new();
        let mut failed: HashSet<Uuid> = HashSet::new();
        for (_, source) in &allowed {
            if pipelines.contains_key(&source.id) || failed.contains(&source.id) {
                continue;
            }
            match state
                .logs
                .pipelines
                .for_source(&state.db, source, None)
                .await
            {
                Ok(pipeline) => {
                    pipelines.insert(source.id, pipeline);
                }
                Err(error) => {
                    tracing::error!(%error, source_id = %source.id, "syslog: log pipeline unavailable");
                    failed.insert(source.id);
                }
            }
        }
        if !failed.is_empty() {
            let before = allowed.len();
            allowed.retain(|(_, source)| !failed.contains(&source.id));
            bump(&self.stats.store_failed, (before - allowed.len()) as u64);
            self.warnings
                .warn(None, "a source's pipeline is unavailable");
            if allowed.is_empty() {
                return;
            }
        }
        let count = allowed.len() as u64;
        let max_bytes = self.max_insert_bytes;
        // Normalization, the pipeline, row building and chunking (which
        // encodes every row to measure it) are CPU work: off the async runtime.
        let built =
            tokio::task::spawn_blocking(move || build_and_chunk(allowed, &pipelines, max_bytes))
                .await;
        let Built {
            rows,
            runs,
            oversized,
            missing,
            budget: _budget,
        } = match built {
            Ok(Ok(built)) => built,
            Ok(Err(error)) => {
                bump(&self.stats.store_failed, count);
                tracing::error!(%error, "syslog: could not encode log rows");
                return;
            }
            Err(error) => {
                bump(&self.stats.store_failed, count);
                tracing::error!(%error, "syslog: building rows failed");
                return;
            }
        };
        if missing > 0 {
            bump(&self.stats.store_failed, missing);
            tracing::error!(
                frames = missing,
                "syslog: frames without a pipeline were dropped"
            );
        }
        if oversized > 0 {
            bump(&self.stats.too_large, oversized as u64);
            self.warnings
                .warn(None, "a frame's row is larger than one insert may be");
        }
        // One insert body at a time, each encoded off the async runtime.
        for run in runs {
            let rows = Arc::clone(&rows);
            let events = run.len() as u64;
            let encoded = tokio::task::spawn_blocking(move || {
                store::encode_run(&rows.rows()[run], max_bytes)
            })
            .await;
            let encoded = match encoded {
                Ok(Ok(encoded)) => encoded,
                Ok(Err(error)) => {
                    bump(&self.stats.store_failed, events);
                    tracing::error!(%error, "syslog: could not encode log rows");
                    continue;
                }
                Err(error) => {
                    bump(&self.stats.store_failed, events);
                    tracing::error!(%error, "syslog: encoding log rows failed");
                    continue;
                }
            };
            match store::insert_encoded_logs(&state.clickhouse, encoded.body).await {
                Ok(()) => {
                    bump(&self.stats.accepted, events);
                    for (source_id, (events, bytes)) in encoded.per_source {
                        state
                            .logs
                            .usage
                            .record_accepted(source_id, today, events, bytes);
                    }
                }
                Err(error) => {
                    bump(&self.stats.store_failed, events);
                    tracing::error!(%error, rows = events, "syslog: failed to store log rows");
                }
            }
        }
        // What the pipeline left out does not depend on the inserts, so it is
        // recorded even when an insert failed. HTTP differs on purpose: there
        // a failed insert records nothing, because the client retries the
        // whole request and would be counted twice; dropped syslog frames are
        // never resent.
        for (source_id, (dropped, sampled_out)) in rows.pipeline_counts() {
            state
                .logs
                .usage
                .record_pipeline(*source_id, today, *dropped, *sampled_out);
        }
    }
}

/// What [`build_and_chunk`] makes of a batch's allowed frames.
struct Built {
    rows: Arc<store::RowBatch>,
    /// Consecutive runs of `rows`, each one insert ([`store::chunk_rows`]).
    runs: Vec<std::ops::Range<usize>>,
    /// Rows left out because one alone encodes to more than an insert may.
    oversized: usize,
    /// Frames left out because their source had no pipeline (a bug; counted
    /// as `store_failed`).
    missing: u64,
    /// The frames' shares of [`QUEUE_MAX_BYTES`], held until the inserts end.
    budget: Vec<OwnedSemaphorePermit>,
}

/// The blocking half of a batch (R-P2-24): each frame's event through its
/// source's pipeline into a row ([`store::RowBatch::push`]), then the rows
/// split into inserts of at most `max_bytes` ([`store::chunk_rows`]). Every
/// source in `allowed` has its entry in `pipelines`; a missing one (a bug)
/// drops the frame rather than storing it unfiltered.
fn build_and_chunk(
    allowed: Vec<(Pending, IngestSource)>,
    pipelines: &HashMap<Uuid, Arc<CompiledPipeline>>,
    max_bytes: usize,
) -> Result<Built, ClickHouseError> {
    // No stored cap here: the queue's byte budget already bounds a batch.
    let mut rows = store::RowBatch::with_capacity(allowed.len(), usize::MAX);
    let mut budget = Vec::with_capacity(allowed.len());
    let mut missing = 0;
    for (pending, source) in allowed {
        budget.push(pending._bytes);
        debug_assert!(
            pipelines.contains_key(&source.id),
            "a frame's source has no pipeline"
        );
        let Some(pipeline) = pipelines.get(&source.id) else {
            // Never stored without its rules (fail closed).
            missing += 1;
            continue;
        };
        let event = pending.message.into_event(pending.received_at);
        // Cannot fail: the batch has no stored cap.
        let _ = rows.push(event, &source, pipeline);
    }
    let (runs, oversized) = store::chunk_rows(rows.rows(), max_bytes)?;
    Ok(Built {
        rows: Arc::new(rows),
        runs,
        oversized,
        missing,
        budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_chunked_inside_the_blocking_build() {
        let budget = Arc::new(Semaphore::new(1 << 20));
        let source = IngestSource {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            retention_days: 30,
            daily_byte_quota: None,
            updated_at: Utc::now(),
        };
        let pending = |message: &str| Pending {
            token_hash: String::new(),
            message: parse::parse(format!("<14>1 - host app - - - {message}").as_bytes()).unwrap(),
            received_at: Utc::now(),
            _bytes: Arc::clone(&budget).try_acquire_many_owned(1).unwrap(),
        };
        let pipeline = crate::logs::pipeline::compile(
            &crate::logs::pipeline::parse_rules(
                &serde_json::json!([{"type":"drop","match":{"message_regex":"^noise"}}]),
            )
            .unwrap(),
        )
        .unwrap();
        let pipelines = HashMap::from([(source.id, Arc::new(pipeline))]);
        let allowed = vec![
            (pending("small one"), source.clone()),
            (pending(&"x".repeat(5000)), source.clone()),
            (pending("noise"), source.clone()),
            (pending("small two"), source.clone()),
        ];
        let built = build_and_chunk(allowed, &pipelines, 2000).unwrap();
        assert_eq!(built.rows.len(), 3, "the dropped frame has no row");
        assert_eq!(built.oversized, 1);
        assert_eq!(built.missing, 0);
        let in_runs: usize = built.runs.iter().map(|run| run.len()).sum();
        assert_eq!(in_runs, 2);
        assert_eq!(built.budget.len(), 4, "every frame's share is held");
        assert_eq!(built.rows.pipeline_counts()[&source.id], (1, 0));
    }

    fn frame_without_pipeline() -> (Vec<(Pending, IngestSource)>, Arc<Semaphore>) {
        let budget = Arc::new(Semaphore::new(16));
        let source = IngestSource {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            retention_days: 30,
            daily_byte_quota: None,
            updated_at: Utc::now(),
        };
        let pending = Pending {
            token_hash: String::new(),
            message: parse::parse(b"<14>1 - host app - - - pw=hunter2").unwrap(),
            received_at: Utc::now(),
            _bytes: Arc::clone(&budget).try_acquire_many_owned(1).unwrap(),
        };
        (vec![(pending, source)], budget)
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a frame's source has no pipeline")]
    fn a_frame_without_a_pipeline_is_a_bug_in_debug_builds() {
        let (allowed, _budget) = frame_without_pipeline();
        let _ = build_and_chunk(allowed, &HashMap::new(), usize::MAX);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn a_frame_without_a_pipeline_is_counted_not_stored() {
        let (allowed, _budget) = frame_without_pipeline();
        let built = build_and_chunk(allowed, &HashMap::new(), usize::MAX).unwrap();
        assert_eq!((built.rows.len(), built.missing), (0, 1));
    }

    #[test]
    fn unknown_tokens_expire_and_stay_bounded() {
        let unknown = UnknownTokens::new(Duration::from_millis(50), 3);
        unknown.insert("a");
        assert!(unknown.contains("a"));
        assert!(!unknown.contains("b"));
        for hash in ["b", "c", "d"] {
            unknown.insert(hash);
        }
        assert_eq!(unknown.len(), 3, "never more than the maximum");
        assert!(!unknown.contains("a"), "the oldest went first");
        assert!(unknown.contains("d"));
        unknown.insert("d");
        assert_eq!(unknown.len(), 3, "a hash is kept once");
        std::thread::sleep(Duration::from_millis(60));
        assert!(!unknown.contains("d"), "expired");
        unknown.insert("e");
        assert_eq!(unknown.len(), 1, "expired ones are forgotten");
        assert!(unknown.contains("e"));
    }

    #[test]
    fn ipv6_peers_count_per_64_and_mapped_ipv4_as_ipv4() {
        let key = |text: &str| peer_key(text.parse().unwrap());
        assert_eq!(key("10.0.0.1"), key("10.0.0.1"));
        assert_ne!(key("10.0.0.1"), key("10.0.0.2"));
        assert_eq!(
            key("2001:db8:1:2::1"),
            key("2001:db8:1:2:ffff:ffff:ffff:ffff")
        );
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2::"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:10.0.0.1"), key("10.0.0.1"));

        // One /64 shares one cap.
        let peers = PeerConnections::new(1);
        let _first = peers.try_enter(key("2001:db8::1")).unwrap();
        assert!(peers.try_enter(key("2001:db8::2")).is_none());
        assert!(peers.try_enter(key("2001:db8:0:1::1")).is_some());
    }

    #[test]
    fn peer_connections_are_capped_per_address_and_forgotten_at_zero() {
        let peers = PeerConnections::new(2);
        let (a, b): (IpAddr, IpAddr) = ("10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap());
        let first = peers.try_enter(a).unwrap();
        let second = peers.try_enter(a).unwrap();
        assert!(
            peers.try_enter(a).is_none(),
            "the third of one address is refused"
        );
        let other = peers.try_enter(b).expect("another address is not affected");
        drop(first);
        let third = peers
            .try_enter(a)
            .expect("a closed connection frees its place");
        assert!(peers.try_enter(a).is_none());
        drop((second, third, other));
        assert_eq!(peers.tracked(), 0, "entries are removed at zero");
    }
}
