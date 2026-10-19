// Requires `docker compose up -d postgres mailpit clickhouse`.
use crate::common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use common::{
    LogCapture, add_member, at, mail_for, make_due, seed_http_monitor, seed_project, seed_user,
    settled_mail, silent_server, state_with_clickhouse, stored_mail_for, unique_email,
};
use nami_engine::{
    core::clickhouse::ClickHouse,
    core::email::Mailer,
    core::state::AppState,
    uptime::monitor::model::Monitor,
    uptime::{
        alert_throttle::ALERT_FLAP_WINDOW,
        alerts::{Transition, TransitionKind, notify_transition, send_deferred},
        checker::{CheckOutcome, CheckStatus},
        store::{UptimeCheckRow, insert_checks},
        worker::{
            MAX_IN_FLIGHT, Notifications, apply_outcome, claim_due_monitors, run_due_checks,
            run_forever, run_pass,
        },
    },
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

async fn server_with_status(status: u16) -> MockServer {
    let server = MockServer::start().await;
    respond_with_status(&server, status).await;
    server
}

async fn respond_with_status(server: &MockServer, status: u16) {
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

/// A project with one member whose address is unique to this test.
async fn project_with_recipient(pool: &sqlx::PgPool) -> (Uuid, String) {
    let project = seed_project(pool).await;
    let email = unique_email("oncall");
    let user = seed_user(pool, &email, "member").await;
    add_member(pool, project, user, "viewer").await;
    (project, email)
}

async fn monitor_state(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, bool) {
    sqlx::query_as("select current_status, consecutive_failures, last_checked_at is not null from monitors where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn incidents_of(
    pool: &sqlx::PgPool,
    monitor: Uuid,
) -> Vec<(String, String, Option<String>, bool)> {
    sqlx::query_as(
        "select status, title, cause, resolved_at is not null from incidents where monitor_id = $1 order by started_at",
    )
    .bind(monitor)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn claim_returns_only_due_unpaused_monitors_and_reschedules_them(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let due = seed_http_monitor(&pool, project, "due", "http://127.0.0.1:9/", 2).await;
    let paused = seed_http_monitor(&pool, project, "paused", "http://127.0.0.1:9/", 2).await;
    let later = seed_http_monitor(&pool, project, "later", "http://127.0.0.1:9/", 2).await;
    sqlx::query("update monitors set paused = true where id = $1")
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(later)
        .execute(&pool)
        .await
        .unwrap();

    let claimed = claim_due_monitors(&pool, 50).await.unwrap();
    assert_eq!(claimed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![due]);
    let seconds_ahead: f64 = sqlx::query_scalar(
        "select extract(epoch from next_check_at - now())::float8 from monitors where id = $1",
    )
    .bind(due)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        (55.0..=60.0).contains(&seconds_ahead),
        "rescheduled {seconds_ahead}s ahead, expected ~60"
    );

    assert!(
        claim_due_monitors(&pool, 50).await.unwrap().is_empty(),
        "a claimed monitor is not handed out twice"
    );
}

#[sqlx::test]
async fn claim_respects_the_limit_and_concurrent_claims_never_overlap(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    for i in 0..10 {
        seed_http_monitor(&pool, project, &format!("m{i}"), "http://127.0.0.1:9/", 2).await;
    }
    let (a, b) = tokio::join!(claim_due_monitors(&pool, 4), claim_due_monitors(&pool, 4));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!((a.len(), b.len()), (4, 4));
    assert!(
        a.iter().all(|m| b.iter().all(|n| n.id != m.id)),
        "the same monitor was claimed twice"
    );
    assert_eq!(
        claim_due_monitors(&pool, 50).await.unwrap().len(),
        2,
        "exactly the remaining two are left"
    );
}

#[sqlx::test]
async fn a_pending_monitor_that_succeeds_goes_up_silently(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(200).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 2).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(
        settled_mail(&recipient, 0).await.is_empty(),
        "pending -> up sends no email"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn failures_below_the_threshold_open_no_incident(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 3).await;

    run_due_checks(&state).await.unwrap();
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("pending".to_string(), 2, true)
    );
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// Review Focus: a monitor that stays down keeps exactly one open incident
/// and sends no further DOWN emails until it recovers.
#[sqlx::test]
async fn reaching_the_threshold_opens_one_incident_and_alerts_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 2).await;

    run_due_checks(&state).await.unwrap();
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "Checkout API is down".to_string(),
            Some("expected status 200-399, got 503".to_string()),
            false
        )]
    );
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, "[Nami] Checkout API is DOWN");
    assert!(
        mail[0].1.contains("expected status 200-399, got 503"),
        "{}",
        mail[0].1
    );
    let link = format!(
        "{}/projects/{project}/monitors/{monitor}",
        state.config.ui_base_url
    );
    assert!(
        mail[0].1.contains(&link),
        "body should link to {link}: {}",
        mail[0].1
    );

    // Still failing: no second incident, no second email.
    for _ in 0..2 {
        make_due(&pool, monitor).await;
        run_due_checks(&state).await.unwrap();
    }
    assert_eq!(
        monitor_state(&pool, monitor).await.1,
        4,
        "failures keep counting while down"
    );
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_pending_monitor_down_at_threshold_one_alerts_immediately(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(500).await;
    let monitor = seed_http_monitor(&pool, project, "Brand new", &server.uri(), 1).await;

    run_due_checks(&state).await.unwrap();

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn recovery_resolves_the_incident_and_sends_one_email(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    run_due_checks(&state).await.unwrap();
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");

    respond_with_status(&server, 200).await;
    make_due(&pool, monitor).await;
    run_due_checks(&state).await.unwrap();

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    let incidents = incidents_of(&pool, monitor).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!(
        (incidents[0].0.as_str(), incidents[0].3),
        ("resolved", true)
    );
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec![
            "[Nami] Checkout API is DOWN",
            "[Nami] Checkout API is back UP"
        ]
    );
    ch.drop_database().await.unwrap();
}

