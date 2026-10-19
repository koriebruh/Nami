// Syslog ingestion (RFC 5424 over TCP and UDP). Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use common::{
    logs::{seed_source, seed_source_with, source_config, stored_rows},
    seed_project, state_with_clickhouse, test_cipher,
};
use nami_engine::{
    core::clickhouse::ClickHouse,
    core::state::AppState,
    logs::source::model::{self as source, SourceConfig},
    logs::syslog::{self, SyslogConfig, SyslogHandle, SyslogStatsSnapshot},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};
use uuid::Uuid;

async fn start(state: AppState) -> SyslogHandle {
    start_with(state, |_| {}).await
}

async fn start_with(state: AppState, adjust: impl FnOnce(&mut SyslogConfig)) -> SyslogHandle {
    let mut config = SyslogConfig {
        tcp_addr: "127.0.0.1:0".parse().unwrap(),
        udp_addr: "127.0.0.1:0".parse().unwrap(),
        idle_timeout: Duration::from_millis(500),
        batch_max_rows: 1000,
        batch_max_wait: Duration::from_millis(50),
        ..SyslogConfig::from_config(&state.config)
    };
    adjust(&mut config);
    syslog::spawn(state, config).await.unwrap()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn frame(token: &str, message: &str) -> String {
    format!(
        "<165>1 {} web-1 nginx 4123 ID47 [nami token=\"{token}\"][origin ip=\"10.0.0.1\" ip=\"10.0.0.2\"] {message}",
        now()
    )
}

/// Waits (up to 10 s) until the project has `count` rows.
async fn rows_eventually(ch: &ClickHouse, project: Uuid, count: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let rows = stored_rows(ch, project).await;
        if rows.len() >= count || std::time::Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits (up to 10 s) until `check` holds for the counters.
async fn stats_eventually(
    handle: &SyslogHandle,
    check: impl Fn(&SyslogStatsSnapshot) -> bool,
) -> SyslogStatsSnapshot {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let stats = handle.stats.snapshot();
        if check(&stats) || std::time::Instant::now() > deadline {
            return stats;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn send_udp(handle: &SyslogHandle, datagram: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.send_to(datagram, handle.udp_addr).await.unwrap();
}

#[sqlx::test]
async fn tcp_frames_in_both_framings_are_stored_and_mapped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "edge").await;

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let counted = frame(&token, "octet counted");
    let wire = format!(
        "{}\n{}\r\n{} {}{} {}\n",
        frame(&token, "newline one"),
        frame(&token, "newline two"),
        counted.len(),
        counted,
        counted.len(),
        counted
    );
    tcp.write_all(wire.as_bytes()).await.unwrap();

    let rows = rows_eventually(&ch, project, 4).await;
    assert_eq!(rows.len(), 4, "{rows:?}");
    let row = rows.iter().find(|r| r["message"] == "newline one").unwrap();
    assert_eq!(row["level"], "notice", "severity 5");
    assert_eq!(row["source_id"], source_id.to_string());
    let fields: Value = serde_json::from_str(row["fields"].as_str().unwrap()).unwrap();
    assert_eq!(
        fields,
        json!({
            "origin": { "ip": ["10.0.0.1", "10.0.0.2"] },
            "facility": 20,
            "hostname": "web-1",
            "app_name": "nginx",
            "procid": "4123",
            "msgid": "ID47"
        })
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r["message"] == "octet counted")
            .count(),
        2
    );
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains(&token), "the token is never stored");
    assert_eq!(
        logs.usage
            .pending(source_id, Utc::now().date_naive())
            .events,
        4,
        "syslog events are counted"
    );
    assert_eq!(handle.stats.snapshot().accepted, 4);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn udp_datagrams_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    let datagram = format!(
        "<11>1 {} host app - - [nami token=\"{token}\"] \u{feff}disk full\n",
        now()
    );
    send_udp(&handle, datagram.as_bytes()).await;
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0]["message"].clone(), rows[0]["level"].clone()),
        (json!("disk full"), json!("error"))
    );
    ch.drop_database().await.unwrap();
}

