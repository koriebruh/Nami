// Requires `docker compose up -d postgres mailpit clickhouse`.
//
// The worker as it runs in production (`run_forever`), timed: a slow check
// must not delay anything but its own monitor, the heartbeat sweep keeps its
// own schedule, and a restart of the engine gives a fresh period only to the
// heartbeats whose deadline fell inside the outage.
use crate::common;

use std::time::{Duration, Instant};

use common::{
    add_member, seed_http_monitor, seed_project, seed_user, settled_mail, silent_server,
    state_with_clickhouse, unique_email,
};
use nami_engine::{core::state::AppState, uptime::worker::run_forever};
use serde::Deserialize;
use uuid::Uuid;

/// A project with one member whose address is unique to this test.
async fn project_with_recipient(pool: &sqlx::PgPool) -> (Uuid, String) {
    let project = seed_project(pool).await;
    let email = unique_email("oncall");
    let user = seed_user(pool, &email, "member").await;
    add_member(pool, project, user, "viewer").await;
    (project, email)
}

/// A heartbeat created `created_seconds_ago` seconds ago that never pinged.
async fn seed_heartbeat(
    pool: &sqlx::PgPool,
    project: Uuid,
    name: &str,
    period: i32,
    created_seconds_ago: i32,
) -> Uuid {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         values ($1, $2, $3, $4, 0, now() - make_interval(secs => $5))
         returning id",
    )
    .bind(project)
    .bind(name)
    .bind(token)
    .bind(period)
    .bind(f64::from(created_seconds_ago))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn heartbeat_status(pool: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select current_status from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn monitor_status(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, bool) {
    sqlx::query_as(
        "select current_status, consecutive_failures, last_checked_at is not null
         from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn incident_count(pool: &sqlx::PgPool, monitor: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from incidents where monitor_id = $1")
        .bind(monitor)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Sets a monitor's timeout (the seed helper uses 5 s).
async fn set_timeout(pool: &sqlx::PgPool, monitor: Uuid, seconds: i32) {
    sqlx::query("update monitors set timeout_seconds = $2 where id = $1")
        .bind(monitor)
        .bind(seconds)
        .execute(pool)
        .await
        .unwrap();
}

/// Polls `condition` every 20 ms until it holds or `within` has passed.
/// Returns how long it took, or `None` if it never held.
async fn wait_for<F, Fut>(within: Duration, mut condition: F) -> Option<Duration>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let started = Instant::now();
    while started.elapsed() < within {
        if condition().await {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Port 9 refuses connections: a check against it fails at once.
const REFUSED: &str = "http://127.0.0.1:9/";

/// One pass claims both monitors. The hung one waits out its 5 s timeout; the
/// refused one must go down at once instead of waiting for it.
#[sqlx::test]
async fn a_fast_failing_monitor_goes_down_while_a_slow_check_is_in_flight(pool: sqlx::PgPool) {
    let (hung_port, _silent) = silent_server().await;
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let hung = seed_http_monitor(
        &pool,
        project,
        "Hung API",
        &format!("http://127.0.0.1:{hung_port}/"),
        1,
    )
    .await;
    let refused = seed_http_monitor(&pool, project, "Refused API", REFUSED, 1).await;

    let worker = tokio::spawn(run_forever(state));
    let took = wait_for(Duration::from_secs(4), || async {
        monitor_status(&pool, refused).await.0 == "down"
    })
    .await;
    let hung_checked = monitor_status(&pool, hung).await.2;
    worker.abort();

    let took = took.expect("the refused monitor never went down while the hung check ran");
    assert!(
        took < Duration::from_secs(2),
        "the refused monitor went down after {took:?}"
    );
    assert!(!hung_checked, "the hung check was still in flight");
    assert_eq!(incident_count(&pool, refused).await, 1);
}

/// A pass does not wait for the one before it: a monitor that falls due while
/// an earlier pass is stuck on a 20 s check is checked by the next tick.
#[sqlx::test]
async fn the_next_pass_starts_while_an_earlier_one_is_still_running(pool: sqlx::PgPool) {
    let (hung_port, _silent) = silent_server().await;
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let hung = seed_http_monitor(
        &pool,
        project,
        "Hung API",
        &format!("http://127.0.0.1:{hung_port}/"),
        1,
    )
    .await;
    set_timeout(&pool, hung, 20).await;
    let later = seed_http_monitor(&pool, project, "Refused API", REFUSED, 1).await;
    sqlx::query("update monitors set next_check_at = now() + interval '2 seconds' where id = $1")
        .bind(later)
        .execute(&pool)
        .await
        .unwrap();

    let worker = tokio::spawn(run_forever(state));
    let took = wait_for(Duration::from_secs(9), || async {
        monitor_status(&pool, later).await.0 == "down"
    })
    .await;
    let hung_checked = monitor_status(&pool, hung).await.2;
    worker.abort();

    assert!(
        took.is_some(),
        "the monitor that fell due was not checked while the first pass hung"
    );
    assert!(!hung_checked, "the first pass was still stuck on its check");
}

/// The heartbeat sweep keeps its own schedule: a heartbeat that falls overdue
/// two seconds after the worker started goes down at the next sweep, while a
/// check started by the first tick still hangs.
#[sqlx::test]
async fn the_sweep_keeps_its_schedule_while_a_check_hangs(pool: sqlx::PgPool) {
    let (hung_port, _silent) = silent_server().await;
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let hung = seed_http_monitor(
        &pool,
        project,
        "Hung API",
        &format!("http://127.0.0.1:{hung_port}/"),
        1,
    )
    .await;
    set_timeout(&pool, hung, 20).await;
    // Period 30 s, created 28 s ago: overdue 2 s from now.
    let heartbeat = seed_heartbeat(&pool, project, "Nightly backup", 30, 28).await;

    let worker = tokio::spawn(run_forever(state));
    let took = wait_for(Duration::from_secs(9), || async {
        heartbeat_status(&pool, heartbeat).await == "down"
    })
    .await;
    let hung_checked = monitor_status(&pool, hung).await.2;
    worker.abort();

    assert!(
        took.is_some(),
        "the heartbeat did not go down while the check hung"
    );
    assert!(!hung_checked, "the check was still hanging");
}

#[derive(Deserialize)]
struct Count {
    n: u64,
}

/// Passes overlap, and outcomes can arrive out of order: each is applied once
/// and a stale one changes nothing. The first pass checks the monitor against
/// a server that holds the request for 8 s; meanwhile the monitor is pointed at
/// a refused port and rescheduled, so the next pass checks it again and it goes
/// down. When the first pass's (older) success finally arrives, it is
/// discarded: the monitor stays down, with one incident, one DOWN email and no
/// recovery.
#[sqlx::test]
async fn overlapping_passes_apply_each_outcome_once_and_drop_stale_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let slow = slow_ok_server(Duration::from_secs(8)).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &slow, 1).await;
    set_timeout(&pool, monitor, 20).await;

    let worker = tokio::spawn(run_forever(state.clone()));
    // The first pass has claimed it once it is rescheduled into the future.
    wait_for(Duration::from_secs(3), || async {
        sqlx::query_scalar::<_, bool>("select next_check_at > now() from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap()
    })
    .await
    .expect("the first pass claimed the monitor");
    sqlx::query("update monitors set url = $2, next_check_at = now() where id = $1")
        .bind(monitor)
        .bind(REFUSED)
        .execute(&pool)
        .await
        .unwrap();

    let went_down = wait_for(Duration::from_secs(7), || async {
        monitor_status(&pool, monitor).await.0 == "down"
    })
    .await;
    assert!(
        went_down.is_some(),
        "the second pass did not run while the first was still waiting"
    );
    // Past the moment the first pass's success arrives (8 s after it began).
    let stored = wait_for(Duration::from_secs(8), || async {
        let rows: Vec<Count> = ch
            .query_rows(
                "SELECT count() AS n FROM uptime_checks WHERE monitor_id = {monitor_id:UUID}",
                &[("monitor_id", monitor.to_string())],
            )
            .await
            .unwrap();
        rows[0].n == 2
    })
    .await;
    let mail = settled_mail(&recipient, 1).await;
    worker.abort();

    assert!(stored.is_some(), "both checks are stored");
    assert_eq!(
        monitor_status(&pool, monitor).await,
        ("down".to_string(), 1, true),
        "the stale success changed nothing, and the failure was applied once"
    );
    let open: Vec<(String,)> = sqlx::query_as("select status from incidents where monitor_id = $1")
        .bind(monitor)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(open, vec![("open".to_string(),)]);
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec!["[Nami] Checkout API is DOWN"]
    );
    ch.drop_database().await.unwrap();
}

/// Answers every request with `200` after holding it for `hold`.
async fn slow_ok_server(hold: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).await;
                tokio::time::sleep(hold).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// Pretends the worker last recorded that it was alive `seconds_ago` seconds
/// ago, before this start.
async fn last_alive(pool: &sqlx::PgPool, seconds_ago: i32) {
    sqlx::query(
        "insert into worker_state (id, last_alive_at)
         values (1, now() - make_interval(secs => $1))
         on conflict (id) do update set last_alive_at = excluded.last_alive_at",
    )
    .bind(f64::from(seconds_ago))
    .execute(pool)
    .await
    .unwrap();
}

/// The worker was alive until 20 s ago and is now restarting. A heartbeat that
/// was already overdue before that (due 90 s ago) could have pinged while the
/// worker was up: the restart gives it no fresh period, it goes down at once.
#[sqlx::test]
async fn a_restart_gives_no_fresh_period_to_a_heartbeat_overdue_before_the_outage(
    pool: sqlx::PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let overdue = seed_heartbeat(&pool, project, "Dead job", 30, 120).await;
    last_alive(&pool, 20).await;

    let worker = tokio::spawn(run_forever(state));
    let down = wait_for(Duration::from_secs(4), || async {
        heartbeat_status(&pool, overdue).await == "down"
    })
    .await;
    worker.abort();

    assert!(
        down.is_some(),
        "the restart hid a heartbeat overdue before it"
    );
}

/// The worker was alive until 50 s ago. A heartbeat whose deadline fell inside
/// the outage (due 30 s ago) gets a full period from the restart; one due
/// later keeps its own deadline.
#[sqlx::test]
async fn a_restart_gives_a_fresh_period_only_to_heartbeats_due_during_the_outage(
    pool: sqlx::PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let during = seed_heartbeat(&pool, project, "Due during the outage", 30, 60).await;
    let later = seed_heartbeat(&pool, project, "Due later", 30, 10).await;
    last_alive(&pool, 50).await;

    let worker = tokio::spawn(run_forever(state));
    let recorded = wait_for(Duration::from_secs(4), || async {
        sqlx::query_scalar::<_, bool>(
            "select last_alive_at > now() - interval '3 seconds' from worker_state where id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    })
    .await;
    // One more sweep's worth of time, in case it is still running.
    tokio::time::sleep(Duration::from_millis(500)).await;
    worker.abort();

    assert!(recorded.is_some(), "the worker records that it is alive");
    assert_eq!(heartbeat_status(&pool, during).await, "pending");
    assert_eq!(heartbeat_status(&pool, later).await, "pending");
    let anchored: Vec<(Uuid, bool)> = sqlx::query_as(
        "select id, outage_anchor_at is not null from heartbeats where project_id = $1 order by name",
    )
    .bind(project)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(anchored, vec![(during, true), (later, false)]);
}