#[derive(Debug, Deserialize)]
struct StoredCheck {
    monitor_id: Uuid,
    status: String,
    status_code: Option<u16>,
    error: Option<String>,
}

#[sqlx::test]
async fn run_due_checks_writes_one_clickhouse_row_per_check(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let healthy = server_with_status(200).await;
    let broken = server_with_status(502).await;
    let up = seed_http_monitor(&pool, project, "up", &healthy.uri(), 2).await;
    let down = seed_http_monitor(&pool, project, "down", &broken.uri(), 2).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 2);

    let mut rows: Vec<StoredCheck> = ch
        .query_rows(
            "SELECT monitor_id, status, status_code, error FROM uptime_checks
             WHERE project_id = {project_id:UUID}",
            &[("project_id", project.to_string())],
        )
        .await
        .unwrap();
    rows.sort_by_key(|r| r.status.clone());
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0].monitor_id,
            rows[0].status.as_str(),
            rows[0].status_code
        ),
        (down, "down", Some(502))
    );
    assert_eq!(
        rows[0].error.as_deref(),
        Some("expected status 200-399, got 502")
    );
    assert_eq!(
        (
            rows[1].monitor_id,
            rows[1].status.as_str(),
            rows[1].status_code
        ),
        (up, "up", Some(200))
    );
    ch.drop_database().await.unwrap();
}