/// Waits (up to 5 s) for the server to close `tcp`.
async fn assert_closed_by_server(tcp: &mut TcpStream) {
    let mut buf = [0u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
        .await
        .expect("the server closes the connection");
    assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
}

/// Security: frames without a live source token are dropped and counted.
#[sqlx::test]
async fn frames_without_a_valid_token_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (source_id, old_token) = seed_source(&pool, project, "edge").await;
    let new_token = source::rotate_token(&pool, &test_cipher(), project, source_id)
        .await
        .unwrap()
        .unwrap()
        .token;

    // Each frame that fails intake (no token, wrong element, a token of the
    // wrong shape) ends its connection.
    for wire in [
        format!("<14>1 {} h a - - - no structured data\n", now()),
        format!(
            "<14>1 {} h a - - [other token=\"{new_token}\"] wrong element\n",
            now()
        ),
        format!("{}\n", frame("not-a-token", "unknown token")),
    ] {
        let mut rejected = TcpStream::connect(handle.tcp_addr).await.unwrap();
        rejected.write_all(wire.as_bytes()).await.unwrap();
        assert_closed_by_server(&mut rejected).await;
    }
    // A well-formed token that belongs to no source passes intake and is
    // dropped by the batcher; the connection stays.
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&old_token, "rotated token")).as_bytes())
        .await
        .unwrap();
    let stats = stats_eventually(&handle, |s| s.unauthenticated == 4).await;
    assert_eq!(
        (stats.received, stats.unauthenticated, stats.accepted),
        (4, 4, 0)
    );

    tcp.write_all(format!("{}\n", frame(&new_token, "valid")).as_bytes())
        .await
        .unwrap();
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "valid");
    ch.drop_database().await.unwrap();
}

/// Security: malformed input of every kind never stops the listeners.
#[sqlx::test]
async fn malformed_input_never_stops_the_listeners(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    // Broken framing (3 malformed): the server closes the connection.
    for wire in [
        b"99999999999999999999 <14>1 - - - - - -".to_vec(),
        b"70000 <14>1".to_vec(),
        vec![b'<'; 70 * 1024],
    ] {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(&wire).await.unwrap();
        let mut buf = [0u8; 16];
        let closed = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
            .await
            .unwrap();
        assert!(
            matches!(closed, Ok(0) | Err(_)),
            "the server closes the connection"
        );
    }
    // A truncated octet-counted frame (1 malformed), a connection whose first
    // frame is garbage (1 malformed; the server closes it, the other lines
    // are never read), and 200 random datagrams, each malformed unless blank.
    let mut state_bits: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state_bits ^= state_bits << 13;
        state_bits ^= state_bits >> 7;
        state_bits ^= state_bits << 17;
        state_bits
    };
    for wire in [
        b"100 <14>1 - - - - - - short".to_vec(),
        b"\x00\xff\xfe garbage\n<<<>>>\n[[[\n".to_vec(),
    ] {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(&wire).await.unwrap();
        tcp.shutdown().await.unwrap();
    }
    let mut random_frames = 0u64;
    for _ in 0..200 {
        let len = (next() % 512) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        // Blank datagrams are ignored, not counted.
        if !bytes.trim_ascii().is_empty() {
            random_frames += 1;
        }
        send_udp(&handle, &bytes).await;
    }
    send_udp(&handle, b"").await;
    // A valid frame whose message is not UTF-8 is stored with replacement characters.
    let mut invalid_utf8 =
        format!("<14>1 {} h a - - [nami token=\"{token}\"] bad ", now()).into_bytes();
    invalid_utf8.extend(b"\xff\xfe bytes");
    send_udp(&handle, &invalid_utf8).await;

    let malformed = 3 + 1 + 1 + random_frames;
    let stats = stats_eventually(&handle, |s| {
        s.malformed >= malformed && s.received > random_frames
    })
    .await;
    assert_eq!(stats.malformed, malformed, "{stats:?}");
    assert_eq!(stats.unauthenticated, 0, "{stats:?}");

    // Both listeners still serve.
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "after the storm tcp")).as_bytes())
        .await
        .unwrap();
    send_udp(&handle, frame(&token, "after the storm udp").as_bytes()).await;
    let rows = rows_eventually(&ch, project, 3).await;
    let mut messages: Vec<&str> = rows
        .iter()
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    messages.sort();
    assert_eq!(
        messages,
        [
            "after the storm tcp",
            "after the storm udp",
            "bad \u{fffd}\u{fffd} bytes"
        ]
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn idle_tcp_connections_are_closed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
        .await
        .expect("closed before 5 s");
    assert!(matches!(read, Ok(0)), "{read:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "closed after the idle timeout, not at once"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn frames_of_a_source_over_its_quota_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let quota = SourceConfig {
        daily_byte_quota: Some(1024 * 1024),
        ..source_config("capped")
    };
    let (capped, capped_token) = seed_source_with(&pool, project, quota).await;
    let (_, free_token) = seed_source(&pool, project, "free").await;
    sqlx::query("insert into source_usage_daily (source_id, day, bytes) values ($1, $2, 2097152)")
        .bind(capped)
        .bind(Utc::now().date_naive())
        .execute(&pool)
        .await
        .unwrap();

    send_udp(&handle, frame(&capped_token, "over quota").as_bytes()).await;
    send_udp(&handle, frame(&free_token, "still flowing").as_bytes()).await;
    let stats = stats_eventually(&handle, |s| s.over_quota == 1 && s.accepted == 1).await;
    assert_eq!((stats.over_quota, stats.accepted), (1, 1));
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "still flowing");

    logs.usage.flush(&pool).await.unwrap();
    let rejected: i64 = sqlx::query_scalar(
        "select requests_over_quota from source_usage_daily where source_id = $1",
    )
    .bind(capped)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rejected, 1);
    ch.drop_database().await.unwrap();
}

/// Security: connections over the limit are closed at once; the ones within it
/// keep working, and a closed one frees its place.
#[sqlx::test]
async fn tcp_connections_over_the_limit_are_refused(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| {
        c.max_tcp_connections = 2;
        c.idle_timeout = Duration::from_secs(60);
    })
    .await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    let mut first = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let second = TcpStream::connect(handle.tcp_addr).await.unwrap();
    // Round-trip a frame on each so both are known to hold their place.
    first
        .write_all(format!("{}\n", frame(&token, "one")).as_bytes())
        .await
        .unwrap();
    rows_eventually(&ch, project, 1).await;
    let mut third = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), third.read(&mut buf))
        .await
        .expect("refused promptly");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert_eq!(handle.stats.snapshot().refused_connections, 1);

    drop(second);
    // The place of the closed connection is free again (retry until the
    // server noticed the close).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut again = TcpStream::connect(handle.tcp_addr).await.unwrap();
        again
            .write_all(format!("{}\n", frame(&token, "after")).as_bytes())
            .await
            .unwrap();
        if rows_eventually_within(&ch, project, 2, Duration::from_millis(500))
            .await
            .len()
            >= 2
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the freed place was never reused"
        );
    }
    ch.drop_database().await.unwrap();
}

async fn rows_eventually_within(
    ch: &ClickHouse,
    project: Uuid,
    count: usize,
    wait: Duration,
) -> Vec<Value> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let rows = stored_rows(ch, project).await;
        if rows.len() >= count || std::time::Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Security: a source over its rate limit loses frames (counted, usage
/// recorded); the other sources are not affected.
#[sqlx::test]
async fn frames_of_a_source_over_its_rate_limit_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let handle = start_with(state, |c| {
        c.events_per_second = 0.0001;
        c.events_burst = 2.0;
    })
    .await;
    let project = seed_project(&pool).await;
    let (noisy, noisy_token) = seed_source(&pool, project, "noisy").await;
    let (_, quiet_token) = seed_source(&pool, project, "quiet").await;

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let mut wire = String::new();
    for i in 0..5 {
        wire.push_str(&format!("{}\n", frame(&noisy_token, &format!("noisy {i}"))));
    }
    wire.push_str(&format!("{}\n", frame(&quiet_token, "quiet")));
    tcp.write_all(wire.as_bytes()).await.unwrap();

    let stats = stats_eventually(&handle, |s| s.rate_limited == 3 && s.accepted == 3).await;
    assert_eq!((stats.rate_limited, stats.accepted), (3, 3), "{stats:?}");
    assert_eq!(rows_eventually(&ch, project, 3).await.len(), 3);
    assert_eq!(
        logs.usage
            .pending(noisy, Utc::now().date_naive())
            .requests_rate_limited,
        3
    );
    ch.drop_database().await.unwrap();
}