/// Review Focus: an SMTP outage during a transition does not abort the
/// pass — the incident is still recorded and the worker returns normally.
#[sqlx::test]
async fn an_smtp_outage_does_not_abort_the_pass(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    // Port 1 is never listening: every send fails fast.
    let state = AppState {
        mailer: Mailer::new("localhost", 1),
        ..state
    };
    let (project, _) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_clickhouse_outage_does_not_block_state_changes_or_alerts(pool: sqlx::PgPool) {
    // AppState::for_test's ClickHouse database does not exist: every insert fails.
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
}

fn failed_check() -> CheckOutcome {
    CheckOutcome {
        status: CheckStatus::Down,
        status_code: None,
        response_time_ms: None,
        error: Some("connection refused".to_string()),
        checked_at: Utc::now(),
    }
}

fn passed_check() -> CheckOutcome {
    CheckOutcome {
        status: CheckStatus::Up,
        status_code: Some(200),
        response_time_ms: Some(12),
        error: None,
        checked_at: Utc::now(),
    }
}

#[sqlx::test]
async fn results_for_monitors_paused_or_deleted_mid_check_are_discarded(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let paused = seed_http_monitor(&pool, project, "paused", "http://127.0.0.1:9/", 1).await;
    let deleted = seed_http_monitor(&pool, project, "deleted", "http://127.0.0.1:9/", 1).await;
    let claimed = claim_due_monitors(&pool, 50).await.unwrap();
    sqlx::query("update monitors set paused = true where id = $1")
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from monitors where id = $1")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();

    for monitor in &claimed {
        assert_eq!(
            apply_outcome(&pool, monitor, &failed_check())
                .await
                .unwrap(),
            None
        );
    }
    assert_eq!(
        monitor_state(&pool, paused).await,
        ("pending".to_string(), 0, false)
    );
    let incidents: i64 = sqlx::query_scalar("select count(*) from incidents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(incidents, 0);
}

// ---------------------------------------------------------------------------
// Beyond the brief's eleven tests: the design claims of "How the pieces fit"
// (atomic claim, row-locked state, one open incident, bounded concurrency)
// and the rules for text a monitored server controls, each pinned by a test.
// None of them waits for a fixed time to *make* something happen: they wait
// on database locks or poll for a condition. The only sleeps are "still
// blocked" checks, which can only fail if the code under test is wrong.
// ---------------------------------------------------------------------------

/// What `claim_due_monitors` hands to a check: the claim-time snapshot that
/// `apply_outcome` must not trust.
async fn claimed_snapshot(pool: &sqlx::PgPool, monitor: Uuid) -> Monitor {
    claim_due_monitors(pool, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.id == monitor)
        .expect("the monitor was due")
}

#[sqlx::test]
async fn claim_with_a_limit_takes_the_most_overdue_monitors(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let mut ids = Vec::new();
    for (name, overdue_seconds) in [("newest", 10.0_f64), ("oldest", 300.0), ("middle", 100.0)] {
        let id = seed_http_monitor(&pool, project, name, "http://127.0.0.1:9/", 2).await;
        sqlx::query(
            "update monitors set next_check_at = now() - make_interval(secs => $2) where id = $1",
        )
        .bind(id)
        .bind(overdue_seconds)
        .execute(&pool)
        .await
        .unwrap();
        ids.push((name, id));
    }
    let id_of = |name: &str| ids.iter().find(|(n, _)| *n == name).unwrap().1;

    let mut claimed: Vec<Uuid> = claim_due_monitors(&pool, 2)
        .await
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();

    claimed.sort();
    let mut expected = vec![id_of("oldest"), id_of("middle")];
    expected.sort();
    assert_eq!(claimed, expected, "the two most overdue monitors go first");
}

/// `for update skip locked`: a monitor that another transaction holds (a
/// claim that has not committed yet) is skipped, not waited for, and stays
/// due. Without `skip locked` the claim blocks on the held row and the
/// timeout fires.
#[sqlx::test]
async fn claim_skips_monitors_that_another_transaction_still_holds(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let held = seed_http_monitor(&pool, project, "held", "http://127.0.0.1:9/", 2).await;
    let free = seed_http_monitor(&pool, project, "free", "http://127.0.0.1:9/", 2).await;
    let mut other = pool.begin().await.unwrap();
    sqlx::query("select id from monitors where id = $1 for update")
        .bind(held)
        .fetch_one(&mut *other)
        .await
        .unwrap();

    let claimed = tokio::time::timeout(Duration::from_secs(10), claim_due_monitors(&pool, 50))
        .await
        .expect("a claim must skip rows another claim holds, not wait for them")
        .unwrap();

    assert_eq!(claimed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![free]);
    other.commit().await.unwrap();
    let next = claim_due_monitors(&pool, 50).await.unwrap();
    assert_eq!(
        next.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![held],
        "the skipped monitor was neither claimed nor rescheduled"
    );
}

/// Three passes started together (what overlapping ticks or several engine
/// instances amount to) account for every due monitor exactly once: one HTTP
/// request and one ClickHouse row each. An end-to-end tally, not a race test:
/// the three claims rarely overlap in time (each is over within about a
/// millisecond, and removing `skip locked` from the claim does not reliably
/// make this fail), so that guarantee is pinned deterministically by
/// `claim_skips_monitors_that_another_transaction_still_holds`.
#[sqlx::test]
async fn passes_started_together_check_each_monitor_once_and_store_one_row_each(
    pool: sqlx::PgPool,
) {
    #[derive(Deserialize)]
    struct Count {
        n: u64,
    }

    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let server = server_with_status(200).await;
    for i in 0..12 {
        seed_http_monitor(&pool, project, &format!("m{i}"), &server.uri(), 2).await;
    }

    let (a, b, c) = tokio::join!(
        run_due_checks(&state),
        run_due_checks(&state),
        run_due_checks(&state)
    );

    assert_eq!(a.unwrap() + b.unwrap() + c.unwrap(), 12);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        12,
        "some monitor was checked twice"
    );
    let stored: Vec<Count> = ch
        .query_rows(
            "SELECT count() AS n FROM uptime_checks WHERE project_id = {project_id:UUID}",
            &[("project_id", project.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(stored[0].n, 12);
    ch.drop_database().await.unwrap();
}

/// Answers every request with `200` after holding it for `hold`, and
/// remembers the most requests it ever held at once.
struct HoldingServer {
    port: u16,
    peak: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
}

impl HoldingServer {
    async fn start(hold: Duration) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (in_flight, peak, served) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let counters = (in_flight, peak.clone(), served.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (in_flight, peak, served) = counters.clone();
                tokio::spawn(async move {
                    peak.fetch_max(
                        in_flight.fetch_add(1, Ordering::SeqCst) + 1,
                        Ordering::SeqCst,
                    );
                    let mut request = [0u8; 1024];
                    let _ = stream.read(&mut request).await;
                    tokio::time::sleep(hold).await;
                    // Leave the count BEFORE answering: the client frees its
                    // slot only once it has the answer, so the count can
                    // never exceed the number of slots the client holds.
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    served.fetch_add(1, Ordering::SeqCst);
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { port, peak, served }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

/// More due monitors than slots: every one is checked, never more than
/// `MAX_IN_FLIGHT` at a time. (`peak <= MAX_IN_FLIGHT` cannot fail for a
/// correct worker however slow the machine is; without the semaphore all 25
/// connections are held at once and it fails.)
#[sqlx::test]
async fn a_pass_never_runs_more_than_max_in_flight_checks(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let monitors = MAX_IN_FLIGHT + 5;
    let server = HoldingServer::start(Duration::from_millis(600)).await;
    for i in 0..monitors {
        seed_http_monitor(&pool, project, &format!("m{i}"), &server.url(), 2).await;
    }

    assert_eq!(run_due_checks(&state).await.unwrap(), monitors);

    assert_eq!(server.served.load(Ordering::SeqCst), monitors);
    let peak = server.peak.load(Ordering::SeqCst);
    assert!(
        peak <= MAX_IN_FLIGHT,
        "{peak} checks were in flight at once, the limit is {MAX_IN_FLIGHT}"
    );
    let checked: i64 = sqlx::query_scalar(
        "select count(*) from monitors where current_status = 'up' and last_checked_at is not null",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(checked, monitors as i64);
    ch.drop_database().await.unwrap();
}

/// The state comes from the locked row, not from the claim-time snapshot:
/// edits made while the check was in flight count.
#[sqlx::test]
async fn apply_outcome_judges_by_the_monitors_current_name_and_threshold(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "Old name", "http://127.0.0.1:9/", 3).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    sqlx::query("update monitors set name = 'New name', confirmation_threshold = 1 where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();

    let transition = apply_outcome(&pool, &snapshot, &failed_check())
        .await
        .unwrap()
        .expect("one failure reaches the threshold the monitor has now");

    assert_eq!(transition.kind, TransitionKind::Down);
    assert_eq!(transition.name, "New name");
    assert_eq!(incidents_of(&pool, monitor).await[0].1, "New name is down");
}

/// `apply_outcome` takes the row lock before it reads anything: while another
/// writer holds the row it waits, and then applies the result on top of what
/// that writer committed. A version that judged by its claim-time snapshot
/// (pending, no failures) would open a second incident here.
#[sqlx::test]
async fn apply_outcome_waits_for_the_row_lock_and_builds_on_what_it_finds(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "locked", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;

    let mut other = pool.begin().await.unwrap();
    sqlx::query("select id from monitors where id = $1 for update")
        .bind(monitor)
        .fetch_one(&mut *other)
        .await
        .unwrap();
    let applying = tokio::spawn({
        let pool = pool.clone();
        async move { apply_outcome(&pool, &snapshot, &failed_check()).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !applying.is_finished(),
        "apply_outcome must wait for the row lock"
    );

    // The other writer, meanwhile, took the monitor down (and so already
    // alerted): this check's failure is just one more.
    sqlx::query(
        "update monitors set current_status = 'down', consecutive_failures = 3 where id = $1",
    )
    .bind(monitor)
    .execute(&mut *other)
    .await
    .unwrap();
    other.commit().await.unwrap();

    assert_eq!(applying.await.unwrap().unwrap(), None);
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("down".to_string(), 4, true)
    );
    assert!(incidents_of(&pool, monitor).await.is_empty());
}

/// Applies every outcome to the monitor at the same moment: they all start
/// while another transaction holds the monitor's row, so every one of them is
/// waiting on the lock when it is released. Returns the transitions they
/// produced. (The pause only makes the test strong: if a task were slow to
/// reach the lock the result would still be right, just less of a test.)
async fn apply_together(
    pool: &sqlx::PgPool,
    snapshot: &Monitor,
    outcomes: &[CheckOutcome],
) -> Vec<Transition> {
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("select id from monitors where id = $1 for update")
        .bind(snapshot.id)
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    let mut applying = tokio::task::JoinSet::new();
    for outcome in outcomes {
        let (pool, snapshot, outcome) = (pool.clone(), snapshot.clone(), outcome.clone());
        applying.spawn(async move { apply_outcome(&pool, &snapshot, &outcome).await });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.commit().await.unwrap();
    let mut transitions = Vec::new();
    while let Some(done) = applying.join_next().await {
        transitions.extend(done.unwrap().unwrap());
    }
    transitions
}

async fn last_checked_at(pool: &sqlx::PgPool, monitor: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select last_checked_at from monitors where id = $1")
        .bind(monitor)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Six failures for one monitor arriving together (a monitor whose edits kept
/// rescheduling it). Whatever order they take the row lock in, exactly one of
/// them is the transition that opens the incident and alerts, and the newest
/// result is the one left recorded (older ones that arrive after it are stale).
#[sqlx::test]
async fn results_applied_at_the_same_time_open_one_incident_and_alert_once(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "busy", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let outcomes: Vec<CheckOutcome> = (1..=6).map(|i| at(failed_check(), i)).collect();

    let transitions = apply_together(&pool, &snapshot, &outcomes).await;

    assert_eq!(transitions.len(), 1, "{transitions:?}");
    assert_eq!(transitions[0].kind, TransitionKind::Down);
    let (status, failures, _) = monitor_state(&pool, monitor).await;
    assert_eq!(status, "down");
    assert!((1..=6).contains(&failures), "{failures} failures");
    assert_eq!(
        last_checked_at(&pool, monitor).await,
        Some(outcomes[5].checked_at)
    );
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
}

/// The same for recovery: four successes arriving together for a monitor that
/// is down recover it once, and only one of them reports the recovery.
#[sqlx::test]
async fn results_applied_at_the_same_time_recover_a_monitor_once(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "flappy", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let down = apply_outcome(&pool, &snapshot, &at(failed_check(), 0))
        .await
        .unwrap();
    assert_eq!(down.map(|t| t.kind), Some(TransitionKind::Down));
    let outcomes: Vec<CheckOutcome> = (1..=4).map(|i| at(passed_check(), i)).collect();

    let transitions = apply_together(&pool, &snapshot, &outcomes).await;

    assert_eq!(transitions.len(), 1, "{transitions:?}");
    assert_eq!(transitions[0].kind, TransitionKind::Recovered);
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    assert_eq!(
        last_checked_at(&pool, monitor).await,
        Some(outcomes[3].checked_at)
    );
    let incidents = incidents_of(&pool, monitor).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!(
        (incidents[0].0.as_str(), incidents[0].3),
        ("resolved", true)
    );
}

/// If an incident is somehow already open, the monitor still goes down but
/// nobody is told a second time.
#[sqlx::test]
async fn a_down_result_that_finds_an_incident_already_open_does_not_alert_again(
    pool: sqlx::PgPool,
) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    sqlx::query(
        "insert into incidents (project_id, monitor_id, title, cause)
         values ($1, $2, 'already open', 'earlier cause')",
    )
    .bind(project)
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        apply_outcome(&pool, &snapshot, &failed_check())
            .await
            .unwrap(),
        None
    );

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("down".to_string(), 1, true)
    );
    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "already open".to_string(),
            Some("earlier cause".to_string()),
            false
        )]
    );
}

/// A result that cannot be applied (here: the incident insert is rejected)
/// leaves its monitor completely untouched, and the pass carries on with the
/// rest.
#[sqlx::test]
async fn a_result_that_fails_to_apply_is_rolled_back_and_does_not_stop_the_pass(
    pool: sqlx::PgPool,
) {
    sqlx::query(
        "create function reject_poison() returns trigger language plpgsql as $$
         begin
             if new.title like 'Poison%' then raise exception 'rejected on purpose'; end if;
             return new;
         end $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger reject_poison before insert on incidents
         for each row execute function reject_poison()",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let poisoned = seed_http_monitor(&pool, project, "Poison", &server.uri(), 1).await;
    let healthy = seed_http_monitor(&pool, project, "Healthy", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 2);

    assert_eq!(
        monitor_state(&pool, poisoned).await,
        ("pending".to_string(), 0, false),
        "the failed transaction left no trace"
    );
    assert_eq!(monitor_state(&pool, healthy).await.0, "down");
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec!["[Nami] Healthy is DOWN"]
    );
    ch.drop_database().await.unwrap();
}

/// Every member of the project, whatever the role, is mailed once; members of
/// other projects are not.
#[sqlx::test]
async fn every_project_member_is_mailed_once_and_nobody_else(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (admin, viewer, outsider) = (
        unique_email("admin"),
        unique_email("viewer"),
        unique_email("outsider"),
    );
    add_member(
        &pool,
        project,
        seed_user(&pool, &admin, "member").await,
        "admin",
    )
    .await;
    add_member(
        &pool,
        project,
        seed_user(&pool, &viewer, "member").await,
        "viewer",
    )
    .await;
    add_member(
        &pool,
        other_project,
        seed_user(&pool, &outsider, "member").await,
        "admin",
    )
    .await;
    let server = server_with_status(503).await;
    seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    run_due_checks(&state).await.unwrap();

    for member in [&admin, &viewer] {
        let mail = settled_mail(member, 1).await;
        assert_eq!(mail.len(), 1, "{member}: {mail:?}");
        assert_eq!(mail[0].0, "[Nami] Checkout API is DOWN");
    }
    assert!(mail_for(&outsider).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// A project nobody belongs to still gets its incident; there is simply
/// nobody to tell.
#[sqlx::test]
async fn a_project_without_members_still_records_the_incident(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    ch.drop_database().await.unwrap();
}

/// A monitor's name is validated on the way in, but a row can reach the
/// database some other way: the alert still goes to its recipient only, with
/// a one-line subject. Judged on the stored message itself: its recipients as
/// Mailpit recorded them (To, Cc, and Bcc, where an envelope recipient that no
/// header names would show up) and its header block.
#[sqlx::test]
async fn a_name_written_around_the_api_cannot_inject_headers_or_recipients(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let victim = unique_email("victim");
    let server = server_with_status(503).await;
    let name = format!("API\r\nBcc: {victim}\r\nCc: {victim}");
    seed_http_monitor(&pool, project, &name, &server.uri(), 1).await;

    run_due_checks(&state).await.unwrap();

    settled_mail(&recipient, 1).await;
    let stored = stored_mail_for(&recipient).await;
    assert_eq!(stored.len(), 1);
    let message = &stored[0];
    assert_eq!(
        message.subject,
        format!("[Nami] APIBcc: {victim}Cc: {victim} is DOWN")
    );
    assert_eq!(message.to, vec![recipient.clone()]);
    assert!(message.cc.is_empty(), "{:?}", message.cc);
    assert!(message.bcc.is_empty(), "{:?}", message.bcc);
    let (headers, _) = message.raw.split_once("\n\n").expect("a header block");
    let headers = headers.to_ascii_lowercase();
    assert!(!headers.contains("\nbcc:"), "{headers}");
    assert!(!headers.contains("\ncc:"), "{headers}");
    assert_eq!(headers.matches("\nsubject:").count(), 1, "{headers}");
    ch.drop_database().await.unwrap();
}

/// Text a hostile monitored server can influence: SQL, header and SMTP
/// command lookalikes (with every line-ending flavour), markup.
fn hostile_text(victim: &str) -> String {
    format!(
        "x'); DROP TABLE incidents; --\r\nBcc: {victim}\r\nContent-Type: text/html\r\n\r\n\
         <script>alert(1)</script>\r\n.\r\nRCPT TO:<{victim}>\r\n\n.\nQUIT\n\r.\rQUIT\r"
    )
}

/// `hostile_text` as it is stored and mailed: every control character
/// (NUL, CR, LF, ...) replaced by a space, everything else untouched.
fn sanitized(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The error text goes into `incidents.cause` as a bound parameter, with its
/// control characters replaced by spaces, and into the email as a plain-text
/// body: the table survives, the member gets exactly one message, nothing it
/// contains becomes a header, a recipient, an SMTP command or HTML.
#[sqlx::test]
async fn a_hostile_error_text_is_sanitized_stored_and_mailed_as_plain_text(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let victim = unique_email("victim");
    let monitor = seed_http_monitor(&pool, project, "Checkout API", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let hostile = hostile_text(&victim);
    let outcome = CheckOutcome {
        error: Some(hostile.clone()),
        ..failed_check()
    };

    let transition = apply_outcome(&pool, &snapshot, &outcome)
        .await
        .unwrap()
        .expect("the monitor goes down");
    notify_transition(&state, &transition).await;

    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "Checkout API is down".to_string(),
            Some(sanitized(&hostile)),
            false
        )],
        "one line of text, and the table is still there"
    );
    settled_mail(&recipient, 1).await;
    let stored = stored_mail_for(&recipient).await;
    assert_eq!(stored.len(), 1);
    let message = &stored[0];
    assert_eq!(message.subject, "[Nami] Checkout API is DOWN");
    assert!(
        message.text.contains("<script>alert(1)</script>"),
        "{}",
        message.text
    );
    assert!(
        message
            .text
            .contains(&format!("Cause: {}\n", sanitized(&hostile))),
        "the cause is one line: {:?}",
        message.text
    );
    assert_eq!(message.to, vec![recipient.clone()]);
    assert!(message.cc.is_empty(), "{:?}", message.cc);
    assert!(message.bcc.is_empty(), "{:?}", message.bcc);
    assert!(
        mail_for(&victim).await.is_empty(),
        "the text named an address; it must not have been mailed, in a message of its own either"
    );
    let (headers, _body) = message
        .raw
        .split_once("\n\n")
        .expect("headers, a blank line, a body");
    let headers = headers.to_ascii_lowercase();
    assert!(!headers.contains("bcc:"), "{headers}");
    assert!(!headers.contains("text/html"), "{headers}");
    assert!(!headers.contains("multipart/"), "{headers}");
    assert_eq!(headers.matches("\nsubject:").count(), 1, "{headers}");
}

/// ClickHouse takes rows as a JSON body, never as SQL: hostile text is stored
/// and read back unchanged, NUL and all.
#[tokio::test]
async fn hostile_error_text_round_trips_through_clickhouse_unchanged() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let row = UptimeCheckRow {
        monitor_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        // `checked_at` is DateTime64(3): keep whole milliseconds.
        checked_at: DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap(),
        status: CheckStatus::Down,
        status_code: None,
        response_time_ms: None,
        error: Some(format!(
            "{}\0 \"quoted\" \\ back\\slash 🦀 {{param}}",
            hostile_text("victim@example.com")
        )),
    };

    insert_checks(&ch, std::slice::from_ref(&row))
        .await
        .unwrap();

    let stored: Vec<UptimeCheckRow> = ch
        .query_rows(
            "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms, error
             FROM uptime_checks WHERE monitor_id = {monitor_id:UUID}",
            &[("monitor_id", row.monitor_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(stored, vec![row]);
    ch.drop_database().await.unwrap();
}

/// `run_forever` runs a pass on its own: the first tick fires at once, so
/// polling for the result is all the test needs.
#[sqlx::test]
async fn run_forever_checks_due_monitors_by_itself(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let server = server_with_status(200).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 2).await;

    let worker = tokio::spawn(run_forever(state.clone()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !monitor_state(&pool, monitor).await.2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    worker.abort();

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    ch.drop_database().await.unwrap();
}

// ---------------------------------------------------------------------------
// Fix round 1: transitions out of `up`, stale results, untrusted cause text,
// and a hung ClickHouse.
// ---------------------------------------------------------------------------

/// A project with one member and a monitor (confirmation threshold 2) that has
/// been checked once against a healthy server, so it is `up`.
async fn up_monitor(pool: &sqlx::PgPool, state: &AppState) -> (MockServer, Uuid, String) {
    let (project, recipient) = project_with_recipient(pool).await;
    let server = server_with_status(200).await;
    let monitor = seed_http_monitor(pool, project, "Checkout API", &server.uri(), 2).await;
    run_due_checks(state).await.unwrap();
    assert_eq!(
        monitor_state(pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    (server, monitor, recipient)
}

/// One more pass, with the server now answering `status`.
async fn check_again(
    pool: &sqlx::PgPool,
    state: &AppState,
    server: &MockServer,
    monitor: Uuid,
    status: u16,
) {
    respond_with_status(server, status).await;
    make_due(pool, monitor).await;
    run_due_checks(state).await.unwrap();
}

#[sqlx::test]
async fn an_up_monitor_with_one_failure_stays_up_and_says_nothing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (server, monitor, recipient) = up_monitor(&pool, &state).await;

    check_again(&pool, &state, &server, monitor, 503).await;

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 1, true)
    );
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty());
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_up_monitor_that_fails_up_to_the_threshold_goes_down_with_one_incident_and_one_email(
    pool: sqlx::PgPool,
) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (server, monitor, recipient) = up_monitor(&pool, &state).await;

    check_again(&pool, &state, &server, monitor, 503).await;
    check_again(&pool, &state, &server, monitor, 503).await;

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("down".to_string(), 2, true)
    );
    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "Checkout API is down".to_string(),
            Some("expected status 200-399, got 503".to_string()),
            false
        )]
    );
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec!["[Nami] Checkout API is DOWN"]
    );
    ch.drop_database().await.unwrap();
}

/// Failures only count while they are consecutive: a success in between
/// resets the count of a monitor that is not down.
#[sqlx::test]
async fn a_success_between_failures_resets_the_count_of_an_up_monitor(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (server, monitor, recipient) = up_monitor(&pool, &state).await;

    check_again(&pool, &state, &server, monitor, 503).await;
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 1, true)
    );
    check_again(&pool, &state, &server, monitor, 200).await;
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    check_again(&pool, &state, &server, monitor, 503).await;

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 1, true),
        "one failure after a success is the first failure, not the second"
    );
    assert!(incidents_of(&pool, monitor).await.is_empty());
    assert!(settled_mail(&recipient, 0).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// A result for a check that started before the last one applied is stale
/// (the two overlapped and finished in the other order): it must not undo the
/// newer one. Equal timestamps count as stale too.
#[sqlx::test]
async fn a_result_older_than_the_last_applied_check_changes_nothing(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let newer_failure = at(failed_check(), 100);
    assert!(
        apply_outcome(&pool, &snapshot, &newer_failure)
            .await
            .unwrap()
            .is_some()
    );

    let older_success = apply_outcome(&pool, &snapshot, &at(passed_check(), 50))
        .await
        .unwrap();

    assert_eq!(older_success, None);
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("down".to_string(), 1, true)
    );
    assert_eq!(
        last_checked_at(&pool, monitor).await,
        Some(newer_failure.checked_at)
    );
    let incidents = incidents_of(&pool, monitor).await;
    assert_eq!(
        (incidents[0].0.as_str(), incidents[0].3),
        ("open", false),
        "the incident was not resolved by the stale success"
    );
}

#[sqlx::test]
async fn a_result_with_the_same_timestamp_as_the_last_applied_check_is_discarded(
    pool: sqlx::PgPool,
) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    apply_outcome(&pool, &snapshot, &at(failed_check(), 100))
        .await
        .unwrap();

    let same_instant = apply_outcome(&pool, &snapshot, &at(passed_check(), 100))
        .await
        .unwrap();

    assert_eq!(same_instant, None);
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
}