/// Security: the bytes waiting in the queue are bounded; frames beyond are
/// dropped and counted, not buffered.
#[sqlx::test]
async fn frames_beyond_the_queue_budget_are_dropped_and_counted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| c.queue_max_bytes = 100).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;
    // Longer than the whole budget: can never be queued.
    send_udp(&handle, frame(&token, "too big for the queue").as_bytes()).await;
    let stats = stats_eventually(&handle, |s| s.overloaded == 1).await;
    assert_eq!(
        (stats.received, stats.overloaded, stats.accepted),
        (1, 1, 0)
    );
    ch.drop_database().await.unwrap();
}

/// A syslog event is one row with the source's project, even for several
/// sources and projects in one batch; unknown tokens in the same batch do not
/// stop the others.
#[sqlx::test]
async fn one_batch_serves_several_sources_and_drops_unknown_tokens(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| c.batch_max_wait = Duration::from_millis(300)).await;
    let project_a = seed_project(&pool).await;
    let project_b = seed_project(&pool).await;
    let (_, token_a) = seed_source(&pool, project_a, "a").await;
    let (_, token_b) = seed_source(&pool, project_b, "b").await;
    let unknown = "A".repeat(43);

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let wire = format!(
        "{}\n{}\n{}\n",
        frame(&token_a, "for a"),
        frame(&unknown, "nobody"),
        frame(&token_b, "for b")
    );
    tcp.write_all(wire.as_bytes()).await.unwrap();
    assert_eq!(
        rows_eventually(&ch, project_a, 1).await[0]["message"],
        "for a"
    );
    assert_eq!(
        rows_eventually(&ch, project_b, 1).await[0]["message"],
        "for b"
    );
    assert_eq!(handle.stats.snapshot().unauthenticated, 1);
    ch.drop_database().await.unwrap();
}

/// Security: a peer cannot keep a slot with garbage: a connection ends at its
/// first frame that does not parse or has no usable token, so with one slot
/// the next peer still gets in.
#[sqlx::test]
async fn a_garbage_peer_loses_its_slot(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| {
        c.max_tcp_connections = 1;
        c.idle_timeout = Duration::from_secs(60);
    })
    .await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    for garbage in ["not syslog at all\n", "<14>1 - h a - - - no token\n"] {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(garbage.as_bytes()).await.unwrap();
        assert_closed_by_server(&mut tcp).await;
    }
    let stats = handle.stats.snapshot();
    assert_eq!(
        (stats.malformed, stats.unauthenticated),
        (1, 1),
        "{stats:?}"
    );

    // The single slot is free again (retry until the server noticed).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        tcp.write_all(format!("{}\n", frame(&token, "served")).as_bytes())
            .await
            .unwrap();
        if rows_eventually_within(&ch, project, 1, Duration::from_millis(500))
            .await
            .len()
            == 1
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the slot was never freed"
        );
    }
    ch.drop_database().await.unwrap();
}

/// Security: one peer address cannot take more than its share of connections.
/// (Another address still connecting is covered by the unit test of the
/// per-address map: the tests only reach the server from 127.0.0.1.)
#[sqlx::test]
async fn one_peer_cannot_open_more_than_its_share_of_connections(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| {
        c.max_tcp_connections_per_ip = 3;
        c.idle_timeout = Duration::from_secs(60);
    })
    .await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    let mut held = Vec::new();
    for _ in 0..3 {
        let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
        // A frame that passes intake proves the place is held.
        tcp.write_all(format!("{}\n", frame(&token, "held")).as_bytes())
            .await
            .unwrap();
        held.push(tcp);
    }
    rows_eventually(&ch, project, 3).await;
    let mut fourth = TcpStream::connect(handle.tcp_addr).await.unwrap();
    assert_closed_by_server(&mut fourth).await;
    assert_eq!(handle.stats.snapshot().refused_connections, 1);
    assert!(
        held[0].write_all(b"\n").await.is_ok(),
        "the held ones are untouched"
    );
    ch.drop_database().await.unwrap();
}

/// Security: bytes trickling in the middle of a frame do not keep a
/// connection open past the idle timeout.
#[sqlx::test]
async fn a_byte_trickle_is_closed_at_the_idle_timeout(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| c.idle_timeout = Duration::from_millis(300)).await;
    let (mut reader, mut writer) = TcpStream::connect(handle.tcp_addr)
        .await
        .unwrap()
        .into_split();
    let started = std::time::Instant::now();
    let trickle = tokio::spawn(async move {
        // Never a complete frame (no newline), a byte every 50 ms.
        let mut sent = b"<14>1 - h a - - - ".iter().cycle();
        while writer.write_all(&[*sent.next().unwrap()]).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buf))
        .await
        .expect("closed despite the trickle");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "not before the timeout"
    );
    trickle.abort();
    ch.drop_database().await.unwrap();
}

/// Security: no path logs a token, whether the frame is accepted, dropped for
/// its token, or malformed.
#[sqlx::test]
async fn tokens_never_reach_the_log(pool: sqlx::PgPool) {
    let log = common::LogCapture::default();
    let _logging = log.install_trace();
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start(state).await;
    let project = seed_project(&pool).await;
    let (source_id, old_token) = seed_source(&pool, project, "edge").await;
    let new_token = source::rotate_token(&pool, &test_cipher(), project, source_id)
        .await
        .unwrap()
        .unwrap()
        .token;
    let unknown = "B".repeat(43);

    send_udp(&handle, frame(&new_token, "accepted").as_bytes()).await;
    send_udp(&handle, frame(&old_token, "rotated").as_bytes()).await;
    send_udp(&handle, frame(&unknown, "unknown").as_bytes()).await;
    // Malformed (version 2) but carrying a token.
    send_udp(
        &handle,
        format!("<14>2 {} h a - - [nami token=\"{new_token}\"] bad", now()).as_bytes(),
    )
    .await;
    // Token of the wrong shape.
    send_udp(&handle, frame("short-token", "bad token").as_bytes()).await;
    stats_eventually(&handle, |s| {
        s.accepted == 1 && s.unauthenticated == 3 && s.malformed == 1
    })
    .await;
    rows_eventually(&ch, project, 1).await;

    let text = log.lines().join("\n");
    assert!(
        text.contains("syslog frame dropped"),
        "drops are logged: {text}"
    );
    for token in [&new_token, &old_token, &unknown] {
        assert!(!text.contains(token.as_str()), "a token reached the log");
    }
    ch.drop_database().await.unwrap();
}

/// Security: a well-formed token the batcher found no source for is refused
/// at intake for a while, so a connection sending made-up (or rotated)
/// tokens loses its slot instead of keeping it alive; UDP frames with it are
/// dropped at intake too. A valid token is not affected.
#[sqlx::test]
async fn a_token_found_to_be_unknown_is_refused_at_intake(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| c.idle_timeout = Duration::from_secs(60)).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;
    let unknown = "B".repeat(43);

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&unknown, "first")).as_bytes())
        .await
        .unwrap();
    // The first one passes intake and is dropped by the batcher.
    let stats = stats_eventually(&handle, |s| s.unauthenticated == 1).await;
    assert_eq!(stats.unauthenticated, 1);
    // From now on it ends the connection at intake.
    tcp.write_all(format!("{}\n", frame(&unknown, "second")).as_bytes())
        .await
        .unwrap();
    assert_closed_by_server(&mut tcp).await;
    send_udp(&handle, frame(&unknown, "over udp").as_bytes()).await;
    let stats = stats_eventually(&handle, |s| s.unauthenticated == 3).await;
    assert_eq!((stats.received, stats.unauthenticated), (3, 3), "{stats:?}");

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "valid")).as_bytes())
        .await
        .unwrap();
    assert_eq!(
        rows_eventually(&ch, project, 1).await[0]["message"],
        "valid"
    );
    ch.drop_database().await.unwrap();
}