/// Stale is about order, not age: a never-checked monitor takes whatever it
/// gets first, and a newer result after an older one is applied normally.
#[sqlx::test]
async fn the_first_result_and_any_newer_result_are_applied(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;

    let ancient = apply_outcome(&pool, &snapshot, &at(failed_check(), -1_000_000))
        .await
        .unwrap();
    assert_eq!(ancient.map(|t| t.kind), Some(TransitionKind::Down));
    let newer = apply_outcome(&pool, &snapshot, &at(passed_check(), 0))
        .await
        .unwrap();

    assert_eq!(newer.map(|t| t.kind), Some(TransitionKind::Recovered));
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
}

/// The incident's cause and the email are one line of text: control
/// characters, NUL included (Postgres text cannot hold it), become spaces, and
/// the transition still commits.
#[sqlx::test]
async fn a_nul_in_the_error_text_does_not_stop_the_transition(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let outcome = CheckOutcome {
        error: Some("bad\0text\r\nmore".to_string()),
        ..failed_check()
    };

    let transition = apply_outcome(&pool, &snapshot, &outcome)
        .await
        .unwrap()
        .expect("the monitor still goes down");

    assert_eq!(transition.cause.as_deref(), Some("bad text  more"));
    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("down".to_string(), 1, true)
    );
    assert_eq!(
        incidents_of(&pool, monitor).await,
        vec![(
            "open".to_string(),
            "api is down".to_string(),
            Some("bad text  more".to_string()),
            false
        )]
    );
}