/// Syslog builds rows with the step HTTP ingest uses and respects the
/// encoded cap: a batch over `max_insert_bytes` is stored in several
/// inserts, and a frame whose row alone is larger is dropped and counted.
#[sqlx::test]
async fn syslog_respects_the_encoded_insert_cap(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let handle = start_with(state, |c| {
        c.batch_max_wait = Duration::from_millis(300);
        c.max_insert_bytes = 3000;
    })
    .await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "edge").await;

    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    let mut wire = String::new();
    for n in 0..6 {
        wire.push_str(&format!(
            "{}\n",
            frame(&token, &format!("small {n} {}", "x".repeat(300)))
        ));
    }
    // Control characters: 4,000 bytes stored, about 24,000 encoded.
    wire.push_str(&format!("{}\n", frame(&token, &"\u{1}".repeat(4000))));
    tcp.write_all(wire.as_bytes()).await.unwrap();
    let stats = stats_eventually(&handle, |s| s.accepted == 6 && s.too_large == 1).await;
    assert_eq!(
        (stats.accepted, stats.too_large, stats.store_failed),
        (6, 1, 0),
        "{stats:?}"
    );
    let rows = rows_eventually(&ch, project, 6).await;
    assert_eq!(rows.len(), 6);
    assert!(
        rows.iter()
            .all(|row| row["message"].as_str().unwrap().starts_with("small"))
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn syslog_frames_go_through_the_pipeline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "syslog").await;
    source::set_pipeline(
        &pool,
        project,
        source_id,
        &json!([
            {"type":"drop","match":{"message_regex":"^noise"}},
            {"type":"redact","regex":"pw=\\S+"}
        ]),
    )
    .await
    .unwrap();
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    for message in ["noise 1", "login pw=hunter2 ok"] {
        tcp.write_all(format!("{}\n", frame(&token, message)).as_bytes())
            .await
            .unwrap();
    }
    rows_eventually(&ch, project, 1).await;
    // Both frames were most likely in one batch; give a straggler time anyway.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = stored_rows(&ch, project).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["message"], "login [REDACTED] ok");
    assert!(!rows[0]["pattern"].as_str().unwrap().contains("hunter2"));
    let pending = logs.usage.pending(source_id, Utc::now().date_naive());
    assert_eq!((pending.events, pending.events_dropped), (1, 1));
    assert_eq!(handle.stats.snapshot().accepted, 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn syslog_frames_of_a_source_whose_pipeline_fails_are_dropped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "syslog").await;
    let (_, other_token) = seed_source(&pool, project, "other").await;
    sqlx::query(
        "update sources set pipeline = '[{\"type\":\"redact\",\"regex\":\"(\"}]'::jsonb,
                updated_at = clock_timestamp() where id = $1",
    )
    .bind(source_id)
    .execute(&pool)
    .await
    .unwrap();
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "pw=hunter2")).as_bytes())
        .await
        .unwrap();
    let stats = stats_eventually(&handle, |s| s.store_failed >= 1).await;
    assert_eq!(stats.store_failed, 1);
    let mut other = TcpStream::connect(handle.tcp_addr).await.unwrap();
    other
        .write_all(format!("{}\n", frame(&other_token, "fine")).as_bytes())
        .await
        .unwrap();
    let rows = rows_eventually(&ch, project, 1).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["message"], "fine", "other sources keep ingesting");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_pipeline_edit_applies_to_the_next_syslog_batch(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "syslog").await;
    let handle = start(state).await;
    let mut tcp = TcpStream::connect(handle.tcp_addr).await.unwrap();
    // Cached: no rules.
    tcp.write_all(format!("{}\n", frame(&token, "secret-1")).as_bytes())
        .await
        .unwrap();
    rows_eventually(&ch, project, 1).await;
    source::set_pipeline(
        &pool,
        project,
        source_id,
        &json!([{"type":"redact","regex":"secret-\\d"}]),
    )
    .await
    .unwrap();
    tcp.write_all(format!("{}\n", frame(&token, "secret-2")).as_bytes())
        .await
        .unwrap();
    let rows = rows_eventually(&ch, project, 2).await;
    let mut messages: Vec<&str> = rows
        .iter()
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    messages.sort();
    assert_eq!(messages, ["[REDACTED]", "secret-1"]);
    ch.drop_database().await.unwrap();
}