/// The history insert comes last: with ClickHouse accepting connections and
/// never answering (the client's own timeout is 30 s), the state change and
/// the alert still happen while the pass is stuck on the insert.
#[sqlx::test]
async fn a_hung_clickhouse_does_not_delay_state_changes_or_alerts(pool: sqlx::PgPool) {
    let (clickhouse_port, _silent) = silent_server().await;
    let state = AppState {
        clickhouse: ClickHouse::new(
            &format!("http://127.0.0.1:{clickhouse_port}"),
            "nami",
            "nami",
            "nami",
        ),
        ..AppState::for_test(pool.clone())
    };
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    let pass = tokio::spawn({
        let state = state.clone();
        async move { run_due_checks(&state).await }
    });
    let mail = settled_mail(&recipient, 1).await;

    assert_eq!(mail.len(), 1, "the alert did not wait for ClickHouse");
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert!(
        !pass.is_finished(),
        "the pass is still waiting on the silent ClickHouse"
    );
    pass.abort();
}

/// An SMTP server that is up but hung must not hold up the checks: alerts are
/// sent by background tasks, so a monitor can go down and recover in the time
/// the two passes need while both alerts are still waiting on the silent
/// server. (Before, a pass sent its alerts itself and so waited for them.)
#[sqlx::test]
async fn a_hung_smtp_server_does_not_hold_up_the_checks(pool: sqlx::PgPool) {
    let (smtp_port, _silent) = silent_server().await;
    let (state, ch) = state_with_clickhouse(&pool).await;
    // The default limit for one email is 10 s: far longer than the passes take.
    let state = AppState {
        mailer: Mailer::new("127.0.0.1", smtp_port),
        ..state
    };
    let (project, _recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    let notifications = Notifications::new();

    let started = Instant::now();
    run_pass(&state, &notifications).await.unwrap();
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    respond_with_status(&server, 200).await;
    make_due(&pool, monitor).await;
    run_pass(&state, &notifications).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        monitor_state(&pool, monitor).await,
        ("up".to_string(), 0, true)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "two passes took {elapsed:?} while the SMTP server never answered"
    );
    assert_eq!(
        notifications.reap(),
        2,
        "both alerts are still waiting on the silent server, off the checks' path"
    );
    ch.drop_database().await.unwrap();
}

/// Such alerts are given up on after the limit for one email, one recipient
/// after the other, so a pass that waits for its alerts (`run_due_checks`)
/// ends too.
#[sqlx::test]
async fn alerts_to_a_hung_smtp_server_are_given_up_on_after_the_timeout(pool: sqlx::PgPool) {
    let (smtp_port, _silent) = silent_server().await;
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        mailer: Mailer::with_timeout("127.0.0.1", smtp_port, Duration::from_millis(700)),
        ..state
    };
    let (project, _first) = project_with_recipient(&pool).await;
    let second = seed_user(&pool, &unique_email("second"), "member").await;
    add_member(&pool, project, second, "viewer").await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    let started = Instant::now();
    let ran = tokio::time::timeout(Duration::from_secs(20), run_due_checks(&state))
        .await
        .expect("alerts must be given up on, not waited for forever");
    let elapsed = started.elapsed();

    assert_eq!(ran.unwrap(), 1);
    assert!(
        elapsed >= Duration::from_millis(1_300),
        "two recipients, 700 ms each: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    ch.drop_database().await.unwrap();
}

/// Names and causes are not always ASCII: the mail declares UTF-8 and both
/// arrive as written.
#[sqlx::test]
async fn a_non_ascii_name_and_cause_are_mailed_intact(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "Café ☕ API", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let outcome = CheckOutcome {
        error: Some("échec de connexion … 日本語 🦀".to_string()),
        ..failed_check()
    };
    let transition = apply_outcome(&pool, &snapshot, &outcome)
        .await
        .unwrap()
        .expect("the monitor goes down");

    notify_transition(&state, &transition).await;

    settled_mail(&recipient, 1).await;
    let stored = stored_mail_for(&recipient).await;
    assert_eq!(stored.len(), 1);
    let message = &stored[0];
    assert_eq!(message.subject, "[Nami] Café ☕ API is DOWN");
    assert!(
        message
            .text
            .contains("Cause: échec de connexion … 日本語 🦀\n"),
        "{:?}",
        message.text
    );
    let (headers, _) = message.raw.split_once("\n\n").expect("a header block");
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("\ncontent-type: text/plain; charset=utf-8\n"),
        "{headers}"
    );
}

/// One line per failed transition, naming the monitor and the kind of
/// transition; the cause is logged once, by the mailer, and neither line
/// carries the recipient's address.
#[sqlx::test]
async fn a_failed_alert_is_logged_once_and_without_the_recipients_address(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install();
    // Port 1 is never listening: every send fails at once.
    let state = AppState {
        mailer: Mailer::new("localhost", 1),
        ..AppState::for_test(pool.clone())
    };
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    run_due_checks(&state).await.unwrap();

    let lines = log.lines();
    let all = lines.join("\n");
    assert!(
        !all.contains(&recipient),
        "the address is in the log:\n{all}"
    );
    let summaries: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("alert email not delivered to every recipient"))
        .collect();
    assert_eq!(summaries.len(), 1, "{all}");
    assert!(
        summaries[0].contains(&monitor.to_string()),
        "{}",
        summaries[0]
    );
    assert!(summaries[0].contains("Down"), "{}", summaries[0]);
    assert!(
        !all.contains("validation error"),
        "the mailer's generic error is not logged a second time:\n{all}"
    );
    let causes = lines
        .iter()
        .filter(|line| line.contains("failed to send email"))
        .count();
    assert_eq!(causes, 1, "the cause is logged once, by the mailer:\n{all}");
}

/// When `limit` alert batches are already sending, a new one is refused: the
/// pass neither waits nor loses the state change, and says what was dropped.
#[sqlx::test]
async fn alerts_are_dropped_and_logged_when_too_many_batches_are_still_sending(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install();
    let (state, ch) = state_with_clickhouse(&pool).await;
    let (project, recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;

    run_pass(&state, &Notifications::with_limit(0))
        .await
        .unwrap();

    assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    assert_eq!(incidents_of(&pool, monitor).await.len(), 1);
    assert!(settled_mail(&recipient, 0).await.is_empty());
    let dropped: Vec<_> = log
        .lines()
        .into_iter()
        .filter(|line| line.contains("alerts dropped"))
        .collect();
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains(&monitor.to_string()), "{}", dropped[0]);
    ch.drop_database().await.unwrap();
}

/// A burst of transitions in one pass is one alert batch: one sending task,
/// handed each alert as its check ends. With the SMTP server silent, three
/// monitors going down take one place in the bounded set, not three.
#[sqlx::test]
async fn a_pass_sends_all_its_alerts_from_one_task(pool: sqlx::PgPool) {
    let (smtp_port, _silent) = silent_server().await;
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        mailer: Mailer::new("127.0.0.1", smtp_port),
        ..state
    };
    let (project, _recipient) = project_with_recipient(&pool).await;
    let server = server_with_status(503).await;
    let mut monitors = Vec::new();
    for i in 0..3 {
        monitors.push(seed_http_monitor(&pool, project, &format!("m{i}"), &server.uri(), 1).await);
    }
    let notifications = Notifications::new();

    assert_eq!(run_pass(&state, &notifications).await.unwrap(), 3);

    for monitor in monitors {
        assert_eq!(monitor_state(&pool, monitor).await.0, "down");
    }
    assert_eq!(
        notifications.reap(),
        1,
        "the pass's three alerts wait in one task"
    );
    ch.drop_database().await.unwrap();
}

/// An alert leaves as soon as its outcome is applied, not when the pass ends:
/// the refused monitor's DOWN email arrives while the pass still waits for the
/// hung monitor's check (5 s timeout).
#[sqlx::test]
async fn an_alert_does_not_wait_for_the_rest_of_its_pass(pool: sqlx::PgPool) {
    let (hung_port, _silent) = silent_server().await;
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    seed_http_monitor(
        &pool,
        project,
        "Hung API",
        &format!("http://127.0.0.1:{hung_port}/"),
        1,
    )
    .await;
    // Port 9 refuses connections: the check fails at once.
    seed_http_monitor(&pool, project, "Refused API", "http://127.0.0.1:9/", 1).await;

    let pass = tokio::spawn({
        let state = state.clone();
        async move { run_due_checks(&state).await }
    });
    let deadline = Instant::now() + Duration::from_secs(4);
    while mail_for(&recipient).await.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mail = mail_for(&recipient).await;

    assert!(
        !pass.is_finished(),
        "the pass still waits for the hung check"
    );
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec!["[Nami] Refused API is DOWN"]
    );
    pass.abort();
}

/// Flap control covers monitors too: a monitor that goes down and up four
/// times in a row costs each member one DOWN + recovery pair; a monitor that
/// then stays down is reported once the window is over.
#[sqlx::test]
async fn a_flapping_monitor_sends_one_pair_per_window_and_reports_staying_down_later(
    pool: sqlx::PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "Flaky API", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(&pool, monitor).await;
    let mut second = 0;
    for cycle in 0..4 {
        for check in [failed_check(), passed_check()] {
            second += 60;
            let transition = apply_outcome(&pool, &snapshot, &at(check, second))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("cycle {cycle}: a transition"));
            notify_transition(&state, &transition).await;
        }
    }
    let mail = settled_mail(&recipient, 2).await;
    let subjects: Vec<&str> = mail.iter().map(|(subject, _)| subject.as_str()).collect();
    assert_eq!(
        subjects,
        vec!["[Nami] Flaky API is DOWN", "[Nami] Flaky API is back UP"]
    );

    // Down once more, and it stays down: held back now, sent after the window.
    let transition = apply_outcome(&pool, &snapshot, &at(failed_check(), second + 60))
        .await
        .unwrap()
        .expect("down again");
    notify_transition(&state, &transition).await;
    let now = std::time::Instant::now();
    assert_eq!(
        send_deferred(&state, now).await,
        0,
        "the window is not over"
    );
    assert_eq!(send_deferred(&state, now + ALERT_FLAP_WINDOW).await, 1);
    assert_eq!(
        send_deferred(&state, now + ALERT_FLAP_WINDOW * 2).await,
        0,
        "sent once"
    );
    let mail = settled_mail(&recipient, 3).await;
    assert_eq!(mail.len(), 3);
    assert_eq!(mail[2].0, "[Nami] Flaky API is DOWN");
}

/// A monitor whose DOWN email the flap control is holding back: down, up,
/// down again within the window.
async fn monitor_with_held_down(state: &AppState, pool: &sqlx::PgPool, project: Uuid) -> Uuid {
    let monitor = seed_http_monitor(pool, project, "Held API", "http://127.0.0.1:9/", 1).await;
    let snapshot = claimed_snapshot(pool, monitor).await;
    for (second, check) in [
        (60, failed_check()),
        (120, passed_check()),
        (180, failed_check()),
    ] {
        let transition = apply_outcome(pool, &snapshot, &at(check, second))
            .await
            .unwrap()
            .expect("a transition");
        notify_transition(state, &transition).await;
    }
    monitor
}

/// A held-back DOWN is sent after the window only for a subject still down:
/// not once it is paused, deleted, or inside a maintenance window (scoped to
/// it or project-wide).
#[sqlx::test]
async fn a_held_down_is_dropped_for_paused_deleted_or_maintained_monitors(pool: sqlx::PgPool) {
    let after_window = || std::time::Instant::now() + ALERT_FLAP_WINDOW;
    let window = |project: Uuid, monitor: Option<Uuid>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
                 values ($1, $2, 'maintenance', 'once', now() - interval '5 minutes', now() + interval '1 hour')",
            )
            .bind(project)
            .bind(monitor)
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    // Still down: sent.
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    monitor_with_held_down(&state, &pool, project).await;
    assert_eq!(send_deferred(&state, after_window()).await, 1);

    let cases: [&str; 4] = [
        "paused",
        "deleted",
        "scoped maintenance",
        "project maintenance",
    ];
    for case in cases {
        let state = AppState::for_test(pool.clone());
        let project = seed_project(&pool).await;
        let monitor = monitor_with_held_down(&state, &pool, project).await;
        match case {
            "paused" => {
                sqlx::query("update monitors set paused = true where id = $1")
                    .bind(monitor)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "deleted" => {
                sqlx::query("delete from monitors where id = $1")
                    .bind(monitor)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "scoped maintenance" => window(project, Some(monitor)).await,
            _ => window(project, None).await,
        }
        assert_eq!(send_deferred(&state, after_window()).await, 0, "{case}");
        assert!(
            state.alert_throttle.due(after_window()).is_empty(),
            "{case}: dropped"
        );
    }
}
